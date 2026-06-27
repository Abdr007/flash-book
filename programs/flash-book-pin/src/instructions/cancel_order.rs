//! cancel_order_v2 — look up a resting order by id and remove it from the book.
use crate::book::MarketBookHandle;
use crate::guard::{assert_market, assert_market_book};
use crate::hypertree::NIL;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

/// data: [side u8][order_id u64]
/// accounts: [trader(signer), market, market_book]
pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 9 || accounts.len() < 3 { return Err(ProgramError::InvalidInstructionData); }
    let side = data[0];
    let order_id = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let trader = &accounts[0];
    if !trader.is_signer() { return Err(ProgramError::MissingRequiredSignature); }
    if side > 1 { return Err(ProgramError::InvalidInstructionData); }
    assert_market(&accounts[1], pid)?;
    assert_market_book(&accounts[2], &accounts[1], pid)?;
    unsafe {
        let book_data = accounts[2].borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        let side_is_bid = side == 0;
        let idx = if side_is_bid { handle.lookup_bid_by_order_id(order_id) } else { handle.lookup_ask_by_order_id(order_id) };
        if idx == NIL { return Err(ProgramError::Custom(4)); } // not found
        if handle.order_at(idx).trader != *trader.key() { return Err(ProgramError::Custom(1100)); } // wrong trader
        if side_is_bid { handle.remove_bid_node(idx); } else { handle.remove_ask_node(idx); }
    }
    Ok(())
}
