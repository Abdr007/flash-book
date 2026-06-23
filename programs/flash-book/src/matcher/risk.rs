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
    /// Phase 2 isolated-margin marker.
    /// `0`  — position is cross-margined and is evaluated against the
    ///        trader's pooled `collateral_quote_lots`.
    /// `>0` — position is isolated. `assess_margin_unified` filters it
    ///        into its own singleton bucket and evaluates it against
    ///        this collateral amount; the cross bucket sees only the
    ///        non-isolated positions.
    pub collateral_quote_lots: u64,
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
    // ─── Wave 28b — OI-scaled MMR inputs ─────────────────────────────
    /// Side OI in lots, for the side this *position* is on. Caller is
    /// responsible for passing `long_oi_lots` for long positions,
    /// `short_oi_lots` for shorts. Defaults to 0 (Wave 28 disabled).
    pub side_oi_lots: u64,
    /// Per-million-lots slope, in bps. `100` ⇒ +1 bp extra MMR per
    /// 10_000 lots of side OI. `0` disables OI scaling entirely.
    pub oi_mmr_slope_bps_per_million_lots: u32,
    /// Cap on the OI-scaled extra. Default 0 (no cap) = relies on the
    /// natural saturation of u32 bps. Production should set non-zero.
    pub oi_mmr_max_extra_bps: u32,
}

impl MarketSnapshot {
    /// Effective maintenance margin in bps for a position of size
    /// `size_lots` on this market. Stacks all three contributions:
    ///   1. base `maintenance_margin_bps`
    ///   2. CME-style concentration extra (size ≥ threshold)
    ///   3. Wave 28b OI-scaled crowded-trade extra (heavy-side OI)
    ///
    /// All terms are additive; total saturates on u32 overflow.
    ///
    /// ⚠️ RISK-H1 — term (3) is **INACTIVE in production**. Every on-chain
    /// `RiskMarketSnap` constructs this snapshot with `side_oi_lots = 0` and
    /// `oi_mmr_slope_bps_per_million_lots = 0`, and there is **no MarketParams
    /// field** to configure them — so `oi_extra` is always 0 and the
    /// crowded-trade penalty does nothing. This is documented (not silently
    /// dead) to avoid false assurance. To activate it: add
    /// `oi_mmr_slope_bps_per_million_lots` + `oi_mmr_max_extra_bps` to
    /// `MarketParams` (a state migration) and thread real per-side OI
    /// (`market.oi_long_lots` / `oi_short_lots`) into the snapshots at the
    /// call sites. Omitting the penalty is conservative-safe (never
    /// under-margins), so this is a missing feature, not a solvency bug.
    pub fn effective_mmr_bps(&self, size_lots: u64) -> u32 {
        let base_with_conc = if self.concentration_threshold_lots > 0
            && size_lots >= self.concentration_threshold_lots
        {
            self.maintenance_margin_bps
                .saturating_add(self.concentration_extra_mmr_bps)
        } else {
            self.maintenance_margin_bps
        };
        let oi_extra = oi_scaled_mmr_extra_bps(
            self.side_oi_lots,
            self.oi_mmr_slope_bps_per_million_lots,
            // 0 max means "no cap"; the helper treats that as
            // min(extra, 0) → 0, which is wrong. Convert to u32::MAX
            // so the cap is effectively absent.
            if self.oi_mmr_max_extra_bps == 0 {
                u32::MAX
            } else {
                self.oi_mmr_max_extra_bps
            },
        );
        base_with_conc.saturating_add(oi_extra)
    }
}

/// Wave 28a — GMX V2-style OI-scaled MMR.
///
/// Adds a *crowded-trade* penalty on top of the existing tier table:
/// when the heavy-side open interest grows, every position on the
/// imbalanced side pays incrementally more maintenance margin. This
/// is the cheap, orthogonal complement to flash-book's stress-lattice
/// scenario margin — the lattice models worst-case scenario losses;
/// OI scaling models concentration risk.
///
/// Formula:
/// ```text
/// oi_extra_bps = floor(side_oi_lots × oi_mmr_slope_bps_per_million_lots / 1_000_000)
/// effective_mmr = base_mmr + oi_extra_bps
/// ```
///
/// `oi_mmr_slope_bps_per_million_lots = 100` means "add 1 bp per
/// million lots of side OI". A side with 50M lots OI sees +50 bps
/// extra MMR on every position. Linear, monotone, cheap.
///
/// Capped by `oi_mmr_max_extra_bps` to bound the worst-case effect.
///
/// Pure function. Returns the **extra** bps to add on top of the
/// existing tier/concentration MMR. Caller stacks them.
pub fn oi_scaled_mmr_extra_bps(
    side_oi_lots: u64,
    slope_bps_per_million_lots: u32,
    max_extra_bps: u32,
) -> u32 {
    if slope_bps_per_million_lots == 0 {
        return 0;
    }
    // side_oi_lots × slope / 1_000_000.
    // side_oi_lots ≤ u64::MAX, slope ≤ u32::MAX → product ≤ u64::MAX × u32::MAX < u128::MAX.
    let scaled = (side_oi_lots as u128).saturating_mul(slope_bps_per_million_lots as u128);
    let extra = scaled / 1_000_000;
    (extra.min(max_extra_bps as u128) as u32).min(max_extra_bps)
}

