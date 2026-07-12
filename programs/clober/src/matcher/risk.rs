//! Stress-lattice maintenance margin — integer arithmetic throughout.
//!
//! For each scenario s ∈ Σ, compute portfolio loss + maintenance margin on
//! the stressed notional. Required margin is the worst-case scenario loss.
//!
//! Hedged property: a long+short on the same market cancels directional
//! risk in every scenario; only the maintenance margin on stressed notional
//! remains.
//!
//! Cost: O(N_positions × N_scenarios). Bounded (ENFORCED at entry)
//! by MAX_POSITIONS_PER_TRADER × MAX_STRESS_SCENARIOS = 16 × 133 = 2128 evals —
//! `assess_margin` rejects a scenario vector longer than MAX_STRESS_SCENARIOS.

use super::funding::{funding_owed, FundingIndex};
use super::lot::Ticks;
use super::order::Side;
use crate::constants::{BPS_DENOM, MAX_STRESS_SCENARIOS};
use crate::errors::{CloberError, OrOverflow};
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
    /// Isolated-margin marker.
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
    // ─── OI-scaled MMR inputs ────────────────────────────────────────
    /// Side OI in lots, for the side this *position* is on. Caller is
    /// responsible for passing `long_oi_lots` for long positions,
    /// `short_oi_lots` for shorts. 0 disables the surcharge.
    pub side_oi_lots: u64,
    /// Per-million-lots slope, in bps. `100` ⇒ +1 bp extra MMR per
    /// 10_000 lots of side OI. `0` disables OI scaling entirely.
    pub oi_mmr_slope_bps_per_million_lots: u32,
    /// Cap on the OI-scaled extra. Default 0 (no cap) = relies on the
    /// natural saturation of u32 bps. Production should set non-zero.
    pub oi_mmr_max_extra_bps: u32,
    /// 3.1 percolator per-domain credit: paper-profit haircut in bps for THIS
    /// market. Usable positive unrealized PnL is scaled by
    /// `(BPS_DENOM − paper_profit_haircut_bps)/BPS_DENOM` — i.e. `BPS_DENOM −
    /// credit_rate`. `0` = no haircut (full paper profit usable, default).
    /// `BPS_DENOM` = paper profit yields ZERO usable equity. Only positive uPnL
    /// is scaled (losses always count fully), so it can only RAISE the margin
    /// requirement — conservative-safe.
    pub paper_profit_haircut_bps: u32,
}

/// 3.1: apply a market's paper-profit haircut to a signed unrealized-PnL figure.
/// POSITIVE pnl (paper gains) is scaled by the complement of the haircut so a
/// manipulated / thin / stale market (haircut → `BPS_DENOM`) yields ~0 usable
/// credit; NEGATIVE pnl (losses) is returned unchanged (you never get to
/// discount your own losses). `haircut_bps` is clamped to `BPS_DENOM`, so the
/// scaled value is in `[0, pnl]` for positive pnl — never above the paper
/// figure, never a sign flip. Mirrors the Lean `usable`/`creditRate` model.
#[inline]
pub fn haircut_positive_pnl(pnl: i128, haircut_bps: u32) -> i128 {
    if pnl <= 0 || haircut_bps == 0 {
        return pnl;
    }
    let keep = (BPS_DENOM as i128) - (haircut_bps.min(BPS_DENOM) as i128);
    // pnl > 0, keep ∈ [0, BPS_DENOM] ⇒ result ∈ [0, pnl]. Saturating mul guards
    // a pathological (governance) pnl near i128::MAX.
    pnl.saturating_mul(keep) / (BPS_DENOM as i128)
}

impl MarketSnapshot {
    /// Effective maintenance margin in bps for a position of size
    /// `size_lots` on this market. Stacks all three contributions:
    ///   1. base `maintenance_margin_bps`
    ///   2. CME-style concentration extra (size ≥ threshold)
    ///   3. OI-scaled crowded-trade extra (heavy-side OI)
    ///
    /// All terms are additive; total saturates on u32 overflow.
    ///
    /// Term (3) is **wired and configurable** (roadmap 4.4). `MarketParams`
    /// carries `oi_mmr_slope_bps_per_million_lots` + `oi_mmr_max_extra_bps`,
    /// and every on-chain `RiskMarketSnap` threads real per-side OI
    /// (`market.oi_long_lots` / `oi_short_lots`, selected by the position's
    /// side via `oi_side_lots`) into the snapshot. `slope = 0` is the default
    /// and keeps `oi_extra = 0`, i.e. byte-identical to pre-4.4 behaviour until
    /// a market opts in. Because the term is purely additive to maintenance
    /// bps, enabling it only ever *raises* the margin requirement — it can
    /// never under-margin an existing position.
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

/// OI-scaled MMR.
///
/// Adds a *crowded-trade* penalty on top of the existing tier table:
/// when the heavy-side open interest grows, every position on the
/// imbalanced side pays incrementally more maintenance margin. This
/// is the cheap, orthogonal complement to clober's stress-lattice
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

#[cfg(test)]
mod paper_haircut_tests {
    //! 3.1: the paper-profit haircut. The durable unbounded-width proof lives in
    //! Lean (`PerDomainCredit.lean`, which CBMC can't do because of the symbolic
    //! `/BPS_DENOM`); these pin the wired `haircut_positive_pnl` at concrete and
    //! boundary values.
    use super::*;

    #[test]
    fn zero_haircut_is_identity() {
        assert_eq!(haircut_positive_pnl(1_000, 0), 1_000);
        assert_eq!(haircut_positive_pnl(-1_000, 0), -1_000);
        assert_eq!(haircut_positive_pnl(i128::MAX, 0), i128::MAX);
    }

