//! flush_haircut_dust — permissionless keeper crank that reclaims a market's
//! accrued haircut dust (floor-rounding remainder from converts) into the
//! insurance fund. Pure ACCOUNTING — no SPL transfer: the dust is value already
//! in the protocol vault, so only the counters move.
//!
//! Balanced move (mirrors the anchor H-3 fix): `insurance.balance += dust`,
//! `dust := 0`, and CRUCIALLY `residual -= dust` — the dust just moved into `I`,
//! so the identity `Residual = V − C_tot − I` requires `ΔResidual = −dust`;
//! without this the dust is double-counted, inflating `h`.
//!
//! accounts: [keeper (signer), haircut_state (PDA, owned, w), insurance (PDA, owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::{HAIRCUT_SEED, INSURANCE_SEED};
use crate::state::{Insurance, MarketHaircutState, HAIRCUT_STATE_DISC, INSURANCE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [keeper, haircut_state, insurance, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(keeper)?;
    assert_owned_by(haircut_state, program_id)?;
    assert_disc(haircut_state, &HAIRCUT_STATE_DISC)?;
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    // Bind the haircut_state to its own canonical PDA (its stored market).
    let market = {
        let d = haircut_state.try_borrow_data()?;
        let h = unsafe { &*(d.as_ptr() as *const MarketHaircutState) };
        h.market
    };
    assert_pda(haircut_state, &[HAIRCUT_SEED, &market[..]], program_id)?;

    unsafe {
        let h = &mut *(haircut_state.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketHaircutState);
        let dust = u128::from_le_bytes(h.dust_accrued_quote_lots);
        if dust == 0 {
            return Err(ProgramError::InvalidArgument); // nothing to flush
        }
        let dust_u64 = if dust > u64::MAX as u128 { u64::MAX } else { dust as u64 };
        let dust_u128 = dust_u64 as u128;

        // Residual identity: the dust moves into I, so debit Residual by the same.
        let residual = u128::from_le_bytes(h.residual_quote_lots)
            .checked_sub(dust_u128)
            .ok_or(ProgramError::InsufficientFunds)?;
        let new_dust = dust
            .checked_sub(dust_u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        h.residual_quote_lots = residual.to_le_bytes();
        h.dust_accrued_quote_lots = new_dust.to_le_bytes();

        let f = &mut *(insurance.borrow_mut_data_unchecked().as_mut_ptr() as *mut Insurance);
        f.balance_quote_lots = f
            .balance_quote_lots
            .checked_add(dust_u64)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        f.total_contributions = f
            .total_contributions
            .checked_add(dust_u64)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
