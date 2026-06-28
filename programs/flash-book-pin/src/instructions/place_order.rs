//! place_limit_order_v2 — validate + insert a RESTING limit order into the book.
//! Trader-signed maker path: validated, assigned a sequence, and inserted into
//! the bid/ask hypertree (it does NOT cross/match here — taker matching is the
//! sequencer-driven `apply_fill` path). Delegates to the ported, host-tested
//! `MarketBookHandle::insert_bid/ask`. Full anchor `place_limit_v2_core` parity.
//!
//! Guards (anchor parity): side/size/price well-formed; flags within the valid
//! mask AND the reduce-only bit (1) rejected LOUDLY (the v2 CLOB has no
//! settlement-time reduce-only check, so a "protective close" must not silently
//! open/flip); expiry in the future; market Active; size ≥ min_base_lots; price
//! on tick; within the anti-stuffing band of the mark; under the per-side OI cap;
//! book bound to its market (PDA + recorded `market_pubkey`).
//!
//! data: [side u8][size_lots u64][limit_ticks u64][expires_at_slot u64][flags u8][sub_index u8]
//! accounts: [trader (signer), market (program-owned, r), market_book (PDA, owned, w)]

use crate::book::{encode_order_id, price_within_band, MarketBookHandle, RestingOrderV2};
use crate::constants::MAX_RESTING_ORDER_DEVIATION_BPS;
use crate::guard::{assert_market, assert_market_book};
use crate::state::{Market, MARKET_STATUS_ACTIVE};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

/// Flag bits: only the low 7 are valid; bit1 (reduce-only) is rejected.
const FLAGS_VALID_MASK: u8 = 0b0111_1111;
const FLAG_REDUCE_ONLY: u8 = 0b0000_0010;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 26 || accounts.len() < 3 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let side = data[0];
    let size_lots = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let limit_ticks = u64::from_le_bytes(data[9..17].try_into().unwrap());
    let expires_at_slot = u64::from_le_bytes(data[17..25].try_into().unwrap());
    let flags = data[25];
    let sub_index = data.get(26).copied().unwrap_or(0);

    let trader = &accounts[0];
    let market = &accounts[1];
    let market_book = &accounts[2];

    let now_slot = Clock::get()?.slot;

    // ── input guards ────────────────────────────────────────────────────
    if !trader.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if side > 1
        || size_lots == 0
        || limit_ticks == 0
        || (flags & !FLAGS_VALID_MASK) != 0
        || (flags & FLAG_REDUCE_ONLY) != 0
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    if expires_at_slot != 0 && expires_at_slot <= now_slot {
        return Err(ProgramError::InvalidArgument);
    }

    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;

    // ── market-state guards ─────────────────────────────────────────────
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if m.status != MARKET_STATUS_ACTIVE {
            return Err(ProgramError::Custom(4)); // market not active
        }
        if size_lots < m.min_base_lots {
            return Err(ProgramError::Custom(1)); // below min lot
        }
        if m.tick_size == 0 || limit_ticks % m.tick_size != 0 {
            return Err(ProgramError::Custom(2)); // off tick
        }
        if !price_within_band(m.mark_price_ticks, limit_ticks, MAX_RESTING_ORDER_DEVIATION_BPS) {
            return Err(ProgramError::Custom(5)); // too far from mark (anti-stuffing)
        }
        if m.max_oi_base_lots > 0 {
            let cur = if side == 0 { m.long_oi_lots } else { m.short_oi_lots };
            if cur.saturating_add(size_lots) > m.max_oi_base_lots {
                return Err(ProgramError::Custom(3)); // OI cap
            }
        }
    }

    // ── allocate seq + insert the resting order ─────────────────────────
    unsafe {
        let book_data = market_book.borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        // Bind the book to THIS market (defence-in-depth beyond the PDA check).
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
            size_lots,
            expires_at_slot,
            trader: *trader.key(),
            last_valid_slot: if now_slot > u32::MAX as u64 { u32::MAX } else { now_slot as u32 },
            side,
            order_type: 0, // 0 = limit
            flags,
            sub_index,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }
    Ok(())
}
