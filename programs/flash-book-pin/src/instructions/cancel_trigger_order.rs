//! cancel_trigger_order — the trader closes their own conditional-order account
//! and reclaims its rent. Mirrors the `close_trader_sub_account` close/refund
//! pattern (move lamports to the owner, then `close()`).
//!
//! accounts: [trader (signer, w), trigger_order (program-owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{TriggerOrderV3, TRIGGER_ORDER_V3_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [trader, trigger_order, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(trader)?;
    assert_owned_by(trigger_order, program_id)?;
    assert_disc(trigger_order, &TRIGGER_ORDER_V3_DISC)?;
    {
        let d = trigger_order.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const TriggerOrderV3) };
        if &t.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
    } // drop the data borrow before close()

    // Move lamports to the trader, then close (must be balanced).
    let lamports = trigger_order.lamports();
    unsafe {
        let to = trader.borrow_mut_lamports_unchecked();
        *to = to
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        *trigger_order.borrow_mut_lamports_unchecked() = 0;
    }
    trigger_order.close()?;
    Ok(())
}
