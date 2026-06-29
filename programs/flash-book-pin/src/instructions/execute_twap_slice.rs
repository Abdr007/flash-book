//! execute_twap_slice — permissionless crank: execute ONE slice of an active
//! TWAP order once its interval has elapsed, injecting the slice into the book.
//! The order STAYS active across slices until `total_size_lots` is executed, then
//! `FLAG_ACTIVE` is cleared. MARK-only (pin has no separate oracle). Faithful port
//! of the Anchor `execute_twap_slice_v3`.
//!
//! accounts: [caller (signer), market (program-owned, r), market_book (PDA, w),
//!            twap_order (program-owned, w)]
//! data: (none)

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_signer};
use crate::state::{Market, TwapOrderV3, TWAP_FLAG_ACTIVE, TWAP_ORDER_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [caller, market, market_book, twap_order, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(caller)?;
    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;
    assert_owned_by(twap_order, pid)?;
    assert_disc(twap_order, &TWAP_ORDER_V3_DISC)?;

    let (t_trader, t_market, t_slice, t_total, t_executed, t_limit, t_interval, t_end, t_last, t_side, t_flags, t_sub) = {
        let t = unsafe { &*(twap_order.borrow_data_unchecked().as_ptr() as *const TwapOrderV3) };
        (
            t.trader, t.market, t.slice_size_lots, t.total_size_lots, t.size_executed_lots,
            t.limit_price_ticks, t.slot_interval, t.end_slot, t.last_slice_at_slot, t.side,
            t.flags, t.sub_index,
        )
    };

    if t_market != *market.key() {
        return Err(ProgramError::InvalidArgument);
    }
    if t_flags & TWAP_FLAG_ACTIVE == 0 {
        return Err(ProgramError::Custom(160)); // inactive / done
    }
    if t_side > 1 || t_limit == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    let now = Clock::get()?.slot;
    if t_end != 0 && now > t_end {
        return Err(ProgramError::Custom(161)); // window ended
    }
    if now < t_last.saturating_add(t_interval) {
        return Err(ProgramError::Custom(162)); // interval not elapsed yet
    }

    let remaining = t_total
        .checked_sub(t_executed)
        .ok_or(ProgramError::InvalidAccountData)?;
    if remaining == 0 {
        return Err(ProgramError::Custom(163)); // fully executed
    }
    let slice = t_slice.min(remaining);
    let min_base = {
        let d = market.try_borrow_data()?;
        unsafe { (*(d.as_ptr() as *const Market)).min_base_lots }
    };
    // No dust: a slice must meet the market minimum unless it is the final remnant.
    if slice < min_base && slice != remaining {
        return Err(ProgramError::Custom(164));
    }

    // ── inject the slice (a normal limit, order_type 0) ─────────────────
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
        let side_is_bid = t_side == 0;
        let order = RestingOrderV2 {
            order_id: encode_order_id(t_limit, seq, side_is_bid),
            seq,
            price_ticks: t_limit,
            size_lots: slice,
            expires_at_slot: 0,
            trader: t_trader,
            last_valid_slot: if now > u32::MAX as u64 { u32::MAX } else { now as u32 },
            side: t_side,
            order_type: 0,
            flags: 0,
            sub_index: t_sub,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }

    // ── advance the schedule; deactivate once fully executed ────────────
    unsafe {
        let t = &mut *(twap_order.borrow_mut_data_unchecked().as_mut_ptr() as *mut TwapOrderV3);
        t.size_executed_lots = t.size_executed_lots.saturating_add(slice);
        t.last_slice_at_slot = now;
        if t.size_executed_lots >= t.total_size_lots {
            t.flags &= !TWAP_FLAG_ACTIVE;
        }
    }
    Ok(())
}
