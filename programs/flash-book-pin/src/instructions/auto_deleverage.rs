//! auto_deleverage — force-close `close_size_lots` of an UNDERWATER position
//! against an opposite-side, profitable COUNTER position at the underwater
//! position's bankruptcy price, when the insurance fund is below its pause
//! threshold (ADL is the last-resort waterfall step). Permissionless: keepers
//! compete off-chain on the (pnl × leverage) ranking and pass the chosen counter
//! as an explicit account; the first valid call wins.
//!
//! Faithful port of the Anchor `auto_deleverage`. The settlement arithmetic
//! (`bankruptcy_price` / `adl_underwater_loss` / `adl_counter_gain` /
//! `counter_eligible_at_bp`) and the isolated-vs-cross collateral routing
//! (`route_adl_loss` / `route_adl_gain`) are the host-tested + Kani-proven
//! `crate::adl` functions; this handler does the account validation, the
//! underwater health gate, and the position/OI/open-position write-backs.
//!
//! HEALTH GATE — deliberately STRICTER than anchor: the underwater position must
//! be below its BASE maintenance requirement AT THE MARK (a single zero-shock
//! `assess_margin`, the `verify_solvency` pattern). Anchor uses a fixed stress
//! lattice; a permissionless ADL caller MUST NOT control the shock set (an
//! attacker could pass an extreme shock to make a HEALTHY position look adverse
//! and wrongfully ADL it), so caller-supplied shocks are intentionally NOT
//! accepted. This admits a strict subset of anchor's ADL candidates — never a
//! superset — so it can only ever REFUSE an ADL anchor would allow, never allow
//! one it forbids. A fixed-lattice gate is a later refinement.
//!
//! DEFERRED vs anchor (additive, no behavioral risk): the indexer-only
//! `trader_state.realized_pnl` tracking and `market.total_liquidations` counter
//! (no such pin fields) and the optional Wave-25b `side_accrual` multiplier
//! reduction (a later maintenance batch).
//!
//! accounts: [caller(signer), market(w), insurance(r), underwater_ts(w),
//!            underwater_pos(w), counter_ts(w), counter_pos(w)]
//! data: [close_size_lots u64]

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda, assert_signer};
use crate::instructions::apply_fill::assert_position;
use crate::instructions::margin_probe::build_snapshot;
use crate::risk::{assess_margin, StressShock};
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, Market, Position, TraderState, INSURANCE_DISC, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

