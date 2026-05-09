//! Stress-lattice maintenance margin — integer-arithmetic Rust port.
//!
//! For each scenario s ∈ Σ, compute portfolio loss + maintenance margin on
//! the stressed notional. Required margin is the worst-case scenario loss.
//!
//! Hedged property: a long+short on the same market cancels directional
//! risk in every scenario; only the maintenance margin on stressed notional
//! remains.
//!
//! Cost: O(N_positions × N_scenarios). Bounded at compile time by
//! MAX_POSITIONS_PER_TRADER × MAX_STRESS_SCENARIOS = 16 × 64 = 1024 evals.

use super::funding::{funding_owed, FundingIndex};
use super::lot::Ticks;
use super::order::Side;
use crate::constants::BPS_DENOM;
use crate::errors::{FlashBookError, OrOverflow};
use anchor_lang::prelude::*;

/// A single per-market shock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AnchorSerialize, AnchorDeserialize)]
pub struct StressShock {
    pub market: Pubkey,
    /// Signed shock in basis points. Positive = price up.
    pub shock_bps: i32,
}

/// A scenario is a vector of per-market shocks. Markets not listed are
/// implicitly unshocked (0 bps).
pub type Scenario = Vec<StressShock>;

/// Snapshot of a position used in margin assessment. The matcher lives in
/// pure-Rust space, so it doesn't reach into Anchor accounts directly.
#[derive(Debug, Clone, Copy)]
pub struct PositionSnapshot {
    pub market: Pubkey,
    pub side: Side,
    pub size_lots: u64,
    pub entry_price: Ticks,
    pub cum_funding_index_at_entry: FundingIndex,
}

/// Snapshot of a market used in margin assessment.
#[derive(Debug, Clone, Copy)]
pub struct MarketSnapshot {
    pub market: Pubkey,
    pub mark_price: Ticks,
    pub cum_funding_index: FundingIndex,
    /// Maintenance margin ratio in bps (e.g. 125 = 1.25%).
    pub maintenance_margin_bps: u32,
    /// `tick_size` for notional computation.
    pub tick_size: u64,
    /// CME-style concentration tier: positions with `size_lots >=
    /// concentration_threshold_lots` use `maintenance_margin_bps +
    /// concentration_extra_mmr_bps` as their effective MMR. Penalises
    /// whales whose size is harder to liquidate without market impact.
    /// 0 threshold = tier disabled (legacy single-mmr behaviour).
    pub concentration_threshold_lots: u64,
    pub concentration_extra_mmr_bps: u32,
}

