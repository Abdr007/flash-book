//! Trailing stop (Wave 52).
//!
//! A trigger whose trigger_price moves with the oracle:
//! - **Long trailing stop**: trigger = `max(highest_seen_oracle, oracle) - offset`.
//!   As price rises, the stop ratchets up. Once price drops by
//!   `offset` from the high, the stop fires (close-long).
//! - **Short trailing stop**: trigger = `min(lowest_seen_oracle, oracle) + offset`.
//!   Symmetric for short positions.
//!
//! The "high water mark" / "low water mark" lives on the trigger
//! order PDA; updated on every oracle update. Pure module + helpers
//! for the wire-in.

/// Update the high-water-mark for a long trailing stop, given the new
/// oracle observation. Returns the new HWM.
#[inline]
pub fn update_hwm_long(current_hwm: u64, new_oracle_ticks: u64) -> u64 {
    current_hwm.max(new_oracle_ticks)
}

/// Update the low-water-mark for a short trailing stop.
#[inline]
pub fn update_lwm_short(current_lwm: u64, new_oracle_ticks: u64) -> u64 {
    if current_lwm == 0 {
        // First observation.
        return new_oracle_ticks;
    }
    current_lwm.min(new_oracle_ticks)
}

/// Compute the current effective trigger price for a long trailing
/// stop. Returns `None` on underflow (offset > HWM).
#[inline]
pub fn effective_trigger_long(hwm_ticks: u64, offset_ticks: u64) -> Option<u64> {
    hwm_ticks.checked_sub(offset_ticks)
}

/// Compute the current effective trigger price for a short trailing
/// stop.
#[inline]
pub fn effective_trigger_short(lwm_ticks: u64, offset_ticks: u64) -> Option<u64> {
    lwm_ticks.checked_add(offset_ticks)
}

/// Should a long trailing stop fire? Pure decision.
#[inline]
pub fn long_trailing_should_fire(
    oracle_ticks: u64,
    hwm_ticks: u64,
    offset_ticks: u64,
) -> bool {
    match effective_trigger_long(hwm_ticks, offset_ticks) {
        Some(t) => oracle_ticks <= t,
        None => false,
    }
}

#[inline]
pub fn short_trailing_should_fire(
    oracle_ticks: u64,
    lwm_ticks: u64,
    offset_ticks: u64,
) -> bool {
    match effective_trigger_short(lwm_ticks, offset_ticks) {
        Some(t) => oracle_ticks >= t,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hwm_ratchets_up_only() {
        assert_eq!(update_hwm_long(100, 110), 110);
        assert_eq!(update_hwm_long(110, 90), 110, "doesn't ratchet down");
        assert_eq!(update_hwm_long(110, 110), 110);
    }

    #[test]
    fn lwm_ratchets_down_only() {
        // First observation seeds.
        assert_eq!(update_lwm_short(0, 100), 100);
        assert_eq!(update_lwm_short(100, 90), 90);
        assert_eq!(update_lwm_short(90, 110), 90, "doesn't ratchet up");
    }

    #[test]
    fn effective_trigger_long_subtracts_offset() {
        assert_eq!(effective_trigger_long(100, 5), Some(95));
        assert_eq!(effective_trigger_long(100, 100), Some(0));
        assert_eq!(effective_trigger_long(50, 100), None);
    }

    #[test]
    fn effective_trigger_short_adds_offset() {
        assert_eq!(effective_trigger_short(100, 5), Some(105));
    }

    #[test]
    fn long_fires_when_oracle_drops_below_trailing() {
        // HWM 100, offset 5 → trigger at 95.
        assert!(!long_trailing_should_fire(96, 100, 5));
        assert!(long_trailing_should_fire(95, 100, 5));
        assert!(long_trailing_should_fire(90, 100, 5));
    }

    #[test]
    fn short_fires_when_oracle_rises_above_trailing() {
        // LWM 100, offset 5 → trigger at 105.
        assert!(!short_trailing_should_fire(104, 100, 5));
        assert!(short_trailing_should_fire(105, 100, 5));
        assert!(short_trailing_should_fire(110, 100, 5));
    }

    #[test]
    fn full_lifecycle_long_trailing() {
        // Trader entry at 100, trailing offset 5.
        let mut hwm = 100u64;
        // Price rises to 110; HWM ratchets.
        hwm = update_hwm_long(hwm, 110);
        assert_eq!(hwm, 110);
        // Drops to 106 — within trail.
        assert!(!long_trailing_should_fire(106, hwm, 5));
        // Drops to 105 — exact trail.
        assert!(long_trailing_should_fire(105, hwm, 5));
    }
}
