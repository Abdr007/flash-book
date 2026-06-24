//! Risk / maintenance-margin math — the pure, self-contained core of
//! `matcher/risk.rs` (MMR tier + OI-scaled + concentration composition).
//!
//! De-anchored port: these functions take only primitives + slices (no Solana
//! types, no `Vec`, no accounts), so they carry over verbatim. The
//! account-iterating `assess_margin` / stress-scenario machinery is a follow-up
//! (it needs the snapshot structs + a `no_std` buffer for scenarios).

/// OI-scaled MMR extra (Wave 28a): `side_oi_lots × slope / 1e6`, capped.
/// `slope_bps_per_million_lots == 0` disables. u128 intermediate (no overflow).
pub fn oi_scaled_mmr_extra_bps(
    side_oi_lots: u64,
    slope_bps_per_million_lots: u32,
    max_extra_bps: u32,
) -> u32 {
    if slope_bps_per_million_lots == 0 {
        return 0;
    }
    let scaled = (side_oi_lots as u128).saturating_mul(slope_bps_per_million_lots as u128);
    let extra = scaled / 1_000_000;
    (extra.min(max_extra_bps as u128) as u32).min(max_extra_bps)
}

/// Full effective MMR: stress-lattice tier + OI-scaled extra.
/// `tiers` = `&[(min_notional, mmr_bps)]` sorted ascending; `&[]` skips.
/// `(oi_slope, oi_max) = (0, 0)` skips the OI term.
pub fn effective_mmr_bps_full(
    base_mmr_bps: u32,
    tiers: &[(u64, u32)],
    position_notional_quote_lots: u128,
    side_oi_lots: u64,
    oi_slope_bps_per_million_lots: u32,
    oi_max_extra_bps: u32,
) -> u32 {
    let tier_mmr = tiered_mmr_bps(base_mmr_bps, tiers, position_notional_quote_lots);
    let oi_extra =
        oi_scaled_mmr_extra_bps(side_oi_lots, oi_slope_bps_per_million_lots, oi_max_extra_bps);
    tier_mmr.saturating_add(oi_extra)
}

/// Hyperliquid-style multi-tier MMR. `tiers` = `&[(min_notional, mmr_bps)]`
/// sorted ascending by notional; the effective MMR is the largest tier whose
/// `min_notional ≤ notional`, else `base_mmr_bps`.
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
    fn oi_scaled_zero_slope_returns_zero() {
        assert_eq!(oi_scaled_mmr_extra_bps(1_000_000, 0, 1_000), 0);
        assert_eq!(oi_scaled_mmr_extra_bps(u64::MAX, 0, 1_000), 0);
    }

    #[test]
    fn oi_scaled_linear_with_oi() {
        assert_eq!(oi_scaled_mmr_extra_bps(1_000_000, 100, 10_000), 100);
        assert_eq!(oi_scaled_mmr_extra_bps(500_000, 100, 10_000), 50);
        assert_eq!(oi_scaled_mmr_extra_bps(10_000_000, 100, 10_000), 1_000);
    }

    #[test]
    fn oi_scaled_capped_at_max() {
        assert_eq!(oi_scaled_mmr_extra_bps(100_000_000, 100, 500), 500);
    }

    #[test]
    fn oi_scaled_handles_extreme_inputs_without_overflow() {
        let _ = oi_scaled_mmr_extra_bps(u64::MAX, u32::MAX, 10_000);
    }

    #[test]
    fn effective_mmr_full_stacks_tier_and_oi() {
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
        let tiers = [
            (1_000_000u64, 100u32),
            (5_000_000, 200),
            (25_000_000, 300),
            (100_000_000, 500),
        ];
        assert_eq!(tiered_mmr_bps(50, &tiers, 500_000), 50);
        assert_eq!(tiered_mmr_bps(50, &tiers, 3_000_000), 100);
        assert_eq!(tiered_mmr_bps(50, &tiers, 7_000_000), 200);
        assert_eq!(tiered_mmr_bps(50, &tiers, 30_000_000), 300);
        assert_eq!(tiered_mmr_bps(50, &tiers, 200_000_000), 500);
    }
}

