//! place_basket_order_v2 / place_basket_order_n_v2 — atomic multi-leg placement:
//! inject K resting limit orders across K distinct markets, gated by a SINGLE
//! joint pre-trade margin check over the PROJECTED post-leg portfolio (all legs
//! assumed to fill at their limits). Faithful port of the Anchor basket orders.
//!
//! pin-model notes (documented degeneracies vs anchor): pin's Market omits the
//! per-trader position-cap params (`max_position_lots_per_trader` /
//! `max_position_ratio_bps`) so the per-leg caps + the `flp_exposure` account are
//! dropped; pin's TraderState has no `orders_this_batch` so the per-batch rate
//! limit is dropped. The joint stress-margin gate (the safety-critical part) is
//! kept in full.
//!
//! accounts: [trader (signer), trader_state (program-owned),
//!            (market, market_book, position) * K]
//! data (v2): leg_a ++ leg_b ; data (n_v2): [K u8] ++ leg * K
//!   leg = [side u8][size_lots u64][limit_ticks u64][post_only u8] (18 bytes)

use crate::book::{encode_order_id, MarketBookHandle, RestingOrderV2};
use crate::constants::BPS_DENOM;
use crate::guard::{assert_disc, assert_market, assert_market_book, assert_owned_by, assert_signer};
use crate::lot::Ticks;
use crate::order::Side;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use crate::state::{Market, Position, TraderState, MARKET_STATUS_ACTIVE, POSITION_DISC, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

const MAX_LEGS: usize = 8;
const LEG_WIRE: usize = 18;
const STRESS_BPS: [i32; 10] = [-3000, -2000, -1000, -500, -200, 200, 500, 1000, 2000, 3000];

#[derive(Clone, Copy)]
struct BasketLeg {
    side: u8,
    size_lots: u64,
    limit_ticks: u64,
    flags: u8,
}

fn parse_leg(data: &[u8], off: usize) -> BasketLeg {
    BasketLeg {
        side: data[off],
        size_lots: u64::from_le_bytes(data[off + 1..off + 9].try_into().unwrap()),
        limit_ticks: u64::from_le_bytes(data[off + 9..off + 17].try_into().unwrap()),
        flags: if data[off + 17] != 0 { 1 } else { 0 }, // post_only (informational)
    }
}

fn im_bps(maintenance_bps: u32, max_leverage: u32) -> u32 {
    if max_leverage == 0 {
        return maintenance_bps;
    }
    let lev_im = BPS_DENOM / max_leverage;
    if lev_im > maintenance_bps { lev_im } else { maintenance_bps }
}

/// Project a position's worst-case post-leg state for the joint margin check.
fn project_post_leg(pos: &Position, leg: &BasketLeg, market_key: &Pubkey) -> PositionSnapshot {
    if pos.size_lots == 0 {
        return PositionSnapshot {
            market: *market_key,
            side: if leg.side == 0 { Side::Long } else { Side::Short },
            size_lots: leg.size_lots,
            entry_price: Ticks(leg.limit_ticks),
            cum_funding_index_at_entry: i128::from_le_bytes(pos.cum_funding_index),
            collateral_quote_lots: pos.collateral_quote_lots,
        };
    }
    let (projected_size, projected_side) = if pos.side == leg.side {
        (pos.size_lots.saturating_add(leg.size_lots), pos.side)
    } else if leg.size_lots >= pos.size_lots {
        (leg.size_lots - pos.size_lots, leg.side) // flip
    } else {
        (pos.size_lots - leg.size_lots, pos.side) // reduce
    };
    PositionSnapshot {
        market: *market_key,
        side: if projected_side == 0 { Side::Long } else { Side::Short },
        size_lots: projected_size,
        entry_price: Ticks(pos.entry_price_ticks),
        cum_funding_index_at_entry: i128::from_le_bytes(pos.cum_funding_index),
        collateral_quote_lots: pos.collateral_quote_lots,
    }
}

fn run(pid: &Pubkey, accounts: &[AccountInfo], legs: &[BasketLeg]) -> ProgramResult {
    let k = legs.len();
    if k == 0 || k > MAX_LEGS {
        return Err(ProgramError::InvalidArgument);
    }
    let [trader, trader_state, triples @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if triples.len() != k * 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    assert_signer(trader)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    let (trader_pk, collateral) = {
        let d = trader_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &s.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
        (s.trader, s.collateral_quote_lots)
    };

    let now_slot = Clock::get()?.slot;
    let mut positions: [PositionSnapshot; MAX_LEGS] = core::array::from_fn(|_| zero_pos());
    let mut markets: [MarketSnapshot; MAX_LEGS] = core::array::from_fn(|_| zero_mkt());
    let mut seen: [[u8; 32]; MAX_LEGS] = [[0u8; 32]; MAX_LEGS];

    // ── validate + project each leg ─────────────────────────────────────
    for i in 0..k {
        let market = &triples[3 * i];
        let book = &triples[3 * i + 1];
        let position = &triples[3 * i + 2];
        let leg = &legs[i];

        assert_market(market, pid)?;
        assert_market_book(book, market, pid)?;
        if seen[..i].iter().any(|m| m == market.key()) {
            return Err(ProgramError::InvalidArgument); // distinct markets per basket
        }
        seen[i] = *market.key();

        let (mark, tick, min_lot, status, maint, max_lev) = {
            let d = market.try_borrow_data()?;
            let m = unsafe { &*(d.as_ptr() as *const Market) };
            (m.mark_price_ticks, m.tick_size, m.min_base_lots, m.status, m.maintenance_margin_bps, m.max_leverage)
        };
        // validate_leg_intake
        if leg.side > 1 || leg.size_lots == 0 || leg.limit_ticks == 0 || status != MARKET_STATUS_ACTIVE {
            return Err(ProgramError::InvalidArgument);
        }
        if leg.size_lots < min_lot || tick == 0 || leg.limit_ticks % tick != 0 {
            return Err(ProgramError::InvalidArgument);
        }

        assert_owned_by(position, pid)?;
        assert_disc(position, &POSITION_DISC)?;
        {
            let d = position.try_borrow_data()?;
            let p = unsafe { &*(d.as_ptr() as *const Position) };
            if (p.size_lots != 0) && (&p.trader != &trader_pk || &p.market != market.key()) {
                return Err(ProgramError::InvalidArgument);
            }
            positions[i] = project_post_leg(p, leg, market.key());
        }
        markets[i] = MarketSnapshot {
            market: *market.key(),
            mark_price: Ticks(mark),
            cum_funding_index: 0,
            maintenance_margin_bps: im_bps(maint, max_lev), // RISK-2: initial margin
            tick_size: tick,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        };
    }

    // ── joint pre-trade health gate over the projected portfolio ────────
    let mut row = [StressShock { market: [0u8; 32], shock_bps: 0 }; MAX_LEGS];
    for &bps in STRESS_BPS.iter() {
        for (cell, m) in row[..k].iter_mut().zip(markets[..k].iter()) {
            *cell = StressShock { market: m.market, shock_bps: bps };
        }
        let a = assess_margin(&positions[..k], &markets[..k], &[&row[..k]], collateral)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
        if !a.is_healthy {
            return Err(ProgramError::Custom(250)); // basket would be liquidatable
        }
    }

    // ── inject each leg into its book (post-gate) ───────────────────────
    for i in 0..k {
        let book = &triples[3 * i + 1];
        let market = &triples[3 * i];
        let leg = &legs[i];
        unsafe {
            let bd = book.borrow_mut_data_unchecked();
            let mut handle = MarketBookHandle::from_account_data(bd)?;
            if &handle.header.market_pubkey != market.key() {
                return Err(ProgramError::InvalidArgument);
            }
            let seq = handle.header.order_seq_counter.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
            handle.header.order_seq_counter = seq;
            let side_is_bid = leg.side == 0;
            let order = RestingOrderV2 {
                order_id: encode_order_id(leg.limit_ticks, seq, side_is_bid),
                seq,
                price_ticks: leg.limit_ticks,
                size_lots: leg.size_lots,
                expires_at_slot: 0,
                trader: trader_pk,
                last_valid_slot: if now_slot > u32::MAX as u64 { u32::MAX } else { now_slot as u32 },
                side: leg.side,
                order_type: 0,
                flags: leg.flags,
                sub_index: 0,
            };
            if side_is_bid { handle.insert_bid(order)?; } else { handle.insert_ask(order)?; }
        }
    }
    Ok(())
}

fn zero_pos() -> PositionSnapshot {
    PositionSnapshot {
        market: [0u8; 32], side: Side::Long, size_lots: 0, entry_price: Ticks(0),
        cum_funding_index_at_entry: 0, collateral_quote_lots: 0,
    }
}
fn zero_mkt() -> MarketSnapshot {
    MarketSnapshot {
        market: [0u8; 32], mark_price: Ticks(0), cum_funding_index: 0, maintenance_margin_bps: 0,
        tick_size: 0, concentration_threshold_lots: 0, concentration_extra_mmr_bps: 0,
        side_oi_lots: 0, oi_mmr_slope_bps_per_million_lots: 0, oi_mmr_max_extra_bps: 0,
    }
}

/// place_basket_order_v2 — exactly 2 legs (data = leg_a ++ leg_b).
pub fn v2(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 2 * LEG_WIRE {
        return Err(ProgramError::InvalidInstructionData);
    }
    let legs = [parse_leg(data, 0), parse_leg(data, LEG_WIRE)];
    run(pid, accounts, &legs)
}

/// place_basket_order_n_v2 — K legs (data = [K u8] ++ leg * K).
pub fn n_v2(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let k = data[0] as usize;
    if k == 0 || k > MAX_LEGS || data.len() < 1 + k * LEG_WIRE {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut legs = [BasketLeg { side: 0, size_lots: 0, limit_ticks: 0, flags: 0 }; MAX_LEGS];
    for (i, slot) in legs[..k].iter_mut().enumerate() {
        *slot = parse_leg(data, 1 + i * LEG_WIRE);
    }
    run(pid, accounts, &legs[..k])
}
