//! vault_place_order_v3 — the strategist rests a limit order on the book with
//! the VAULT PDA as the trader (so fills route to the vault's TraderState).
//! Faithful port of the Anchor `vault_place_order_v3`. Same book-insert path +
//! guards as `place_order` (the order lives in the SAME MarketBookHandle, so it
//! obeys the identical flag / tick / OI / anti-stuffing rules), but authorized
//! by the vault's strategist instead of the order's owner.
//!
//! accounts: [strategist (signer), vault (program-owned, r), market
//!            (program-owned, r), market_book (PDA, w)]
//! data: [side u8][size_lots u64][limit_ticks u64][expires_at_slot u64][flags u8]
//!       — 26 bytes

use crate::book::{encode_order_id, price_within_band, MarketBookHandle, RestingOrderV2};
use crate::constants::MAX_RESTING_ORDER_DEVIATION_BPS;
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_signer};
use crate::state::{Market, VaultV3, MARKET_STATUS_ACTIVE, VAULT_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

const FLAGS_VALID_MASK: u8 = 0b0111_1111;
const FLAG_REDUCE_ONLY: u8 = 0b0000_0010;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [strategist, vault, market, market_book, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 26 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let side = data[0];
    let size_lots = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let limit_ticks = u64::from_le_bytes(data[9..17].try_into().unwrap());
    let expires_at_slot = u64::from_le_bytes(data[17..25].try_into().unwrap());
    let flags = data[25];

    assert_signer(strategist)?;
    if side > 1
        || size_lots == 0
        || limit_ticks == 0
        || (flags & !FLAGS_VALID_MASK) != 0
        || (flags & FLAG_REDUCE_ONLY) != 0
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    let now_slot = Clock::get()?.slot;
    if expires_at_slot != 0 && expires_at_slot <= now_slot {
        return Err(ProgramError::InvalidArgument);
    }

    // Only the vault's strategist may trade the vault PDA.
    assert_owned_by(vault, pid)?;
    assert_disc(vault, &VAULT_V3_DISC)?;
    let vault_pk = *vault.key();
    {
        let d = vault.try_borrow_data()?;
        let v = unsafe { &*(d.as_ptr() as *const VaultV3) };
        if &v.strategist != strategist.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    assert_market(market, pid)?;
    assert_market_book(market_book, market, pid)?;

    // ── market-state guards (same as place_order) ───────────────────────
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if m.status != MARKET_STATUS_ACTIVE {
            return Err(ProgramError::Custom(4));
        }
        if size_lots < m.min_base_lots {
            return Err(ProgramError::Custom(1));
        }
        if m.tick_size == 0 || limit_ticks % m.tick_size != 0 {
            return Err(ProgramError::Custom(2));
        }
        if !price_within_band(m.mark_price_ticks, limit_ticks, MAX_RESTING_ORDER_DEVIATION_BPS) {
            return Err(ProgramError::Custom(5));
        }
        if m.max_oi_base_lots > 0 {
            let cur = if side == 0 { m.long_oi_lots } else { m.short_oi_lots };
            if cur.saturating_add(size_lots) > m.max_oi_base_lots {
                return Err(ProgramError::Custom(3));
            }
        }
    }

    // ── allocate seq + insert the resting order (trader = vault PDA) ─────
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
            size_lots,
            expires_at_slot,
            trader: vault_pk,
            last_valid_slot: if now_slot > u32::MAX as u64 { u32::MAX } else { now_slot as u32 },
            side,
            order_type: 0, // limit
            flags,
            sub_index: 0, // the vault's main TraderState
        };
        if side_is_bid {
            handle.insert_bid(order)?;
        } else {
            handle.insert_ask(order)?;
        }
    }
    Ok(())
}
