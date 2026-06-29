//! Insurance fund auto-replenish (Wave 38).
//!
//! Self-healing rule: when FLP NAV grows past a configured threshold,
//! route a fraction of accrued fees to the insurance fund until
//! insurance hits its target balance. Keeps insurance solvent without
//! manual operator intervention.
//!
//! Pure math, host-tested. NOTE (re-audit 2026-06-30): this helper is NOT yet
//! wired into `apply_fill` — auto-replenish does not run on the deployed surface
//! (no security impact; insurance is still funded by the per-fill contribution
//! split). Wiring it into the fee-accrual path is a documented follow-up.

use crate::constants::BPS_DENOM;

/// Compute the amount to redirect from FLP fees → insurance fund this
/// settlement window.
///
/// Inputs:
/// - `flp_nav` — current FLP net asset value (quote lots).
/// - `flp_nav_threshold` — replenish only kicks in above this.
/// - `insurance_balance` — current insurance balance.
/// - `insurance_target` — target balance (replenish stops when reached).
/// - `total_fee_accrued` — fees just accrued this window.
/// - `replenish_share_bps` — fraction of fees redirected (e.g. 2000 = 20%).
///
/// Returns the amount to redirect; the remainder stays with FLP.
pub fn compute_replenish_amount(
    flp_nav: u64,
    flp_nav_threshold: u64,
    insurance_balance: u64,
    insurance_target: u64,
    total_fee_accrued: u64,
    replenish_share_bps: u32,
) -> u64 {
    if total_fee_accrued == 0 || replenish_share_bps == 0 {
        return 0;
    }
    if flp_nav < flp_nav_threshold {
        return 0;
    }
    if insurance_balance >= insurance_target {
        return 0;
    }
    // Up to `replenish_share_bps` of the fees.
    let candidate = (total_fee_accrued as u128)
        .saturating_mul(replenish_share_bps as u128)
        / (BPS_DENOM as u128);
    // Bounded by the gap to target.
    let gap = (insurance_target - insurance_balance) as u128;
    (candidate.min(gap)).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fees_no_replenish() {
        assert_eq!(compute_replenish_amount(100, 50, 0, 100, 0, 2_000), 0);
    }

    #[test]
    fn zero_share_no_replenish() {
        assert_eq!(compute_replenish_amount(100, 50, 0, 100, 1_000, 0), 0);
    }

    #[test]
    fn below_flp_threshold_no_replenish() {
        assert_eq!(compute_replenish_amount(40, 50, 0, 100, 1_000, 2_000), 0);
    }

    #[test]
    fn at_or_above_insurance_target_no_replenish() {
        assert_eq!(compute_replenish_amount(100, 50, 100, 100, 1_000, 2_000), 0);
        assert_eq!(compute_replenish_amount(100, 50, 150, 100, 1_000, 2_000), 0);
    }

    #[test]
    fn replenish_at_configured_share() {
        // 20% of 1000 = 200, gap = 100 → take 100.
        assert_eq!(compute_replenish_amount(100, 50, 0, 100, 1_000, 2_000), 100);
        // 20% of 1000 = 200, gap = 1000 → take 200.
        assert_eq!(compute_replenish_amount(100, 50, 0, 1_000, 1_000, 2_000), 200);
    }

    #[test]
    fn replenish_capped_at_gap() {
        // 20% of 10_000 = 2000, gap = 50 → take 50.
        assert_eq!(compute_replenish_amount(100, 50, 50, 100, 10_000, 2_000), 50);
    }

    #[test]
    fn replenish_zero_when_balance_already_above_target() {
        // Insurance overfunded; no further replenish.
        assert_eq!(compute_replenish_amount(1_000, 50, 200, 100, 1_000, 2_000), 0);
    }
}
