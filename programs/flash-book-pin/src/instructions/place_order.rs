//! place_limit_order_v2 — validate + insert a resting order into the book.
//! Delegates to the ported (tested) MarketBookHandle::insert_bid/ask.
use crate::book::{self, MarketBookHandle, RestingOrderV2};
use crate::state::Market;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

#[inline(always)]
unsafe fn market_of(ai: &AccountInfo) -> &Market { &*(ai.borrow_data_unchecked().as_ptr() as *const Market) }

/// data: [side u8][size_lots u64][limit_ticks u64][expires u64][flags u8][sub_index u8]
/// accounts: [trader(signer), market, market_book]
pub fn process(_pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 26 || accounts.len() < 3 { return Err(ProgramError::InvalidInstructionData); }
    let side = data[0];
    let size_lots = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let limit_ticks = u64::from_le_bytes(data[9..17].try_into().unwrap());
    let expires = u64::from_le_bytes(data[17..25].try_into().unwrap());
    let flags = data[25];
    let trader = &accounts[0];
    if !trader.is_signer() { return Err(ProgramError::MissingRequiredSignature); }
    if side > 1 { return Err(ProgramError::InvalidInstructionData); }
    unsafe {
        let market = market_of(&accounts[1]);
        if size_lots < market.min_base_lots { return Err(ProgramError::Custom(1)); }
        if market.tick_size > 0 && limit_ticks % market.tick_size != 0 { return Err(ProgramError::Custom(2)); }
        if market.max_oi_base_lots > 0 {
            let cur = if side == 0 { market.long_oi_lots } else { market.short_oi_lots };
            if cur.saturating_add(size_lots) > market.max_oi_base_lots { return Err(ProgramError::Custom(3)); }
        }
        let mut book_data = accounts[2].borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        let seq = handle.header.order_seq_counter.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
        handle.header.order_seq_counter = seq;
        let side_is_bid = side == 0;
        let order = RestingOrderV2 {
            order_id: book::encode_order_id(limit_ticks, seq, side_is_bid),
            seq, price_ticks: limit_ticks, size_lots, expires_at_slot: expires,
            trader: *trader.key(), last_valid_slot: 0, side, order_type: 0, flags, sub_index: data.get(26).copied().unwrap_or(0),
        };
        if side_is_bid { handle.insert_bid(order)?; } else { handle.insert_ask(order)?; }
    }
    Ok(())
}
