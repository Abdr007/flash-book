//! Borrow fee — GMX V2-style utilization-based fee paid by all open
//! positions to the LP pool (Wave 35).
//!
//! Distinct from funding (which is zero-sum between longs and shorts).
//! Borrow fee is paid by **both** sides and accrues entirely to the
//! FLP. Models the cost of warehouse: every open position is using
//! pool capital that could otherwise be deployed elsewhere.
//!
//! ## Formula
//!
//! ```text
//! reservedUsd     = side_oi × reserve_factor_bps / BPS_DENOM
//! utilization     = reservedUsd / pool_nav         (in [0, 1])
//! borrow_rate     = borrow_factor × utilization^exp
//! ```
//!
//! Per-slot fee = `borrow_rate × dt`. Lazy-accrual via cumulative
//! index, same pattern as funding: each position carries
//! `cum_borrow_factor` snapshot at attach, debited on touch.
//!
//! `exp = 1` (linear) is the simplest. GMX V2 uses 1 or 2; 2 makes
//! crowded markets punitive.
//!
//! Pure module. Wave 35b wires this into apply_fill + position settle.

use crate::constants::BPS_DENOM;

/// Per-slot, per-side borrow-rate denominator. Same Q1e9 scale as
/// funding so we can stack the two without unit conversion.
pub const BORROW_RATE_DEN: u128 = 1_000_000_000;

/// Compute borrow rate per slot for a side given utilization.
///
/// `borrow_factor_e9` is the per-slot saturation rate (at 100%
/// utilization). E.g. `2_800` (2.8e-6 per slot) at 400ms slot ≈
/// 0.024%/hour at full utilization.
///
/// `exp` is the utilization exponent: 1 = linear, 2 = quadratic.
/// Returns a per-slot rate scaled by `BORROW_RATE_DEN`.
pub fn borrow_rate_per_slot_e9(
    side_oi_lots: u64,
    pool_nav_quote_lots: u64,
    reserve_factor_bps: u32,
    borrow_factor_e9: u64,
    exp: u8,
) -> u64 {
    if pool_nav_quote_lots == 0 || borrow_factor_e9 == 0 {
        return 0;
    }
    // reservedUsd = side_oi × reserve_factor_bps / BPS_DENOM (approximating
    // notional in lots ≈ usd since 1 quote lot = 1 USDC micro-unit).
    let reserved = (side_oi_lots as u128)
        .saturating_mul(reserve_factor_bps as u128)
        / (BPS_DENOM as u128);
    if reserved == 0 {
        return 0;
    }
    // utilization in Q1e9: util_e9 = reserved × 1e9 / pool_nav.
    let util_e9 = reserved
        .saturating_mul(BORROW_RATE_DEN)
        .checked_div(pool_nav_quote_lots as u128)
        .unwrap_or(0)
        .min(BORROW_RATE_DEN);

    let util_pow = match exp {
        0 => BORROW_RATE_DEN, // exp=0 → flat factor (no util scaling)
        1 => util_e9,
        2 => util_e9.saturating_mul(util_e9) / BORROW_RATE_DEN,
        _ => {
            // Powers ≥ 3 saturate fast; clamp via repeated squaring.
            let mut p = util_e9;
            for _ in 1..exp {
                p = p.saturating_mul(util_e9) / BORROW_RATE_DEN;
            }
            p
        }
    };

    // rate = borrow_factor × util^exp / BORROW_RATE_DEN.
    let rate = (borrow_factor_e9 as u128)
        .saturating_mul(util_pow)
        .checked_div(BORROW_RATE_DEN)
        .unwrap_or(0);
    rate.min(u64::MAX as u128) as u64
}

/// Advance the per-side cumulative borrow factor by `dt` slots at the
/// current rate. Mirrors funding's cumulative-index pattern.
///
/// `cum_borrow_e9` is the running accumulator; returns the new value.
pub fn advance_cum_borrow_e9(
    cum_borrow_e9: u128,
    rate_per_slot_e9: u64,
    dt_slots: u64,
) -> u128 {
    let delta = (rate_per_slot_e9 as u128).saturating_mul(dt_slots as u128);
    cum_borrow_e9.saturating_add(delta)
}

/// Compute the borrow fee owed by a position whose snapshot was taken
/// at `cum_at_entry`. Fee is **always positive** (longs and shorts
/// both pay).
///
/// fee = notional × (cum_now - cum_at_entry) / BORROW_RATE_DEN
pub fn settle_borrow_fee(
    position_notional_quote_lots: u128,
    cum_borrow_now_e9: u128,
    cum_borrow_at_entry_e9: u128,
) -> u128 {
    let delta = cum_borrow_now_e9.saturating_sub(cum_borrow_at_entry_e9);
    position_notional_quote_lots
        .saturating_mul(delta)
        / BORROW_RATE_DEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_oi_zero_rate() {
        assert_eq!(borrow_rate_per_slot_e9(0, 1_000_000, 8_000, 10_000, 1), 0);
    }

    #[test]
    fn zero_pool_zero_rate() {
        assert_eq!(borrow_rate_per_slot_e9(100_000, 0, 8_000, 10_000, 1), 0);
    }

    #[test]
    fn linear_util_scales_rate() {
        // 50% utilization → 50% of borrow_factor.
        // reserved = 100k × 8000/10000 = 80k, pool = 160k → util = 0.5
        let r = borrow_rate_per_slot_e9(100_000, 160_000, 8_000, 10_000, 1);
        assert_eq!(r, 5_000);
    }

    #[test]
    fn quadratic_util_punishes_crowded() {
        // 50% util → 25% rate (0.5^2 = 0.25).
        let r = borrow_rate_per_slot_e9(100_000, 160_000, 8_000, 10_000, 2);
        assert_eq!(r, 2_500);
    }

    #[test]
    fn util_capped_at_one() {
        // Reserved > pool → util capped at 1.0.
        let r = borrow_rate_per_slot_e9(10_000_000, 100_000, 10_000, 10_000, 1);
        assert_eq!(r, 10_000);
    }

    #[test]
    fn cum_borrow_monotone() {
        let cum0 = 0u128;
        let cum1 = advance_cum_borrow_e9(cum0, 10_000, 100);
        let cum2 = advance_cum_borrow_e9(cum1, 10_000, 100);
        assert!(cum1 > cum0);
        assert!(cum2 > cum1);
        assert_eq!(cum2, 2 * cum1);
    }

    #[test]
    fn settle_fee_is_zero_at_same_index() {
        assert_eq!(settle_borrow_fee(1_000_000, 1_000, 1_000), 0);
    }

    #[test]
    fn settle_fee_proportional_to_notional_and_index_delta() {
        // notional 1M, delta_cum = 1000 e-9, fee = 1M × 1000 / 1e9 = 1.
        assert_eq!(settle_borrow_fee(1_000_000, 1_000, 0), 1);
        // 10× notional → 10× fee.
        assert_eq!(settle_borrow_fee(10_000_000, 1_000, 0), 10);
    }

    #[test]
    fn exp_zero_returns_flat_borrow_factor() {
        // exp=0 → util^0 = 1, rate = borrow_factor regardless of OI.
        let r = borrow_rate_per_slot_e9(100_000, 1_000_000, 8_000, 5_000, 0);
        assert_eq!(r, 5_000);
    }
}