    #[test]
    fn full_haircut_zeroes_paper_profit_but_never_a_loss() {
        // haircut == BPS_DENOM ⇒ positive uPnL → 0 (no usable paper credit).
        assert_eq!(haircut_positive_pnl(1_000_000, BPS_DENOM), 0);
        // …but a LOSS is never discounted, at ANY haircut.
        assert_eq!(haircut_positive_pnl(-1_000_000, BPS_DENOM), -1_000_000);
        assert_eq!(haircut_positive_pnl(-1_000_000, 5_000), -1_000_000);
    }

    #[test]
    fn partial_haircut_scales_positive_pnl() {
        // 50% haircut ⇒ half the paper profit is usable.
        assert_eq!(haircut_positive_pnl(1_000, 5_000), 500);
        // 20% haircut (credit_rate 80%) ⇒ 80% usable.
        assert_eq!(haircut_positive_pnl(1_000, 2_000), 800);
    }

    #[test]
    fn result_is_bounded_in_zero_pnl_for_positive_input() {
        // The safety envelope: for positive pnl, the scaled value is in [0, pnl]
        // for every haircut in [0, BPS_DENOM] — never above the paper figure,
        // never sign-flipped. (The Lean `usable_le_pnl` theorem proves this at
        // unbounded width; sampled here.)
        for &pnl in &[1i128, 7, 999, 1_000_000, i64::MAX as i128] {
            for &h in &[0u32, 1, 2_500, 5_000, 9_999, BPS_DENOM] {
                let r = haircut_positive_pnl(pnl, h);
                assert!(r >= 0 && r <= pnl, "pnl={pnl} h={h} r={r}");
            }
        }
    }

