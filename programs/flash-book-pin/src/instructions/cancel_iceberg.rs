//! cancel_iceberg — the trader closes their own iceberg order account and
//! reclaims its rent. Mirrors the `cancel_trigger_order` close/refund pattern.
//!
//! NOTE: as in anchor `cancel_iceberg_v3`, the currently-displayed child order
//! (if any) is left resting on the book — the trader cancels it separately via
//! `cancel_order` using `child_order_seq`. Closing the iceberg only stops future
//! replenishment and returns rent.
//!
//! accounts: [trader (signer, w), iceberg_order (program-owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{IcebergOrderV3, ICEBERG_ORDER_V3_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [trader, iceberg_order, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(trader)?;
    assert_owned_by(iceberg_order, program_id)?;
    assert_disc(iceberg_order, &ICEBERG_ORDER_V3_DISC)?;
    {
        let d = iceberg_order.try_borrow_data()?;
        let i = unsafe { &*(d.as_ptr() as *const IcebergOrderV3) };
        if &i.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
    } // drop the data borrow before close()

    // Move lamports to the trader, then close (must be balanced).
    let lamports = iceberg_order.lamports();
    unsafe {
        let to = trader.borrow_mut_lamports_unchecked();
        *to = to
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        *iceberg_order.borrow_mut_lamports_unchecked() = 0;
    }
    iceberg_order.close()?;
    Ok(())
}
