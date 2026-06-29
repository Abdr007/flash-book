//! `apply_fill` — the matcher settlement hot path, ported to Pinocchio.
//! Pointer-casts the 6 accounts (no Borsh) and applies the fill: position
//! update (weighted entry / realized PnL / side flip — identical math to the
//! Anchor `apply_fill_to_position`), market OI, fee/rebate split, funding stamp.
use crate::guard::{assert_disc, assert_owned_by};
use crate::state::{
    Insurance, Market, Position, TraderState, INSURANCE_DISC, MARKET_DISC, POSITION_DISC,
    TRADER_STATE_DISC,
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
    if d.len() < 8 {
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
pub(crate) fn bind_or_stamp_position(pos: &mut Position, trader: &Pubkey, market: &Pubkey) -> ProgramResult {
    if pos.trader == [0u8; 32] && pos.size_lots == 0 {
        pos.trader = *trader;
        pos.market = *market;
        Ok(())
    } else if &pos.trader != trader || &pos.market != market {
        Err(ProgramError::InvalidArgument)
    } else {
        Ok(())
    }
}

/// data: [size_lots u64][price_ticks u64][taker_side u8]
/// accounts: [sequencer(signer), market, insurance, taker_ts, maker_ts, taker_pos, maker_pos]
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

    unsafe {
        let market: &mut Market = view(&accounts[1]);
        // C-1 settlement authorization: signer must be the market's sequencer.
        if market.sequencer != *sequencer.key() { return Err(ProgramError::IllegalOwner); }
        // Paused markets settle no fills.
        if market.status == crate::state::MARKET_STATUS_PAUSED { return Err(ProgramError::InvalidArgument); }
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
        bind_or_stamp_position(taker_pos, &taker_ts.trader, &mkt_key)?;
        bind_or_stamp_position(maker_pos, &maker_ts.trader, &mkt_key)?;

        let fidx = market.cum_funding();
        // Snapshot sizes so we can maintain each trader's open_positions count
        // across the open (0 → >0) / close (>0 → 0) transitions below.
        let taker_before = taker_pos.size_lots;
        let maker_before = maker_pos.size_lots;
        // Also snapshot each leg's OLD side, so the open-interest delta below
        // removes its prior contribution from the correct side (a fill may flip).
        let taker_old_side = taker_pos.side;
        let maker_old_side = maker_pos.side;
        // Fills update both legs with identical matcher math.
        crate::fill_math::apply_to_position(taker_pos, taker_side, size, price, fidx).map_err(|_| ProgramError::ArithmeticOverflow)?;
        crate::fill_math::apply_to_position(maker_pos, maker_side, size, price, fidx).map_err(|_| ProgramError::ArithmeticOverflow)?;
        // Maintain open_positions (gates withdraw_collateral). Pure transition.
        taker_ts.open_positions =
            TraderState::open_positions_after(taker_ts.open_positions, taker_before, taker_pos.size_lots);
        maker_ts.open_positions =
            TraderState::open_positions_after(maker_ts.open_positions, maker_before, maker_pos.size_lots);

        // Open interest. Each position contributes its `size_lots` to OI on its
        // side; a fill changes BOTH legs (one long, one short). Remove each leg's
        // OLD contribution and add its NEW one (host-tested `oi_after_leg`) —
        // correct across open / close / flip, and (crucially) keeps
        // `long_oi_lots == short_oi_lots`, the conservation invariant
        // `verify_market_invariants` enforces. (The prior code added the fill
        // size to ONLY the taker side, breaking the invariant on every fill.)
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

        // Fee / rebate split (integer bps, like the matcher).
        let notional = (size as u128)
            .checked_mul(price as u128).ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(market.tick_size as u128).ok_or(ProgramError::ArithmeticOverflow)?;
        let gross_fee = (notional.checked_mul(market.taker_fee_bps as u128).ok_or(ProgramError::ArithmeticOverflow)? / BPS_DENOM) as u64;
        // Apply the taker's per-trader fee discount (0 by default ⇒ no change).
        let fee = crate::fees::discounted_fee(gross_fee, taker_ts.fee_discount_bps);
        let rebate = if market.maker_rebate_bps > 0 {
            (notional.checked_mul(market.maker_rebate_bps as u128).ok_or(ProgramError::ArithmeticOverflow)? / BPS_DENOM) as u64
        } else { 0 };
        let rebate = rebate.min(fee);
        taker_ts.collateral_quote_lots = taker_ts.collateral_quote_lots.saturating_sub(fee);
        maker_ts.collateral_quote_lots = maker_ts.collateral_quote_lots.saturating_add(rebate);
        insurance.balance_quote_lots = insurance.balance_quote_lots.saturating_add(fee - rebate);
    }
    Ok(())
}
