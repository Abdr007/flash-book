//! transfer_insurance_authority — rotate the protocol admin authority on the
//! insurance fund. Gated by the CURRENT authority.
//!
//! accounts: [authority (signer), insurance (PDA, owned, w)]
//! data: new_authority (32 bytes)

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
    if data.len() < 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let new_authority: Pubkey = data[0..32].try_into().unwrap();

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

    unsafe {
        let ins = &mut *(insurance.borrow_mut_data_unchecked().as_mut_ptr() as *mut Insurance);
        ins.authority = new_authority;
    }
    Ok(())
}