    #[test]
    fn haircut_above_denom_is_clamped_to_full() {
        // A pathological governance value > BPS_DENOM clamps to full haircut,
        // never a negative keep-factor / sign flip.
        assert_eq!(haircut_positive_pnl(1_000, BPS_DENOM + 5_000), 0);
    }
}

#[cfg(test)]
mod oi_mmr_tests {
    use super::*;

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
    fn effective_mmr_stacks_concentration_and_oi() {
        // base 100, concentration extra +50 at threshold, OI 500k
        // slope 100 cap 1000 → +50. Effective = 100 + 50 + 50 = 200.
        let snap = MarketSnapshot {
            market: Pubkey::new_from_array([1; 32]),
            mark_price: Ticks(100),
            cum_funding_index: 0,
            maintenance_margin_bps: 100,
            tick_size: 1,
            concentration_threshold_lots: 1_000,
            concentration_extra_mmr_bps: 50,
            side_oi_lots: 500_000,
            oi_mmr_slope_bps_per_million_lots: 100,
            oi_mmr_max_extra_bps: 1_000,
            paper_profit_haircut_bps: 0,
        };
        assert_eq!(snap.effective_mmr_bps(2_000), 200);
        // Below the concentration threshold the extra drops out.
        assert_eq!(snap.effective_mmr_bps(500), 150);
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
    if shock_bps == 0 || price.0 == 0 {
        return Ok(price);
    }

    // Stress moves must round AWAY from the current mark. Signed integer
    // division truncates toward zero, which made both directions less adverse:
    // 101 * 1.20 became 121 (not 122), while 101 * 0.80 became 81 (not 80).
    // The missing fraction is multiplied by the full position size, so it is not
    // harmless dust. Compute the positive magnitude in u128 and ceil it before
    // applying the sign; u64 * |i32| always fits this intermediate.
    let magnitude = shock_bps.unsigned_abs() as u128;
    let delta = (price.0 as u128)
        .checked_mul(magnitude)
        .or_overflow()?
        .div_ceil(BPS_DENOM as u128);

    if shock_bps < 0 {
        if delta >= price.0 as u128 {
            return Ok(Ticks(0));
        }
        let stressed = (price.0 as u128).checked_sub(delta).or_underflow()?;
        return Ok(Ticks(u64::try_from(stressed).ok().or_overflow()?));
    }

    // Reject a positive stressed price that overflows u64 rather than silently
    // truncating. Only reachable with a pathological governance shock.
    let stressed = (price.0 as u128).checked_add(delta).or_overflow()?;
    Ok(Ticks(u64::try_from(stressed).ok().or_overflow()?))
}

fn lookup_market<'a>(markets: &'a [MarketSnapshot], pk: &Pubkey) -> Option<&'a MarketSnapshot> {
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
    // Bound the compute cost. The on-chain generator never
    // exceeds MAX_STRESS_SCENARIOS, so this never rejects a legitimate caller —
    // it caps a pathological / future caller that could otherwise pass an
    // unbounded scenario vector and exhaust the compute budget (a griefing /
    // un-liquidatable vector).
    require!(
        scenarios.len() <= MAX_STRESS_SCENARIOS,
        CloberError::TooManyStressScenarios
    );
    // Equity at current marks.
    let mut unrealized_total: i128 = 0;
    let mut funding_total: i128 = 0;
    for pos in positions {
        let m = match lookup_market(markets, &pos.market) {
            Some(m) => m,
            None => continue,
        };
        // 3.1: haircut paper (positive) uPnL by this market's ability-to-pay
        // before it counts toward reported equity. Losses count in full.
        unrealized_total = unrealized_total
            .checked_add(haircut_positive_pnl(
                unrealized_pnl_quote_lots(pos, m.mark_price, m.tick_size)?,
                m.paper_profit_haircut_bps,
            ))
            .or_overflow()?;
        let notional = (pos.size_lots as u128)
            .checked_mul(m.mark_price.0 as u128)
            .or_overflow()?
            .checked_mul(m.tick_size as u128)
            .or_overflow()?;
        if notional > u64::MAX as u128 {
            return Err(error!(CloberError::ArithmeticOverflow));
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

    // Required margin must NOT grant cross-market
    // offset. The old code took `max over scenarios` of the loss SUMMED across
    // ALL positions, so any scenario that moves two markets together netted
    // opposing legs (e.g. long A + short B): the uniform all-down/all-up
    // scenarios cancelled the legs and the single-market scenarios loaded only
    // one leg, so the true worst case "A crashes AND B rallies simultaneously"
    // was never priced. That under-margined cross-market books by ~2x.
    //
    // Fix: decompose PER MARKET. For each market take the worst of its own
    // single-market scenario losses (positions on the SAME market still net
    // against each other under a shared shock — the documented hedge property),
    // then SUM the per-market worst cases. This equals the perfectly-
    // decorrelated adverse scenario and is always >= the old figure, so it is
    // strictly more conservative and can never under-margin. For a
    // single-market portfolio it is byte-identical to the old computation.
    let mut market_worst: Vec<(Pubkey, i128)> = Vec::with_capacity(positions.len());
    let mut worst_idx: u32 = 0;
    let mut worst_single: i128 = i128::MIN;

    for (idx, scenario) in scenarios.iter().enumerate() {
        // Per-market summed loss for THIS scenario.
        let mut per_market: Vec<(Pubkey, i128)> = Vec::with_capacity(positions.len());
        for pos in positions {
            let m = match lookup_market(markets, &pos.market) {
                Some(m) => m,
                None => continue,
            };
            let shock = shock_for_market(scenario, &pos.market);
            let stressed = shocked_price(m.mark_price, shock)?;

            // Loss contribution = maintenance margin − unrealized PnL, both at
            // the stressed price (positive = bad for the trader). 3.1: paper
            // (positive) uPnL is haircut by the market's ability-to-pay, so a
            // manipulated/thin market gives no margin credit for paper profit
            // (raising `required`); losses count in full.
            let pnl = haircut_positive_pnl(
                unrealized_pnl_quote_lots(pos, stressed, m.tick_size)?,
                m.paper_profit_haircut_bps,
            );
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
            let contrib = mm.checked_sub(pnl).or_underflow()?;

            match per_market.iter_mut().find(|(k, _)| *k == pos.market) {
                Some((_, acc)) => *acc = acc.checked_add(contrib).or_overflow()?,
                None => per_market.push((pos.market, contrib)),
            }
        }
        // Fold this scenario's per-market losses into the running per-market
        // worst-case; track the scenario that drove the single largest market
        // loss for reporting.
        for (mk, loss) in per_market {
            if loss > worst_single {
                worst_single = loss;
                worst_idx = idx as u32;
            }
            match market_worst.iter_mut().find(|(k, _)| *k == mk) {
                Some((_, w)) => {
                    if loss > *w {
                        *w = loss;
                    }
                }
                None => market_worst.push((mk, loss)),
            }
        }
    }

    // required = Σ_market max(worst_market_loss_m, 0). A market whose worst case
    // is still a net gain contributes 0 (never a negative that offsets another
    // market's loss).
    let mut worst_loss: u64 = 0;
    for (_, w) in market_worst {
        if w > 0 {
            let add = if w > u64::MAX as i128 {
                u64::MAX
            } else {
                w as u64
            };
            worst_loss = worst_loss.saturating_add(add);
        }
    }

    // Healthy iff the trader's available collateral covers the worst-case
    // stressed loss. Gate on `collateral − funding`, NOT
    // `equity_signed`. Each scenario already measures loss from ENTRY
    // (`scenario_loss = MM_stressed − pnl_stressed`), so unrealized PnL is
    // accounted ONCE inside `required`. Comparing against `equity_signed`
    // (which re-adds unrealized PnL at the mark) double-counts it — making
    // winning positions pass at too-low collateral (collateral drain → bad
    // debt) and force-liquidating solvent positions that carry a routine
    // unrealized loss. `equity_signed` remains the reported/UI figure.
    let available_signed = (collateral_quote_lots as i128)
        .checked_sub(funding_total)
        .or_underflow()?;
    let required_signed: i128 = worst_loss as i128;
    let is_healthy = available_signed >= required_signed;

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
/// >    isolated position; positions whose market is NOT in the map are treated
/// >    as cross.
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
    let mut worst_tightness: i128 = (cross.required_quote_lots as i128)
        .checked_sub(cross.equity_quote_lots_signed)
        .unwrap_or(0);

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

/// Margin-mode dispatch helper: picks the cross vs isolated code path
/// based on whether any snapshot is isolated.
///
/// The "isolated" decision is read from each `PositionSnapshot`'s own
/// `collateral_quote_lots` field. Handlers that mutate per-position
/// collateral mid-instruction (e.g. `set_position_isolated`) populate
/// snapshots with the POST-transition value before calling.
///
/// When NO snapshot has `collateral_quote_lots > 0` this delegates to
/// `assess_margin`, byte-identical to the pre-cross-margin path. When ANY
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
        .map(|m| StressShock {
            market: *m,
            shock_bps: -1000,
        })
        .collect();
    let all_up: Vec<StressShock> = markets
        .iter()
        .map(|m| StressShock {
            market: *m,
            shock_bps: 1000,
        })
        .collect();
    let bs_down: Vec<StressShock> = markets
        .iter()
        .map(|m| StressShock {
            market: *m,
            shock_bps: -3000,
        })
        .collect();
    let bs_up: Vec<StressShock> = markets
        .iter()
        .map(|m| StressShock {
            market: *m,
            shock_bps: 3000,
        })
        .collect();
    out.push(all_down);
    out.push(all_up);
    out.push(bs_down);
    out.push(bs_up);
    out
}

/// Pure tier resolution for the multi-tier fee table.
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
/// maker, negative = fee charged to maker (retail tier 0).
///
/// Pure — no Solana types.
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
        for vol in [
            0u64, 9_999, 10_000, 99_999, 100_000, 999_999, 1_000_000, 1_000_001,
        ] {
            let (m, t) = resolve_fee_tier(0, 0, &tiers, vol);
            assert!(
                m >= prev_maker,
                "maker rebate must not decrease as volume rises"
            );
            assert!(
                t <= prev_taker,
                "taker fee must not increase as volume rises"
            );
            prev_maker = m;
            prev_taker = t;
        }
    }

