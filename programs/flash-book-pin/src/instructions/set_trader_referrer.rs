//! set_trader_referrer — the trader records their affiliate referrer. WRITE-ONCE:
//! it may only be set while currently unset (all-zero), so a referral can't be
//! hijacked or rewritten later. Trader-signed.
//!
//! accounts: [trader (signer), trader_state (program-owned, w)]
//! data: referrer (32-byte pubkey, must be non-zero)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, trader_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut referrer = [0u8; 32];
    referrer.copy_from_slice(&data[0..32]);
    // A zero referrer is the "unset" sentinel — refuse to "set" it.
    if referrer == [0u8; 32] {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(trader)?;
    assert_owned_by(trader_state, program_id)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;

    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        if &ts.trader != trader.key() {
            return Err(ProgramError::IllegalOwner);
        }
        // Write-once: reject if a referrer is already recorded.
        if ts.referrer != [0u8; 32] {
            return Err(ProgramError::InvalidArgument);
        }
        ts.referrer = referrer;
    }
    Ok(())
}
