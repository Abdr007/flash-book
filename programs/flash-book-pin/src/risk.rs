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
