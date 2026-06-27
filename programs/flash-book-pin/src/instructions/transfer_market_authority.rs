//! transfer_market_authority — rotate a market's admin authority. Gated by the
//! CURRENT authority (owner + disc + `market.authority == signer`).
//!
//! accounts: [authority (signer), market (PDA, owned, w)]
//! data: new_authority (32 bytes)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let new_authority: Pubkey = data[0..32].try_into().unwrap();

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
        m.authority = new_authority;
    }
    Ok(())
}
