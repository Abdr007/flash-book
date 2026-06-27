//! set_market_status — pause / resume a market. Authority-gated.
//!
//! accounts: [authority (signer), market (PDA, owned, w)]
//! data: status (1 byte: 0 = active, 1 = paused)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC, MARKET_STATUS_ACTIVE, MARKET_STATUS_PAUSED};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let status = data[0];
    if status != MARKET_STATUS_ACTIVE && status != MARKET_STATUS_PAUSED {
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
        m.status = status;
    }
    Ok(())
}