/// Compose the full effective MMR for a position: stress-lattice tier
/// + concentration extra (existing) + OI-scaled extra (Wave 28a).
///
/// `tiers` is the Hyperliquid-style tier table; pass `&[]` to skip.
/// `oi_slope` + `oi_max` are the Wave 28a knobs; pass `(0, 0)` to skip.
pub fn effective_mmr_bps_full(
    base_mmr_bps: u32,
    tiers: &[(u64, u32)],
    position_notional_quote_lots: u128,
    side_oi_lots: u64,
    oi_slope_bps_per_million_lots: u32,
    oi_max_extra_bps: u32,
) -> u32 {
    let tier_mmr = tiered_mmr_bps(base_mmr_bps, tiers, position_notional_quote_lots);
    let oi_extra = oi_scaled_mmr_extra_bps(side_oi_lots, oi_slope_bps_per_million_lots, oi_max_extra_bps);
    tier_mmr.saturating_add(oi_extra)
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

    // ─── Wave 28a tests ─────────────────────────────────────────

    #[test]
    fn oi_scaled_zero_slope_returns_zero() {
        assert_eq!(oi_scaled_mmr_extra_bps(1_000_000, 0, 1_000), 0);
        assert_eq!(oi_scaled_mmr_extra_bps(u64::MAX, 0, 1_000), 0);
    }

    #[test]
    fn oi_scaled_linear_with_oi() {
        // slope=100 bps per million lots → 1 bp per 10_000 lots.
        // 1M lots × 100 / 1M = 100 → 100 bps.
        assert_eq!(oi_scaled_mmr_extra_bps(1_000_000, 100, 10_000), 100);
        // 500k lots × 100 / 1M = 50 → 50 bps.
        assert_eq!(oi_scaled_mmr_extra_bps(500_000, 100, 10_000), 50);
        // 10M lots × 100 / 1M = 1000 → 1000 bps.
        assert_eq!(oi_scaled_mmr_extra_bps(10_000_000, 100, 10_000), 1_000);
    }

    #[test]
    fn oi_scaled_capped_at_max() {
        // Slope would give 10_000 bps; cap is 500 → 500.
        assert_eq!(oi_scaled_mmr_extra_bps(100_000_000, 100, 500), 500);
    }

    #[test]
    fn oi_scaled_handles_extreme_inputs_without_overflow() {
        // u64::MAX × u32::MAX would overflow u64 but fits in u128.
        let _ = oi_scaled_mmr_extra_bps(u64::MAX, u32::MAX, 10_000);
        // Doesn't panic.
    }

    #[test]
    fn effective_mmr_full_stacks_tier_and_oi() {
        // base 100, tier @ 1M → 200, OI 500k slope 100 cap 1000 → +50.
        // Effective = 200 + 50 = 250.
        let tiers = [(1_000_000u64, 200u32)];
        let r = effective_mmr_bps_full(100, &tiers, 1_500_000, 500_000, 100, 1_000);
        assert_eq!(r, 250);
    }

    #[test]
    fn effective_mmr_full_no_oi_matches_pure_tiered() {
        let tiers = [(1_000_000u64, 200u32)];
        let with_oi = effective_mmr_bps_full(100, &tiers, 1_500_000, 0, 100, 1_000);
        let without = tiered_mmr_bps(100, &tiers, 1_500_000);
        assert_eq!(with_oi, without);
    }

    #[test]
    fn effective_mmr_full_no_tier_matches_pure_oi() {
        // No tiers → just base + OI extra.
        let extra = oi_scaled_mmr_extra_bps(1_000_000, 100, 1_000);
        let composed = effective_mmr_bps_full(100, &[], 0, 1_000_000, 100, 1_000);
        assert_eq!(composed, 100 + extra);
    }

    #[test]
    fn effective_mmr_full_monotone_in_oi() {
        let tiers = [(1_000u64, 200u32)];
        let mut prev = 0u32;
        for oi in [0u64, 100_000, 500_000, 1_000_000, 5_000_000] {
            let now = effective_mmr_bps_full(100, &tiers, 10_000, oi, 50, 2_000);
            assert!(now >= prev, "non-monotone at oi={}", oi);
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

/// Assess margin when some positions are isolated-margin.
///
/// ─── ISOLATED MARGIN MODEL ───────────────────────────────────────────
///
/// A position is "isolated" when its own `PositionAccount.collateral_quote_lots`
/// > 0. The caller passes a map of (market → isolated_collateral) for every
/// isolated position; positions whose market is NOT in the map are treated
/// as cross.
///
/// The trader is HEALTHY iff:
///   (a) The cross set, assessed against `cross_collateral_quote_lots`, is
///       healthy, AND
///   (b) EVERY isolated position, assessed as a singleton against its own
///       isolated_collateral, is healthy.
///
/// Failure of ANY isolated position is sufficient to mark the trader
/// unhealthy — but the failure is bounded: an isolated position's
/// liquidation can only touch its own collateral and the insurance fund,
/// never the trader's cross pool. The cross pool is insulated.
///
/// Returned `required_quote_lots` is the SUM of required margins across
/// all buckets (cross required + Σ isolated required) so off-chain UIs
/// can show total locked margin. `equity_quote_lots_signed` is the
/// trader's TOTAL equity across both pools so the UI can render
/// available headroom. `worst_scenario_idx` is the scenario index from
/// the most-loaded bucket (whichever ran the closest to liquidation).
pub fn assess_margin_split(
    positions: &[PositionSnapshot],
    markets: &[MarketSnapshot],
    scenarios: &[Scenario],
    cross_collateral_quote_lots: u64,
    isolated_collaterals_by_market: &[(Pubkey, u64)],
) -> Result<MarginAssessment> {
    let find_isolated = |market: &Pubkey| -> Option<u64> {
        isolated_collaterals_by_market
            .iter()
            .find(|(m, _)| m == market)
            .map(|(_, c)| *c)
    };

    let mut cross_positions: Vec<PositionSnapshot> = Vec::with_capacity(positions.len());
    let mut isolated_positions: Vec<(PositionSnapshot, u64)> = Vec::new();
    for pos in positions {
        match find_isolated(&pos.market) {
            Some(c) => isolated_positions.push((*pos, c)),
            None => cross_positions.push(*pos),
        }
    }

    // (a) Cross-bucket assessment.
    let cross = assess_margin(
        &cross_positions,
        markets,
        scenarios,
        cross_collateral_quote_lots,
    )?;

    // (b) Each isolated position assessed as a singleton against its own
    // collateral. Track the worst loadedness ratio so we can surface the
    // scenario index from the bucket that's closest to liquidation.
    let mut total_required: u64 = cross.required_quote_lots;
    let mut total_equity: i128 = cross.equity_quote_lots_signed;
    let mut all_healthy = cross.is_healthy;
    // Tightness metric: required − equity. Larger = closer to (or past) liquidation.
    let mut worst_idx = cross.worst_scenario_idx;
    let mut worst_tightness: i128 =
        (cross.required_quote_lots as i128).checked_sub(cross.equity_quote_lots_signed).unwrap_or(0);

    for (pos, iso_collateral) in &isolated_positions {
        let singleton = [*pos];
        let a = assess_margin(&singleton, markets, scenarios, *iso_collateral)?;
        total_required = total_required.saturating_add(a.required_quote_lots);
        total_equity = total_equity.saturating_add(a.equity_quote_lots_signed);
        if !a.is_healthy {
            all_healthy = false;
        }
        let tightness = (a.required_quote_lots as i128)
            .checked_sub(a.equity_quote_lots_signed)
            .unwrap_or(0);
        if tightness > worst_tightness {
            worst_tightness = tightness;
            worst_idx = a.worst_scenario_idx;
        }
    }

    Ok(MarginAssessment {
        required_quote_lots: total_required,
        equity_quote_lots_signed: total_equity,
        is_healthy: all_healthy,
        worst_scenario_idx: worst_idx,
    })
}

/// Phase 2 dispatch helper. Call this from any handler that previously
/// called `assess_margin` directly — it picks the right code path based
/// on whether any snapshot is isolated.
///
/// The "isolated" decision is read from each `PositionSnapshot`'s own
/// `collateral_quote_lots` field. Handlers that mutate per-position
/// collateral mid-instruction (e.g. `set_position_isolated`) populate
/// snapshots with the POST-transition value before calling.
///
/// When NO snapshot has `collateral_quote_lots > 0` this delegates to
/// `assess_margin`, byte-identical to the pre-Phase-2 path. When ANY
/// snapshot is isolated, all isolated snapshots are filtered into their
/// own singleton buckets and the remainder is evaluated as the cross
/// set against `cross_collateral_quote_lots`.
pub fn assess_margin_unified(
    positions: &[PositionSnapshot],
    markets: &[MarketSnapshot],
    scenarios: &[Scenario],
    cross_collateral_quote_lots: u64,
) -> Result<MarginAssessment> {
    let has_isolated = positions.iter().any(|p| p.collateral_quote_lots > 0);
    if !has_isolated {
        return assess_margin(positions, markets, scenarios, cross_collateral_quote_lots);
    }
    let isolated: Vec<(Pubkey, u64)> = positions
        .iter()
        .filter(|p| p.collateral_quote_lots > 0)
        .map(|p| (p.market, p.collateral_quote_lots))
        .collect();
    assess_margin_split(
        positions,
        markets,
        scenarios,
        cross_collateral_quote_lots,
        &isolated,
    )
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
/// `maker_rebate_bps` is SIGNED (i32) — positive = rebate paid to
/// maker, negative = fee charged to maker (wave 22 retail tier 0).
///
/// Same shape as `tiered_mmr_bps` — pure, no Solana types.
pub fn resolve_fee_tier(
    default_maker_rebate_bps: i32,
    default_taker_fee_bps: u32,
    tiers: &[(u64, i32, u32)],
    trader_volume_quote_lots: u64,
) -> (i32, u32) {
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
            (0u64, 2i32, 5u32),
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
        let tiers = [(0u64, 5i32, 10u32), (1_000_000, 4, 8)];
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 1_000_000), (4, 8));
    }

    #[test]
    fn monotone_improvement_across_volume_sweep() {
        // Maker rebate must monotonically RISE as volume rises;
        // taker fee must monotonically FALL.
        let tiers = [
            (0u64, 1i32, 10u32),
            (10_000, 2, 9),
            (100_000, 3, 7),
            (1_000_000, 5, 5),
        ];
        let mut prev_maker = i32::MIN;
        let mut prev_taker = u32::MAX;
        for vol in [0u64, 9_999, 10_000, 99_999, 100_000, 999_999, 1_000_000, 1_000_001] {
            let (m, t) = resolve_fee_tier(0, 0, &tiers, vol);
            assert!(m >= prev_maker, "maker rebate must not decrease as volume rises");
            assert!(t <= prev_taker, "taker fee must not increase as volume rises");
            prev_maker = m;
            prev_taker = t;
        }
    }

    #[test]
    fn negative_maker_rebate_for_retail_tier() {
        // Wave 22: tier 0 retail PAYS a maker fee (negative rebate);
        // higher-volume tiers cross zero into rebate.
        //   tier 0 (vol 0):      maker -10 (10 bps fee), taker 10
        //   tier 1 ($1M):        maker  -5 (5 bps fee),  taker 8
        //   tier 2 ($10M):       maker   0 (free),       taker 5
        //   tier 3 ($100M):      maker  +3 (rebate),     taker 3
        let tiers = [
            (0u64, -10i32, 10u32),
            (1_000_000, -5, 8),
            (10_000_000, 0, 5),
            (100_000_000, 3, 3),
        ];
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 0), (-10, 10));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 999_999), (-10, 10));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 1_000_000), (-5, 8));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 10_000_000), (0, 5));
        assert_eq!(resolve_fee_tier(0, 0, &tiers, 100_000_000), (3, 3));
    }

    #[test]
    fn signed_monotone_across_zero_crossing() {
        // Same schedule as above; verify monotone invariant holds.
        let tiers = [
            (0u64, -10i32, 10u32),
            (1_000_000, -5, 8),
            (10_000_000, 0, 5),
            (100_000_000, 3, 3),
        ];
        let mut prev_maker = i32::MIN;
        let mut prev_taker = u32::MAX;
        for vol in [0u64, 1_000_000, 10_000_000, 100_000_000, u64::MAX] {
            let (m, t) = resolve_fee_tier(0, 0, &tiers, vol);
            assert!(m >= prev_maker, "maker rate must not regress (signed)");
            assert!(t <= prev_taker, "taker fee must not increase");
            prev_maker = m;
            prev_taker = t;
        }
    }
}