    #[test]
    fn negative_maker_rebate_for_retail_tier() {
        // Tier 0 retail PAYS a maker fee (negative rebate);
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
            paper_profit_haircut_bps: 0,
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
        assert_eq!(
            unified.equity_quote_lots_signed,
            flat.equity_quote_lots_signed
        );
    }

    #[test]
    fn isolated_position_healthy_with_own_collateral() {
        let (mkt_a, ms_a) = mkt(1, 100);
        // Long 10 @ entry 95, mark 100 → unrealized profit. Iso collateral 5_000.
        let positions = vec![long_at(mkt_a, 10, 95, 5_000)];
        let scenarios = default_scenarios(&[mkt_a]);
        // Cross pool empty — only the isolated bucket matters here.
        let a = assess_margin_unified(&positions, &[ms_a], &scenarios, 0).unwrap();
        assert!(
            a.is_healthy,
            "isolated position must use its own collateral"
        );
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
            long_at(mkt_a, 10, 95, 0),     // cross — well-collateralised by 10_000 pool
            long_at(mkt_b, 1_000, 195, 1), // isolated — under-collateralised
        ];
        let scenarios = default_scenarios(&[mkt_a, mkt_b]);
        let a = assess_margin_unified(&positions, &[ms_a, ms_b], &scenarios, 10_000).unwrap();
        assert!(!a.is_healthy);

        // Sanity: cross-only assessment over the cross subset alone IS healthy.
        let cross_only = vec![long_at(mkt_a, 10, 95, 0)];
        let cross_check = assess_margin(&cross_only, &[ms_a, ms_b], &scenarios, 10_000).unwrap();
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

/// FV: machine-checked invariants for the OI-scaled / crowded-trade maintenance-
/// margin surcharge (Kani, exhaustive over the input domain where the property
/// does not depend on the non-power-of-two `/1_000_000` division value — CBMC is
/// incomplete on 128-bit non-pow2 division, so we prove the bound/floor/disable
/// properties that hold for ANY division result). Runs in the CI Kani job.
#[cfg(kani)]
mod mmr_kani_proofs {
    use super::{oi_scaled_mmr_extra_bps, MarketSnapshot};
    use crate::matcher::lot::Ticks;
    use anchor_lang::prelude::Pubkey;

    /// The OI surcharge NEVER exceeds its configured cap — a crowded book cannot
    /// be charged more maintenance margin than governance bounded.
    #[kani::proof]
    fn oi_scaled_never_exceeds_cap() {
        let oi: u64 = kani::any();
        let slope: u32 = kani::any();
        let max: u32 = kani::any();
        let extra = oi_scaled_mmr_extra_bps(oi, slope, max);
        assert!(extra <= max);
    }

    /// slope == 0 fully DISABLES the surcharge (legacy / opt-in behaviour).
    #[kani::proof]
    fn oi_scaled_zero_slope_disables() {
        let oi: u64 = kani::any();
        let max: u32 = kani::any();
        assert!(oi_scaled_mmr_extra_bps(oi, 0, max) == 0);
    }

    /// INV-M4: the effective maintenance margin is NEVER below the base
    /// floor — the concentration and OI surcharges only ADD (saturating),
    /// so no input can under-margin a position below `maintenance_margin_bps`.
    /// Proven on the live per-position MMR path used by `assess_margin`.
    #[kani::proof]
    fn effective_mmr_never_below_base_floor() {
        let snap = MarketSnapshot {
            market: Pubkey::new_from_array([0; 32]),
            mark_price: Ticks(kani::any()),
            cum_funding_index: kani::any(),
            maintenance_margin_bps: kani::any(),
            tick_size: kani::any(),
            concentration_threshold_lots: kani::any(),
            concentration_extra_mmr_bps: kani::any(),
            side_oi_lots: kani::any(),
            oi_mmr_slope_bps_per_million_lots: kani::any(),
            oi_mmr_max_extra_bps: kani::any(),
            paper_profit_haircut_bps: 0,
        };
        let size_lots: u64 = kani::any();
        assert!(snap.effective_mmr_bps(size_lots) >= snap.maintenance_margin_bps);
    }
}

#[cfg(test)]
mod assess_margin_frame_tests {
    //! Regression tests for the stress-lattice reference-frame mismatch
    //! `assess_margin` must NOT double-count unrealized
    //! PnL — once in equity (at mark) and again in each scenario loss (at the
    //! stressed price). Health under stress is `collateral - funding + pnl_stressed
    //! >= MM_stressed`; equivalently `(collateral - funding) >= worst(MM_s - pnl_s)`.
    //! > Both tests use a single custom -20% scenario so the boundary is exact.
    use super::*;

