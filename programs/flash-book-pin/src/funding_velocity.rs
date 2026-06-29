//! Funding velocity smoothing — GMX V2-style PID-ish ramp (Wave 37).
//!
//! Stock funding mechanisms recompute the rate every block from a
//! snapshot of OI skew. This oscillates badly around skew flips: a
//! market that's 51% long pays one rate, 49% long pays the opposite,
//! and a market flopping near 50% gets violently noisy funding.
//!
//! Velocity smoothing fixes this by treating the funding rate as
//! **state** that **ramps** toward the skew-implied target at a
//! bounded velocity. Per-slot updates only nudge the rate; persistent
//! skew is what produces high funding, not transient noise.
//!
//! ## State
//!
//! `current_rate_e9` — the present funding rate (what positions pay).
//! `target_rate_e9` — derived from skew each call.
//! `velocity_e9_per_slot` — how fast `current` can move toward `target`.
//! `max_rate_e9` — saturating cap.
//!
//! ## Update
//!
//! ```text
//! step = velocity × dt    (signed, toward target)
//! current += step, clamped to [-max, +max] and not overshooting target
//! ```
//!
//! Pure module. Wire-in lives in `lib.rs::settle_funding` alongside
//! Wave 25b's K/F advance.

/// Compute the new funding rate given the previous rate, the target,
/// the velocity, and the elapsed slot count. Saturating arithmetic.
///
/// Returns the new rate. Caller is responsible for applying global
/// caps (e.g. `max_abs_funding_e9_per_slot` from envelope).
pub fn ramp_rate_e9(
    current_rate_e9: i64,
    target_rate_e9: i64,
    velocity_e9_per_slot: u32,
    dt_slots: u64,
    max_rate_e9: u32,
) -> i64 {
    if dt_slots == 0 || velocity_e9_per_slot == 0 {
        return clamp_to_max(current_rate_e9, max_rate_e9);
    }
    let delta = target_rate_e9.saturating_sub(current_rate_e9);
    if delta == 0 {
        return clamp_to_max(current_rate_e9, max_rate_e9);
    }
    let step_magnitude: i64 = (velocity_e9_per_slot as i64)
        .saturating_mul(dt_slots.min(i64::MAX as u64) as i64);
    let signed_step = if delta > 0 {
        step_magnitude.min(delta)
    } else {
        step_magnitude.saturating_neg().max(delta)
    };
    let new_rate = current_rate_e9.saturating_add(signed_step);
    clamp_to_max(new_rate, max_rate_e9)
}

/// Derive the target funding rate from an OI skew (long_oi - short_oi).
///
/// Positive skew (longs > shorts) → positive rate (longs pay shorts).
/// Negative skew → negative rate.
///
/// `skew_factor_e9_per_unit` is how much funding each lot of skew
/// adds. `denom` is the OI normalizer (typically `long_oi + short_oi`).
pub fn target_rate_from_skew_e9(
    skew_lots: i64,
    denom_lots: u64,
    skew_factor_e9_per_unit: u32,
    max_rate_e9: u32,
) -> i64 {
    if denom_lots == 0 || skew_factor_e9_per_unit == 0 {
        return 0;
    }
    let abs_skew = skew_lots.unsigned_abs() as u128;
    let scaled = abs_skew
        .saturating_mul(skew_factor_e9_per_unit as u128)
        / (denom_lots as u128);
    let rate = scaled.min(max_rate_e9 as u128) as i64;
    if skew_lots < 0 {
        -rate
    } else {
        rate
    }
}

/// The Q64.64 cumulative-funding-index delta for accruing `rate_e9` (per-slot, e9
/// fixed-point) over `dt_slots`. The cumulative rate fraction is `rate_e9·dt/1e9`;
/// scaled into Q64.64 that is `·2^64`, i.e. `ΔI = rate_e9·dt·2^64 / 1e9`. Checked
/// (overflow → `None`). Sign-preserving: a positive rate (long-heavy skew) raises
/// the index so `funding_owed(long)` is positive (longs pay), matching
/// `settle_position_funding`. Pure + host-tested.
#[inline]
pub fn funding_index_delta_q64(rate_e9: i64, dt_slots: u64) -> Option<i128> {
    const Q: i128 = 1i128 << 64;
    const E9: i128 = 1_000_000_000;
    (rate_e9 as i128)
        .checked_mul(dt_slots as i128)?
        .checked_mul(Q)?
        .checked_div(E9)
}