// ─── Margin assessment (stress-lattice) ─────────────────────────────
//
// De-anchored port of `assess_margin` + its snapshot structs. The matcher works
// in pure-Rust space (it never reaches into accounts), so this carries over
// almost verbatim. Two no_std adaptations:
//   * `Scenario` is a `&[StressShock]` slice (Anchor used `Vec<StressShock>`);
//     `assess_margin` takes `scenarios: &[&[StressShock]]`.
//   * `FundingIndex` is `i128` (matches `funding::funding_owed`, which returns
//     `Option` here — discharged with `or_overflow`).

use crate::error::{FlashBookError, OrOverflow, Result};
use crate::funding::funding_owed;
use crate::lot::Ticks;
use crate::order::Side;
use crate::state::Pubkey;

const BPS_DENOM: u128 = 10_000;

#[derive(Debug, Clone, Copy)]
pub struct StressShock {
    pub market: Pubkey,
    /// Signed shock in bps. Positive = price up.
    pub shock_bps: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct PositionSnapshot {
    pub market: Pubkey,
    pub side: Side,
    pub size_lots: u64,
    pub entry_price: Ticks,
    pub cum_funding_index_at_entry: i128,
    /// Isolated-margin marker (>0 = isolated against this amount; 0 = cross).
    pub collateral_quote_lots: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct MarketSnapshot {
    pub market: Pubkey,
    pub mark_price: Ticks,
    pub cum_funding_index: i128,
    pub maintenance_margin_bps: u32,
    pub tick_size: u64,
    pub concentration_threshold_lots: u64,
    pub concentration_extra_mmr_bps: u32,
    pub side_oi_lots: u64,
    pub oi_mmr_slope_bps_per_million_lots: u32,
    pub oi_mmr_max_extra_bps: u32,
}

impl MarketSnapshot {
    /// Effective MMR (bps): base + CME concentration extra + OI-scaled extra.
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
            if self.oi_mmr_max_extra_bps == 0 { u32::MAX } else { self.oi_mmr_max_extra_bps },
        );
        base_with_conc.saturating_add(oi_extra)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MarginAssessment {
    pub required_quote_lots: u64,
    pub equity_quote_lots_signed: i128,
    pub is_healthy: bool,
    pub worst_scenario_idx: u32,
}

/// Unrealized PnL (quote-lots, signed; positive = trader gains).
fn unrealized_pnl_quote_lots(pos: &PositionSnapshot, at_price: Ticks, tick_size: u64) -> Result<i128> {
    let sign: i128 = if pos.side == Side::Long { 1 } else { -1 };
    let price_diff: i128 = (at_price.0 as i128) - (pos.entry_price.0 as i128);
    let prod = (pos.size_lots as i128)
        .checked_mul(price_diff)
        .or_overflow()?
        .checked_mul(tick_size as i128)
        .or_overflow()?;
    Ok(sign * prod)
}

/// Apply a bps shock to a price (clamps to 0 if it would go negative).
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

fn lookup_market<'a>(markets: &'a [MarketSnapshot], pk: &Pubkey) -> Option<&'a MarketSnapshot> {
    markets.iter().find(|m| m.market == *pk)
}

fn shock_for_market(scenario: &[StressShock], market: &Pubkey) -> i32 {
    scenario.iter().find(|s| s.market == *market).map(|s| s.shock_bps).unwrap_or(0)
}

/// Assess a trader's margin health across all stress scenarios; the worst-case
/// loss sets `required`. Healthy iff equity ≥ required.
pub fn assess_margin(
    positions: &[PositionSnapshot],
    markets: &[MarketSnapshot],
    scenarios: &[&[StressShock]],
    collateral_quote_lots: u64,
) -> Result<MarginAssessment> {
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
            return Err(FlashBookError::ArithmeticOverflow);
        }
        funding_total = funding_total
            .checked_add(
                funding_owed(
                    pos.side == Side::Long,
                    notional as u64,
                    m.cum_funding_index,
                    pos.cum_funding_index_at_entry,
                )
                .or_overflow()?,
            )
            .or_overflow()?;
    }
    let equity_signed = (collateral_quote_lots as i128)
        .checked_add(unrealized_total)
        .or_overflow()?
        .checked_sub(funding_total)
        .or_underflow()?;

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

            let pnl = unrealized_pnl_quote_lots(pos, stressed, m.tick_size)?;
            scenario_loss_signed = scenario_loss_signed.checked_sub(pnl).or_underflow()?;

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

    let is_healthy = equity_signed >= worst_loss as i128;
    Ok(MarginAssessment {
        required_quote_lots: worst_loss,
        equity_quote_lots_signed: equity_signed,
        is_healthy,
        worst_scenario_idx: worst_idx,
    })
}