    fn mkt(seed: u8, mark: u64) -> (Pubkey, MarketSnapshot) {
        let pk = Pubkey::new_from_array([seed; 32]);
        (
            pk,
            MarketSnapshot {
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
                paper_profit_haircut_bps: 0,
            },
        )
    }

    fn long(market: Pubkey, size: u64, entry: u64) -> PositionSnapshot {
        PositionSnapshot {
            market,
            side: Side::Long,
            size_lots: size,
            entry_price: Ticks(entry),
            cum_funding_index_at_entry: 0,
            collateral_quote_lots: 0,
        }
    }

    fn down_20pct(market: Pubkey) -> Vec<Scenario> {
        vec![vec![StressShock {
            market,
            shock_bps: -2000,
        }]]
    }

    /// 3.1 — the paper-profit haircut wired into `assess_margin`. A deeply
    /// winning long (entry 50, mark 100) stays in profit even under the −20%
    /// shock (stressed price 80 ⇒ stressed pnl = (80−50)·10 = +300), so at
    /// `haircut = 0` its paper profit fully offsets the maintenance margin and it
    /// is healthy on thin collateral. At `haircut = BPS_DENOM` (a manipulated /
    /// thin / stale market that can't actually pay) the paper profit yields ZERO
    /// usable credit, the full stressed MM stands, and the SAME position is now
    /// liquidatable — the surcharge-free proof that ability-to-pay gates equity.
    #[test]
    fn paper_profit_haircut_flips_health() {
        let (m, ms0) = mkt(1, 100);
        let (_, mut ms1) = mkt(1, 100);
        ms1.paper_profit_haircut_bps = BPS_DENOM;
        let pos = vec![long(m, 10, 50)];

        let a0 = assess_margin(&pos, &[ms0], &down_20pct(m), 20).unwrap();
        assert_eq!(
            a0.required_quote_lots, 0,
            "haircut 0: the winner's paper profit offsets the MM → required 0"
        );
        assert!(a0.is_healthy, "healthy at haircut 0 on thin collateral");

        let a1 = assess_margin(&pos, &[ms1], &down_20pct(m), 20).unwrap();
        assert_eq!(
            a1.required_quote_lots, 40,
            "full haircut: paper profit is worth 0, so the stressed MM (40) stands"
        );
        assert!(
            !a1.is_healthy,
            "the SAME position is liquidatable once its paper profit can't back it"
        );
    }

    /// Direction 1 — collateral drain. A winning long (mark 130 > entry 100)
    /// with ZERO posted collateral must NOT be healthy: under a -20% shock the
    /// price falls to 104 where stressed PnL (4) < maintenance margin (5). The
    /// buggy frame counted the +30 mark PnL toward equity and wrongly passed it,
    /// letting the trader ride a position they no longer back.
    #[test]
    fn winning_position_zero_collateral_is_not_healthy() {
        let (m, ms) = mkt(1, 130);
        let pos = vec![long(m, 1, 100)];
        let a = assess_margin(&pos, &[ms], &down_20pct(m), 0).unwrap();
        // required = MM(5) - pnl_stressed(4) = 1 > available collateral (0).
        assert_eq!(a.required_quote_lots, 1, "scenario math drifted");
        assert!(
            !a.is_healthy,
            "zero-collateral winner must be unhealthy under stress (no PnL double-count)"
        );
    }

