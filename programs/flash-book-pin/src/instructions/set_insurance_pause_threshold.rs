//! set_insurance_pause_threshold — the protocol admin sets the insurance-fund
//! balance floor. When the fund falls to/below it, markets are meant to
//! auto-pause (the consuming check is a later batch). `0` disables. Gated on the
//! recorded insurance authority.
//!
//! accounts: [authority (signer), insurance (PDA, owned, w)]
//! data: new_threshold_quote_lots (u64 LE)

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, INSURANCE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, insurance, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let new_threshold = u64::from_le_bytes(data[0..8].try_into().unwrap());

    assert_signer(authority)?;
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    {
        let d = insurance.try_borrow_data()?;
        let f = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &f.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    unsafe {
        let f = &mut *(insurance.borrow_mut_data_unchecked().as_mut_ptr() as *mut Insurance);
        f.pause_threshold_quote_lots = new_threshold;
    }
    Ok(())
}
