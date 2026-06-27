//! cancel_twap_order — the trader closes their own TWAP order account and
//! reclaims its rent. Mirrors the `cancel_trigger_order` close/refund pattern.
//!
//! accounts: [trader (signer, w), twap_order (program-owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{TwapOrderV3, TWAP_ORDER_V3_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [trader, twap_order, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(trader)?;
    assert_owned_by(twap_order, program_id)?;
    assert_disc(twap_order, &TWAP_ORDER_V3_DISC)?;
    {
        let d = twap_order.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const TwapOrderV3) };
        if &t.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
    } // drop the data borrow before close()

    let lamports = twap_order.lamports();
    unsafe {
        let to = trader.borrow_mut_lamports_unchecked();
        *to = to
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        *twap_order.borrow_mut_lamports_unchecked() = 0;
    }
    twap_order.close()?;
    Ok(())
}
