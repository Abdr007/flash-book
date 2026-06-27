//! close_trader_sub_account — close an EMPTY sub-account and refund its rent to
//! the wallet.
//!
//! Guards: the wallet signs; the account is a program-owned trader_state whose
//! `.trader` is the wallet; it is a SUB-account (`sub_index != 0`, so the main
//! account can't be closed through here); and it is empty (`collateral == 0`,
//! `open_positions == 0`). Lamports are moved to the wallet before `close()` to
//! keep the instruction balanced.
//!
//! accounts: [wallet (signer, w), sub_state (owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [wallet, sub_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(wallet)?;
    assert_owned_by(sub_state, program_id)?;
    assert_disc(sub_state, &TRADER_STATE_DISC)?;
    {
        let d = sub_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != wallet.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if ts.sub_index == 0 {
            // the main account is not closable through this instruction.
            return Err(ProgramError::InvalidArgument);
        }
        if ts.collateral_quote_lots != 0 || ts.open_positions != 0 {
            return Err(ProgramError::InvalidArgument);
        }
    } // drop the data borrow before close()

    // Move lamports to the wallet, then close (must be balanced).
    let lamports = sub_state.lamports();
    unsafe {
        let w = wallet.borrow_mut_lamports_unchecked();
        *w = w
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        *sub_state.borrow_mut_lamports_unchecked() = 0;
    }
    sub_state.close()?;
    Ok(())
}
