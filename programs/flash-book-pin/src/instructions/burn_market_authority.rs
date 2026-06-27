//! burn_market_authority — the current market authority RENOUNCES admin control,
//! setting `market.authority` to the zero key. After this the market's config
//! (params, status, risk knobs, sequencer rotation, …) is permanently immutable —
//! a one-way decentralization step. Gated by the current authority; an
//! already-burned market (authority == zero) is rejected so the burn is
//! genuinely irreversible.
//!
//! accounts: [authority (signer), market (PDA, owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;
    assert_owned_by(market, program_id)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        // The signer must be the current authority, and a zero authority (already
        // burned) can't be "burned" again — and crucially can't be spoofed, since
        // no one can sign as the zero key.
        if &m.authority != authority.key() || m.authority == [0u8; 32] {
            return Err(ProgramError::IllegalOwner);
        }
    }

    unsafe {
        let m = &mut *(market.borrow_mut_data_unchecked().as_mut_ptr() as *mut Market);
        m.authority = [0u8; 32];
    }
    Ok(())
}
