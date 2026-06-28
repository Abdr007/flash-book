//! modify_order_v2 — atomic cancel + place in one instruction. Validates the new
//! params with the same gates as place BEFORE removing the old order, so a
//! malformed modify never silently drops the original. The replacement gets a
//! fresh `seq`; the original order's `sub_index` is preserved.
//!
//! Faithful port of the Anchor `modify_order_v2`.
use crate::book::{self, price_within_band, MarketBookHandle, RestingOrderV2};
use crate::constants::MAX_RESTING_ORDER_DEVIATION_BPS;
use crate::guard::{assert_market, assert_market_book};
use crate::hypertree::NIL;
use crate::state::{Market, MARKET_STATUS_ACTIVE};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar}, ProgramResult,
};

#[inline(always)]
unsafe fn market_of(ai: &AccountInfo) -> &Market {
    &*(ai.borrow_data_unchecked().as_ptr() as *const Market)
}

/// data: [side u8][old_order_id u64][new_size u64][new_limit u64][new_expires u64][new_flags u8]
/// accounts: [trader(signer), market, market_book]
pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 34 || accounts.len() < 3 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let side = data[0];
    let old_order_id = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let new_size_lots = u64::from_le_bytes(data[9..17].try_into().unwrap());
    let new_limit_ticks = u64::from_le_bytes(data[17..25].try_into().unwrap());
    let new_expires_at_slot = u64::from_le_bytes(data[25..33].try_into().unwrap());
    let new_flags = data[33];

    let trader = &accounts[0];
    if !trader.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if side > 1
        || new_size_lots == 0
        || new_limit_ticks == 0
        || (new_flags & !0b0111_1111) != 0
        // H4: reduce_only (bit1) is unenforced on the v2 CLOB — reject it loudly
        // (mirrors place_limit_order so a "protective close" can't open/flip).
        || (new_flags & 0b0000_0010) != 0
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    let now_slot = Clock::get()?.slot;
    if new_expires_at_slot != 0 && new_expires_at_slot <= now_slot {
        return Err(ProgramError::InvalidInstructionData);
    }
    let trader_pk = *trader.key();

    assert_market(&accounts[1], pid)?;
    assert_market_book(&accounts[2], &accounts[1], pid)?;
    unsafe {
        let market = market_of(&accounts[1]);
        if market.status != MARKET_STATUS_ACTIVE {
            return Err(ProgramError::Custom(4)); // market not active
        }
        if new_size_lots < market.min_base_lots {
            return Err(ProgramError::Custom(1)); // SizeBelowMinLot
        }
        if market.tick_size == 0 || new_limit_ticks % market.tick_size != 0 {
            return Err(ProgramError::Custom(2)); // PriceNotOnTick
        }
        // Anti-stuffing: the replacement must sit within the band of the mark.
        if !price_within_band(market.mark_price_ticks, new_limit_ticks, MAX_RESTING_ORDER_DEVIATION_BPS) {
            return Err(ProgramError::Custom(5)); // too far from mark
        }
        if market.max_oi_base_lots > 0 {
            let cur = if side == 0 { market.long_oi_lots } else { market.short_oi_lots };
            if cur.saturating_add(new_size_lots) > market.max_oi_base_lots {
                return Err(ProgramError::Custom(3)); // OpenInterestCapExceeded
            }
        }

        let book_data = accounts[2].borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;
        // Bind the book to THIS market (defence beyond the PDA check).
        if &handle.header.market_pubkey != accounts[1].key() {
            return Err(ProgramError::InvalidArgument);
        }
        let side_is_bid = side == 0;

        // Phase 1: locate + verify ownership of the old order.
        let old_idx = if side_is_bid {
            handle.lookup_bid_by_order_id(old_order_id)
        } else {
            handle.lookup_ask_by_order_id(old_order_id)
        };
        if old_idx == NIL {
            return Err(ProgramError::Custom(4)); // not found
        }
        let old_sub_index = {
            let order = handle.order_at(old_idx);
            if order.trader != trader_pk {
                return Err(ProgramError::Custom(1100)); // wrong trader
            }
            order.sub_index
        };

        // Phase 2: remove the old order.
        if side_is_bid {
            handle.remove_bid_node(old_idx);
        } else {
            handle.remove_ask_node(old_idx);
        }

        // Phase 3: fresh seq, build + insert the replacement (preserves sub_index).
        let new_seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        handle.header.order_seq_counter = new_seq;
        let order = RestingOrderV2 {
            order_id: book::encode_order_id(new_limit_ticks, new_seq, side_is_bid),
            seq: new_seq,
            price_ticks: new_limit_ticks,
            size_lots: new_size_lots,
            expires_at_slot: new_expires_at_slot,
            trader: trader_pk,
            last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
            side,
            order_type: 0,
            flags: new_flags,
            sub_index: old_sub_index,
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }
    Ok(())
}
