//! transfer_collateral — move recorded collateral between two of a wallet's own
//! trader_states (e.g. main ↔ sub), without touching the token vault. Pure
//! internal accounting.
//!
//! Both accounts must be program-owned trader_states whose `.trader` is the
//! signing wallet, so a wallet can only move its own collateral. The SOURCE must
//! be flat (`open_positions == 0`) — same conservative rule as withdraw, so
//! collateral backing live positions cannot be moved out from under them.
//!
//! accounts: [wallet (signer), from_state (owned, w), to_state (owned, w)]
//! data: amount (u64 LE)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [wallet, from_state, to_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    // Distinct accounts (a same-account "transfer" would be a no-op / confusing).
    if from_state.key() == to_state.key() {
        return Err(ProgramError::InvalidArgument);
    }

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(wallet)?;
    assert_owned_by(from_state, program_id)?;
    assert_disc(from_state, &TRADER_STATE_DISC)?;
    assert_owned_by(to_state, program_id)?;
    assert_disc(to_state, &TRADER_STATE_DISC)?;

    // both must belong to the signing wallet; source must be flat + funded.
    {
        let d = from_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != wallet.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if ts.open_positions != 0 {
            return Err(ProgramError::InvalidArgument);
        }
        if ts.collateral_quote_lots < amount {
            return Err(ProgramError::InsufficientFunds);
        }
    }
    {
        let d = to_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != wallet.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // ── move the collateral (checked both ways) ─────────────────────────
    unsafe {
        let f = &mut *(from_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        f.collateral_quote_lots = f
            .collateral_quote_lots
            .checked_sub(amount)
            .ok_or(ProgramError::InsufficientFunds)?;
    }
    unsafe {
        let t = &mut *(to_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        t.collateral_quote_lots = t
            .collateral_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
