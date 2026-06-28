//! place_bracket_order — atomic bracket: a parent limit order rested on the book
//! plus two reduce-only TP/SL trigger orders that close the resulting position.
//! Faithful port of the Anchor `place_bracket_order_v3`. The triggers reuse the
//! existing TriggerOrderV3 account + seed; they fire via `execute_trigger_order`
//! (which already supports the reduce-only close path).
//!
//! For a LONG parent (side 0): TP fires when mark RISES to/through its trigger
//! (kind 1, price above parent), SL fires when mark FALLS (kind 0, price below).
//! For a SHORT parent (side 1): mirrored. Both children are side = 1 - parent.
//!
//! accounts: [trader (signer, payer, w), market (program-owned, r),
//!            market_book (PDA, w), tp_trigger (PDA, w, uninit),
//!            sl_trigger (PDA, w, uninit), system_program]
//! data: [parent_side u8][sub_index u8][tp_trigger_id u8][sl_trigger_id u8]
//!       [size_lots u64][parent_limit_ticks u64]
//!       [tp_trigger_price u64][tp_limit u64]
//!       [sl_trigger_price u64][sl_limit u64][expires_at_slot u64]   — 60 bytes

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_market_book, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::TRIGGER_ORDER_SEED;
use crate::state::{
    Market, TriggerOrderV3, TRIGGER_FLAG_ACTIVE, TRIGGER_FLAG_REDUCE_ONLY, TRIGGER_ORDER_V3_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const TRIGGER_LEN: usize = core::mem::size_of::<TriggerOrderV3>();

#[allow(clippy::too_many_arguments)]
fn write_trigger(
    trader: &AccountInfo,
    market: &AccountInfo,
    trigger: &AccountInfo,
    system_program: &AccountInfo,
    program_id: &Pubkey,
    trigger_id: u8,
    close_side: u8,
    kind: u8,
    size_lots: u64,
    trigger_price_ticks: u64,
    limit_price_ticks: u64,
    created_at_slot: u64,
    expires_at_slot: u64,
    sub_index: u8,
) -> ProgramResult {
    assert_uninitialized(trigger)?;
    let id_arr = [trigger_id];
    let bump = assert_pda(
        trigger,
        &[
            TRIGGER_ORDER_SEED,
            &market.key()[..],
            &trader.key()[..],
            &id_arr[..],
        ],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(TRIGGER_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(TRIGGER_ORDER_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&trader.key()[..]),
        Seed::from(&id_arr[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        trader,
        trigger,
        system_program,
        lamports,
        TRIGGER_LEN as u64,
        program_id,
        &signer,
    )?;
    unsafe {
        let t = &mut *(trigger.borrow_mut_data_unchecked().as_mut_ptr() as *mut TriggerOrderV3);
        t.disc = TRIGGER_ORDER_V3_DISC;
        t.trader = *trader.key();
        t.market = *market.key();
        t.size_lots = size_lots;
        t.trigger_price_ticks = trigger_price_ticks;
        t.limit_price_ticks = limit_price_ticks;
        t.created_at_slot = created_at_slot;
        t.expires_at_slot = expires_at_slot;
        t.acceptable_price_ticks = 0;
        t.bump = bump;
        t.trigger_id = trigger_id;
        t.side = close_side;
        t.kind = kind;
        t.flags = TRIGGER_FLAG_ACTIVE | TRIGGER_FLAG_REDUCE_ONLY;
        t.sub_index = sub_index;
        t.trailing_offset_bps = 0; // bracket TP/SL are fixed, not trailing
        t.trailing_anchor_ticks = 0;
    }
    Ok(())
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, market, market_book, tp_trigger, sl_trigger, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 60 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let parent_side = data[0];
    let sub_index = data[1];
    let tp_trigger_id = data[2];
    let sl_trigger_id = data[3];
    let size_lots = u64::from_le_bytes(data[4..12].try_into().unwrap());
    let parent_limit_ticks = u64::from_le_bytes(data[12..20].try_into().unwrap());
    let tp_trigger_price_ticks = u64::from_le_bytes(data[20..28].try_into().unwrap());
    let tp_limit_ticks = u64::from_le_bytes(data[28..36].try_into().unwrap());
    let sl_trigger_price_ticks = u64::from_le_bytes(data[36..44].try_into().unwrap());
    let sl_limit_ticks = u64::from_le_bytes(data[44..52].try_into().unwrap());
    let expires_at_slot = u64::from_le_bytes(data[52..60].try_into().unwrap());

    assert_signer(trader)?;
    assert_market(market, program_id)?;
    assert_market_book(market_book, market, program_id)?;

    // ── validate (mirror anchor place_bracket_order_v3) ─────────────────
    if parent_side > 1 || tp_trigger_id == sl_trigger_id || size_lots == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    for p in [
        parent_limit_ticks,
        tp_trigger_price_ticks,
        sl_trigger_price_ticks,
        tp_limit_ticks,
        sl_limit_ticks,
    ] {
        if p == 0 {
            return Err(ProgramError::InvalidArgument);
        }
    }
    let (min_base_lots, tick_size) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.min_base_lots, m.tick_size)
    };
    if size_lots < min_base_lots || tick_size == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    for p in [
        parent_limit_ticks,
        tp_trigger_price_ticks,
        sl_trigger_price_ticks,
        tp_limit_ticks,
        sl_limit_ticks,
    ] {
        if p % tick_size != 0 {
            return Err(ProgramError::InvalidArgument); // price not on tick
        }
    }

    let now = Clock::get()?.slot;
    if expires_at_slot > 0 && expires_at_slot <= now {
        return Err(ProgramError::InvalidArgument);
    }

    // TP above / SL below for a long; mirrored for a short.
    if parent_side == 0 {
        if tp_trigger_price_ticks <= parent_limit_ticks
            || sl_trigger_price_ticks >= parent_limit_ticks
        {
            return Err(ProgramError::InvalidArgument);
        }
    } else if tp_trigger_price_ticks >= parent_limit_ticks
        || sl_trigger_price_ticks <= parent_limit_ticks
    {
        return Err(ProgramError::InvalidArgument);
    }

    // ── 1. inject the parent limit order ────────────────────────────────
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
        let side_is_bid = parent_side == 0;
        let order = RestingOrderV2 {
            order_id: encode_order_id(parent_limit_ticks, seq, side_is_bid),
            seq,
            price_ticks: parent_limit_ticks,
            size_lots,
            expires_at_slot: 0,
            trader: *trader.key(),
            last_valid_slot: if now > u32::MAX as u64 { u32::MAX } else { now as u32 },
            side: parent_side,
            order_type: 0, // limit
            flags: 0,
            sub_index,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }

    // ── 2. wire the two reduce-only TP/SL triggers ──────────────────────
    let close_side = 1 - parent_side;
    let (tp_kind, sl_kind) = if parent_side == 0 { (1u8, 0u8) } else { (0u8, 1u8) };

    write_trigger(
        trader, market, tp_trigger, system_program, program_id,
        tp_trigger_id, close_side, tp_kind, size_lots,
        tp_trigger_price_ticks, tp_limit_ticks, now, expires_at_slot, sub_index,
    )?;
    write_trigger(
        trader, market, sl_trigger, system_program, program_id,
        sl_trigger_id, close_side, sl_kind, size_lots,
        sl_trigger_price_ticks, sl_limit_ticks, now, expires_at_slot, sub_index,
    )?;

    Ok(())
}
