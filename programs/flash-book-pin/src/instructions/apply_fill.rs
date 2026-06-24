//! `apply_fill` — the matcher settlement hot path, ported to Pinocchio.
//! Pointer-casts the 6 accounts (no Borsh) and applies the fill: position
//! update (weighted entry / realized PnL / side flip — identical math to the
//! Anchor `apply_fill_to_position`), market OI, fee/rebate split, funding stamp.
use crate::state::{Insurance, Market, Position, TraderState, POSITION_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

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

/// Identical math to the Anchor `apply_fill_to_position`.
fn apply_to_position(pos: &mut Position, fill_side: u8, fill_size: u64, fill_price: u64, fidx: i128) -> Result<(), ProgramError> {
    if pos.size_lots == 0 {
        pos.side = fill_side;
        pos.size_lots = fill_size;
        pos.entry_price_ticks = fill_price;
        pos.set_cum_funding(fidx);
        return Ok(());
    }
    if pos.side == fill_side {
        let new_size = pos.size_lots.checked_add(fill_size).ok_or(ProgramError::ArithmeticOverflow)?;
        let weighted = (pos.entry_price_ticks as u128)
            .checked_mul(pos.size_lots as u128).ok_or(ProgramError::ArithmeticOverflow)?
            .checked_add((fill_price as u128).checked_mul(fill_size as u128).ok_or(ProgramError::ArithmeticOverflow)?)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / new_size as u128;
        pos.entry_price_ticks = weighted as u64;
        pos.size_lots = new_size;
        return Ok(());
    }
    // Opposite side: realize PnL on the closed portion.
    let close = fill_size.min(pos.size_lots);
    let sign: i128 = if pos.side == 0 { 1 } else { -1 };
    let pnl = sign
        .checked_mul(close as i128).ok_or(ProgramError::ArithmeticOverflow)?
        .checked_mul((fill_price as i128) - (pos.entry_price_ticks as i128)).ok_or(ProgramError::ArithmeticOverflow)?;
    let pnl_c = if pnl > i64::MAX as i128 { i64::MAX } else if pnl < i64::MIN as i128 { i64::MIN } else { pnl as i64 };
    pos.realized_pnl_quote_lots = pos.realized_pnl_quote_lots.checked_add(pnl_c).ok_or(ProgramError::ArithmeticOverflow)?;
    if fill_size <= pos.size_lots {
        pos.size_lots -= fill_size;
        if pos.size_lots == 0 { pos.entry_price_ticks = 0; pos.set_cum_funding(fidx); }
    } else {
        pos.side = fill_side;
        pos.size_lots = fill_size - pos.size_lots;
        pos.entry_price_ticks = fill_price;
        pos.set_cum_funding(fidx);
    }
    Ok(())
}

/// data: [size_lots u64][price_ticks u64][taker_side u8]
/// accounts: [sequencer(signer), market, insurance, taker_ts, maker_ts, taker_pos, maker_pos]
pub fn process(_pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 17 || accounts.len() < 7 { return Err(ProgramError::InvalidInstructionData); }
    let size = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let price = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let taker_side = data[16];
    let maker_side = 1 - taker_side;

    let sequencer = &accounts[0];
    if !sequencer.is_signer() { return Err(ProgramError::MissingRequiredSignature); }

    unsafe {
        let market: &mut Market = view(&accounts[1]);
        // C-1 settlement authorization: signer must be the market's sequencer.
        if market.sequencer != *sequencer.key() { return Err(ProgramError::IllegalOwner); }
        let insurance: &mut Insurance = view(&accounts[2]);
        let taker_ts: &mut TraderState = view(&accounts[3]);
        let maker_ts: &mut TraderState = view(&accounts[4]);
        ensure_pos_disc(&accounts[5]);
        ensure_pos_disc(&accounts[6]);
        let taker_pos: &mut Position = view(&accounts[5]);
        let maker_pos: &mut Position = view(&accounts[6]);

        let fidx = market.cum_funding();
        // Fills update both legs with identical matcher math.
        apply_to_position(taker_pos, taker_side, size, price, fidx)?;
        apply_to_position(maker_pos, maker_side, size, price, fidx)?;

        // Open interest.
        if taker_side == 0 { market.long_oi_lots = market.long_oi_lots.saturating_add(size); }
        else { market.short_oi_lots = market.short_oi_lots.saturating_add(size); }

        // Fee / rebate split (integer bps, like the matcher).
        let notional = (size as u128)
            .checked_mul(price as u128).ok_or(ProgramError::ArithmeticOverflow)?
            .checked_mul(market.tick_size as u128).ok_or(ProgramError::ArithmeticOverflow)?;
        let fee = (notional.checked_mul(market.taker_fee_bps as u128).ok_or(ProgramError::ArithmeticOverflow)? / BPS_DENOM) as u64;
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
