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

use crate::constants::BPS_DENOM;

/// Ratchet a trailing stop against a new mark observation. Faithful to the
/// Anchor `update_trailing_stop`.
///
/// `kind == 0` (long SL): anchor = the running MAX mark; trigger = mark − offset,
/// tick-CEILed (keeps the stop tighter / fires sooner on a drop). `kind == 1`
/// (short SL): anchor = the running MIN mark; trigger = mark + offset,
/// tick-FLOORed. Returns `Some((new_anchor, new_aligned_trigger))` when the stop
/// advanced, or `None` on no progress (mark didn't beat the anchor) or a
/// sub-tick no-op. `offset_bps == 0` (not a trailing trigger) → `None`.
pub fn ratchet(
    kind: u8,
    mark_ticks: u64,
    offset_bps: u16,
    prev_anchor_ticks: u64,
    tick_size: u64,
    current_trigger_ticks: u64,
) -> Option<(u64, u64)> {
    if offset_bps == 0 || mark_ticks == 0 || tick_size == 0 {
        return None;
    }
    let offset_ticks: u128 =
        (mark_ticks as u128).saturating_mul(offset_bps as u128) / BPS_DENOM as u128;

    let (new_anchor, raw_trigger): (u64, i128) = if kind == 0 {
        // Long-side SL: ratchet up only.
        if prev_anchor_ticks != 0 && mark_ticks <= prev_anchor_ticks {
            return None; // no progress
        }
        (mark_ticks, (mark_ticks as i128) - (offset_ticks as i128))
    } else {
        // Short-side SL: ratchet down only.
        if prev_anchor_ticks != 0 && mark_ticks >= prev_anchor_ticks {
            return None; // no progress
        }
        (mark_ticks, (mark_ticks as i128) + (offset_ticks as i128))
    };

    // Clamp to at least one tick, then align conservatively.
    let clamped = if raw_trigger < tick_size as i128 {
        tick_size as i128
    } else {
        raw_trigger
    };
    let unsigned = clamped as u128;
    let aligned: u64 = if kind == 0 {
        // Ceil — keep the long SL tighter (fires earlier on a drop).
        let floored = (unsigned / tick_size as u128) * tick_size as u128;
        let ceiled = floored.saturating_add(if unsigned % tick_size as u128 != 0 {
            tick_size as u128
        } else {
            0
        });
        if ceiled > u64::MAX as u128 { u64::MAX } else { ceiled as u64 }
    } else {
        // Floor — keep the short SL tighter (fires earlier on a rally).
        let floored = (unsigned / tick_size as u128) * tick_size as u128;
        if floored > u64::MAX as u128 { u64::MAX } else { floored as u64 }
    };

    // Sub-tick no-op: mark moved but the aligned trigger + anchor didn't change.
    if aligned == current_trigger_ticks && new_anchor == prev_anchor_ticks {
        return None;
    }
    Some((new_anchor, aligned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratchet_long_advances_up_and_ceils() {
        // mark 1000, 5% offset → offset 50; trigger 950, anchor 1000.
        assert_eq!(ratchet(0, 1_000, 500, 0, 1, 0), Some((1_000, 950)));
        // mark rises to 1200 → trigger 1140, anchor 1200.
        assert_eq!(ratchet(0, 1_200, 500, 1_000, 1, 950), Some((1_200, 1_140)));
        // mark drops below anchor → no progress.
        assert_eq!(ratchet(0, 1_100, 500, 1_200, 1, 1_140), None);
    }

    #[test]
    fn ratchet_long_ceils_to_tick() {
        // mark 1000, 333 bps → offset = 33 (floor of 33.3); trigger 967; tick 10
        // → ceil to 970 (tighter SL).
        assert_eq!(ratchet(0, 1_000, 333, 0, 10, 0), Some((1_000, 970)));
    }

    #[test]
    fn ratchet_short_advances_down_and_floors() {
        // mark 1000, 5% offset → trigger 1050, anchor 1000.
        assert_eq!(ratchet(1, 1_000, 500, 0, 1, 0), Some((1_000, 1_050)));
        // mark falls to 800 → trigger 840, anchor 800.
        assert_eq!(ratchet(1, 800, 500, 1_000, 1, 1_050), Some((800, 840)));
        // mark rises above anchor → no progress.
        assert_eq!(ratchet(1, 900, 500, 800, 1, 840), None);
    }

    #[test]
    fn ratchet_short_floors_to_tick() {
        // mark 1000, 333 bps → offset 33; trigger 1033; tick 10 → floor 1030.
        assert_eq!(ratchet(1, 1_000, 333, 0, 10, 0), Some((1_000, 1_030)));
    }

    #[test]
    fn ratchet_rejects_non_trailing_and_zero_inputs() {
        assert_eq!(ratchet(0, 1_000, 0, 0, 1, 0), None, "offset 0 = not trailing");
        assert_eq!(ratchet(0, 0, 500, 0, 1, 0), None, "zero mark");
        assert_eq!(ratchet(0, 1_000, 500, 0, 0, 0), None, "zero tick");
    }

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
