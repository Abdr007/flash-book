//! Per-trader position size cap (Wave 31).
//!
//! Trader-settable max position notional. The trader sets it once via
//! `set_trader_position_cap` (Wave 31b ix); thereafter, every order
//! intake checks `prospective_notional ≤ trader.max_position_notional`.
//!
//! Useful as a personal risk control: a trader can prevent their own
//! bot from running them into an oversized position. Also useful for
//! shared keypair scenarios (multiple ops with limited authority).
//!
//! Pure math. No Solana types.

/// Check whether a prospective position notional respects the trader's
/// per-position cap. `cap == 0` means "no cap configured" — admits
/// any size (legacy behavior).
///
/// Returns `true` if the order would breach the cap. The wire-in
/// rejects the intake when this is true.
#[inline]
pub fn cap_breached(
    prospective_notional_quote_lots: u128,
    trader_cap_quote_lots: u64,
) -> bool {
    if trader_cap_quote_lots == 0 {
        return false;
    }
    prospective_notional_quote_lots > trader_cap_quote_lots as u128
}

/// Compute the maximum admissible incremental size in lots given the
/// existing position size + cap + market mark + tick size.
///
/// Returns 0 when the position is already at or past the cap. Returns
/// `u64::MAX` when `cap == 0` (no cap configured) — admit any size.
pub fn max_incremental_lots(
    existing_position_lots: u64,
    mark_price_ticks: u64,
    tick_size: u64,
    trader_cap_quote_lots: u64,
) -> u64 {
    if trader_cap_quote_lots == 0 {
        return u64::MAX;
    }
    if mark_price_ticks == 0 || tick_size == 0 {
        return 0;
    }
    // notional_per_lot = mark × tick_size.
    let per_lot_notional = (mark_price_ticks as u128).saturating_mul(tick_size as u128);
    if per_lot_notional == 0 {
        return 0;
    }
    let max_total_lots = (trader_cap_quote_lots as u128) / per_lot_notional;
    let remaining = max_total_lots.saturating_sub(existing_position_lots as u128);
    remaining.min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_zero_means_no_cap() {
        assert!(!cap_breached(u128::MAX, 0));
        assert_eq!(max_incremental_lots(0, 1_000, 1, 0), u64::MAX);
    }

    #[test]
    fn cap_breached_when_notional_exceeds_cap() {
        assert!(cap_breached(1_001, 1_000));
        assert!(!cap_breached(1_000, 1_000));
        assert!(!cap_breached(999, 1_000));
    }

    #[test]
    fn incremental_lots_full_at_zero_existing() {
        // Cap 1M, mark 100, tick 1 → notional/lot = 100 → max 10_000 lots.
        assert_eq!(max_incremental_lots(0, 100, 1, 1_000_000), 10_000);
    }

    #[test]
    fn incremental_lots_partial_when_existing_present() {
        // Cap 1M, mark 100, tick 1 → max 10_000 lots. Existing 3_000.
        // Remaining = 7_000.
        assert_eq!(max_incremental_lots(3_000, 100, 1, 1_000_000), 7_000);
    }

    #[test]
    fn incremental_lots_zero_when_at_or_past_cap() {
        // Existing 10_000, cap allows 10_000 → 0 remaining.
        assert_eq!(max_incremental_lots(10_000, 100, 1, 1_000_000), 0);
        // Past cap → still 0 (saturating).
        assert_eq!(max_incremental_lots(15_000, 100, 1, 1_000_000), 0);
    }

    #[test]
    fn incremental_lots_handles_zero_mark_safely() {
        assert_eq!(max_incremental_lots(0, 0, 1, 1_000_000), 0);
    }

    #[test]
    fn incremental_lots_handles_zero_tick_safely() {
        assert_eq!(max_incremental_lots(0, 100, 0, 1_000_000), 0);
    }

    #[test]
    fn cap_breached_handles_extreme_values() {
        // u128::MAX notional vs u64::MAX cap → breach.
        assert!(cap_breached(u128::MAX, u64::MAX));
        // Just-under-cap notional → no breach.
        assert!(!cap_breached(u64::MAX as u128, u64::MAX));
    }
}
