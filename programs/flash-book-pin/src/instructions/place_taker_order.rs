//! place_taker_order_v2 — walk the opposite side of the book best-first, fill
//! every crossing resting order (honouring self-trade-prevention, expiry, and
//! the post-only / IOC / FOK flags), then rest any residual as a limit order.
//!
//! Faithful port of the Anchor `place_taker_order_v2` matcher core. Two
//! differences forced by the `no_std`, zero-allocation target:
//!   * matches are collected into fixed-size stack buffers (no `Vec`), bounded
//!     by `MAX_TAKER_MATCHES` per instruction (CU + 4 KB stack frame bound);
//!   * fills are applied silently — the Pinocchio port does not emit events yet
//!     (mirrors the ported `place`/`cancel`).
use crate::book::{self, MarketBookHandle, RestingOrderV2};
use crate::hypertree::{DataIndex, NIL};
use crate::state::Market;
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar}, ProgramResult,
};

#[inline(always)]
unsafe fn market_of(ai: &AccountInfo) -> &Market {
    &*(ai.borrow_data_unchecked().as_ptr() as *const Market)
}

// Advanced flags (Phoenix / Manifest / HL parity) — same bit layout as Anchor.
const FLAG_POST_ONLY: u8 = 1 << 0;
const FLAG_IOC: u8 = 1 << 2;
const FLAG_FOK: u8 = 1 << 6;
// STP modes (2 bits at positions 4-5):
const STP_CANCEL_OLDEST: u8 = 0b01; // cancel the resting maker, keep walking
const STP_CANCEL_BOTH: u8 = 0b10; // reject the entire taker order

// Per-instruction walk cap. Bounds CU and keeps the match buffers inside BPF's
// 4 KB stack frame (64×(u32+u64+u32) ≈ 1 KB).
const MAX_TAKER_MATCHES: usize = 64;

/// data: [side u8][size_lots u64][limit_ticks u64][expires u64][flags u8][sub_index u8]
/// accounts: [trader(signer), market, market_book]
pub fn process(_pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
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
    if !trader.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if side > 1 || size_lots == 0 || limit_ticks == 0 || (flags & !0b0111_1111) != 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let now_slot = Clock::get()?.slot;
    if expires_at_slot != 0 && expires_at_slot <= now_slot {
        return Err(ProgramError::InvalidInstructionData);
    }
    let trader_pk = *trader.key();

    unsafe {
        let market = market_of(&accounts[1]);
        if size_lots < market.min_base_lots {
            return Err(ProgramError::Custom(1)); // SizeBelowMinLot
        }
        if market.tick_size > 0 && limit_ticks % market.tick_size != 0 {
            return Err(ProgramError::Custom(2)); // PriceNotOnTick
        }
        // Per-market OI hard cap — mirror the limit path (MATCH-H1): bound the
        // full taker size against the side's current OI.
        if market.max_oi_base_lots > 0 {
            let cur = if side == 0 { market.long_oi_lots } else { market.short_oi_lots };
            if cur.saturating_add(size_lots) > market.max_oi_base_lots {
                return Err(ProgramError::Custom(3)); // OpenInterestCapExceeded
            }
        }
        let min_base_lots = market.min_base_lots;

        let book_data = accounts[2].borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;

        let taker_seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        handle.header.order_seq_counter = taker_seq;
        let side_is_bid = side == 0;
        let taker_order_id = book::encode_order_id(limit_ticks, taker_seq, side_is_bid);

        let post_only = flags & FLAG_POST_ONLY != 0;
        let ioc = flags & FLAG_IOC != 0;
        let fok = flags & FLAG_FOK != 0;
        let stp_mode = (flags >> 4) & 0b11;

        // ── Phase 1: walk opposite side best-first, collect matches ──────────
        // Mid-iteration removal corrupts the RBT traversal, so collect
        // (node index, fill size) for matches + indices for STP_CANCEL_OLDEST,
        // then apply mutations after the walk.
        let mut match_idx = [NIL; MAX_TAKER_MATCHES];
        let mut match_fill = [0u64; MAX_TAKER_MATCHES];
        let mut n_matches: usize = 0;
        let mut stp_cancel = [NIL; MAX_TAKER_MATCHES];
        let mut n_stp: usize = 0;
        let mut remaining = size_lots;
        let mut stp_aborted = false;

        {
            let mut walk = |idx: DataIndex, o: &RestingOrderV2| -> bool {
                if n_matches >= MAX_TAKER_MATCHES || remaining == 0 {
                    return false;
                }
                // Cross check: a bid takes asks at price ≤ limit; an ask takes
                // bids at price ≥ limit. best-first ⇒ first non-crossing stops.
                let crosses = if side_is_bid {
                    o.price_ticks <= limit_ticks
                } else {
                    o.price_ticks >= limit_ticks
                };
                if !crosses {
                    return false;
                }
                if o.expires_at_slot > 0 && now_slot > o.expires_at_slot {
                    return true; // skip expired, keep walking
                }
                if o.trader == trader_pk {
                    match stp_mode {
                        STP_CANCEL_OLDEST => {
                            stp_cancel[n_stp] = idx;
                            n_stp += 1;
                            return true;
                        }
                        STP_CANCEL_BOTH => {
                            stp_aborted = true;
                            return false;
                        }
                        _ => return true, // STP_SKIP (default): skip self, keep walking
                    }
                }
                let fill = o.size_lots.min(remaining);
                match_idx[n_matches] = idx;
                match_fill[n_matches] = fill;
                n_matches += 1;
                remaining -= fill;
                true
            };
            if side_is_bid {
                handle.for_each_ask_best_first(&mut walk);
            } else {
                handle.for_each_bid_best_first(&mut walk);
            }
        }

        if stp_aborted {
            return Err(ProgramError::Custom(10)); // SelfTrade
        }
        // STP_CANCEL_OLDEST: cancel each self-matched resting order first.
        for i in 0..n_stp {
            if side_is_bid {
                handle.remove_ask_node(stp_cancel[i]);
            } else {
                handle.remove_bid_node(stp_cancel[i]);
            }
        }
        // post_only would-cross ⇒ reject (caller wants guaranteed-maker).
        if post_only && n_matches > 0 {
            return Err(ProgramError::Custom(11)); // PostOnlyWouldCross
        }
        // FOK ⇒ require the entire order filled.
        if fok && remaining > 0 {
            return Err(ProgramError::Custom(12)); // FillOrKillNotFilled
        }

        // ── Phase 2: apply each match (decrement, remove if fully consumed) ──
        for i in 0..n_matches {
            let new_size = handle.decrement_size_at(match_idx[i], match_fill[i])?;
            if new_size == 0 {
                if side_is_bid {
                    handle.remove_ask_node(match_idx[i]);
                } else {
                    handle.remove_bid_node(match_idx[i]);
                }
            }
        }

        // ── Phase 3: residual — IOC drops it; else rest as a limit order, but
        // only if it still meets the per-market minimum lot (MATCH-H2: no dust).
        if remaining > 0 && !ioc && remaining >= min_base_lots {
            let order = RestingOrderV2 {
                order_id: taker_order_id,
                seq: taker_seq,
                price_ticks: limit_ticks,
                size_lots: remaining,
                expires_at_slot,
                trader: trader_pk,
                last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
                side,
                order_type: 0,
                flags,
                sub_index,
            };
            if side_is_bid {
                handle.insert_bid(order)?;
            } else {
                handle.insert_ask(order)?;
            }
        }
    }
    Ok(())
}
