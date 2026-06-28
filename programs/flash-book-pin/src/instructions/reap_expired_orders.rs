//! reap_expired_orders — permissionless keeper crank that removes genuinely
//! EXPIRED resting orders (GTT past their `expires_at_slot`) from the book,
//! reclaiming node-arena space. The caller passes up to `MAX_REAP_PER_CALL`
//! order ids; each is looked up (bid then ask) and removed ONLY if expired.
//! A GTC order (`expires == 0`) or a still-live one (`expires > now`) is skipped
//! — so the reaper can never grief a valid order. Mirrors anchor
//! `reap_expired_orders`.
//!
//! accounts: [cranker (signer), market (program-owned, r), market_book (PDA, owned, w)]
//! data: [n u8][order_id u64 LE; n]   (1 ≤ n ≤ MAX_REAP_PER_CALL)

use crate::book::MarketBookHandle;
use crate::guard::{assert_market, assert_market_book, assert_signer};
use crate::hypertree::NIL;
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

const MAX_REAP_PER_CALL: usize = 64;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [cranker, market, market_book, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let n = *data.first().ok_or(ProgramError::InvalidInstructionData)? as usize;
    if n == 0 || n > MAX_REAP_PER_CALL {
        return Err(ProgramError::InvalidInstructionData);
    }
    if data.len() < 1 + n * 8 {
        return Err(ProgramError::InvalidInstructionData);
    }

    assert_signer(cranker)?;
    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;

    let now = Clock::get()?.slot;
    unsafe {
        let book_data = market_book.borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        if &handle.header.market_pubkey != market.key() {
            return Err(ProgramError::InvalidArgument);
        }

        for i in 0..n {
            let off = 1 + i * 8;
            let order_id = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            // Globally-unique id → try the bid book, then the ask book.
            let b = handle.lookup_bid_by_order_id(order_id);
            let (idx, is_bid) = if b != NIL {
                (b, true)
            } else {
                (handle.lookup_ask_by_order_id(order_id), false)
            };
            if idx == NIL {
                continue; // already gone / not on this book
            }
            // Only reap a GENUINELY-expired GTT order. `expires == 0` is GTC;
            // `expires > now` is still live. Both are skipped.
            let expires = handle.order_at(idx).expires_at_slot;
            if expires == 0 || expires > now {
                continue;
            }
            if is_bid {
                handle.remove_bid_node(idx);
            } else {
                handle.remove_ask_node(idx);
            }
        }
    }
    Ok(())
}
