//! Cumulative funding index — fixed-point Q64.64.
//!
//! A position's funding charge at settlement is
//! sign·notional·(I_now − I_entry). The index is advanced by the live,
//! permissionless `crank_funding` instruction (rate-limited, oracle-gated;
//! see `funding_index_delta`), and the settlement-side charge math below
//! applies it. Both sides — the rate driver and the charge — are covered by
//! the proofs/tests in this module.
//!
//! The index is an i128 in Q64.64: ±2^63 in the integer part, which no
//! realistic rate can overflow.

use crate::constants::{BPS_DENOM, FUNDING_INDEX_FRACTIONAL_BITS};
use crate::errors::OrOverflow;
use anchor_lang::prelude::*;

/// Q64.64 fixed-point cumulative funding index (signed).
pub type FundingIndex = i128;

pub const FUNDING_INDEX_ONE: i128 = 1i128 << FUNDING_INDEX_FRACTIONAL_BITS;

/// One funding crank tick: the Q64.64 index delta to add to
/// `market.cum_funding_index`, and the clamped rate to stamp. PURE and fully
/// gated so a permissionless caller can never drive value beyond the bounds:
///
///   * `dt_seconds` is clamped to `period_seconds` (≥ 1) — a stale crank cannot
///     apply the rate over an unbounded Δt.
///   * the rate `(mark − oracle)·k / oracle` is clamped to `±rate_max_bps_per_sec`
///     — the premium can never push funding past the market's cap.
///   * `oracle == 0` (no anchor) or `dt == 0` (same-second / idempotent) accrues
///     exactly nothing.
///   * every multiply/divide is checked, so extreme params fail closed rather
///     than wrapping the index.
///
/// Returns `(delta_index, rate_bps_per_sec)`. The crank only advances the index;
/// value moves later, per position, through the Kani-proven `route_funding`
/// settle path (Δcollateral == −Δresidual), so this tick mints nothing.
pub fn funding_index_delta(
    mark_ticks: u64,
    oracle_ticks: u64,
    dt_seconds: u64,
    k_bps: u32,
    rate_max_bps_per_sec: u32,
    period_seconds: u32,
) -> Result<(i128, i64)> {
    if oracle_ticks == 0 || dt_seconds == 0 {
        return Ok((0, 0));
    }
    let dt = dt_seconds.min(period_seconds.max(1) as u64);
    let premium = (mark_ticks as i128) - (oracle_ticks as i128);
    let rate_max = rate_max_bps_per_sec as i128;
    let rate = premium
        .checked_mul(k_bps as i128)
        .or_overflow()?
        .checked_div(oracle_ticks as i128)
        .or_div_zero()?
        .clamp(-rate_max, rate_max);
    let delta = rate
        .checked_mul(dt as i128)
        .or_overflow()?
        .checked_mul(FUNDING_INDEX_ONE)
        .or_overflow()?
        .checked_div(BPS_DENOM as i128)
        .or_div_zero()?;
    Ok((delta, rate as i64))
}

