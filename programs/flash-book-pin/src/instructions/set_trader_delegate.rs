//! set_trader_delegate — the trader authorizes (or clears) a delegate key that
//! may act on their behalf. Trader-signed; freely re-settable. Default zero =
//! no delegate. (The consuming auth checks land with the delegated-trade paths.)
//!
//! accounts: [trader (signer), trader_state (program-owned, w)]
//! data: new_delegate (32-byte pubkey; all-zero clears it)

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
    let mut new_delegate = [0u8; 32];
    new_delegate.copy_from_slice(&data[0..32]);

    assert_signer(trader)?;
    assert_owned_by(trader_state, program_id)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;

    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        if &ts.trader != trader.key() {
            return Err(ProgramError::IllegalOwner);
        }
        ts.delegate = new_delegate;
    }
    Ok(())
}
