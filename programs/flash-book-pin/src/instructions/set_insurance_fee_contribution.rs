//! set_insurance_fee_contribution — the protocol authority updates the fraction
//! (bps) of each net fee routed to the insurance fund (the rest is protocol
//! revenue). Authority-gated, bounds-checked.
//!
//! accounts: [authority (signer), insurance (PDA, owned, w)]
//! data: fee_contribution_bps (u32 LE, 0..=BPS_DENOM)

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
    if data.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let fee_contribution_bps = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if fee_contribution_bps > crate::constants::BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }

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
        ins.fee_contribution_bps = fee_contribution_bps;
    }
    Ok(())
}