/// One funding crank tick with **dt-weighted premium smoothing** (audit MED: no TWAP →
/// a momentary band-edge premium stamps a full-period tick). Computes the instantaneous
/// rate exactly as `funding_index_delta`, then blends it into the PRIOR stamped rate with
/// a time-weighted EMA so a momentary spike at a rapid crank barely moves the rate — only
/// a premium SUSTAINED over ~the TWAP window converges it. This is why a bare min-interval
/// is the wrong fix (a larger Δt would weight a momentary edge premium MORE, not less);
/// the dt-weighted blend does the opposite.
///
///   w = min(1, dt / (twap_window_periods · period))    (w = 1 ⇒ no smoothing)
///   smoothed = prev + (instant − prev) · w
///
/// `twap_window_periods == 0` disables smoothing — byte-identical to `funding_index_delta`.
/// Returns `(delta_index, smoothed_rate_bps_per_sec)`; the caller stores the smoothed rate
/// as the new `last_funding_rate_bps_per_sec` (its EMA state, advanced every crank).
///
/// The smoothed rate is a convex combination of two `[-rate_max, rate_max]` values and is
/// additionally clamped, so the market's rate cap is preserved unconditionally.
#[allow(clippy::too_many_arguments)]
pub fn funding_index_delta_smoothed(
    mark_ticks: u64,
    oracle_ticks: u64,
    dt_seconds: u64,
    k_bps: u32,
    rate_max_bps_per_sec: u32,
    period_seconds: u32,
    prev_rate_bps_per_sec: i64,
    twap_window_periods: u8,
) -> Result<(i128, i64)> {
    // No anchor / same-second crank ⇒ accrue nothing; keep the prior smoothed rate.
    if oracle_ticks == 0 || dt_seconds == 0 {
        return Ok((0, prev_rate_bps_per_sec));
    }
    let dt = dt_seconds.min(period_seconds.max(1) as u64);
    let premium = (mark_ticks as i128) - (oracle_ticks as i128);
    let rate_max = rate_max_bps_per_sec as i128;
    let instant_rate = premium
        .checked_mul(k_bps as i128)
        .or_overflow()?
        .checked_div(oracle_ticks as i128)
        .or_div_zero()?
        .clamp(-rate_max, rate_max);
    // dt-weighted EMA toward the instant rate. window_seconds == 0 (window param 0) or a
    // Δt ≥ the window ⇒ w = 1 (no smoothing / full catch-up on a long gap).
    let window_seconds = (twap_window_periods as u64).saturating_mul(period_seconds.max(1) as u64);
    let smoothed_rate: i128 = if window_seconds == 0 || dt >= window_seconds {
        instant_rate
    } else {
        let prev = prev_rate_bps_per_sec as i128;
        let diff = instant_rate - prev;
        prev + diff
            .checked_mul(dt as i128)
            .or_overflow()?
            .checked_div(window_seconds as i128)
            .or_div_zero()?
    }
    // Defensive: hold the rate cap even if `prev` predates a rate_max reduction.
    .clamp(-rate_max, rate_max);
    let delta = smoothed_rate
        .checked_mul(dt as i128)
        .or_overflow()?
        .checked_mul(FUNDING_INDEX_ONE)
        .or_overflow()?
        .checked_div(BPS_DENOM as i128)
        .or_div_zero()?;
    Ok((delta, smoothed_rate as i64))
}

/// Per-period funding backstop. Clamp a single crank's funding-index `delta`
/// so the cumulative funding charged over one full `period_seconds` cannot
/// exceed `per_period_max_bps` of a position's notional. The cap is pro-rated
/// by `dt / period` (dt is already ≤ period at the crank), so several short
/// cranks inside one period together stay under the full-period bound. A zero
/// cap disables the backstop (unbounded). This bounds the worst-case value
/// transfer per period even when the per-second rate cap is loose — the
/// guarantee a permissionless market's untrusted funding params rely on.
#[inline]
pub fn clamp_delta_to_period_cap(
    delta: i128,
    per_period_max_bps: u32,
    dt_seconds: u64,
    period_seconds: u32,
) -> i128 {
    if per_period_max_bps == 0 {
        return delta;
    }
    let period = period_seconds.max(1) as i128;
    let dt = dt_seconds.min(period_seconds.max(1) as u64) as i128;
    // cap = per_period_max_bps · ONE · dt / (BPS · period). Saturating multiplies
    // so an extreme cap only ever widens the bound, never wraps it tight/negative.
    let cap = (per_period_max_bps as i128)
        .saturating_mul(FUNDING_INDEX_ONE)
        .saturating_mul(dt)
        / (BPS_DENOM as i128 * period);
    clamp_to_symmetric_bound(delta, cap)
}

/// Symmetric clamp to a non-negative bound: returns `delta` confined to
/// `[-|bound|, |bound|]`. `bound` is taken as its magnitude (`max(0)`), so this can
/// NEVER panic on an inverted `min > max` regardless of sign — panic-freedom is
/// structural, not a fact about the caller's cap. Factored out of
/// [`clamp_delta_to_period_cap`] so the per-period bound property is machine-
/// provable (Kani) over a SYMBOLIC bound: the full function derives `cap` via an
/// i128 divide by `BPS_DENOM · period` with a symbolic `period`, and CBMC cannot
/// discharge a symbolic 128-bit divider in a practical bound (the same limitation
/// documented for `incremental_im` / the OI breaker). The division-dependent cap
/// magnitude is instead pinned by the host proptest
/// `clamp_delta_to_period_cap_never_exceeds_bound`. Division-free, total.
#[inline]
pub fn clamp_to_symmetric_bound(delta: i128, bound: i128) -> i128 {
    let b = bound.max(0);
    delta.clamp(-b, b)
}

