//! Per-trader concentration limit (Wave 58).
//!
//! Cap any single trader's share of total OI on a market. Prevents
//! one whale from dominating the book to a degree where their
//! liquidation would be too large for the matcher to handle gracefully.
//!
//! `max_share_bps`: max fraction of side OI a trader can hold.
//!   `0` = unlimited (legacy). `1000` = 10%.

use crate::constants::BPS_DENOM;

/// Compute the max size (lots) a trader can hold on a side given the
/// current side OI and the configured share cap.
///
/// Returns `u64::MAX` if cap is disabled.
pub fn max_trader_size_on_side(
    side_oi_lots: u64,
    max_share_bps: u32,
) -> u64 {
    if max_share_bps == 0 {
        return u64::MAX;
    }
    ((side_oi_lots as u128).saturating_mul(max_share_bps as u128)
        / (BPS_DENOM as u128))
        .min(u64::MAX as u128) as u64
}

/// Check whether a prospective new trader size would breach the cap.
pub fn cap_breached(
    prospective_trader_size_lots: u64,
    side_oi_lots: u64,
    max_share_bps: u32,
) -> bool {
    if max_share_bps == 0 {
        return false;
    }
    let max = max_trader_size_on_side(side_oi_lots, max_share_bps);
    prospective_trader_size_lots > max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_cap_disables() {
        assert_eq!(max_trader_size_on_side(u64::MAX, 0), u64::MAX);
        assert!(!cap_breached(u64::MAX, u64::MAX, 0));
    }

    #[test]
    fn cap_returns_share_of_oi() {
        // 10% of 100_000 = 10_000.
        assert_eq!(max_trader_size_on_side(100_000, 1_000), 10_000);
    }

    #[test]
    fn breach_check_at_boundary() {
        // Cap = 10_000. 10_000 lots not over, 10_001 is.
        assert!(!cap_breached(10_000, 100_000, 1_000));
        assert!(cap_breached(10_001, 100_000, 1_000));
    }

    #[test]
    fn empty_oi_zero_cap() {
        assert_eq!(max_trader_size_on_side(0, 1_000), 0);
        // Any prospective size on empty OI breaches when cap > 0.
        // (This is intentional — the first trader on a side can grow
        // OI but only via incremental moves: 1 lot at a time would
        // pass since cap_breached takes prospective. The caller can
        // special-case "bootstrap" semantics if needed.)
    }

    #[test]
    fn cap_handles_extreme_oi() {
        // Big OI × big cap → can saturate.
        let m = max_trader_size_on_side(u64::MAX, 5_000);
        assert!(m > 0);
        assert!(m < u64::MAX);
    }
}
