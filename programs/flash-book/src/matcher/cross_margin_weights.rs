//! Cross-margin asset weights (Wave 60).
//!
//! For traders running offsetting positions across correlated markets
//! (e.g. long BTC + short ETH), the combined collateral requirement is
//! less than the sum of individual requirements — the positions
//! partially hedge.
//!
//! `cross_correlation_bps`: pairwise correlation between two markets
//! (e.g. 7000 = 0.7 for BTC/ETH). Used to reduce the joint margin:
//!
//! ```text
//! joint_margin = sqrt( m1² + m2² + 2 × ρ × m1 × m2 )
//! ```
//!
//! Where ρ = correlation_bps / 10_000.
//!
//! Pure math. Square root computed via integer Newton's method since
//! we can't depend on libm.
//!
//! Wire-in (Wave 60b): assess_margin_unified composes joint margin
//! across cross-set positions instead of summing individual margins.

use crate::constants::BPS_DENOM;

/// Integer square root via Newton's method. Returns floor(sqrt(n)).
pub fn isqrt(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    if n < 4 {
        return 1;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Compute the joint margin requirement for two positions with the
/// given correlation. `m1` and `m2` are individual margin requirements
/// in quote lots; `correlation_bps` is signed (negative for inverse
/// correlation; positive for direct).
///
/// Sign convention: positive correlation = same-direction positions
/// hedge LESS (correlation amplifies joint risk). Negative correlation
/// = opposing positions hedge MORE (joint margin reduced).
///
/// For two HEDGING positions (one long market A, one short market B
/// when A and B are positively correlated), the caller should pass
/// `-correlation` so the hedge math works correctly.
pub fn joint_margin(
    m1: u128,
    m2: u128,
    correlation_bps: i32,
) -> u128 {
    if m1 == 0 {
        return m2;
    }
    if m2 == 0 {
        return m1;
    }
    let m1_sq = m1.saturating_mul(m1);
    let m2_sq = m2.saturating_mul(m2);
    let prod = m1.saturating_mul(m2);
    let cross = if correlation_bps == 0 {
        0i128
    } else {
        // 2 × ρ × m1 × m2 where ρ = correlation / 10_000.
        // Compute via i128 to handle sign.
        let signed_prod = (prod as i128) * 2 * (correlation_bps.unsigned_abs() as i128)
            / (BPS_DENOM as i128);
        if correlation_bps < 0 { -signed_prod } else { signed_prod }
    };
    let sum_signed: i128 = (m1_sq as i128)
        .saturating_add(m2_sq as i128)
        .saturating_add(cross);
    let radicand: u128 = if sum_signed < 0 { 0 } else { sum_signed as u128 };
    isqrt(radicand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isqrt_basic() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(4), 2);
        assert_eq!(isqrt(9), 3);
        assert_eq!(isqrt(100), 10);
        assert_eq!(isqrt(10_000), 100);
    }

    #[test]
    fn isqrt_floors() {
        // sqrt(10) ≈ 3.16 → floor 3.
        assert_eq!(isqrt(10), 3);
        // sqrt(99) ≈ 9.95 → 9.
        assert_eq!(isqrt(99), 9);
    }

    #[test]
    fn zero_correlation_returns_pythagorean_sum() {
        // m1=3, m2=4 → sqrt(9+16) = sqrt(25) = 5.
        assert_eq!(joint_margin(3, 4, 0), 5);
    }

    #[test]
    fn positive_correlation_amplifies_margin() {
        // m1=3, m2=4, ρ=+1.0 → sqrt(9+16+2×1×12) = sqrt(49) = 7.
        // (Effectively m1 + m2 when fully correlated.)
        assert_eq!(joint_margin(3, 4, 10_000), 7);
    }

    #[test]
    fn negative_correlation_reduces_margin() {
        // m1=3, m2=4, ρ=-1.0 → sqrt(9+16-2×1×12) = sqrt(1) = 1.
        // (Effectively |m1 - m2| when fully anti-correlated.)
        assert_eq!(joint_margin(3, 4, -10_000), 1);
    }

    #[test]
    fn partial_negative_correlation() {
        // m1=10, m2=10, ρ=-0.5 → sqrt(100+100-100) = sqrt(100) = 10.
        // Joint margin = max of the two, not sum.
        assert_eq!(joint_margin(10, 10, -5_000), 10);
    }

    #[test]
    fn zero_margin_pass_through() {
        assert_eq!(joint_margin(0, 100, 5_000), 100);
        assert_eq!(joint_margin(100, 0, 5_000), 100);
        assert_eq!(joint_margin(0, 0, 5_000), 0);
    }

    #[test]
    fn realistic_btc_eth_hedge() {
        // 1000 lot long BTC, 1000 lot short ETH, ρ≈0.7 → pass -7000.
        // Individual margins both 50. sqrt(2500+2500-2×0.7×2500)
        //   = sqrt(5000 - 3500) = sqrt(1500) ≈ 38.7 → 38.
        let m = joint_margin(50, 50, -7_000);
        assert!(m < 100, "joint margin must be < sum on hedged trade");
        assert!(m > 25, "but not zero");
        // Sum would be 100. Joint should be ~38.
        assert_eq!(m, 38);
    }
}