#[cfg(test)]
mod isolated_margin_tests {
    use super::*;

    fn mkt(seed: u8, mark: u64) -> (Pubkey, MarketSnapshot) {
        let pk = Pubkey::new_from_array([seed; 32]);
        let m = MarketSnapshot {
            market: pk,
            mark_price: Ticks(mark),
            cum_funding_index: 0,
            maintenance_margin_bps: 500, // 5%
            tick_size: 1,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        };
        (pk, m)
    }

    fn long_at(market: Pubkey, size: u64, entry: u64, iso: u64) -> PositionSnapshot {
        PositionSnapshot {
            market,
            side: Side::Long,
            size_lots: size,
            entry_price: Ticks(entry),
            cum_funding_index_at_entry: 0,
            collateral_quote_lots: iso,
        }
    }

    #[test]
    fn unified_no_isolated_matches_assess_margin() {
        let (mkt_a, ms_a) = mkt(1, 100);
        let positions = vec![long_at(mkt_a, 10, 95, 0)];
        let scenarios = default_scenarios(&[mkt_a]);
        let unified = assess_margin_unified(&positions, &[ms_a], &scenarios, 5_000).unwrap();
        let flat = assess_margin(&positions, &[ms_a], &scenarios, 5_000).unwrap();
        assert_eq!(unified.is_healthy, flat.is_healthy);
        assert_eq!(unified.required_quote_lots, flat.required_quote_lots);
        assert_eq!(unified.equity_quote_lots_signed, flat.equity_quote_lots_signed);
    }

