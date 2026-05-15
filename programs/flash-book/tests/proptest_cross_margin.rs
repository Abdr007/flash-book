//! Property tests for cross-margin asset weights (Wave 60).

use flash_book::matcher::cross_margin_weights::{isqrt, joint_margin};
use proptest::prelude::*;

proptest! {
    /// isqrt is monotone: bigger n → bigger or equal sqrt.
    #[test]
    fn isqrt_monotone(a in 0u128..1_000_000_000_000, b_delta in 0u128..1_000_000) {
        let b = a.saturating_add(b_delta);
        prop_assert!(isqrt(b) >= isqrt(a));
    }

    /// isqrt(n²) == n.
    #[test]
    fn isqrt_perfect_squares(n in 0u128..1_000_000) {
        prop_assert_eq!(isqrt(n * n), n);
    }

    /// Bounded: floor(sqrt(n)) ≤ sqrt(n) < floor(sqrt(n)) + 1.
    #[test]
    fn isqrt_bounded(n in 0u128..1_000_000_000_000) {
        let r = isqrt(n);
        prop_assert!(r * r <= n);
        prop_assert!((r + 1).saturating_mul(r + 1) > n);
    }

    /// joint_margin is symmetric in m1, m2.
    #[test]
    fn joint_margin_symmetric(
        m1 in 0u128..1_000_000,
        m2 in 0u128..1_000_000,
        correlation_bps in -10_000i32..=10_000,
    ) {
        let a = joint_margin(m1, m2, correlation_bps);
        let b = joint_margin(m2, m1, correlation_bps);
        prop_assert_eq!(a, b);
    }

    /// Zero correlation: joint margin equals sqrt(m1² + m2²).
    #[test]
    fn joint_margin_zero_correlation_pythagorean(
        m1 in 0u128..10_000,
        m2 in 0u128..10_000,
    ) {
        let r = joint_margin(m1, m2, 0);
        let expected = isqrt(m1 * m1 + m2 * m2);
        prop_assert_eq!(r, expected);
    }

    /// Negative correlation → joint margin ≤ uncorrelated.
    /// Positive correlation → joint margin ≥ uncorrelated.
    #[test]
    fn joint_margin_correlation_direction(
        m1 in 1u128..10_000,
        m2 in 1u128..10_000,
        rho_bps in 1i32..10_000,
    ) {
        let zero = joint_margin(m1, m2, 0);
        let positive = joint_margin(m1, m2, rho_bps);
        let negative = joint_margin(m1, m2, -rho_bps);
        prop_assert!(positive >= zero, "positive correlation amplifies");
        prop_assert!(negative <= zero, "negative correlation hedges");
    }

    /// Joint margin always ≤ sum of individual margins.
    #[test]
    fn joint_margin_bounded_by_sum(
        m1 in 0u128..100_000,
        m2 in 0u128..100_000,
        rho_bps in -10_000i32..=10_000,
    ) {
        let r = joint_margin(m1, m2, rho_bps);
        prop_assert!(r <= m1 + m2 + 1, "joint can't exceed sum (modulo 1 for rounding)");
    }
}
