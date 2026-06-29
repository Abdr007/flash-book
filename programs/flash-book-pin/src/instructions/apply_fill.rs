//! `apply_fill` — the matcher settlement hot path, ported to Pinocchio.
//! Pointer-casts the 6 accounts (no Borsh) and applies the fill: position
//! update (weighted entry / realized PnL / side flip — identical math to the
//! Anchor `apply_fill_to_position`), market OI, fee/rebate split, funding stamp.
use crate::guard::{assert_disc, assert_owned_by, assert_pda};
use crate::seeds::HAIRCUT_SEED;
use crate::state::{
    Insurance, Market, MarketHaircutState, Position, TraderState, HAIRCUT_STATE_DISC,
    INSURANCE_DISC, MARKET_DISC, POSITION_DISC, TRADER_STATE_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

/// A position account must be program-owned and either FRESH (zero disc, to be
/// stamped on first fill) or already a `POSITION_DISC` position — never another
/// program-owned account type (which would be type-confused by the cast).
pub(crate) fn assert_position(ai: &AccountInfo, pid: &Pubkey) -> ProgramResult {
    assert_owned_by(ai, pid)?;
    let d = ai.try_borrow_data()?;
    // Require the FULL struct length before any `*const/*mut Position` cast — the
    // disc gate alone admits a fresh ZERO-disc account, which an attacker can
    // create program-owned at only 8 bytes (System CreateAccount). Casting it and
    // reading `size_lots`@88 / writing through 128 would be an OOB read/write.
    if d.len() < core::mem::size_of::<Position>() {
        return Err(ProgramError::InvalidAccountData);
    }
    if d[..8] != [0u8; 8] && d[..8] != POSITION_DISC {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

const BPS_DENOM: u128 = 10_000;

#[inline(always)]
unsafe fn view<T>(ai: &AccountInfo) -> &mut T {
    &mut *(ai.borrow_mut_data_unchecked().as_mut_ptr() as *mut T)
}

/// Stamp the discriminator on a freshly-created (zero-disc) position so reads
/// succeed within the same instruction.
#[inline(always)]
unsafe fn ensure_pos_disc(ai: &AccountInfo) {
    let d = ai.borrow_mut_data_unchecked();
    if d[..8] == [0u8; 8] { d[..8].copy_from_slice(&POSITION_DISC); }
}

/// Bind a settlement position to its `(trader, market)` identity. A FRESH
/// position (zero identity, flat) is STAMPED on its first fill; an existing one
/// must MATCH. Without this, a (trusted-but-buggy/compromised) sequencer could
/// settle a fill onto a mismatched or foreign position and corrupt collateral/OI
/// accounting — parity with the Anchor `position.trader == trader_state.trader`
/// binding (Anchor additionally derives the position by PDA; pin is field-bound).
#[inline]
pub(crate) fn bind_or_stamp_position(pos: &mut Position, trader: &Pubkey, market: &Pubkey, sub_index: u8) -> ProgramResult {
    if pos.trader == [0u8; 32] && pos.size_lots == 0 {
        pos.trader = *trader;
        pos.market = *market;
        pos.sub_index = sub_index; // bind to the trader_state sub-account
        Ok(())
    } else if &pos.trader != trader || &pos.market != market || pos.sub_index != sub_index {
        Err(ProgramError::InvalidArgument)
    } else {
        Ok(())
    }
}

/// Materialize a realized-PnL `delta` into the leg's collateral bucket — the
/// position's own collateral (`iso`) or the trader_state pool (cross), sampled
/// pre-resize. A gain credits; a loss debits, draining the bucket to 0 and
/// covering any shortfall from the insurance fund (then ADL for an uncovered
/// remainder once the fund is exhausted). This is the R1 fix: without it a closed
/// loss never debited collateral (→ bad debt) and a closed gain never credited it.
#[inline]
pub(crate) fn materialize_realized(
    delta: i64,
    iso: bool,
    pos: &mut Position,
    ts: &mut TraderState,
    insurance: &mut Insurance,
) -> ProgramResult {
    if delta == 0 {
        return Ok(());
    }
    let bucket = if iso { pos.collateral_quote_lots } else { ts.collateral_quote_lots };
    let (new_bucket, shortfall) = crate::fill_math::route_realized_pnl(bucket, delta)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if iso {
        pos.collateral_quote_lots = new_bucket;
    } else {
        ts.collateral_quote_lots = new_bucket;
    }
    if shortfall > 0 {
        // Bad-debt waterfall: insurance covers up to its balance; any uncovered
        // remainder is left for ADL (`auto_deleverage`). Same as `cover_bad_debt`.
        let (covered, _uncovered) = crate::liquidation::cover_shortfall(insurance.balance_quote_lots, shortfall);
        insurance.balance_quote_lots -= covered;
        insurance.total_payouts = insurance.total_payouts.saturating_add(covered);
    }
    Ok(())
}

/// data: [size_lots u64][price_ticks u64][taker_side u8][fill_seq u64]
/// accounts: [sequencer(signer), market, insurance, taker_ts, maker_ts, taker_pos,
///            maker_pos, (haircut_state OPTIONAL)]
/// When the trailing `haircut_state` (the market's `[b"haircut", market]` PDA) is
/// supplied, funding is settled on each leg's PRE-trade size before the resize
/// (R2 — stops a same-side add being charged funding for the whole prior interval);
/// omitting it preserves the legacy 7-account behavior (funding settled lazily by
/// the `settle_funding` crank). Mirrors anchor's `market_haircut.is_some()` gate.
pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // data: [size u64][price u64][taker_side u8][fill_seq u64]
    if data.len() < 25 || accounts.len() < 7 { return Err(ProgramError::InvalidInstructionData); }
    let size = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let price = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let taker_side = data[16];
    let fill_seq = u64::from_le_bytes(data[17..25].try_into().unwrap());
    if taker_side > 1 { return Err(ProgramError::InvalidInstructionData); }
    let maker_side = 1 - taker_side;

    let sequencer = &accounts[0];
    if !sequencer.is_signer() { return Err(ProgramError::MissingRequiredSignature); }
    // A processed fill is an ER-liveness event — stamp it (mark half of the
    // liveness signal `verify_market_invariants` reads), so an actively-trading
    // market is never flagged as stalled even with no separate heartbeat.
    let now_slot = Clock::get()?.slot;

    // Hardening: every account we pointer-cast must be program-owned with the
    // correct discriminator — otherwise a caller could pass a FAKE account (e.g.
    // a "market" whose `sequencer` field they control) and the cast would trust
    // attacker-supplied bytes. This closes the fake-account vector the original
    // benchmark handler left open on the settlement path.
    assert_owned_by(&accounts[1], pid)?; assert_disc(&accounts[1], &MARKET_DISC)?;
    assert_owned_by(&accounts[2], pid)?; assert_disc(&accounts[2], &INSURANCE_DISC)?;
    assert_owned_by(&accounts[3], pid)?; assert_disc(&accounts[3], &TRADER_STATE_DISC)?;
    assert_owned_by(&accounts[4], pid)?; assert_disc(&accounts[4], &TRADER_STATE_DISC)?;
    assert_position(&accounts[5], pid)?;
    assert_position(&accounts[6], pid)?;
    // The two legs MUST be distinct accounts. Both trader_states and both
    // positions are taken as `&mut` via `view()`; aliasing either pair would be
    // two simultaneous `&mut` to one allocation (UB). Distinct traders is also
    // the matcher's invariant (self-trades are prevented upstream).
    if accounts[3].key() == accounts[4].key() || accounts[5].key() == accounts[6].key() {
        return Err(ProgramError::InvalidArgument);
    }
    // Optional trailing haircut_state (R2 inline funding settle). Validate it as
    // THIS market's canonical `[b"haircut", market]` PDA so the residual we move
    // is the right market's accumulator, not an attacker-supplied account.
    let has_haircut = accounts.len() > 7;
    if has_haircut {
        assert_owned_by(&accounts[7], pid)?;
        assert_pda(&accounts[7], &[HAIRCUT_SEED, &accounts[1].key()[..]], pid)?;
        assert_disc(&accounts[7], &HAIRCUT_STATE_DISC)?;
    }

    unsafe {
        let market: &mut Market = view(&accounts[1]);
        // C-1 settlement authorization: signer must be the market's sequencer.
        if market.sequencer != *sequencer.key() { return Err(ProgramError::IllegalOwner); }
        // Paused markets settle no fills.
        if market.status == crate::state::MARKET_STATUS_PAUSED { return Err(ProgramError::InvalidArgument); }
        // Settlement price band: both legs' resting limits are within the
        // anti-stuffing band of the mark, so a legitimate cross is too. Reject a
        // sequencer-supplied price outside it — bounds how far a compromised
        // sequencer can move collateral between the two legs (apply_flp_fill has
        // the tighter FLP band; trader-vs-trader uses the resting band).
        if !crate::book::price_within_band(market.mark_price_ticks, price, crate::constants::MAX_RESTING_ORDER_DEVIATION_BPS) {
            return Err(ProgramError::Custom(247)); // fill price out of band
        }
        // Replay/reorder guard: `fill_seq` must STRICTLY exceed the market's
        // settlement nonce; advance it atomically with the fill. A re-submitted or
        // reordered sequencer-signed settlement is rejected before any mutation
        // (parity with Anchor `advance_settlement_seq` / FillSeqReplay).
        let next_seq = crate::fill_commitment::advance_settlement_seq(market.settlement_seq(), fill_seq)
            .map_err(|_| ProgramError::Custom(246))?;
        market.set_settlement_seq(next_seq);
        let insurance: &mut Insurance = view(&accounts[2]);
        let taker_ts: &mut TraderState = view(&accounts[3]);
        let maker_ts: &mut TraderState = view(&accounts[4]);
        ensure_pos_disc(&accounts[5]);
        ensure_pos_disc(&accounts[6]);
        let taker_pos: &mut Position = view(&accounts[5]);
        let maker_pos: &mut Position = view(&accounts[6]);

        // Bind/stamp each leg's (trader, market) identity BEFORE any mutation, so
        // a mismatched position is rejected atomically with no state change.
        let mkt_key = *accounts[1].key();
        bind_or_stamp_position(taker_pos, &taker_ts.trader, &mkt_key, taker_ts.sub_index)?;
        bind_or_stamp_position(maker_pos, &maker_ts.trader, &mkt_key, maker_ts.sub_index)?;

        let fidx = market.cum_funding();
        let tick = market.tick_size;
        // Snapshot pre-trade sizes/sides (for open_positions + OI deltas) and
        // sample each leg's collateral bucket — isolated = the position's own
        // collateral, cross = the trader_state pool — BEFORE any mutation, so
        // realized PnL routes to whichever bucket backed the position at fill time.
        let taker_before = taker_pos.size_lots;
        let maker_before = maker_pos.size_lots;
        let taker_old_side = taker_pos.side;
        let maker_old_side = maker_pos.side;
        let taker_iso = taker_pos.collateral_quote_lots > 0;
        let maker_iso = maker_pos.collateral_quote_lots > 0;

        // ── Fees FIRST, before the resize (anchor order) — so a position closing
        // at a loss still has collateral to pay its fee. ─────────────────────
        let notional = (size as u128)
            .checked_mul(price as u128).ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(tick as u128).ok_or(ProgramError::ArithmeticOverflow)?;
        // M-1: clamp every u128→u64 fee/rebate cast (anchor clamps to u64::MAX);
        // a raw `as u64` would WRAP mod 2^64 at extreme notional → near-zero fee.
        let clamp = |x: u128| -> u64 { x.min(u64::MAX as u128) as u64 };
        let gross_fee = clamp(notional.checked_mul(market.taker_fee_bps as u128).ok_or(ProgramError::ArithmeticOverflow)? / BPS_DENOM);
        // Apply the taker's per-trader fee discount (0 by default ⇒ no change).
        let fee = crate::fees::discounted_fee(gross_fee, taker_ts.fee_discount_bps);
        let rebate = if market.maker_rebate_bps > 0 {
            clamp(notional.checked_mul(market.maker_rebate_bps as u128).ok_or(ProgramError::ArithmeticOverflow)? / BPS_DENOM)
        } else { 0 };
        let rebate = rebate.min(fee);
        // H-1: debit the taker fee with checked_sub and ABORT the fill if they
        // can't cover it (anchor parity). The prior `saturating_sub` floored the
        // taker's debit at their balance while still crediting the maker the FULL
        // rebate + insurance the FULL net fee → quote-lots minted from nothing.
        taker_ts.collateral_quote_lots = taker_ts.collateral_quote_lots
            .checked_sub(fee)
            .ok_or(ProgramError::Custom(249))?; // InsufficientCollateral
        maker_ts.collateral_quote_lots = maker_ts.collateral_quote_lots.saturating_add(rebate);
        insurance.balance_quote_lots = insurance.balance_quote_lots.saturating_add(fee - rebate);

        // ── (R2) Settle each leg's funding on its PRE-trade size, BEFORE the
        // resize, when the optional haircut_state is supplied. The shared helper
        // is the SAME one `settle_funding` uses, so the inline settle and the
        // crank can't diverge; settling here re-stamps the entry index, so a
        // following same-side add can't be charged funding for the prior interval.
        if has_haircut {
            let haircut: &mut MarketHaircutState = view(&accounts[7]);
            if &haircut.market != accounts[1].key() {
                return Err(ProgramError::InvalidArgument);
            }
            let mark = market.mark_price_ticks;
            let mut residual = u128::from_le_bytes(haircut.residual_quote_lots);
            crate::funding::settle_position_funding(taker_pos, mark, tick, fidx, &mut taker_ts.collateral_quote_lots, &mut residual)
                .map_err(|_| ProgramError::InvalidArgument)?;
            crate::funding::settle_position_funding(maker_pos, mark, tick, fidx, &mut maker_ts.collateral_quote_lots, &mut residual)
                .map_err(|_| ProgramError::InvalidArgument)?;
            haircut.residual_quote_lots = residual.to_le_bytes();
        }

        // ── Resize each leg + capture the realized-PnL delta for this fill (R1).
        let taker_delta = crate::fill_math::apply_to_position(taker_pos, taker_side, size, price, tick, fidx)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
        let maker_delta = crate::fill_math::apply_to_position(maker_pos, maker_side, size, price, tick, fidx)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
        // ── Materialize realized PnL into the right collateral bucket; a loss
        // beyond the bucket drains it to 0 and draws the shortfall from insurance.
        materialize_realized(taker_delta, taker_iso, taker_pos, taker_ts, insurance)?;
        materialize_realized(maker_delta, maker_iso, maker_pos, maker_ts, insurance)?;

        // Maintain open_positions (gates withdraw_collateral). Pure transition.
        taker_ts.open_positions =
            TraderState::open_positions_after(taker_ts.open_positions, taker_before, taker_pos.size_lots);
        maker_ts.open_positions =
            TraderState::open_positions_after(maker_ts.open_positions, maker_before, maker_pos.size_lots);

        // Open interest. Each position contributes its `size_lots` to OI on its
        // side; a fill changes BOTH legs (one long, one short). Remove each leg's
        // OLD contribution and add its NEW one (host-tested `oi_after_leg`) —
        // correct across open / close / flip, and (crucially) keeps
        // `long_oi_lots == short_oi_lots`, the conservation invariant.
        let (long_oi, short_oi) = crate::fill_math::oi_after_leg(
            market.long_oi_lots, market.short_oi_lots,
            taker_old_side, taker_before, taker_pos.side, taker_pos.size_lots,
        );
        let (long_oi, short_oi) = crate::fill_math::oi_after_leg(
            long_oi, short_oi,
            maker_old_side, maker_before, maker_pos.side, maker_pos.size_lots,
        );
        market.long_oi_lots = long_oi;
        market.short_oi_lots = short_oi;

        // Mark-freshness stamp (liveness). Monotonic guard against re-ordering.
        if now_slot > market.last_mark_update_slot { market.last_mark_update_slot = now_slot; }
    }
    Ok(())
}
