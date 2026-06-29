//! place_iceberg_order — create a v3 iceberg order account, PDA
//! `[b"iceberg_v3", market, trader, iceberg_id]`, and rest its FIRST displayed
//! chunk on the book. Only `displayed_size_lots` is ever visible at once; the
//! rest (`remaining_lots`) is hidden until a keeper calls `replenish_iceberg`.
//! Faithful port of the Anchor `place_iceberg_order_v3`.
//!
//! accounts: [trader (signer, payer, w), market (program-owned, r),
//!            market_book (PDA, w), iceberg_order (PDA, w, uninit),
//!            system_program]
//! data: [iceberg_id u8][side u8][sub_index u8]
//!       [total_size_lots u64][displayed_size_lots u64]
//!       [limit_ticks u64][expires_at_slot u64]   — 35 bytes

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::cpi::create_pda_account;
use crate::guard::{assert_market, assert_market_book, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::ICEBERG_ORDER_SEED;
use crate::state::{IcebergOrderV3, Market, ICEBERG_FLAG_ACTIVE, ICEBERG_ORDER_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const ICEBERG_LEN: usize = core::mem::size_of::<IcebergOrderV3>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, market, market_book, iceberg_order, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 35 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let iceberg_id = data[0];
    let side = data[1];
    let sub_index = data[2];
    let total_size_lots = u64::from_le_bytes(data[3..11].try_into().unwrap());
    let displayed_size_lots = u64::from_le_bytes(data[11..19].try_into().unwrap());
    let limit_ticks = u64::from_le_bytes(data[19..27].try_into().unwrap());
    let expires_at_slot = u64::from_le_bytes(data[27..35].try_into().unwrap());

    assert_signer(trader)?;
    assert_market(market, program_id)?;
    assert_market_book(market_book, market, program_id)?;

    // ── validate (mirror anchor place_iceberg_order_v3) ─────────────────
    if side > 1 || total_size_lots == 0 || displayed_size_lots == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if displayed_size_lots > total_size_lots || limit_ticks == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    let (min_base_lots, tick_size, mark_price_ticks) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.min_base_lots, m.tick_size, m.mark_price_ticks)
    };
    if displayed_size_lots < min_base_lots {
        return Err(ProgramError::InvalidArgument); // size below min lot
    }
    if tick_size == 0 || limit_ticks % tick_size != 0 {
        return Err(ProgramError::InvalidArgument); // price not on tick
    }
    // Anti-stuffing band — every other resting path (place/modify/taker-residual/
    // vault) enforces it; iceberg dropped it, letting a trader plant arbitrarily-
    // priced resting liquidity (poisoned depth / off-book TWAP feed). Parity fix.
    if !crate::book::price_within_band(mark_price_ticks, limit_ticks, crate::constants::MAX_RESTING_ORDER_DEVIATION_BPS) {
        return Err(ProgramError::InvalidArgument); // price outside anti-stuffing band
    }

    let now = Clock::get()?.slot;
    if expires_at_slot > 0 && expires_at_slot <= now {
        return Err(ProgramError::InvalidArgument);
    }

    let first_chunk = displayed_size_lots.min(total_size_lots);

    // ── create the PDA (unique per (market, trader, iceberg_id)) ────────
    assert_uninitialized(iceberg_order)?;
    let id_arr = [iceberg_id];
    let bump = assert_pda(
        iceberg_order,
        &[
            ICEBERG_ORDER_SEED,
            &market.key()[..],
            &trader.key()[..],
            &id_arr[..],
        ],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(ICEBERG_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(ICEBERG_ORDER_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&trader.key()[..]),
        Seed::from(&id_arr[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        trader,
        iceberg_order,
        system_program,
        lamports,
        ICEBERG_LEN as u64,
        program_id,
        &signer,
    )?;

    // ── insert the FIRST displayed chunk into the book ──────────────────
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
        let side_is_bid = side == 0;
        let order = RestingOrderV2 {
            order_id: encode_order_id(limit_ticks, seq, side_is_bid),
            seq,
            price_ticks: limit_ticks,
            size_lots: first_chunk,
            expires_at_slot,
            trader: *trader.key(),
            last_valid_slot: if now > u32::MAX as u64 { u32::MAX } else { now as u32 },
            side,
            order_type: 0, // limit
            flags: 0,
            sub_index,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
        inserted_seq = seq;
    }

    // ── write the iceberg account ───────────────────────────────────────
    unsafe {
        let ice = &mut *(iceberg_order.borrow_mut_data_unchecked().as_mut_ptr() as *mut IcebergOrderV3);
        ice.disc = ICEBERG_ORDER_V3_DISC;
        ice.trader = *trader.key();
        ice.market = *market.key();
        ice.limit_ticks = limit_ticks;
        ice.total_size_lots = total_size_lots;
        ice.remaining_lots = total_size_lots.saturating_sub(first_chunk);
        ice.displayed_size_lots = displayed_size_lots;
        ice.child_order_seq = inserted_seq;
        ice.created_at_slot = now;
        ice.expires_at_slot = expires_at_slot;
        ice.bump = bump;
        ice.iceberg_id = iceberg_id;
        ice.side = side;
        ice.flags = ICEBERG_FLAG_ACTIVE;
        ice.sub_index = sub_index;
        ice._reserved = [0u8; 3];
    }
    Ok(())
}
