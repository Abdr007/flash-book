//! set_market_maintenance_margin — the market authority updates the maintenance
//! margin requirement (bps) used by `verify_solvency` / the risk engine.
//! Authority-gated, bounds-checked (0 < mmr < BPS_DENOM).
//!
//! accounts: [authority (signer), market (PDA, owned, w)]
//! data: maintenance_margin_bps (u32 LE)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let maintenance_margin_bps = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if maintenance_margin_bps == 0 || maintenance_margin_bps >= crate::constants::BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(authority)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.maintenance_margin_bps = maintenance_margin_bps;
    }
    Ok(())
}
