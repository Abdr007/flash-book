//! `apply_flp_fill` — settlement when the **FLP pool is the maker** (no maker
//! TraderState / Position). Pointer-casts the accounts and applies the taker-leg
//! fill: taker position update (same `apply_to_position` math), market OI, and
//! the fee split — but the maker rebate accrues to **FLP capital** (lifting
//! NAV/share for all LPs) and the insurance fund takes its
//! `fee_contribution_bps` cut of the net fee, with the remainder booked to
//! `market.total_fees_collected` as protocol revenue.
//!
//! Core scope, matching the ported `apply_fill`. Deferred (follow-ups, noted in
//! README): the FLP per-market exposure (size/entry) update, the toxicity tax
//! (vpin is ported — wiring pending), fee-tier resolution, events, and the
//! `init_if_needed` position-create CPI / PDA verification.
use crate::guard::{assert_disc, assert_owned_by};
use crate::instructions::apply_fill::{assert_position, bind_or_stamp_position};
use crate::state::{
    FlpExposure, Insurance, Market, Position, TraderState, FLP_EXPOSURE_DISC, INSURANCE_DISC,
    MARKET_DISC, POSITION_DISC, TRADER_STATE_DISC,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

const BPS_DENOM: u128 = 10_000;

#[inline(always)]
unsafe fn view<T>(ai: &AccountInfo) -> &mut T {
    &mut *(ai.borrow_mut_data_unchecked().as_mut_ptr() as *mut T)
}

#[inline(always)]
unsafe fn ensure_pos_disc(ai: &AccountInfo) {
    let d = ai.borrow_mut_data_unchecked();
    if d[..8] == [0u8; 8] {
        d[..8].copy_from_slice(&POSITION_DISC);
    }
}

/// data: [size_lots u64][price_ticks u64][taker_side u8][fill_seq u64]
/// accounts: [sequencer(signer), market, insurance, flp_exposure, taker_ts, taker_pos]
pub fn process(_pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 25 || accounts.len() < 6 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let size = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let price = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let taker_side = data[16];
    let fill_seq = u64::from_le_bytes(data[17..25].try_into().unwrap());
    if size == 0 || price == 0 || taker_side > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let sequencer = &accounts[0];
    if !sequencer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // Hardening: validate ownership + discriminator before casting (closes the
    // fake-account vector — same as apply_fill).
    // accounts: [sequencer, market, insurance, flp_exposure, taker_ts, taker_pos]
    assert_owned_by(&accounts[1], _pid)?; assert_disc(&accounts[1], &MARKET_DISC)?;
    assert_owned_by(&accounts[2], _pid)?; assert_disc(&accounts[2], &INSURANCE_DISC)?;
    assert_owned_by(&accounts[3], _pid)?; assert_disc(&accounts[3], &FLP_EXPOSURE_DISC)?;
    assert_owned_by(&accounts[4], _pid)?; assert_disc(&accounts[4], &TRADER_STATE_DISC)?;
    assert_position(&accounts[5], _pid)?;

    unsafe {
        let market: &mut Market = view(&accounts[1]);
        // C-1 settlement authorization: signer must be the market's sequencer.
        if market.sequencer != *sequencer.key() {
            return Err(ProgramError::IllegalOwner);
        }
        // Paused markets settle no fills (parity with apply_fill).
        if market.status == crate::state::MARKET_STATUS_PAUSED {
            return Err(ProgramError::InvalidArgument);
        }
        // Price band: the FLP pool is the maker, with NO opposing trader to
        // consent to the price, so a fill outside FLP_MAX_FILL_DEVIATION_BPS of the
        // mark is rejected — stops a compromised sequencer settling far from the
        // mark to drain pool capital (parity with Anchor's oracle-band check; pin
        // is mark-only, so the band is taken against `mark_price_ticks`).
        if !crate::book::price_within_band(
            market.mark_price_ticks, price, crate::constants::FLP_MAX_FILL_DEVIATION_BPS,
        ) {
            return Err(ProgramError::Custom(247)); // FLP fill price out of band
        }
        // Replay/reorder guard (same as apply_fill).
        let next_seq = crate::fill_commitment::advance_settlement_seq(market.settlement_seq(), fill_seq)
            .map_err(|_| ProgramError::Custom(246))?;
        market.set_settlement_seq(next_seq);
        let insurance: &mut Insurance = view(&accounts[2]);
        let flp: &mut FlpExposure = view(&accounts[3]);
        let taker_ts: &mut TraderState = view(&accounts[4]);
        ensure_pos_disc(&accounts[5]);
        let taker_pos: &mut Position = view(&accounts[5]);

        // Bind/stamp the taker position's (trader, market) identity BEFORE any
        // mutation — the sequencer cannot settle onto a mismatched/foreign
        // position (parity with apply_fill).
        bind_or_stamp_position(taker_pos, &taker_ts.trader, accounts[1].key())?;

        // Taker-leg position update (FLP is the maker — no maker leg on-chain).
        let fidx = market.cum_funding();
        // Snapshot the taker's OLD side/size so the OI delta removes its prior
        // contribution from the correct side (a fill may close or flip it).
        let taker_old_side = taker_pos.side;
        let taker_before = taker_pos.size_lots;
        crate::fill_math::apply_to_position(taker_pos, taker_side, size, price, fidx)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
        // CRITICAL (audit 2026-06): maintain `open_positions` here too. It gates
        // every collateral-exit (withdraw/partial/sweep); omitting it let a trader
        // open a position against the FLP pool, keep `open_positions == 0`, and
        // withdraw ALL collateral while fully exposed → uncovered bad debt.
        taker_ts.open_positions =
            TraderState::open_positions_after(taker_ts.open_positions, taker_before, taker_pos.size_lots);

        // Open interest. The pool is the maker (no on-chain position), so OI is
        // the taker leg plus the pool's mirror counter-leg — keeping
        // `long_oi == short_oi`, the conservation invariant. (The prior code
        // added `size` to ONLY the taker side, breaking it on every fill.)
        let (long_oi, short_oi) = crate::fill_math::oi_after_flp_fill(
            market.long_oi_lots,
            market.short_oi_lots,
            taker_old_side,
            taker_before,
            taker_pos.side,
            taker_pos.size_lots,
        );
        market.long_oi_lots = long_oi;
        market.short_oi_lots = short_oi;

        // Fee / rebate split. FLP-as-maker: ignore a negative maker_rebate_bps
        // (the protocol cannot charge itself a fee).
        let notional = (size as u128)
            .checked_mul(price as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(market.tick_size as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let fee = (notional
            .checked_mul(market.taker_fee_bps as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / BPS_DENOM) as u64;
        let rebate_bps = if market.maker_rebate_bps > 0 { market.maker_rebate_bps as u128 } else { 0 };
        let rebate = (notional
            .checked_mul(rebate_bps)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / BPS_DENOM) as u64;
        let rebate = rebate.min(fee);
        let net_fee = fee - rebate;

        // Taker pays the fee; rebate lifts FLP capital (NAV/share for all LPs).
        taker_ts.collateral_quote_lots = taker_ts.collateral_quote_lots.saturating_sub(fee);
        flp.total_capital_quote_lots = flp.total_capital_quote_lots.saturating_add(rebate);

        // Insurance takes its bps cut of the net fee; the rest is protocol revenue.
        let contribution =
            ((net_fee as u128) * (insurance.fee_contribution_bps as u128) / BPS_DENOM) as u64;
        insurance.balance_quote_lots = insurance.balance_quote_lots.saturating_add(contribution);
        insurance.total_contributions = insurance.total_contributions.saturating_add(contribution);
        market.total_fees_collected = market.total_fees_collected.saturating_add(net_fee);
    }
    Ok(())
}