    #[test]
    fn isolated_position_healthy_with_own_collateral() {
        let (mkt_a, ms_a) = mkt(1, 100);
        // Long 10 @ entry 95, mark 100 → unrealized profit. Iso collateral 5_000.
        let positions = vec![long_at(mkt_a, 10, 95, 5_000)];
        let scenarios = default_scenarios(&[mkt_a]);
        // Cross pool empty — only the isolated bucket matters here.
        let a = assess_margin_unified(&positions, &[ms_a], &scenarios, 0).unwrap();
        assert!(a.is_healthy, "isolated position must use its own collateral");
    }

    #[test]
    fn isolated_unhealthy_when_underfunded_even_if_cross_pool_huge() {
        let (mkt_a, ms_a) = mkt(1, 100);
        // Long with 1 lot of isolated collateral against a 1000-lot position
        // is grossly underfunded. The cross pool of 1B quote lots MUST NOT
        // rescue an isolated position — that's the whole point of isolation.
        let positions = vec![long_at(mkt_a, 1_000, 95, 1)];
        let scenarios = default_scenarios(&[mkt_a]);
        let a = assess_margin_unified(&positions, &[ms_a], &scenarios, 1_000_000_000).unwrap();
        assert!(
            !a.is_healthy,
            "fat cross pool must not insulate an under-collateralised isolated position"
        );
    }