#[inline(always)]
unsafe fn view_mut<T>(ai: &AccountInfo) -> &mut T {
    &mut *(ai.borrow_mut_data_unchecked().as_mut_ptr() as *mut T)
}

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [caller, market, insurance, uw_ts, uw_pos, ct_ts, ct_pos, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let close_size_lots = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if close_size_lots == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(caller)?;
    assert_market(market, pid)?;
    assert_owned_by(insurance, pid)?;
    assert_pda(insurance, &[INSURANCE_SEED], pid)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    assert_owned_by(uw_ts, pid)?;
    assert_disc(uw_ts, &TRADER_STATE_DISC)?;
    assert_owned_by(ct_ts, pid)?;
    assert_disc(ct_ts, &TRADER_STATE_DISC)?;
    assert_position(uw_pos, pid)?;
    assert_position(ct_pos, pid)?;

    let market_key = *market.key();

    // ── snapshot both positions + trader states (Copy scalars) ──────────
    let (
        uw_trader, uw_market, uw_side, uw_size, uw_entry, uw_pos_collat,
        ct_trader, ct_market, ct_side, ct_size, ct_entry, ct_pos_collat,
        uw_ts_trader, uw_ts_collat, uw_ts_open,
        ct_ts_trader, ct_ts_collat,
        tick_size,
    ) = {
        let uwp = unsafe { &*(uw_pos.borrow_data_unchecked().as_ptr() as *const Position) };
        let ctp = unsafe { &*(ct_pos.borrow_data_unchecked().as_ptr() as *const Position) };
        let uwt = unsafe { &*(uw_ts.borrow_data_unchecked().as_ptr() as *const TraderState) };
        let ctt = unsafe { &*(ct_ts.borrow_data_unchecked().as_ptr() as *const TraderState) };
        let m = unsafe { &*(market.borrow_data_unchecked().as_ptr() as *const Market) };
        (
            uwp.trader, uwp.market, uwp.side, uwp.size_lots, uwp.entry_price_ticks, uwp.collateral_quote_lots,
            ctp.trader, ctp.market, ctp.side, ctp.size_lots, ctp.entry_price_ticks, ctp.collateral_quote_lots,
            uwt.trader, uwt.collateral_quote_lots, uwt.open_positions,
            ctt.trader, ctt.collateral_quote_lots,
            m.tick_size,
        )
    };

    // ── sanity: same market, opposite sides, sizes, alignment, not self ─
    if uw_market != market_key || ct_market != market_key {
        return Err(ProgramError::InvalidArgument);
    }
    if uw_size == 0 || ct_size == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if uw_side == ct_side || uw_side > 1 {
        return Err(ProgramError::InvalidArgument);
    }
    if close_size_lots > uw_size || close_size_lots > ct_size {
        return Err(ProgramError::InvalidArgument);
    }
    if uw_trader != uw_ts_trader || ct_trader != ct_ts_trader {
        return Err(ProgramError::InvalidArgument);
    }
    // Cannot ADL yourself — also rules out aliasing uw_pos/ct_pos & uw_ts/ct_ts
    // (same account ⇒ same trader), so the mutable views below never alias.
    if uw_trader == ct_trader {
        return Err(ProgramError::InvalidArgument);
    }
    // H-5: the single-leg underwater health check is sound only for an isolated
    // position or a single-position cross trader; otherwise a winning cross leg
    // (excluded here) could wrongly mark the trader underwater.
    let uw_isolated = uw_pos_collat > 0;
    if !uw_isolated && uw_ts_open > 1 {
        return Err(ProgramError::InvalidArgument); // CrossLiquidationNeedsPortfolio
    }
    // The collateral BACKING this position: the isolated bucket if isolated, else
    // the cross pool (sound because cross is gated to a single position by H-5).
    // Used for BOTH the health gate and the bankruptcy price.
    let backing = if uw_isolated { uw_pos_collat } else { uw_ts_collat };

    // ── trigger gate: insurance below its pause threshold ───────────────
    {
        let ins = unsafe { &*(insurance.borrow_data_unchecked().as_ptr() as *const Insurance) };
        if ins.balance_quote_lots >= ins.pause_threshold_quote_lots {
            return Err(ProgramError::InvalidArgument); // AdlNotEligible
        }
    }

    // ── underwater health gate: must be UNHEALTHY at base maintenance ───
    // build_snapshot validates+binds (market, uw_ts, uw_pos) and returns the
    // owned snapshot + cross collateral. A single zero-shock scenario prices the
    // base maintenance requirement at mark (shocked_price(p,0)==p); shocks are
    // NOT caller-controlled (see header — anti-manipulation).
    // build_snapshot validates+binds (market, uw_ts, uw_pos) and builds the
    // snapshots; its cross-collateral return is IGNORED — we assess against
    // `backing` (pin's `assess_margin` uses only the passed collateral, never the
    // snapshot's, so an isolated position must be assessed against its bucket).
    let Some((pos_snap, mkt_snap, _cross_collateral)) =
        build_snapshot(pid, market, uw_ts, uw_pos, &[])?
    else {
        return Err(ProgramError::InvalidArgument); // flat — not ADL-eligible
    };
    let no_shock: &[StressShock] = &[];
    let assessment = assess_margin(&[pos_snap], &[mkt_snap], &[no_shock], backing)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if assessment.is_healthy {
        return Err(ProgramError::InvalidArgument); // NotLiquidatable
    }

    // ── bankruptcy price + counter eligibility (pure, proven) ───────────
    let bp = crate::adl::bankruptcy_price(uw_side, uw_entry, backing, uw_size, tick_size)
        .ok_or(ProgramError::InvalidArgument)?; // size·tick == 0
    if !crate::adl::counter_eligible_at_bp(ct_side, ct_entry, bp) {
        return Err(ProgramError::InvalidArgument); // AdlNotEligible
    }

    // ── settle: underwater loss + counter gain (pure, proven) ───────────
    let loss = crate::adl::adl_underwater_loss(backing, close_size_lots, uw_size);
    let gain = crate::adl::adl_counter_gain(ct_side, ct_entry, bp, close_size_lots, tick_size);
    let ct_isolated = ct_pos_collat > 0;

    let (uw_new_pos_collat, uw_new_ts_collat) =
        crate::adl::route_adl_loss(uw_isolated, loss, uw_pos_collat, uw_ts_collat);
    let (ct_new_pos_collat, ct_new_ts_collat) =
        crate::adl::route_adl_gain(ct_isolated, gain, ct_pos_collat, ct_ts_collat)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;

    let uw_post_size = uw_size - close_size_lots; // close ≤ size (checked)
    let ct_post_size = ct_size - close_size_lots;

    // ── writes (effects after all checks); accounts are pairwise distinct ─
    unsafe {
        let uwp: &mut Position = view_mut(uw_pos);
        uwp.collateral_quote_lots = uw_new_pos_collat;
        uwp.size_lots = uw_post_size;
        if uw_post_size == 0 {
            uwp.entry_price_ticks = 0;
        }
        let ctp: &mut Position = view_mut(ct_pos);
        ctp.collateral_quote_lots = ct_new_pos_collat;
        ctp.size_lots = ct_post_size;
        if ct_post_size == 0 {
            ctp.entry_price_ticks = 0;
        }
        let uwt: &mut TraderState = view_mut(uw_ts);
        uwt.collateral_quote_lots = uw_new_ts_collat;
        if uw_post_size == 0 {
            uwt.open_positions = uwt.open_positions.saturating_sub(1);
        }
        let ctt: &mut TraderState = view_mut(ct_ts);
        ctt.collateral_quote_lots = ct_new_ts_collat;
        if ct_post_size == 0 {
            ctt.open_positions = ctt.open_positions.saturating_sub(1);
        }
        // OI: remove each leg's old contribution, add its new. Both legs only
        // REDUCE (same side, smaller size), and they are opposite sides, so
        // long_oi and short_oi each drop by close_size_lots — `long == short`
        // is preserved.
        let m: &mut Market = view_mut(market);
        let (long_oi, short_oi) = crate::fill_math::oi_after_leg(
            m.long_oi_lots, m.short_oi_lots, uw_side, uw_size, uw_side, uw_post_size,
        );
        let (long_oi, short_oi) = crate::fill_math::oi_after_leg(
            long_oi, short_oi, ct_side, ct_size, ct_side, ct_post_size,
        );
        m.long_oi_lots = long_oi;
        m.short_oi_lots = short_oi;
    }
    Ok(())
}