#[cfg(kani)]
#[kani::proof]
fn clamp_to_symmetric_bound_is_bounded_and_panic_free() {
    let delta: i128 = kani::any();
    let bound: i128 = kani::any();
    // Whole i128 domain over both inputs — no division, so CBMC discharges it fast.
    let out = clamp_to_symmetric_bound(delta, bound);
    let b = bound.max(0);
    // The clamped delta is confined to the symmetric bound in either direction,
    // and never panics (a negative `bound` is treated as 0, not an inverted clamp).
    assert!(out <= b && out >= -b);
    // Idempotent: a value already within the bound is unchanged.
    if delta >= -b && delta <= b {
        assert!(out == delta);
    }
}

#[cfg(kani)]
#[kani::proof]
fn clamp_delta_to_period_cap_zero_is_identity() {
    // The division-free contract of the full function: a zero cap opts out
    // (returns delta unchanged). `per_period_max_bps == 0` short-circuits BEFORE
    // the i128 divide, so this stays fast. Whole domain over delta/dt/period.
    let delta: i128 = kani::any();
    let dt: u64 = kani::any();
    let period: u32 = kani::any();
    assert!(clamp_delta_to_period_cap(delta, 0, dt, period) == delta);
}

/// Funding owed by a position since last settlement. Returns signed Q-units
/// of quote-lots (positive = trader owes).
///
/// `notional_quote_lots` is the position's notional in quote-lots
/// (size × price × tick_size_factor).
pub fn funding_owed(
    is_long: bool,
    notional_quote_lots: u64,
    cum_index_now: FundingIndex,
    cum_index_at_entry: FundingIndex,
) -> Result<i128> {
    let delta = cum_index_now
        .checked_sub(cum_index_at_entry)
        .or_underflow()?;
    let sign: i128 = if is_long { 1 } else { -1 };
    // owed = sign * notional * delta / 2^64  (Q64.64 → linear). The arithmetic
    // right shift rounds toward -infinity, so `scaled` under-states a positive
    // charge and over-states (in magnitude) a negative one by at most one
    // quote-lot. Settlement moves collateral and the Residual bucket by this
    // same equal-and-opposite amount, so the dust is a transfer direction,
    // never a mint (see the truncation-direction tests below).
    let prod = (notional_quote_lots as i128)
        .checked_mul(delta)
        .or_overflow()?;
    let scaled = prod >> FUNDING_INDEX_FRACTIONAL_BITS;
    Ok(sign * scaled)
}

/// Median of three `u64` (branch-free, overflow-free): drops the min and max,
/// leaving the middle value.
#[inline]
pub fn median3(a: u64, b: u64, c: u64) -> u64 {
    a.max(b).min(a.max(c)).min(b.max(c))
}