    #[test]
    fn cross_set_protected_when_isolated_fails() {
        // Mixed portfolio: cross set is robust, isolated position is bust.
        // The trader is overall unhealthy (any isolated failure trips the
        // unified flag) but the equity and required totals tell the UI
        // where the deficit is.
        let (mkt_a, ms_a) = mkt(1, 100);
        let (mkt_b, ms_b) = mkt(2, 200);
        let positions = vec![
            long_at(mkt_a, 10, 95, 0),  // cross — well-collateralised by 10_000 pool
            long_at(mkt_b, 1_000, 195, 1), // isolated — under-collateralised
        ];
        let scenarios = default_scenarios(&[mkt_a, mkt_b]);
        let a = assess_margin_unified(&positions, &[ms_a, ms_b], &scenarios, 10_000).unwrap();
        assert!(!a.is_healthy);

        // Sanity: cross-only assessment over the cross subset alone IS healthy.
        let cross_only = vec![long_at(mkt_a, 10, 95, 0)];
        let cross_check =
            assess_margin(&cross_only, &[ms_a, ms_b], &scenarios, 10_000).unwrap();
        assert!(
            cross_check.is_healthy,
            "cross subset must be self-sufficient — isolated failure does NOT bleed back"
        );
    }

    #[test]
    fn split_external_map_overrides_snapshot_field() {
        // set_position_isolated passes a POST-transition isolated map even
        // though the snapshot's own field still reads 0 (the on-chain
        // mutation happens after the health check). The split path must
        // honour the external map, not the field. This locks in that
        // contract.
        let (mkt_a, ms_a) = mkt(1, 100);
        let positions = vec![long_at(mkt_a, 10, 95, 0)]; // field says cross
        let scenarios = default_scenarios(&[mkt_a]);
        let iso_map = [(mkt_a, 5_000)]; // explicit isolated
        let a = assess_margin_split(&positions, &[ms_a], &scenarios, 0, &iso_map).unwrap();
        // Cross pool empty, but isolated map covers it → healthy.
        assert!(a.is_healthy);
    }
}
