//! Cumulative funding index — fixed-point Q64.64.
//!
//! A position's funding charge at settlement is
//! sign·notional·(I_now − I_entry). The index itself is never advanced by
//! any instruction (no on-chain path moves it off zero), so funding is
//! economically inert until a rate driver ships; the settlement-side charge
//! math below is live and covered so that wiring a driver cannot change
//! settlement semantics.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
