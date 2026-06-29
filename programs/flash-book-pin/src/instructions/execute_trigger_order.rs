//! execute_trigger_order_v3 — permissionless crank that FIRES an active trigger
//! when the mark crosses its trigger price: inject the trigger's order into the
//! book and clear `FLAG_ACTIVE` (one-shot; the trader reclaims rent via
//! `cancel_trigger_order`). `kind == 0` fires when `mark <= trigger_price` (a
//! stop below); `kind == 1` fires when `mark >= trigger_price`. MARK-only (pin
//! has no separate oracle). Faithful port of the Anchor `execute_trigger_order_v3`.
//!
//! Reduce-only triggers (a position-closing trigger) need a position account to
//! validate against; that path is a follow-up and is rejected here.
//!
//! accounts: [caller (signer), market (program-owned, r), market_book (PDA, w),
//!            trigger_order (program-owned, w)]
//! data: (none)

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_signer};
use crate::instructions::apply_fill::assert_position;
use crate::state::{
    Market, Position, TriggerOrderV3, TRIGGER_FLAG_ACTIVE, TRIGGER_FLAG_REDUCE_ONLY,
    TRIGGER_ORDER_V3_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [caller, market, market_book, trigger_order, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(caller)?;
    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;
    assert_owned_by(trigger_order, pid)?;
    assert_disc(trigger_order, &TRIGGER_ORDER_V3_DISC)?;

    let (t_trader, t_market, t_size, t_trigger_price, t_limit, t_expires, t_side, t_kind, t_flags, t_sub) = {
        let t = unsafe { &*(trigger_order.borrow_data_unchecked().as_ptr() as *const TriggerOrderV3) };
        (
            t.trader, t.market, t.size_lots, t.trigger_price_ticks, t.limit_price_ticks,
            t.expires_at_slot, t.side, t.kind, t.flags, t.sub_index,
        )
    };

    if t_market != *market.key() {
        return Err(ProgramError::InvalidArgument);
    }
    if t_flags & TRIGGER_FLAG_ACTIVE == 0 {
        return Err(ProgramError::Custom(150)); // inactive / already fired
    }
    // Reduce-only: the trigger may only CLOSE an existing opposite-side position
    // — validate the trader's position (passed as the first extra account).
    if t_flags & TRIGGER_FLAG_REDUCE_ONLY != 0 {
        let position = rest.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
        assert_position(position, pid)?;
        let p = unsafe { &*(position.borrow_data_unchecked().as_ptr() as *const Position) };
        if p.trader != t_trader || p.market != *market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if p.size_lots == 0 || p.side == t_side || t_size > p.size_lots {
            return Err(ProgramError::InvalidArgument); // not a valid reducing close
        }
    }
    if t_side > 1 || t_size == 0 || t_limit == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    let now = Clock::get()?.slot;
    if t_expires != 0 && now > t_expires {
        return Err(ProgramError::Custom(152)); // expired
    }

    let mark = {
        let d = market.try_borrow_data()?;
        unsafe { (*(d.as_ptr() as *const Market)).mark_price_ticks }
    };
    // Fired? kind 0 = mark crossed DOWN to/through the trigger; kind 1 = UP.
    let fired = if t_kind == 0 { mark <= t_trigger_price } else { mark >= t_trigger_price };
    if !fired {
        return Err(ProgramError::Custom(153)); // condition not met
    }

    // ── inject the trigger's order (a normal limit, order_type 0) ───────
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
            size_lots: t_size,
            expires_at_slot: 0,
            trader: t_trader,
            last_valid_slot: if now > u32::MAX as u64 { u32::MAX } else { now as u32 },
            side: t_side,
            order_type: 0, // limit
            flags: 0,
            sub_index: t_sub,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }

    // ── one-shot: clear FLAG_ACTIVE (trader reclaims rent via cancel) ───
    unsafe {
        let t = &mut *(trigger_order.borrow_mut_data_unchecked().as_mut_ptr() as *mut TriggerOrderV3);
        t.flags &= !TRIGGER_FLAG_ACTIVE;
    }
    Ok(())
}