/// Robust funding/display mark (roadmap 4.7). The median of three sources —
/// `mark` (the oracle-band-clamped fill EMA, i.e. the oracle+EMA blend), `oracle`
/// (the external reference), and the on-book `mid` — so a manipulated or thin-book
/// mid is out-voted by the two oracle-anchored sources, and a lagging oracle is
/// out-voted by the mark+mid. Used ONLY for funding and display.
///
/// SAFETY: liquidation/health MUST NOT call this — it keeps the strict
/// `worse_of(mark, oracle)` (never the median), so a benign median can never soften
/// a liquidation. When a proper three-source median can't be formed (no two-sided
/// book mid, or an unset oracle/mark), this falls back to `mark` — the pre-4.7
/// funding input — so behaviour is unchanged rather than pulled toward a zero source.
#[inline]
pub fn robust_median_mark(mark_ticks: u64, oracle_ticks: u64, book_mid_ticks: Option<u64>) -> u64 {
    match book_mid_ticks {
        Some(mid) if mid > 0 && oracle_ticks > 0 && mark_ticks > 0 => {
            median3(mark_ticks, oracle_ticks, mid)
        }
        _ => mark_ticks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median3_returns_the_middle() {
        assert_eq!(median3(1, 2, 3), 2);
        assert_eq!(median3(3, 1, 2), 2);
        assert_eq!(median3(2, 3, 1), 2);
        assert_eq!(median3(5, 5, 1), 5); // ties
        assert_eq!(median3(1, 5, 5), 5);
        assert_eq!(median3(7, 7, 7), 7);
        assert_eq!(median3(0, u64::MAX, 100), 100); // no overflow across the full range
    }

    #[test]
    fn robust_median_mark_uses_median_when_all_three_present() {
        // book mid is the outlier ⇒ out-voted by mark+oracle (median = the middle).
        assert_eq!(robust_median_mark(100, 102, Some(500)), 102);
        assert_eq!(robust_median_mark(100, 102, Some(50)), 100);
        // book mid between the two oracle-anchored sources ⇒ it IS the median.
        assert_eq!(robust_median_mark(100, 104, Some(101)), 101);
    }

    #[test]
    fn robust_median_mark_falls_back_to_mark_without_a_full_triplet() {
        // No two-sided book mid ⇒ keep the pre-4.7 funding input (the mark).
        assert_eq!(robust_median_mark(100, 102, None), 100);
        // A zero source is never allowed to pull the mark toward 0.
        assert_eq!(robust_median_mark(100, 102, Some(0)), 100);
        assert_eq!(robust_median_mark(100, 0, Some(101)), 100);
        assert_eq!(robust_median_mark(0, 102, Some(101)), 0);
    }

    #[test]
    fn twap_window_zero_is_identical_to_the_instant_delta() {
        // window == 0 disables smoothing ⇒ byte-identical to funding_index_delta.
        for (mark, oracle, dt) in [(110u64, 100u64, 60u64), (100, 110, 30), (100, 100, 60)] {
            let instant = funding_index_delta(mark, oracle, dt, BPS_DENOM, 2000, 3600).unwrap();
            let smoothed = funding_index_delta_smoothed(
                mark, oracle, dt, BPS_DENOM, 2000, 3600, /*prev*/ 12345, /*window*/ 0,
            )
            .unwrap();
            assert_eq!(instant, smoothed, "window=0 must match the instant delta");
        }
    }

    #[test]
    fn a_momentary_spike_barely_moves_the_smoothed_rate() {
        // prev rate 0; a full-cap instant premium at a RAPID crank (dt=1s) over an 8-period
        // (8h) window ⇒ the stamped rate is ~prev, not the spike (the MED fix).
        let rate_max = 1000u32;
        let (_delta, smoothed) = funding_index_delta_smoothed(
            2_000_000, 100, /*dt*/ 1, BPS_DENOM, rate_max, /*period*/ 3600,
            /*prev*/ 0, /*window*/ 8,
        )
        .unwrap();
        // instant would clamp to +rate_max (1000); after dt/window = 1/28800 weighting it is ~0.
        assert!(
            smoothed.unsigned_abs() < 10,
            "a momentary spike must be heavily damped, got {smoothed}"
        );
        // A premium SUSTAINED over many full-period cranks converges toward the cap (each
        // crank's Δt is clamped to the period, so it adds ~1/window of the gap per crank).
        let mut prev = 0i64;
        for _ in 0..40 {
            let (_d2, r) = funding_index_delta_smoothed(
                2_000_000, 100, 3600, BPS_DENOM, rate_max, 3600, prev, 8,
            )
            .unwrap();
            prev = r;
        }
        assert!(
            prev > (rate_max as i64) * 9 / 10 && prev <= rate_max as i64,
            "a sustained premium converges toward the cap, got {prev}"
        );
    }

    #[test]
    fn smoothed_rate_stays_within_the_cap_and_gates_on_no_anchor() {
        let rate_max = 500u32;
        // even with an out-of-band prev, the output is clamped into [-rate_max, rate_max].
        let (_d, r) = funding_index_delta_smoothed(
            9_999_999, 100, 100, BPS_DENOM, rate_max, 3600, /*prev*/ 30_000, 4,
        )
        .unwrap();
        assert!(r.unsigned_abs() <= rate_max as u64);
        // no oracle anchor / zero Δt ⇒ accrue nothing, keep the prior rate.
        assert_eq!(
            funding_index_delta_smoothed(100, 0, 60, BPS_DENOM, rate_max, 3600, 77, 4).unwrap(),
            (0, 77)
        );
        assert_eq!(
            funding_index_delta_smoothed(100, 90, 0, BPS_DENOM, rate_max, 3600, 77, 4).unwrap(),
            (0, 77)
        );
    }

    /// Pins the truncation direction of the Q64.64 -> quote-lot conversion.
    /// The shift floors toward -infinity: a positive charge loses its
    /// sub-quote-lot fraction (trader pays slightly less), a negative charge
    /// keeps a full extra quote-lot of magnitude (trader receives slightly
    /// more). Both directions move collateral and the Residual bucket
    /// equal-and-opposite at settlement, so neither can mint value; this test
    /// exists so wiring a live rate driver cannot silently change the
    /// direction.
    #[test]
    fn truncation_direction_is_floor_toward_negative_infinity() {
        // 1.5 quote-lots owed (positive delta): floors to 1 — trader pays 1.
        let delta_1_5 = FUNDING_INDEX_ONE + FUNDING_INDEX_ONE / 2;
        assert_eq!(funding_owed(true, 1, delta_1_5, 0).unwrap(), 1);
        // -1.5 quote-lots (negative delta): floors to -2 — the long RECEIVES 2.
        assert_eq!(funding_owed(true, 1, -delta_1_5, 0).unwrap(), -2);
        // Short side mirrors through the sign flip applied AFTER the shift:
        // shorts pay 2 on the negative delta and receive 1 on the positive.
        assert_eq!(funding_owed(false, 1, -delta_1_5, 0).unwrap(), 2);
        assert_eq!(funding_owed(false, 1, delta_1_5, 0).unwrap(), -1);
    }

    /// An exact multiple of one quote-lot converts with zero dust, both signs.
    #[test]
    fn exact_amounts_have_no_dust() {
        let delta_3 = 3 * FUNDING_INDEX_ONE;
        assert_eq!(funding_owed(true, 7, delta_3, 0).unwrap(), 21);
        assert_eq!(funding_owed(true, 7, -delta_3, 0).unwrap(), -21);
        assert_eq!(funding_owed(false, 7, delta_3, 0).unwrap(), -21);
    }

    /// Zero delta or zero notional owes exactly zero.
    #[test]
    fn zero_cases() {
        assert_eq!(funding_owed(true, u64::MAX, 5, 5).unwrap(), 0);
        assert_eq!(funding_owed(false, 0, FUNDING_INDEX_ONE, 0).unwrap(), 0);
    }

    // ── funding crank tick (funding_index_delta) ──────────────────────────────
    #[test]
    fn crank_no_anchor_or_no_time_accrues_nothing() {
        assert_eq!(
            funding_index_delta(100, 0, 60, 10_000, 1000, 3600).unwrap(),
            (0, 0)
        );
        assert_eq!(
            funding_index_delta(100, 90, 0, 10_000, 1000, 3600).unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn crank_rate_is_clamped_to_cap_both_signs() {
        // Huge positive premium with k=1x would blow past the cap → clamp to +max.
        let (_, up) = funding_index_delta(1_000_000, 100, 1, BPS_DENOM, 5, 3600).unwrap();
        assert_eq!(up, 5);
        // Huge negative premium → clamp to −max.
        let (_, down) = funding_index_delta(1, 1_000_000, 1, BPS_DENOM, 5, 3600).unwrap();
        assert_eq!(down, -5);
    }

    #[test]
    fn crank_dt_is_clamped_to_one_period() {
        // dt far exceeds the period; the accrual uses the clamped period, so a
        // long-stale crank can't apply the rate over an unbounded interval.
        let (d_huge, _) = funding_index_delta(200, 100, u64::MAX, BPS_DENOM, 100, 60).unwrap();
        let (d_period, _) = funding_index_delta(200, 100, 60, BPS_DENOM, 100, 60).unwrap();
        assert_eq!(d_huge, d_period);
    }

    #[test]
    fn crank_delta_is_exact_for_a_known_tick() {
        // premium = 110−100 = 10; rate = 10·10_000/100 = 1000 bps/sec (≤ cap 2000).
        // delta = 1000 · 60 · 2^64 / 10_000 = 6 · 2^64.
        let (delta, rate) = funding_index_delta(110, 100, 60, BPS_DENOM, 2000, 3600).unwrap();
        assert_eq!(rate, 1000);
        assert_eq!(delta, 6i128 * FUNDING_INDEX_ONE);
    }

    use proptest::prelude::*;
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4000))]
        /// The per-position zero-sum CORE at the FULL i128 value domain: a long and
        /// a short with EQUAL notional and entry index owe exactly equal-and-
        /// opposite funding, so what one side pays the other receives to the
        /// quote-lot. (This is an algebraic identity — both sides compute the same
        /// floored `scaled` and differ only in the trailing sign — exercised here
        /// natively across the whole i128 delta range, which CBMC cannot bit-blast.
        /// Any residue across positions with DIFFERENT notionals/entries is per-
        /// position floor-dust that the settle path routes to the Residual via the
        /// Kani-proven `route_funding`, Δcollateral == −Δresidual.)
        #[test]
        fn funding_owed_long_short_zero_sum(notional in any::<u64>(), delta in any::<i128>()) {
            if let (Ok(long), Ok(short)) = (
                funding_owed(true, notional, delta, 0),
                funding_owed(false, notional, delta, 0),
            ) {
                prop_assert_eq!(long, -short);
            }
        }

        /// The per-period cap clamps every funding step into ±cap (never wider),
        /// where cap = per_period_max_bps · ONE · dt / (BPS · period); a zero cap
        /// is the identity. This is the stateless per-period funding bound.
        #[test]
        fn clamp_delta_to_period_cap_never_exceeds_bound(
            delta in any::<i128>(),
            ppmb in 0u32..=10_000,
            dt in 0u64..=604_800,
            period in 1u32..=604_800,
        ) {
            let out = clamp_delta_to_period_cap(delta, ppmb, dt, period);
            if ppmb == 0 {
                prop_assert_eq!(out, delta);
            } else {
                let d = dt.min(period.max(1) as u64) as i128;
                let cap = (ppmb as i128)
                    .saturating_mul(FUNDING_INDEX_ONE)
                    .saturating_mul(d)
                    / (BPS_DENOM as i128 * period.max(1) as i128);
                prop_assert!(out <= cap && out >= -cap);
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// The crank tick is fully gated and safe: the stamped rate never exceeds the
    /// market's `rate_max` cap, an absent oracle or zero Δt accrues exactly
    /// nothing, and every path is overflow-checked (Ok with bounded output, or a
    /// clean Err — never a wrap/panic). This is the un-griefable guarantee: no
    /// caller-reachable input drives the index past the gated bounds.
    #[kani::proof]
    fn funding_index_delta_is_gated_and_safe() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let dt: u64 = kani::any();
        let k: u32 = kani::any();
        let rate_max: u32 = kani::any();
        let period: u32 = kani::any();
        if let Ok((delta, rate)) = funding_index_delta(mark, oracle, dt, k, rate_max, period) {
            assert!(rate.unsigned_abs() <= rate_max as u64);
            if oracle == 0 || dt == 0 {
                assert!(delta == 0 && rate == 0);
            }
        }
    }

    /// The robust median mark (4.7) never fabricates a value outside its sources:
    /// with a full triplet the output equals one of {mark, oracle, mid} and lies in
    /// [min, max]; otherwise it is exactly the mark. So funding can never be driven by
    /// a mark outside the real source range — the median only ever picks a middle
    /// source, never invents one.
    #[kani::proof]
    fn robust_median_mark_is_bounded_by_its_sources() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let mid: u64 = kani::any();
        let with = robust_median_mark(mark, oracle, Some(mid));
        if mid > 0 && oracle > 0 && mark > 0 {
            let lo = mark.min(oracle).min(mid);
            let hi = mark.max(oracle).max(mid);
            assert!(with >= lo && with <= hi);
            assert!(with == mark || with == oracle || with == mid);
        } else {
            assert!(with == mark);
        }
        assert!(robust_median_mark(mark, oracle, None) == mark);
    }

    /// The dt-weighted smoothed funding tick is as gated as the instant one: the stamped
    /// rate never exceeds the market's `rate_max` (the smoothing can only ever move the
    /// rate BETWEEN two capped values, then it is clamped), an absent oracle or zero Δt
    /// accrues exactly nothing and preserves the prior rate, and every path is
    /// overflow-checked. So no caller-reachable input — not even an out-of-band prior
    /// rate — drives funding past the gated bounds.
    #[kani::proof]
    fn funding_index_delta_smoothed_is_gated_and_safe() {
        let mark: u64 = kani::any();
        let oracle: u64 = kani::any();
        let dt: u64 = kani::any();
        let k: u32 = kani::any();
        let rate_max: u32 = kani::any();
        let period: u32 = kani::any();
        let prev: i64 = kani::any();
        let window: u8 = kani::any();
        if let Ok((delta, rate)) =
            funding_index_delta_smoothed(mark, oracle, dt, k, rate_max, period, prev, window)
        {
            if oracle == 0 || dt == 0 {
                // No anchor / same-second: accrue nothing, preserve the prior rate.
                assert!(delta == 0 && rate == prev);
            } else {
                // Every stamped (funding-affecting) rate is capped.
                assert!(rate.unsigned_abs() <= rate_max as u64);
            }
        }
    }
}
