//! Duration-weighted tiered LP rewards (Wave 56).
//!
//! Reward LPs for committing capital long-term. Each LP earns a
//! `share_multiplier` based on how long their capital has been
//! locked in the FLP. This makes flash flow into the LP pool
//! economically unattractive (the rewards aren't there for short
//! holders).
//!
//! Tier table is `(min_slots_held, multiplier_bps)`, sorted by
//! `min_slots_held`. Tiers stack like Hyperliquid leverage tiers —
//! the LP gets the multiplier of the highest tier they qualify for.
//!
//! Pure math.

use crate::constants::BPS_DENOM;

/// Compute the share multiplier (bps over 10_000 base) for an LP
/// given their slot-held count.
///
/// `tiers` MUST be sorted ascending by `min_slots_held`. The first
/// tier is the baseline (typically `(0, 10_000)` = 1.0x).
pub fn share_multiplier_bps(slots_held: u64, tiers: &[(u64, u32)]) -> u32 {
    let mut mult = BPS_DENOM; // default 1.0x
    for (min_slots, m) in tiers {
        if slots_held >= *min_slots {
            mult = *m;
        } else {
            break;
        }
    }
    mult
}

/// Apply the multiplier to a base reward amount. `base × mult / BPS_DENOM`.
pub fn apply_multiplier(base_reward_quote_lots: u128, mult_bps: u32) -> u128 {
    base_reward_quote_lots.saturating_mul(mult_bps as u128) / (BPS_DENOM as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_returns_no_premium() {
        let tiers = [(0u64, BPS_DENOM)];
        assert_eq!(share_multiplier_bps(0, &tiers), BPS_DENOM);
        assert_eq!(share_multiplier_bps(u64::MAX, &tiers), BPS_DENOM);
    }

    #[test]
    fn hl_style_tier_table() {
        let tiers = [
            (0u64, 10_000),      // baseline
            (100_000, 11_000),   // > ~11h held → 1.1x
            (1_000_000, 12_500), // > ~5d held → 1.25x
            (10_000_000, 15_000),// > ~46d held → 1.5x
        ];
        assert_eq!(share_multiplier_bps(0, &tiers), 10_000);
        assert_eq!(share_multiplier_bps(50_000, &tiers), 10_000);
        assert_eq!(share_multiplier_bps(100_000, &tiers), 11_000);
        assert_eq!(share_multiplier_bps(500_000, &tiers), 11_000);
        assert_eq!(share_multiplier_bps(1_000_000, &tiers), 12_500);
        assert_eq!(share_multiplier_bps(20_000_000, &tiers), 15_000);
    }

    #[test]
    fn empty_tiers_returns_baseline() {
        assert_eq!(share_multiplier_bps(123_456, &[]), BPS_DENOM);
    }

    #[test]
    fn apply_multiplier_scales() {
        assert_eq!(apply_multiplier(1_000, 10_000), 1_000);
        assert_eq!(apply_multiplier(1_000, 11_000), 1_100);
        assert_eq!(apply_multiplier(1_000, 5_000), 500);
    }

    #[test]
    fn apply_multiplier_zero_zeros() {
        assert_eq!(apply_multiplier(1_000, 0), 0);
    }

    #[test]
    fn monotone_in_slots_held() {
        let tiers = [
            (0u64, 10_000),
            (100, 11_000),
            (1_000, 12_000),
        ];
        let mut prev = 0u32;
        for s in [0u64, 50, 100, 500, 1_000, 10_000] {
            let m = share_multiplier_bps(s, &tiers);
            assert!(m >= prev);
            prev = m;
        }
    }
}
