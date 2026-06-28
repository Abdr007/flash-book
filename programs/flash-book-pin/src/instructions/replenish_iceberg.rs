//! replenish_iceberg — permissionless keeper crank that rests the iceberg's
//! NEXT displayed chunk on the book once the prior child has been consumed.
//! Injects `displayed_size_lots.min(remaining_lots)` as a fresh limit order,
//! decrements `remaining_lots`, and clears `FLAG_ACTIVE` when the iceberg is
//! fully placed. Faithful port of the Anchor `replenish_iceberg_v3`.
//!
//! NOTE: like anchor, this does NOT verify the prior child is gone — resting a
//! new chunk while the old one lives just shows more size (harmless, and the
//! trader chose the displayed size). The keeper is expected to crank on fill.
//!
//! accounts: [caller (signer), market (program-owned, r), market_book (PDA, w),
//!            iceberg_order (program-owned, w)]
//! data: (none)

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_signer};
use crate::state::{IcebergOrderV3, ICEBERG_FLAG_ACTIVE, ICEBERG_ORDER_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [caller, market, market_book, iceberg_order, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(caller)?;
    assert_market(market, program_id)?;
    assert_market_book(market_book, market, program_id)?;
    assert_owned_by(iceberg_order, program_id)?;
    assert_disc(iceberg_order, &ICEBERG_ORDER_V3_DISC)?;

    let (i_trader, i_market, i_limit, i_displayed, i_remaining, i_expires, i_side, i_sub) = {
        let d = iceberg_order.try_borrow_data()?;
        let i = unsafe { &*(d.as_ptr() as *const IcebergOrderV3) };
        if i.flags & ICEBERG_FLAG_ACTIVE == 0 {
            return Err(ProgramError::Custom(160)); // inactive / fully placed
        }
        (
            i.trader, i.market, i.limit_ticks, i.displayed_size_lots, i.remaining_lots,
            i.expires_at_slot, i.side, i.sub_index,
        )
    };

    if i_market != *market.key() {
        return Err(ProgramError::InvalidArgument);
    }
    if i_remaining == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    let now = Clock::get()?.slot;
    if i_expires > 0 && now > i_expires {
        return Err(ProgramError::Custom(161)); // expired
    }

    let chunk = i_displayed.min(i_remaining);

    // ── inject the next chunk (a normal limit, order_type 0) ────────────
    let inserted_seq;
    unsafe {
        let book_data = market_book.borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        if &handle.header.market_pubkey != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        let seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        handle.header.order_seq_counter = seq;
        let side_is_bid = i_side == 0;
        let order = RestingOrderV2 {
            order_id: encode_order_id(i_limit, seq, side_is_bid),
            seq,
            price_ticks: i_limit,
            size_lots: chunk,
            expires_at_slot: i_expires,
            trader: i_trader,
            last_valid_slot: if now > u32::MAX as u64 { u32::MAX } else { now as u32 },
            side: i_side,
            order_type: 0, // limit
            flags: 0,
            sub_index: i_sub,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
        inserted_seq = seq;
    }

    // ── advance the iceberg; clear ACTIVE when fully placed ─────────────
    unsafe {
        let i = &mut *(iceberg_order.borrow_mut_data_unchecked().as_mut_ptr() as *mut IcebergOrderV3);
        i.remaining_lots = i.remaining_lots.saturating_sub(chunk);
        i.child_order_seq = inserted_seq;
        if i.remaining_lots == 0 {
            i.flags &= !ICEBERG_FLAG_ACTIVE;
        }
    }
    Ok(())
}
