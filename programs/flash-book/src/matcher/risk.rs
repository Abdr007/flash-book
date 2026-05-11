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

/// Hyperliquid-style multi-tier MMR. Each tier is a
/// `(min_notional_quote_lots, mmr_bps)` pair sorted ascending by notional.
/// A position's effective MMR = `mmr_bps` of the largest tier whose
/// `min_notional` is ≤ the position's notional, OR `base_mmr_bps` if no
/// tier matches (notional below the first tier's threshold).
///
/// Example tier table (typical HL BTC market):
///   [(0,        100),    // tier 1: ≤  ~$1M  → 1.0% MMR
///    (1_000_000, 200),   // tier 2:  ~$5M    → 2.0% MMR
///    (5_000_000, 300),   // tier 3: ~$25M    → 3.0% MMR
///    (25_000_000, 500)]  // tier 4: > $25M   → 5.0% MMR
///
/// `tiers` MUST be sorted ascending by `min_notional`. The caller is
/// responsible for sort order; helpers in `lib.rs:init_market_leverage_tiers`
/// enforce this at write time.
///
/// Pure function — no Solana types beyond u64/u32. Unit-tested directly.
pub fn tiered_mmr_bps(
    base_mmr_bps: u32,
    tiers: &[(u64, u32)],
    position_notional_quote_lots: u128,
) -> u32 {
    let mut effective = base_mmr_bps;
    for (min_notional, tier_mmr) in tiers {
        if position_notional_quote_lots >= *min_notional as u128 {
            effective = *tier_mmr;
        } else {
            break;
        }
    }
    effective
}

#[cfg(test)]
mod tier_tests {
    use super::*;

    #[test]
    fn empty_tiers_returns_base() {
        assert_eq!(tiered_mmr_bps(100, &[], 0), 100);
        assert_eq!(tiered_mmr_bps(100, &[], 1_000_000_000), 100);
    }

    #[test]
    fn below_first_tier_returns_base() {
        let tiers = [(1_000_000u64, 200u32), (5_000_000, 300)];
        assert_eq!(tiered_mmr_bps(100, &tiers, 0), 100);
        assert_eq!(tiered_mmr_bps(100, &tiers, 999_999), 100);
    }

    #[test]
    fn at_or_above_tier_returns_tier_mmr() {
        let tiers = [(1_000_000u64, 200u32), (5_000_000, 300), (25_000_000, 500)];
        assert_eq!(tiered_mmr_bps(100, &tiers, 1_000_000), 200);
        assert_eq!(tiered_mmr_bps(100, &tiers, 4_999_999), 200);
        assert_eq!(tiered_mmr_bps(100, &tiers, 5_000_000), 300);
        assert_eq!(tiered_mmr_bps(100, &tiers, 24_999_999), 300);
        assert_eq!(tiered_mmr_bps(100, &tiers, 25_000_000), 500);
        assert_eq!(tiered_mmr_bps(100, &tiers, u128::MAX), 500);
    }

    #[test]
    fn monotone_in_notional() {
        let tiers = [(100u64, 150u32), (1_000, 250), (10_000, 400)];
        let mut prev = tiered_mmr_bps(100, &tiers, 0);
        for n in [99u128, 100, 999, 1_000, 9_999, 10_000, 1_000_000] {
            let now = tiered_mmr_bps(100, &tiers, n);
            assert!(now >= prev, "non-monotone at {}: prev={} now={}", n, prev, now);
            prev = now;
        }
    }