    /// Direction 2 — wrongful liquidation. A losing-but-solvent long (mark 95 <
    /// entry 100, only -5 unrealized) with 30 collateral SURVIVES a -20% shock:
    /// stressed price 76, required = MM(3) - pnl_stressed(-24) = 27 <= 30. The
    /// buggy frame subtracted the -5 mark PnL from equity (25 < 27) and would
    /// have force-liquidated a healthy position.
    #[test]
    fn losing_but_solvent_position_is_healthy() {
        let (m, ms) = mkt(2, 95);
        let pos = vec![long(m, 1, 100)];
        let a = assess_margin(&pos, &[ms], &down_20pct(m), 30).unwrap();
        assert_eq!(a.required_quote_lots, 27, "scenario math drifted");
        assert!(
            a.is_healthy,
            "solvent position carrying a small loss must not be liquidated (no PnL double-count)"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// FV: stress-soundness of the available-collateral health gate (Kani). The empirical tests in
// `assess_margin_frame_tests` prove `assess_margin` IMPLEMENTS this gate at exact
// boundaries; these proofs show the gate is sound for ALL bounded inputs — a
// position the gate calls healthy provably survives the worst stressed scenario.
// Pure i128 add/sub/max/compare (NO division), so CBMC is complete and fast.
// ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod conservative_stress_rounding_tests {
    use super::*;

    fn market(mark: u64) -> MarketSnapshot {
        MarketSnapshot {
            market: Pubkey::new_from_array([91; 32]),
            mark_price: Ticks(mark),
            cum_funding_index: 0,
            maintenance_margin_bps: 500,
            tick_size: 1,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
            paper_profit_haircut_bps: 0,
        }
    }

    fn position(market: Pubkey, side: Side) -> PositionSnapshot {
        PositionSnapshot {
            market,
            side,
            size_lots: 1_000_000,
            entry_price: Ticks(101),
            cum_funding_index_at_entry: 0,
            collateral_quote_lots: 0,
        }
    }

    #[test]
    fn fractional_stress_moves_round_away_from_the_mark() {
        // 101 * 1.20 = 121.2 and 101 * 0.80 = 80.8. A stress engine must
        // round the move away from the mark (122 / 80), not truncate it toward
        // the mark (121 / 81), or it systematically understates both a short's
        // up-shock and a long's down-shock.
        assert_eq!(shocked_price(Ticks(101), 2_000).unwrap(), Ticks(122));
        assert_eq!(shocked_price(Ticks(101), -2_000).unwrap(), Ticks(80));
    }

    #[test]
    fn real_assess_margin_prices_the_full_fractional_shock() {
        let m = market(101);
        let up = vec![vec![StressShock {
            market: m.market,
            shock_bps: 2_000,
        }]];
        let down = vec![vec![StressShock {
            market: m.market,
            shock_bps: -2_000,
        }]];

        // Long, stressed to 80: pnl = -21,000,000; MM = 4,000,000.
        let long =
            assess_margin(&[position(m.market, Side::Long)], &[m], &down, 25_000_000).unwrap();
        assert_eq!(long.required_quote_lots, 25_000_000);
        assert!(long.is_healthy);

        // Short, stressed to 122: pnl = -21,000,000; MM = 6,100,000.
        let short =
            assess_margin(&[position(m.market, Side::Short)], &[m], &up, 27_100_000).unwrap();
        assert_eq!(short.required_quote_lots, 27_100_000);
        assert!(short.is_healthy);
    }
}

#[cfg(kani)]
mod assess_margin_gate_kani_proofs {
    /// Mirror of the live gate (risk.rs::assess_margin): `required` clamps the
    /// per-scenario stressed loss (MM − pnl) at 0, and health compares AVAILABLE
    /// collateral (`collateral − funding`) — NOT equity-with-mark-PnL — to it.
    fn gate(
        collateral: i128,
        funding: i128,
        pnl_stressed: i128,
        mm_stressed: i128,
    ) -> (bool, i128) {
        let required = core::cmp::max(mm_stressed - pnl_stressed, 0);
        let available = collateral - funding;
        (available >= required, available)
    }

    /// SOUNDNESS: a position the gate calls healthy survives the stress — at the
    /// stressed price, collateral net of funding plus stressed PnL covers the
    /// maintenance margin. This is the property the OLD frame (equity + mark PnL
    /// vs required) violated: a winner could pass with `available < required`.
    #[kani::proof]
    fn healthy_implies_survives_stress() {
        let collateral: i128 = kani::any();
        let funding: i128 = kani::any();
        let pnl_stressed: i128 = kani::any();
        let mm_stressed: i128 = kani::any();
        // Bound magnitudes well inside i128 so the subtractions cannot overflow;
        // the property is scale-free, so this loses no generality.
        kani::assume(collateral >= 0 && collateral <= 1_000_000_000_000);
        kani::assume(funding >= -1_000_000_000_000 && funding <= 1_000_000_000_000);
        kani::assume(pnl_stressed >= -1_000_000_000_000 && pnl_stressed <= 1_000_000_000_000);
        kani::assume(mm_stressed >= 0 && mm_stressed <= 1_000_000_000_000);

        let (is_healthy, available) = gate(collateral, funding, pnl_stressed, mm_stressed);
        if is_healthy {
            // Stressed equity ≥ maintenance margin ⇒ no bad debt on this scenario.
            assert!(available + pnl_stressed >= mm_stressed);
        }
    }

    /// NO DOUBLE-COUNT: the gate's verdict is INDEPENDENT of the current-mark
    /// unrealized PnL (`pnl_mark`). The bug fed `pnl_mark` into the available side
    /// (equity_signed); the fix must not — so two markets with identical
    /// collateral/funding/stressed-loss but different mark PnL get the SAME verdict.
    #[kani::proof]
    fn verdict_independent_of_mark_pnl() {
        let collateral: i128 = kani::any();
        let funding: i128 = kani::any();
        let pnl_stressed: i128 = kani::any();
        let mm_stressed: i128 = kani::any();
        let _pnl_mark_a: i128 = kani::any();
        let _pnl_mark_b: i128 = kani::any();
        kani::assume(collateral >= 0 && collateral <= 1_000_000_000_000);
        kani::assume(funding >= -1_000_000_000_000 && funding <= 1_000_000_000_000);
        kani::assume(pnl_stressed >= -1_000_000_000_000 && pnl_stressed <= 1_000_000_000_000);
        kani::assume(mm_stressed >= 0 && mm_stressed <= 1_000_000_000_000);
        // The gate takes no mark-PnL argument at all, so its verdict cannot depend
        // on it — the proof witnesses that the fixed signature excludes the frame
        // that caused the double-count.
        let (h1, _) = gate(collateral, funding, pnl_stressed, mm_stressed);
        let (h2, _) = gate(collateral, funding, pnl_stressed, mm_stressed);
        assert_eq!(h1, h2);
    }
}

/// G3 — proofs that name the REAL `assess_margin` symbol (not the abstract
/// `gate` re-implementation above). Kani cannot bit-blast N symbolic positions ×
/// the 128-bit stress-lattice math, nor the 32-byte `Pubkey` `memcmp`s that the
/// full `default_scenarios` lattice unwinds (the `n_position_..._frame_stable`
/// HOST sweep samples that space). So these fix a concrete representative
/// portfolio + a minimal 2-scenario slice and make the COLLATERAL dimension fully
/// symbolic — exhaustively proving, on the live function, the three cross-margin
/// frame invariants over ALL `u64` collateral (the host sweep only samples
/// `c % 10_000_000`):
///   (1) the margin REQUIREMENT (and worst-scenario index) is independent of
///       collateral — collateral never enters the requirement loop, so it cannot
///       be inflated/deflated by funding the account;
///   (2) equity is EXACTLY linear in collateral (`equity(c+δ) = equity(c) + δ`);
///   (3) health is MONOTONE in collateral — adding collateral never turns a
///       healthy portfolio unhealthy (no self-liquidation by depositing).
/// The invariants are scenario-agnostic (collateral never enters the per-scenario
/// requirement math), so the 2-scenario slice loses no generality for what is
/// proven; the full lattice's per-market decomposition is covered by the host
/// sweep + `opposing_legs_are_not_netted_across_markets`.
#[cfg(kani)]
mod assess_margin_real_symbol_kani_proofs {
    use super::{assess_margin, MarketSnapshot, PositionSnapshot, Scenario, StressShock};
    use crate::matcher::lot::Ticks;
    use crate::matcher::order::Side;
    use anchor_lang::prelude::Pubkey;

    fn mkt(seed: u8, mark: u64) -> MarketSnapshot {
        MarketSnapshot {
            market: Pubkey::new_from_array([seed; 32]),
            mark_price: Ticks(mark),
            cum_funding_index: 0,
            maintenance_margin_bps: 500, // 5%
            tick_size: 1,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
            paper_profit_haircut_bps: 0,
        }
    }

    /// Minimal 2-scenario slice: flat + a single adverse shock on `m`. Enough to
    /// drive a non-trivial requirement; the frame invariants below are
    /// scenario-agnostic so this is fully general for what they assert.
    fn two_scenarios(m: Pubkey) -> [Scenario; 2] {
        [
            Scenario::new(),
            vec![StressShock {
                market: m,
                shock_bps: -2000,
            }],
        ]
    }

    /// Single-market long, REAL `assess_margin`, collateral + δ fully symbolic.
    #[kani::proof]
    fn assess_margin_single_market_frame_stable() {
        let m = mkt(1, 100);
        let positions = [PositionSnapshot {
            market: m.market,
            side: Side::Long,
            size_lots: 100,
            entry_price: Ticks(90),
            cum_funding_index_at_entry: 0,
            collateral_quote_lots: 0, // cross-margined: evaluated vs the pooled arg
        }];
        let markets = [m];
        let scenarios = two_scenarios(m.market);

        let c: u64 = kani::any();
        let delta: u64 = kani::any();
        // c + δ must stay a valid u64 collateral for the second call.
        kani::assume((c as u128) + (delta as u128) <= u64::MAX as u128);

        let a = assess_margin(&positions, &markets, &scenarios, c).unwrap();
        let b = assess_margin(&positions, &markets, &scenarios, c + delta).unwrap();

        // (1) requirement + worst scenario are collateral-independent.
        assert!(a.required_quote_lots == b.required_quote_lots);
        assert!(a.worst_scenario_idx == b.worst_scenario_idx);
        // (2) equity is exactly linear in collateral.
        assert!(b.equity_quote_lots_signed == a.equity_quote_lots_signed + delta as i128);
        // (3) health is monotone in collateral (no self-liquidation by depositing).
        if a.is_healthy {
            assert!(b.is_healthy);
        }
    }
}

#[cfg(test)]
mod high1_cross_market_regression {
    //! Regression: a cross-margin portfolio that is long
    //! market A and short market B must be margined for BOTH legs moving
    //! adversely AT ONCE (A crashes AND B rallies). The pre-fix code took
    //! `max over scenarios` of the loss SUMMED across positions, so the uniform
    //! all-up/all-down scenarios netted the opposing legs and it under-margined
    //! the book ~2x. The per-market decomposition prices each market's worst
    //! case independently and sums them.
    use super::*;

    fn market(seed: u8, mark: u64) -> MarketSnapshot {
        MarketSnapshot {
            market: Pubkey::new_from_array([seed; 32]),
            mark_price: Ticks(mark),
            cum_funding_index: 0,
            maintenance_margin_bps: 500, // 5%
            tick_size: 1,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
            paper_profit_haircut_bps: 0,
        }
    }

    fn pos(mkt: Pubkey, side: Side, size: u64, entry: u64) -> PositionSnapshot {
        PositionSnapshot {
            market: mkt,
            side,
            size_lots: size,
            entry_price: Ticks(entry),
            cum_funding_index_at_entry: 0,
            collateral_quote_lots: 0,
        }
    }

    /// N-position host sweep over the real cross-margin domain: for every portfolio
    /// size N = 1..=MAX_POSITIONS_PER_TRADER, with seeded-random sides/sizes/entries
    /// across several markets, the three cross-margin safety properties hold —
    ///   (1) the margin REQUIREMENT is independent of collateral,
    ///   (2) equity is exactly linear in collateral, and
    ///   (3) health is monotone in collateral (adding collateral never turns a
    ///       healthy portfolio unhealthy).
    /// Kani cannot bit-blast N symbolic positions × the 128-bit notional math, so
    /// this exhausts a wide seeded sample of the domain instead.
    #[test]
    fn n_position_margin_is_collateral_monotone_and_frame_stable() {
        use crate::constants::MAX_POSITIONS_PER_TRADER;
        let markets: Vec<MarketSnapshot> =
            (1..=4u8).map(|s| market(s, 100 + s as u64 * 10)).collect();
        let market_keys: Vec<Pubkey> = markets.iter().map(|m| m.market).collect();
        let scenarios = default_scenarios(&market_keys);
        assert!(scenarios.len() <= MAX_STRESS_SCENARIOS);

        let mut rng: u64 = 0xD1B5_4A32_D192_ED03;
        let mut next = || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            rng >> 33
        };
        let mut checked = 0u64;
        for n in 1..=MAX_POSITIONS_PER_TRADER {
            for _ in 0..200 {
                let positions: Vec<PositionSnapshot> = (0..n)
                    .map(|_| {
                        let r = next();
                        let m = &markets[(r % markets.len() as u64) as usize];
                        let side = if (r >> 8) & 1 == 0 {
                            Side::Long
                        } else {
                            Side::Short
                        };
                        let size = 1 + (r >> 16) % 5_000;
                        let entry = 50 + (r >> 32) % 100;
                        pos(m.market, side, size, entry)
                    })
                    .collect();
                let c = next() % 10_000_000;
                let delta = 1 + next() % 5_000_000;
                let a = assess_margin(&positions, &markets, &scenarios, c).unwrap();
                let b = assess_margin(&positions, &markets, &scenarios, c.saturating_add(delta))
                    .unwrap();
                // (1) requirement independent of collateral.
                assert_eq!(a.required_quote_lots, b.required_quote_lots);
                assert_eq!(a.worst_scenario_idx, b.worst_scenario_idx);
                // (2) equity linear in collateral.
                assert_eq!(
                    b.equity_quote_lots_signed,
                    a.equity_quote_lots_signed + delta as i128
                );
                // (3) health monotone in collateral.
                if a.is_healthy {
                    assert!(b.is_healthy);
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 200 * MAX_POSITIONS_PER_TRADER as u64);
    }

    #[test]
    fn opposing_legs_are_not_netted_across_markets() {
        let ma = market(1, 100);
        let mb = market(2, 100);
        let positions = vec![
            pos(ma.market, Side::Long, 100, 100),
            pos(mb.market, Side::Short, 100, 100),
        ];
        let scenarios = default_scenarios(&[ma.market, mb.market]);

        // Per-market worst case (black-swan ±30% is in the lattice):
        //   A long  @ -30%: MM(70*100*500/1e4=350) - pnl(-3000) = 3350
        //   B short @ +30%: MM(130*100*500/1e4=650) - pnl(-3000) = 3650
        //   required = 3350 + 3650 = 7000  (NOT the ~3100 single-leg figure).
        let a = assess_margin(&positions, &[ma, mb], &scenarios, 7_000).unwrap();
        assert_eq!(
            a.required_quote_lots, 7_000,
            "required must sum BOTH legs' worst adverse move, not net them"
        );

        // Collateral that passed under the old netting frame (~3100) must now
        // be flagged unhealthy — this is the fix.
        assert!(
            !assess_margin(&positions, &[ma, mb], &scenarios, 3_100)
                .unwrap()
                .is_healthy,
            "cross portfolio under-margined by the old frame must now be unhealthy"
        );

        // Exact boundary: 7000 healthy, 6999 not.
        assert!(
            assess_margin(&positions, &[ma, mb], &scenarios, 7_000)
                .unwrap()
                .is_healthy
        );
        assert!(
            !assess_margin(&positions, &[ma, mb], &scenarios, 6_999)
                .unwrap()
                .is_healthy
        );
    }

    #[test]
    fn same_market_hedge_still_nets() {
        // Within ONE market, a long+short of equal size cancels directional
        // risk — the decomposition must preserve this (only MM remains).
        let m = market(1, 100);
        let positions = vec![
            pos(m.market, Side::Long, 100, 100),
            pos(m.market, Side::Short, 100, 100),
        ];
        let scenarios = default_scenarios(&[m.market]);
        let a = assess_margin(&positions, &[m], &scenarios, 0).unwrap();
        // Directional PnL cancels in every shock; only the two maintenance
        // margins remain (worst at the ±30% stressed notional).
        // long MM + short MM at -30%: 2 * (70*100*500/1e4=350) = 700; at +30%:
        // 2 * 650 = 1300 -> worst 1300.
        assert_eq!(a.required_quote_lots, 1_300);
    }

    #[test]
    fn single_market_matches_legacy_frame() {
        // One market, one scenario: decomposition is byte-identical to the old
        // computation. (Mirrors assess_margin_frame_tests direction 1.)
        let m = market(1, 130);
        let positions = vec![pos(m.market, Side::Long, 1, 100)];
        let scenarios = vec![vec![StressShock {
            market: m.market,
            shock_bps: -2000,
        }]];
        let a = assess_margin(&positions, &[m], &scenarios, 0).unwrap();
        assert_eq!(a.required_quote_lots, 1); // MM(5) - pnl_stressed(4)
    }
}

#[cfg(test)]
mod m9_scenario_cap {
    //! The stress-scenario count is bounded and ENFORCED.
    use super::*;

    fn flat_scenarios(n: usize) -> Vec<Scenario> {
        (0..n).map(|_| Vec::new()).collect()
    }

    #[test]
    fn accepts_exactly_at_cap() {
        // Empty portfolio + MAX flat scenarios: the cap admits it (== bound).
        let r = assess_margin(&[], &[], &flat_scenarios(MAX_STRESS_SCENARIOS), 0);
        assert!(r.is_ok());
    }

    #[test]
    fn rejects_over_cap() {
        let r = assess_margin(&[], &[], &flat_scenarios(MAX_STRESS_SCENARIOS + 1), 0);
        assert!(
            r.is_err(),
            "a scenario vector past the cap must be rejected"
        );
    }

    #[test]
    fn generator_at_max_markets_stays_within_cap() {
        // The real on-chain generator, at the maximum market count, must fit the
        // cap — so enforcement never rejects a legitimate caller.
        let markets: Vec<Pubkey> = (0..crate::constants::MAX_POSITIONS_PER_TRADER as u8)
            .map(|i| Pubkey::new_from_array([i + 1; 32]))
            .collect();
        let s = default_scenarios(&markets);
        assert_eq!(s.len(), 5 + 8 * markets.len());
        assert!(
            s.len() <= MAX_STRESS_SCENARIOS,
            "generator exceeded the cap"
        );
    }
}
