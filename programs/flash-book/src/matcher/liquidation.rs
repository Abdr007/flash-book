//! Liquidation engine — Rust port of the in-loop, single-batch model.
//!
//! Per-batch flow:
//!   1. detect_liquidations() identifies traders whose stress-lattice
//!      assessment puts equity < required.
//!   2. generate_liquidation_orders() converts each unhealthy position into
//!      a synthetic taker order on the opposite side, limit at oracle ±
//!      liq_penalty.
//!   3. The matcher clears these in the same batch as everything else.
//!   4. compute_shortfall() examines each filled liquidation; bankrupt
//!      shortfall flows to insurance fund / ADL.
//!
//! Properties:
//!   - Deterministic: same inputs → same liquidation price (no keeper race).
//!   - Cascade-resilient: all liqs in a batch clear at the same uniform price.
//!   - No MEV: no external party captures liquidation fees.

use super::lot::{BaseLots, Ticks};
use super::order::{Order, OrderType, Side};
use super::risk::{assess_margin, MarketSnapshot, PositionSnapshot, Scenario};
use crate::constants::BPS_DENOM;
use crate::errors::OrOverflow;
use anchor_lang::prelude::*;

/// A trader who has been identified as liquidatable.
#[derive(Debug, Clone)]
pub struct LiquidationCandidate {
    pub trader: Pubkey,
    pub positions: Vec<PositionSnapshot>,
    pub equity_signed: i128,
    pub required: u64,
    pub worst_scenario_idx: u32,
}

/// All traders + their positions are surfaced via this trait so the
/// matcher core stays storage-agnostic. The Anchor program implements it
/// over its account iterator; tests implement it over Vecs.
pub fn detect_liquidations(
    traders: &[(Pubkey, Vec<PositionSnapshot>, u64 /* collateral */)],
    markets: &[MarketSnapshot],
    scenarios: &[Scenario],
) -> Result<Vec<LiquidationCandidate>> {
    let mut out = Vec::new();
    for (trader, positions, collateral) in traders {
        if positions.is_empty() {
            continue;
        }
        let a = assess_margin(positions, markets, scenarios, *collateral)?;
        if !a.is_healthy {
            out.push(LiquidationCandidate {
                trader: *trader,
                positions: positions.clone(),
                equity_signed: a.equity_quote_lots_signed,
                required: a.required_quote_lots,
                worst_scenario_idx: a.worst_scenario_idx,
            });
        }
    }
    Ok(out)
}

/// Generate one liquidation order per position of each candidate.
/// `base_seq` is the starting monotonic sequence number; each emitted
/// order gets a unique seq for FIFO ordering.
pub fn generate_liquidation_orders(
    candidates: &[LiquidationCandidate],
    markets: &[MarketSnapshot],
    base_seq: u64,
    liq_penalty_bps: u32,
) -> Result<Vec<Order>> {
    let mut out = Vec::new();
    let mut seq = base_seq;
    for c in candidates {
        for pos in &c.positions {
            let m = match markets.iter().find(|m| m.market == pos.market) {
                Some(m) => m,
                None => continue,
            };
            if pos.size_lots == 0 {
                continue;
            }
            let close_side = pos.side.opposite();
            // Limit = oracle adjusted by ± penalty depending on close side.
            let penalty_delta = (m.mark_price.0 as i128)
                .checked_mul(liq_penalty_bps as i128)
                .or_overflow()?
                .checked_div(BPS_DENOM as i128)
                .or_div_zero()?;
            let limit = match close_side {
                Side::Short => m.mark_price.0 as i128 - penalty_delta,
                Side::Long => m.mark_price.0 as i128 + penalty_delta,
            };
            let limit = if limit < 0 { 0 } else { limit as u64 };

            seq = seq.checked_add(1).or_overflow()?;
            out.push(Order {
                id: seq,
                trader: c.trader,
                side: close_side,
                order_type: OrderType::Liquidation,
                size: BaseLots(pos.size_lots),
                limit_price: Ticks(limit),
                seq,
                post_only: false,
            });
        }
    }
    Ok(out)
}

/// Bankruptcy resolution result for a single liquidation fill.
#[derive(Debug, Clone, Copy)]
pub struct ShortfallResult {
    pub liquidation_penalty_quote_lots: u64,
    pub shortfall_quote_lots: u64,
    pub collateral_recovered_quote_lots: u64,
}

/// Compute realized shortfall for a position liquidated at `fill_price`.
/// `collateral` is the trader's pre-fill collateral.
pub fn compute_shortfall(
    pos: &PositionSnapshot,
    fill_price: Ticks,
    collateral_quote_lots: u64,
    market_snapshot: &MarketSnapshot,
    liq_penalty_bps: u32,
) -> Result<ShortfallResult> {
    let sign: i128 = if pos.side == Side::Long { 1 } else { -1 };
    let pnl = sign
        * (pos.size_lots as i128)
        * ((fill_price.0 as i128) - (pos.entry_price.0 as i128))
        * (market_snapshot.tick_size as i128);
    let penalty = (pos.size_lots as i128)
        .checked_mul(fill_price.0 as i128)
        .or_overflow()?
        .checked_mul(market_snapshot.tick_size as i128)
        .or_overflow()?
        .checked_mul(liq_penalty_bps as i128)
        .or_overflow()?
        .checked_div(BPS_DENOM as i128)
        .or_div_zero()?;
    let remaining = (collateral_quote_lots as i128)
        .checked_add(pnl)
        .or_overflow()?
        .checked_sub(penalty)
        .or_underflow()?;
    let penalty_u64 = if penalty < 0 { 0 } else if penalty > u64::MAX as i128 { u64::MAX } else { penalty as u64 };
    if remaining >= 0 {
        let recovered = if remaining > u64::MAX as i128 { u64::MAX } else { remaining as u64 };
        Ok(ShortfallResult {
            liquidation_penalty_quote_lots: penalty_u64,
            shortfall_quote_lots: 0,
            collateral_recovered_quote_lots: recovered,
        })
    } else {
        let shortfall_signed = -remaining;
        let shortfall = if shortfall_signed > u64::MAX as i128 { u64::MAX } else { shortfall_signed as u64 };
        Ok(ShortfallResult {
            liquidation_penalty_quote_lots: penalty_u64,
            shortfall_quote_lots: shortfall,
            collateral_recovered_quote_lots: 0,
        })
    }
}