    #[test]
    fn hl_btc_table() {
        // HL's typical BTC tier table:
        //   <$1M  → base 0.5% MMR (retail tier — uses caller's base_mmr)
        //   $1M+  → 1.0% MMR
        //   $5M+  → 2.0%
        //   $25M+ → 3.0%
        //   $100M+→ 5.0% (whale tier)
        let tiers = [
            (1_000_000u64, 100u32),
            (5_000_000, 200),
            (25_000_000, 300),
            (100_000_000, 500),
        ];
        // Tiny retail position (< $1M) → base MMR (50 bps from caller).
        assert_eq!(tiered_mmr_bps(50, &tiers, 500_000), 50);
        // $3M position → first tier matches (≥$1M), second doesn't (<$5M).
        assert_eq!(tiered_mmr_bps(50, &tiers, 3_000_000), 100);
        // $7M position → second tier active.
        assert_eq!(tiered_mmr_bps(50, &tiers, 7_000_000), 200);
        // $30M position → third tier.
        assert_eq!(tiered_mmr_bps(50, &tiers, 30_000_000), 300);
        // $200M whale → top tier (5% MMR).
        assert_eq!(tiered_mmr_bps(50, &tiers, 200_000_000), 500);
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

/// WAVE 22 — pure tier resolution for the multi-tier fee table.
///
/// Picks the HIGHEST tier (by `min_volume`) that the trader's
/// rolling-window volume satisfies. Returns `(maker_rebate_bps,
/// taker_fee_bps)` for use by `apply_fill`.
///
/// `tiers` MUST be sorted ascending by `min_volume_quote_lots` AND
/// the first tier MUST have `min_volume == 0` (the default tier).
/// Both invariants are enforced at write time in
/// `lib.rs:init_fee_tiers / update_fee_tiers`. With an empty slice
/// (no FeeTiersAccount supplied), falls back to `(default_maker_bps,
/// default_taker_bps)`.
///
/// Same shape as `tiered_mmr_bps` — pure, no Solana types.
pub fn resolve_fee_tier(
    default_maker_rebate_bps: u32,
    default_taker_fee_bps: u32,
    tiers: &[(u64, u32, u32)],
    trader_volume_quote_lots: u64,
) -> (u32, u32) {
    let mut maker = default_maker_rebate_bps;
    let mut taker = default_taker_fee_bps;
    for (min_vol, m, t) in tiers {
        if trader_volume_quote_lots >= *min_vol {
            maker = *m;
            taker = *t;
        } else {
            break;
        }
    }
    (maker, taker)
}

#[cfg(test)]
mod fee_tier_tests {
    use super::*;

    #[test]
    fn empty_tiers_returns_defaults() {
        let (m, t) = resolve_fee_tier(2, 5, &[], 1_000_000_000);
        assert_eq!(m, 2);
        assert_eq!(t, 5);
    }

    #[test]
    fn picks_highest_satisfied_tier() {
        // HL-style schedule (monotone improving):
        //   tier 0 (vol 0):       maker 2 bps rebate, taker 5 bps fee
        //   tier 1 ($1M):         maker 3 bps rebate, taker 4 bps fee
        //   tier 2 ($5M):         maker 4 bps rebate, taker 3 bps fee
        //   tier 3 ($25M):        maker 6 bps rebate, taker 2 bps fee
        let tiers = [
            (0u64, 2u32, 5u32),
            (1_000_000, 3, 4),
            (5_000_000, 4, 3),
            (25_000_000, 6, 2),
        ];
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 0), (2, 5));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 999_999), (2, 5));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 1_000_000), (3, 4));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 4_999_999), (3, 4));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 5_000_000), (4, 3));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 25_000_000), (6, 2));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, u64::MAX), (6, 2));
    }

    #[test]
    fn boundary_inclusive() {
        // EXACTLY the threshold qualifies for the tier (`>=`, not `>`).
        let tiers = [(0u64, 5u32, 10u32), (1_000_000, 4, 8)];
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 1_000_000), (4, 8));
    }

    #[test]
    fn monotone_improvement_across_volume_sweep() {
        // Maker rebate must monotonically RISE as volume rises;
        // taker fee must monotonically FALL.
        let tiers = [
            (0u64, 1u32, 10u32),
            (10_000, 2, 9),
            (100_000, 3, 7),
            (1_000_000, 5, 5),
        ];
        let mut prev_maker = 0u32;
        let mut prev_taker = u32::MAX;
        for vol in [0u64, 9_999, 10_000, 99_999, 100_000, 999_999, 1_000_000, 1_000_001] {
            let (m, t) = resolve_fee_tier(0, 0, &tiers, vol);
            assert!(m >= prev_maker, "maker rebate must not decrease as volume rises");
            assert!(t <= prev_taker, "taker fee must not increase as volume rises");
            prev_maker = m;
            prev_taker = t;
        }
    }
}
