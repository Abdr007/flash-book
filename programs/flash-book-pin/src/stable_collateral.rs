//! Stable cross-collateral with weighting (Wave 65).
//!
//! Accept multiple stable assets as collateral (USDC, USDT, DAI, etc.)
//! with per-asset weights that reflect their relative trust.
//!
//! Example weighting:
//! - USDC: 10_000 bps (100%, baseline)
//! - USDT: 9_950 bps (99.5%, slight peg-risk discount)
//! - DAI:  9_800 bps (98%, larger peg-risk discount)
//! - PYUSD: 9_700 bps
//!
//! A trader's effective collateral = Σ (raw_amount_i × weight_i / BPS_DENOM).
//! Used in margin computations instead of raw token balance.

use crate::constants::BPS_DENOM;

/// Compute the haircut-weighted effective collateral for a single asset.
///
/// `raw_amount_quote_lots`: the actual on-chain balance.
/// `weight_bps`: per-asset trust weight (10_000 = no haircut).
///
/// Returns effective collateral in quote lots.
pub fn weighted_collateral(raw_amount_quote_lots: u64, weight_bps: u32) -> u64 {
    if weight_bps == BPS_DENOM {
        return raw_amount_quote_lots;
    }
    let scaled = (raw_amount_quote_lots as u128)
        .saturating_mul(weight_bps as u128)
        / (BPS_DENOM as u128);
    scaled.min(u64::MAX as u128) as u64
}

/// Sum multiple weighted collateral contributions. Saturating.
pub fn total_weighted_collateral(contributions: &[(u64, u32)]) -> u64 {
    let mut total: u128 = 0;
    for (amount, weight) in contributions {
        total = total.saturating_add(weighted_collateral(*amount, *weight) as u128);
    }
    total.min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_weight_returns_raw() {
        assert_eq!(weighted_collateral(1_000_000, BPS_DENOM), 1_000_000);
    }

    #[test]
    fn half_weight_halves() {
        assert_eq!(weighted_collateral(1_000_000, 5_000), 500_000);
    }

    #[test]
    fn typical_usdt_weight() {
        // 99.5% weight on USDT.
        assert_eq!(weighted_collateral(1_000_000, 9_950), 995_000);
    }

    #[test]
    fn zero_weight_zeros() {
        assert_eq!(weighted_collateral(1_000_000, 0), 0);
    }

    #[test]
    fn total_sums_weighted() {
        // 1M USDC × 100% + 1M USDT × 99.5% + 1M DAI × 98% = 1M + 995k + 980k = 2_975_000.
        let contribs = [(1_000_000u64, BPS_DENOM), (1_000_000, 9_950), (1_000_000, 9_800)];
        assert_eq!(total_weighted_collateral(&contribs), 2_975_000);
    }

    #[test]
    fn empty_total_is_zero() {
        assert_eq!(total_weighted_collateral(&[]), 0);
    }

    #[test]
    fn saturating_on_overflow() {
        // u64::MAX × 2 × full weight would overflow → saturates.
        let contribs = [(u64::MAX, BPS_DENOM), (u64::MAX, BPS_DENOM)];
        assert_eq!(total_weighted_collateral(&contribs), u64::MAX);
    }
}