#[inline]
fn clamp_to_max(rate: i64, max: u32) -> i64 {
    let m = max as i64;
    if rate > m {
        m
    } else if rate < -m {
        -m
    } else {
        rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_zero_dt_no_op() {
        assert_eq!(ramp_rate_e9(1_000, 5_000, 100, 0, 10_000), 1_000);
    }

    #[test]
    fn ramp_zero_velocity_no_op() {
        assert_eq!(ramp_rate_e9(1_000, 5_000, 0, 100, 10_000), 1_000);
    }

    #[test]
    fn ramp_moves_toward_target() {
        // velocity 100 × dt 10 = 1000 step. current 1000 → 2000 (still below 5000).
        assert_eq!(ramp_rate_e9(1_000, 5_000, 100, 10, 10_000), 2_000);
    }

    #[test]
    fn ramp_does_not_overshoot() {
        // Target 1500, step would be 2000 → cap at target.
        assert_eq!(ramp_rate_e9(1_000, 1_500, 100, 100, 10_000), 1_500);
    }

    #[test]
    fn ramp_moves_negative_toward_target() {
        assert_eq!(ramp_rate_e9(1_000, -5_000, 100, 10, 10_000), 0);
        assert_eq!(ramp_rate_e9(0, -5_000, 100, 10, 10_000), -1_000);
    }

    #[test]
    fn ramp_clamps_to_max() {
        // step would push to 50_000, but max is 10_000.
        assert_eq!(ramp_rate_e9(0, 100_000, 1_000, 100, 10_000), 10_000);
        assert_eq!(ramp_rate_e9(0, -100_000, 1_000, 100, 10_000), -10_000);
    }

    #[test]
    fn target_from_balanced_skew_is_zero() {
        assert_eq!(target_rate_from_skew_e9(0, 1_000, 100, 10_000), 0);
    }

    #[test]
    fn target_proportional_to_skew() {
        // skew 100 of 1000 OI = 10% → factor 1000 → rate 100.
        assert_eq!(target_rate_from_skew_e9(100, 1_000, 1_000, 10_000), 100);
        // Negative skew → negative rate.
        assert_eq!(target_rate_from_skew_e9(-100, 1_000, 1_000, 10_000), -100);
    }

    #[test]
    fn target_capped_at_max() {
        // Tiny denom, large skew → would exceed max.
        assert_eq!(target_rate_from_skew_e9(100_000, 1, 10_000, 5_000), 5_000);
        assert_eq!(target_rate_from_skew_e9(-100_000, 1, 10_000, 5_000), -5_000);
    }

    #[test]
    fn index_delta_sign_and_scale() {
        const Q: i128 = 1i128 << 64;
        // rate 1e9 (= 1.0/slot) over 1 slot ⇒ ΔI = 1.0 in Q64.64 = 2^64.
        assert_eq!(funding_index_delta_q64(1_000_000_000, 1), Some(Q));
        // half rate over 2 slots ⇒ same 1.0.
        assert_eq!(funding_index_delta_q64(500_000_000, 2), Some(Q));
        // zero rate or zero dt ⇒ no move.
        assert_eq!(funding_index_delta_q64(0, 100), Some(0));
        assert_eq!(funding_index_delta_q64(1_000_000, 0), Some(0));
        // NEGATIVE rate (short-heavy skew) ⇒ index FALLS (shorts pay / longs receive).
        assert_eq!(funding_index_delta_q64(-1_000_000_000, 1), Some(-Q));
        // bounded inputs (cap × max dt) never overflow i128.
        assert!(funding_index_delta_q64(crate::constants::MAX_FUNDING_RATE_E9 as i64, crate::constants::MAX_FUNDING_DT_SLOTS).is_some());
    }

    #[test]
    fn ramp_stable_when_at_target() {
        // current == target → no change.
        assert_eq!(ramp_rate_e9(5_000, 5_000, 100, 100, 10_000), 5_000);
    }

    #[test]
    fn ramp_oscillation_dampened_vs_naive() {
        // Demonstrate that small target flips don't whip the current rate.
        // Naive: target flips ±1 → current flips ±1.
        // Velocity-smoothed: target flips ±1 but velocity 1000 × dt 1 = 1000
        // step → current still pinned near target. Doesn't help here, but
        // showcase: when velocity is small and target oscillates rapidly,
        // current tracks the AVERAGE not the instantaneous.
        let mut current = 0i64;
        let targets = [1_000_i64, -1_000, 1_000, -1_000, 1_000];
        for t in targets {
            // Small velocity × dt = 100 step. Current crawls toward each
            // target but never reaches it before the flip.
            current = ramp_rate_e9(current, t, 10, 10, 10_000);
        }
        // After 5 flips with step=100, current is bounded close to 0.
        assert!(current.abs() < 1_000);
    }
}
