//! set_trader_fee_tier — the protocol authority sets a trader's taker-fee
//! discount (applied in `apply_fill`).
//!
//! Gated by `insurance.authority` (the protocol admin set at fund init). The
//! insurance account is verified by program-ownership + its `[b"insurance_fund"]`
//! PDA + discriminator, and the signer must equal `insurance.authority`.
//!
//! accounts: [authority (signer), insurance (PDA, owned, r), trader_state (owned, w)]
//! data: discount_bps (u32 LE, 0..=BPS_DENOM)

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, TraderState, INSURANCE_DISC, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, insurance, trader_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let discount_bps = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if discount_bps > crate::constants::BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }

    // ── authority gate ──────────────────────────────────────────────────
    assert_signer(authority)?;
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // ── set the discount ────────────────────────────────────────────────
    assert_owned_by(trader_state, program_id)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.fee_discount_bps = discount_bps;
    }
    Ok(())
}