#[cfg(test)]
mod assess_tests {
    use super::*;

    const MKT: Pubkey = [7u8; 32];

    fn pos(side: Side, size: u64, entry: u64) -> PositionSnapshot {
        PositionSnapshot {
            market: MKT,
            side,
            size_lots: size,
            entry_price: Ticks(entry),
            cum_funding_index_at_entry: 0,
            collateral_quote_lots: 0,
        }
    }
    fn mkt(mark: u64, mmr_bps: u32) -> MarketSnapshot {
        MarketSnapshot {
            market: MKT,
            mark_price: Ticks(mark),
            cum_funding_index: 0,
            maintenance_margin_bps: mmr_bps,
            tick_size: 1,
            concentration_threshold_lots: 0,
            concentration_extra_mmr_bps: 0,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        }
    }

    #[test]
    fn flat_trader_is_healthy() {
        let a = assess_margin(&[], &[], &[], 1_000).unwrap();
        assert!(a.is_healthy);
        assert_eq!(a.required_quote_lots, 0);
        assert_eq!(a.equity_quote_lots_signed, 1_000);
    }

    #[test]
    fn long_in_profit_at_mark_no_shock() {
        // long 10 @ 100, mark 120 → unrealized +200. No scenarios → required 0.
        let a = assess_margin(&[pos(Side::Long, 10, 100)], &[mkt(120, 100)], &[], 50).unwrap();
        assert_eq!(a.equity_quote_lots_signed, 50 + 200);
        assert!(a.is_healthy);
    }

    #[test]
    fn down_shock_stresses_long() {
        // long 10 @ 100, mark 100. Scenario −5000 bps (−50%) → stressed 50.
        // loss = -unrealized@50 = -(50-100)*10 = +500; mm = 50*10*100/10000 = 5.
        // worst_loss = 505. equity = collateral 100 + unrealized@mark 0 = 100 < 505 → unhealthy.
        let down = [StressShock { market: MKT, shock_bps: -5000 }];
        let a = assess_margin(&[pos(Side::Long, 10, 100)], &[mkt(100, 100)], &[&down[..]], 100).unwrap();
        assert_eq!(a.required_quote_lots, 505);
        assert!(!a.is_healthy);
        assert_eq!(a.worst_scenario_idx, 0);
    }

    #[test]
    fn enough_collateral_survives_shock() {
        let down = [StressShock { market: MKT, shock_bps: -5000 }];
        // Same shock, but 1_000 collateral ≥ 505 → healthy.
        let a = assess_margin(&[pos(Side::Long, 10, 100)], &[mkt(100, 100)], &[&down[..]], 1_000).unwrap();
        assert!(a.is_healthy);
    }

    #[test]
    fn worst_of_multiple_scenarios_wins() {
        let mild = [StressShock { market: MKT, shock_bps: -1000 }];
        let severe = [StressShock { market: MKT, shock_bps: -5000 }];
        let a = assess_margin(
            &[pos(Side::Long, 10, 100)],
            &[mkt(100, 100)],
            &[&mild[..], &severe[..]],
            100,
        )
        .unwrap();
        assert_eq!(a.worst_scenario_idx, 1); // severe is worse
    }

    #[test]
    fn short_stressed_by_up_shock() {
        // short 10 @ 100, mark 100. +5000 bps → stressed 150.
        // loss = -unrealized@150 = -(-(150-100)*10) = +500; mm = 150*10*100/10000 = 15.
        let up = [StressShock { market: MKT, shock_bps: 5000 }];
        let a = assess_margin(&[pos(Side::Short, 10, 100)], &[mkt(100, 100)], &[&up[..]], 100).unwrap();
        assert_eq!(a.required_quote_lots, 515);
        assert!(!a.is_healthy);
    }
}