impl MarketSnapshot {
    /// Effective maintenance margin in bps for a position of size
    /// `size_lots` on this market. Applies the concentration tier
    /// extra if the position crosses the threshold.
    pub fn effective_mmr_bps(&self, size_lots: u64) -> u32 {
        if self.concentration_threshold_lots > 0
            && size_lots >= self.concentration_threshold_lots
        {
            self.maintenance_margin_bps
                .saturating_add(self.concentration_extra_mmr_bps)
        } else {
            self.maintenance_margin_bps
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MarginAssessment {
    /// Required margin in quote-lots, ≥ 0.
    pub required_quote_lots: u64,
    /// Trader's equity = collateral + unrealized PnL − funding owed (signed).
    pub equity_quote_lots_signed: i128,
    /// Healthy iff equity ≥ required.
    pub is_healthy: bool,
    /// 0-based scenario index that produced the worst case.
    pub worst_scenario_idx: u32,
}

/// Compute unrealized PnL of a position at `at_price`. Result in quote-lots
/// (signed; positive = trader gains).
fn unrealized_pnl_quote_lots(
    pos: &PositionSnapshot,
    at_price: Ticks,
    tick_size: u64,
) -> Result<i128> {
    let sign: i128 = if pos.side == Side::Long { 1 } else { -1 };
    let price_diff: i128 = (at_price.0 as i128) - (pos.entry_price.0 as i128);
    let prod = (pos.size_lots as i128)
        .checked_mul(price_diff)
        .or_overflow()?
        .checked_mul(tick_size as i128)
        .or_overflow()?;
    Ok(sign * prod)
}

/// Apply a bps shock to a price. `+shock_bps` raises price; `-shock_bps`
/// lowers. Returns 0 if the shock would drive price negative.
fn shocked_price(price: Ticks, shock_bps: i32) -> Result<Ticks> {
    let p = price.0 as i128;
    let delta = p
        .checked_mul(shock_bps as i128)
        .or_overflow()?
        .checked_div(BPS_DENOM as i128)
        .or_div_zero()?;
    let r = p.checked_add(delta).or_overflow()?;
    if r <= 0 {
        return Ok(Ticks(0));
    }
    Ok(Ticks(r as u64))
}

fn lookup_market<'a>(
    markets: &'a [MarketSnapshot],
    pk: &Pubkey,
) -> Option<&'a MarketSnapshot> {
    markets.iter().find(|m| m.market == *pk)
}

fn shock_for_market(scenario: &Scenario, market: &Pubkey) -> i32 {
    scenario
        .iter()
        .find(|s| s.market == *market)
        .map(|s| s.shock_bps)
        .unwrap_or(0)
}

/// Assess a trader's margin health.
///
/// `collateral_quote_lots` is the trader's collateral balance in quote-lots.
/// All scenarios are evaluated; the worst-case loss determines `required`.
pub fn assess_margin(
    positions: &[PositionSnapshot],
    markets: &[MarketSnapshot],
    scenarios: &[Scenario],
    collateral_quote_lots: u64,
) -> Result<MarginAssessment> {
    // Equity at current marks.
    let mut unrealized_total: i128 = 0;
    let mut funding_total: i128 = 0;
    for pos in positions {
        let m = match lookup_market(markets, &pos.market) {
            Some(m) => m,
            None => continue,
        };
        unrealized_total = unrealized_total
            .checked_add(unrealized_pnl_quote_lots(pos, m.mark_price, m.tick_size)?)
            .or_overflow()?;
        let notional = (pos.size_lots as u128)
            .checked_mul(m.mark_price.0 as u128)
            .or_overflow()?
            .checked_mul(m.tick_size as u128)
            .or_overflow()?;
        if notional > u64::MAX as u128 {
            return Err(error!(FlashBookError::ArithmeticOverflow));
        }
        funding_total = funding_total
            .checked_add(funding_owed(
                pos.side == Side::Long,
                notional as u64,
                m.cum_funding_index,
                pos.cum_funding_index_at_entry,
            )?)
            .or_overflow()?;
    }
    let equity_signed = (collateral_quote_lots as i128)
        .checked_add(unrealized_total)
        .or_overflow()?
        .checked_sub(funding_total)
        .or_underflow()?;

    // For each scenario, compute total loss + maintenance margin.
    let mut worst_loss: u64 = 0;
    let mut worst_idx: u32 = 0;

    for (idx, scenario) in scenarios.iter().enumerate() {
        let mut scenario_loss_signed: i128 = 0;
        for pos in positions {
            let m = match lookup_market(markets, &pos.market) {
                Some(m) => m,
                None => continue,
            };
            let shock = shock_for_market(scenario, &pos.market);
            let stressed = shocked_price(m.mark_price, shock)?;

            // Loss = -unrealized at stressed price (positive = bad for trader).
            let pnl = unrealized_pnl_quote_lots(pos, stressed, m.tick_size)?;
            scenario_loss_signed = scenario_loss_signed.checked_sub(pnl).or_underflow()?;

            // Maintenance margin on stressed notional.
            let stressed_notional = (pos.size_lots as i128)
                .checked_mul(stressed.0 as i128)
                .or_overflow()?
                .checked_mul(m.tick_size as i128)
                .or_overflow()?;
            let eff_mmr = m.effective_mmr_bps(pos.size_lots);
            let mm = stressed_notional
                .checked_mul(eff_mmr as i128)
                .or_overflow()?
                .checked_div(BPS_DENOM as i128)
                .or_div_zero()?;
            scenario_loss_signed = scenario_loss_signed.checked_add(mm).or_overflow()?;
        }
        let loss_unsigned: u64 = if scenario_loss_signed <= 0 {
            0
        } else if scenario_loss_signed > u64::MAX as i128 {
            u64::MAX
        } else {
            scenario_loss_signed as u64
        };
        if loss_unsigned > worst_loss {
            worst_loss = loss_unsigned;
            worst_idx = idx as u32;
        }
    }

    // Healthy iff equity ≥ required.
    let required_signed: i128 = worst_loss as i128;
    let is_healthy = equity_signed >= required_signed;

    Ok(MarginAssessment {
        required_quote_lots: worst_loss,
        equity_quote_lots_signed: equity_signed,
        is_healthy,
        worst_scenario_idx: worst_idx,
    })
}

/// Generate a default scenario lattice for a list of markets:
/// - flat
/// - per-market ±{2, 5, 10, 20}%
/// - all-down 10%, all-up 10%
/// - black swan ±30%
pub fn default_scenarios(markets: &[Pubkey]) -> Vec<Scenario> {
    let mut out: Vec<Scenario> = Vec::new();
    out.push(vec![]); // flat

    let single_shocks: [i32; 8] = [-2000, -1000, -500, -200, 200, 500, 1000, 2000];
    for m in markets {
        for s in single_shocks.iter() {
            out.push(vec![StressShock {
                market: *m,
                shock_bps: *s,
            }]);
        }
    }

    let all_down: Vec<StressShock> = markets
        .iter()
        .map(|m| StressShock { market: *m, shock_bps: -1000 })
        .collect();
    let all_up: Vec<StressShock> = markets
        .iter()
        .map(|m| StressShock { market: *m, shock_bps: 1000 })
        .collect();
    let bs_down: Vec<StressShock> = markets
        .iter()
        .map(|m| StressShock { market: *m, shock_bps: -3000 })
        .collect();
    let bs_up: Vec<StressShock> = markets
        .iter()
        .map(|m| StressShock { market: *m, shock_bps: 3000 })
        .collect();
    out.push(all_down);
    out.push(all_up);
    out.push(bs_down);
    out.push(bs_up);
    out
}
