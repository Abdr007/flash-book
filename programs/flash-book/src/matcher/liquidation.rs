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
                stp_mode: crate::matcher::order::StpMode::CancelNewest,
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
    // RISK-H3: checked, matching the `penalty` path below. The old raw `*`
    // chain could panic (debug) / wrap (release) on a numerically extreme
    // position, producing a garbage shortfall → wrong insurance draw.
    let price_diff = (fill_price.0 as i128)
        .checked_sub(pos.entry_price.0 as i128)
        .or_underflow()?;
    let pnl = (pos.size_lots as i128)
        .checked_mul(price_diff)
        .or_overflow()?
        .checked_mul(market_snapshot.tick_size as i128)
        .or_overflow()?
        .checked_mul(sign)
        .or_overflow()?;
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
    // ─── i128 → u64 saturation: INTENTIONAL, not a bug. ─────────────────
    // Every arithmetic step above used `checked_*` and would have errored
    // out on legitimate overflow (e.g. wrap, sign confusion). Reaching this
    // point means the math succeeded; we only need to fit the i128 results
    // into the u64 wire fields of `ShortfallResult`. The clamps below are
    // a defensive "this notional is implausibly large; pin to u64::MAX so
    // downstream consumers see the largest representable value rather than
    // a wrapped tiny number". With realistic market params (max_oi_base_lots
    // bounded, leverage capped, BPS_DENOM 10⁴) reaching u64::MAX requires
    // a position whose dollar-notional exceeds ~$18.4 quintillion — five
    // orders of magnitude beyond global derivatives notional.
    //
    // DO NOT replace this with checked_into / try_into — that would
    // *abort the liquidation* on a numerically extreme position, leaving
    // an unwinnable position open. Saturation is the safe failure mode.
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

// ─────────────────────────────────────────────────────────────────────────────
// Dual-source health price (P-LIQ-1). The WORSE of mark and oracle for the
// position's direction — the pure core of `liquidate_position_v2`'s health gate.
// LONG: lower is worse → min(mark, oracle); SHORT: higher is worse → max(mark,
// oracle). A LONG ignores an UNSET oracle (`oracle == 0`) — otherwise it would
// read as price 0 (max loss) and wrongfully liquidate. Returns (price, source)
// with source 0 = mark, 1 = oracle, 2 = equal. Extracted so the worse-of
// selection is machine-checked and the handler calls the proven function.
// ─────────────────────────────────────────────────────────────────────────────

/// Worse-of-(mark, oracle) health price for a position. See module note.
#[inline]
pub fn worse_of_health_price(mark_t: u64, oracle_t: u64, is_long: bool) -> (u64, u8) {
    if is_long {
        if oracle_t > 0 && oracle_t < mark_t {
            (oracle_t, 1)
        } else if oracle_t > 0 && oracle_t == mark_t {
            (mark_t, 2)
        } else {
            (mark_t, 0)
        }
    } else if oracle_t > mark_t {
        (oracle_t, 1)
    } else if oracle_t == mark_t {
        (mark_t, 2)
    } else {
        (mark_t, 0)
    }
}

/// FV: machine-checked correctness of the dual-source health price (Kani,
/// comparison-only → fast). Proves P-LIQ-1's core: the health price is always the
/// WORSE of the two real sources for the position's side, never under-states risk,
/// and never invents a price.
#[cfg(kani)]
mod health_price_kani_proofs {
    use super::worse_of_health_price;

    /// LONG: the health price is never HIGHER than the mark, and never higher than
    /// a LIVE oracle — i.e. it is the worse (lower) of the two. So a long is never
    /// liquidated at a price more favourable than both sources.
    #[kani::proof]
    fn health_price_worse_for_long() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let (hp, _) = worse_of_health_price(mark, oracle, true);
        assert!(hp <= mark);
        assert!(oracle == 0 || hp <= oracle);
    }

    /// SHORT: the health price is never LOWER than the mark OR the oracle — the
    /// worse (higher) of the two.
    #[kani::proof]
    fn health_price_worse_for_short() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let (hp, _) = worse_of_health_price(mark, oracle, false);
        assert!(hp >= mark);
        assert!(hp >= oracle);
    }

    /// The health price is ALWAYS one of the two real sources — never a fabricated
    /// value (no third price can enter the liquidation decision).
    #[kani::proof]
    fn health_price_is_a_real_source() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let is_long: bool = kani::any();
        let (hp, src) = worse_of_health_price(mark, oracle, is_long);
        assert!(hp == mark || hp == oracle);
        assert!(src <= 2);
    }
}
