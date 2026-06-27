//! set_market_sequencer — rotate the market's settlement authority (the key
//! that signs `apply_fill`). Authority-gated.
//!
//! Security: the market must be program-owned with the market discriminator, and
//! the signer must equal `market.authority`. The authority binding is the gate —
//! a program-owned market necessarily came from `initialize_market`, which set
//! `authority` to its creator — so the PDA seeds (base/quote mint) need not be
//! re-derived here, keeping the account list minimal.
//!
//! accounts: [authority (signer), market (PDA, owned, w)]
//! data: new_sequencer (32 bytes)

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
    let new_sequencer: Pubkey = data[0..32].try_into().unwrap();

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
        m.sequencer = new_sequencer;
    }
    Ok(())
}
