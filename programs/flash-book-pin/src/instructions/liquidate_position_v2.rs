//! liquidate_position_v2 — inject a forced-liquidation order for an UNHEALTHY
//! position into the book at the synthetic penalty price, and pay the caller the
//! Dutch-auction liquidator reward. The close itself settles when the matcher
//! fills the injected order (`apply_fill`). Permissionless; self-liquidation is
//! forbidden (M-2). Faithful port of the Anchor `liquidate_position_v2`, NON-JIT
//! (the JIT auction only ever IMPROVES the price; falling back to the synthetic
//! limit is the correct, faithful subset) and MARK-only (pin has no separate
//! oracle — `worse_of_health_price` degenerates to the mark when oracle == 0).
//!
//! The settlement pure-math is the host-tested + Kani-proven `crate::liquidation`
//! (`worse_of_health_price`, `liquidation_penalty_price`, `reward_bps_effective`,
//! `liquidator_reward_lots`); the per-position timestamps live in the separate
//! `PositionLiquidationState` PDA (Position is full at 128 B).
//!
//! DEFERRED vs anchor (documented): the JIT auction, the oracle-freshness gate
//! (no pin oracle), and events.
//!
//! accounts: [caller(signer), market(w), market_book(PDA,w), trader_state(w),
//!            caller_trader_state(w), position(w), position_liq(PDA,w)]
//! data: [requested_close_lots u64]   (0 = full size)

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_pda, assert_signer};
use crate::instructions::apply_fill::assert_position;
use crate::instructions::margin_probe::build_snapshot;
use crate::liquidation::{
    health_price_with_staleness, liquidation_penalty_price, liquidator_reward_lots, reward_bps_effective,
};
use crate::risk::{assess_margin, StressShock};
use crate::seeds::POSITION_LIQ_STATE_SEED;
use crate::state::{
    JitLiquidationOffer, Market, Position, PositionLiquidationState, TraderState,
    JIT_LIQ_OFFER_DISC, POSITION_LIQ_STATE_DISC, TRADER_STATE_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

#[inline(always)]
unsafe fn view_mut<T>(ai: &AccountInfo) -> &mut T {
    &mut *(ai.borrow_mut_data_unchecked().as_mut_ptr() as *mut T)
}

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [caller, market, market_book, trader_state, caller_trader_state, position, position_liq, jit_offers @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let requested_close_lots = u64::from_le_bytes(data[0..8].try_into().unwrap());

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(caller)?;
    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(caller_trader_state, pid)?;
    assert_disc(caller_trader_state, &TRADER_STATE_DISC)?;
    assert_position(position, pid)?;

    let market_key = *market.key();
    let caller_key = *caller.key();

    // L-1: the caller's reward account must be DISTINCT from the liquidatee's
    // trader_state — both are taken as `&mut` (view_mut) when the reward is moved,
    // and aliasing one allocation is UB — and it must belong to the caller (the
    // reward is credited to it). `borrow_mut_data_unchecked` bypasses the RefCell
    // guard that would otherwise catch the alias, so check explicitly.
    if caller_trader_state.key() == trader_state.key() {
        return Err(ProgramError::InvalidArgument);
    }
    {
        let cts = unsafe { &*(caller_trader_state.borrow_data_unchecked().as_ptr() as *const TraderState) };
        if cts.trader != caller_key {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // ── snapshot position / trader_state / market params / liq state ────
    let (
        pos_trader, pos_market, pos_sub, pos_side, pos_size, pos_collat,
        ts_trader, ts_collat, ts_open, ts_sub,
        mark, tick, penalty_bps, reward_bps, auction_dur, cooldown, last_mark_update,
    ) = {
        let p = unsafe { &*(position.borrow_data_unchecked().as_ptr() as *const Position) };
        let ts = unsafe { &*(trader_state.borrow_data_unchecked().as_ptr() as *const TraderState) };
        let m = unsafe { &*(market.borrow_data_unchecked().as_ptr() as *const Market) };
        (
            p.trader, p.market, p.sub_index, p.side, p.size_lots, p.collateral_quote_lots,
            ts.trader, ts.collateral_quote_lots, ts.open_positions, ts.sub_index,
            m.mark_price_ticks, m.tick_size, m.liq_penalty_bps, m.liquidator_reward_bps,
            m.liquidation_auction_duration_slots, m.liquidation_cooldown_slots, m.last_mark_update_slot,
        )
    };

    // ── validations ────────────────────────────────────────────────────
    if pos_size == 0 {
        return Err(ProgramError::InvalidArgument); // nothing to liquidate
    }
    if pos_trader != ts_trader || pos_market != market_key || pos_sub != ts_sub {
        return Err(ProgramError::InvalidArgument); // position must belong to THIS sub-account
    }
    if pos_side > 1 {
        return Err(ProgramError::InvalidArgument);
    }
    // M-2: self-liquidation forbidden (the reward would be self-dealt).
    if caller_key == pos_trader {
        return Err(ProgramError::InvalidArgument);
    }
    // H-4: single-leg path is sound only for an isolated position or a cross
    // trader with ≤ 1 open position; route a multi-leg cross trader to portfolio.
    let isolated = pos_collat > 0;
    if !isolated && ts_open > 1 {
        return Err(ProgramError::InvalidArgument);
    }

    let close_size = if requested_close_lots == 0 {
        pos_size
    } else {
        if requested_close_lots > pos_size {
            return Err(ProgramError::InvalidArgument);
        }
        requested_close_lots
    };

    let now = Clock::get()?.slot;

    // Dual-source health price via the proven `health_price_with_staleness`
    // helper (the mark half of Anchor's F4 freshness gate). pin is mark-only
    // (oracle == 0), so on a FRESH mark this returns `(mark, _)`; on a STALE mark
    // (sequencer stalled) there is no oracle fallback, so it returns `None` and we
    // refuse — a permissionless caller can't liquidate against a frozen adverse
    // mark. `is_long = pos_side == 0`. The returned price feeds the penalty price.
    let mark_stale = now.saturating_sub(last_mark_update) > crate::constants::MARK_STALENESS_MAX_SLOTS;
    let (health_price, _hp_src) = health_price_with_staleness(mark, 0, mark_stale, pos_side == 0)
        .ok_or(ProgramError::Custom(248))?; // stale mark, no oracle ⇒ refuse

    // ── re-liquidation cooldown ─────────────────────────────────────────
    let (unhealthy_since, last_liquidated) = {
        assert_owned_by(position_liq, pid)?;
        assert_pda(
            position_liq,
            &[POSITION_LIQ_STATE_SEED, &market_key[..], &position.key()[..]],
            pid,
        )?;
        assert_disc(position_liq, &POSITION_LIQ_STATE_DISC)?;
        let s = unsafe {
            &*(position_liq.borrow_data_unchecked().as_ptr() as *const PositionLiquidationState)
        };
        if s.market != market_key || &s.position != position.key() {
            return Err(ProgramError::InvalidArgument);
        }
        (s.unhealthy_since_slot, s.last_liquidated_at_slot)
    };
    if cooldown > 0 && last_liquidated > 0 && now.saturating_sub(last_liquidated) < cooldown {
        return Err(ProgramError::InvalidArgument); // RateLimited
    }
    // Unconditional same-slot guard (independent of the configurable cooldown,
    // which defaults to 0): the reward is paid on order INJECTION, and the close
    // only settles later via apply_fill, so within one slot the position stays
    // full-size + unhealthy. Without this, N stacked LiquidatePositionV2 calls in
    // one tx each skim `reward.min(backing)` and drain the liquidatee's bucket.
    if last_liquidated == now {
        return Err(ProgramError::InvalidArgument); // already liquidated this slot
    }

    // ── health gate: must be UNHEALTHY at base maintenance ──────────────
    // The health price is the worse of (mark, oracle); pin has no separate
    // oracle, so `worse_of_health_price` degenerates to the mark — which is
    // exactly what `build_snapshot` prices at. Assess against `backing` (isolated
    // bucket or cross pool — pin assess_margin uses only the passed collateral).
    let backing = if isolated { pos_collat } else { ts_collat };
    let Some((pos_snap, mkt_snap, _cross)) =
        build_snapshot(pid, market, trader_state, position, &[])?
    else {
        return Err(ProgramError::InvalidArgument); // flat (already rejected)
    };
    let no_shock: &[StressShock] = &[];
    let assessment = assess_margin(&[pos_snap], &[mkt_snap], &[no_shock], backing)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if assessment.is_healthy {
        return Err(ProgramError::InvalidArgument); // NotLiquidatable
    }

    // ── synthetic close price ───────────────────────────────────────────
    let close_side = 1 - pos_side;
    // Penalty price off the staleness-checked health price (== mark in the
    // mark-only model, but routed through the proven helper / oracle-ready).
    let synthetic = liquidation_penalty_price(close_side, health_price, penalty_bps);
    if synthetic == 0 {
        return Err(ProgramError::InvalidArgument); // degenerate price
    }

    // ── JIT auction: a maker offer (in remaining_accounts) that BEATS the
    // synthetic improves the trader's outcome — close_side==1 (selling a long):
    // higher price is better; close_side==0 (buying a short): lower is better.
    // The offer must be a program-owned JIT offer on this market, for this trader
    // (or the wildcard), with `side == pos_side`, not expired, remaining > 0. The
    // winner's commitment is reserved (remaining −= filled) after the inject.
    // No offers ⇒ `limit` stays the synthetic.
    let mut best_price: Option<u64> = None;
    let mut best_idx: Option<usize> = None;
    let mut best_remaining: u64 = 0;
    for (idx, off) in jit_offers.iter().enumerate() {
        if !off.is_owned_by(pid) {
            continue;
        }
        let d = match off.try_borrow_data() {
            Ok(d) => d,
            Err(_) => continue,
        };
        if d.len() < core::mem::size_of::<JitLiquidationOffer>() || d[..8] != JIT_LIQ_OFFER_DISC {
            continue;
        }
        let o = unsafe { &*(d.as_ptr() as *const JitLiquidationOffer) };
        if o.market != *market.key() || o.side != pos_side || o.remaining_size_lots == 0 {
            continue;
        }
        if o.target_trader != [0u8; 32] && o.target_trader != pos_trader {
            continue; // not the wildcard and not this trader
        }
        if o.expires_at_slot != 0 && now >= o.expires_at_slot {
            continue;
        }
        let price = o.offer_price_ticks;
        // H-2: bound the offer to the anti-stuffing band of the mark. The offer may
        // only IMPROVE the close price for the liquidatee, but an UNBOUNDED "better"
        // price (e.g. ~u64::MAX for a long close) would inject an order that rests
        // un-fillable forever — freezing the liquidation (the dup-scan then blocks
        // re-liquidation) while the caller has already skimmed the reward. Reject
        // any offer outside the band so the injected order stays marketable.
        if !crate::book::price_within_band(mark, price, crate::constants::MAX_RESTING_ORDER_DEVIATION_BPS) {
            continue;
        }
        let beats = if close_side == 1 { price > synthetic } else { price < synthetic };
        if !beats {
            continue;
        }
        let better = match best_price {
            None => true,
            Some(bp) => if close_side == 1 { price > bp } else { price < bp },
        };
        if better {
            best_price = Some(price);
            best_idx = Some(idx);
            best_remaining = o.remaining_size_lots;
        }
    }
    let limit = best_price.unwrap_or(synthetic);
    let elapsed = if unhealthy_since > 0 { now.saturating_sub(unhealthy_since) } else { 0 };
    let reward_bps_eff = reward_bps_effective(reward_bps, elapsed, auction_dur);
    let gross_reward = liquidator_reward_lots(close_size, mark, tick, reward_bps_eff);
    // Pay from the position's backing bucket only — capped at its balance.
    let reward_paid = gross_reward.min(backing);

    // ── writes ──────────────────────────────────────────────────────────
    // (1) reward: debit the liquidatee's bucket, credit the caller's cross pool.
    if reward_paid > 0 {
        unsafe {
            if isolated {
                let p: &mut Position = view_mut(position);
                p.collateral_quote_lots = p.collateral_quote_lots.saturating_sub(reward_paid);
            } else {
                let ts: &mut TraderState = view_mut(trader_state);
                ts.collateral_quote_lots = ts.collateral_quote_lots.saturating_sub(reward_paid);
            }
            let cts: &mut TraderState = view_mut(caller_trader_state);
            cts.collateral_quote_lots = cts
                .collateral_quote_lots
                .checked_add(reward_paid)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    // (2) inject the forced-liquidation order (order_type 3) into the book.
    unsafe {
        let book_data = market_book.borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        if &handle.header.market_pubkey != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        // Anti-stacking (port of the portfolio path's H3 scan): if a forced-
        // liquidation order (type 3) for this trader already rests on the close
        // side, the position is already being liquidated — refuse to inject a
        // second one. The reward is paid on INJECTION and the close only settles
        // later via apply_fill, so without this an attacker could stack N adjacent-
        // slot liquidations (the same-slot guard only blocks intra-slot; cooldown
        // defaults to 0) and skim the reward N times, draining the liquidatee.
        let mut dup = false;
        {
            let mut scan = |_idx: crate::hypertree::DataIndex, o: &RestingOrderV2| -> bool {
                if o.order_type == 3 && o.trader == pos_trader && o.sub_index == ts_sub {
                    dup = true;
                    return false;
                }
                true
            };
            if close_side == 0 {
                handle.for_each_bid_best_first(&mut scan);
            } else {
                handle.for_each_ask_best_first(&mut scan);
            }
        }
        if dup {
            return Err(ProgramError::Custom(140)); // already being liquidated
        }
        let seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        handle.header.order_seq_counter = seq;
        let side_is_bid = close_side == 0;
        let order = RestingOrderV2 {
            order_id: encode_order_id(limit, seq, side_is_bid),
            seq,
            price_ticks: limit,
            size_lots: close_size,
            expires_at_slot: 0,
            trader: pos_trader,
            last_valid_slot: if now > u32::MAX as u64 { u32::MAX } else { now as u32 },
            side: close_side,
            order_type: 3, // forced-liquidation (matcher promotes priority)
            flags: 0,
            sub_index: ts_sub,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }

    // (3) reserve the winning JIT offer's commitment (remaining −= filled).
    if let Some(idx) = best_idx {
        let fill = close_size.min(best_remaining);
        unsafe {
            let o = &mut *(jit_offers[idx].borrow_mut_data_unchecked().as_mut_ptr()
                as *mut JitLiquidationOffer);
            o.remaining_size_lots = o.remaining_size_lots.saturating_sub(fill);
        }
    }

    // (4) stamp the liquidation state (cooldown anchor + first-unhealthy slot).
    unsafe {
        let s: &mut PositionLiquidationState = view_mut(position_liq);
        if s.unhealthy_since_slot == 0 {
            s.unhealthy_since_slot = now;
        }
        s.last_liquidated_at_slot = now;
    }
    Ok(())
}
