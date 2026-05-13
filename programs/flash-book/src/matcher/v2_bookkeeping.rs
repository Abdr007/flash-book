//! Pure-math helpers used by the matcher and by off-chain consumers
//! that need to mirror the on-chain transforms (vol-adaptive bands,
//! funding-rate smoothing, mark drift checks). Each function captures
//! one idea, takes simple primitive inputs, and is unit-tested in
//! isolation — see this file's `#[cfg(test)]` block.
//!
//! The helpers here are the parts of bookkeeping that can be expressed
//! as pure math, decoupled from MarketAccount / FLP exposure / commit
//! buffer plumbing:
//!
//!   • `vol_adaptive_band_bps`   — vol-scaled oracle band width
//!   • `ema_blend_funding_rate`  — smoothed funding-rate transition

use crate::constants::BPS_DENOM;

/// Vol-adaptive oracle band width in bps. Replaces HL's fixed
/// `oracle ± fixed_pct` bound during the mark-price update.
///
/// Multiplier: `1 + 10 × realized_vol_bps / BPS_DENOM`, capped at 4×.
/// Concretely:
///   • realized_vol_bps =    0 → 1.0× base (calm market, tight band)
///   • realized_vol_bps =  300 → 1.3× base
///   • realized_vol_bps = 1000 → 2.0× base
///   • realized_vol_bps = 3000 → 4.0× base (cap; legitimate vol spike)
///   • realized_vol_bps = 9999 → 4.0× base (still capped)
///
/// Why 4× cap: prevents a runaway-vol scenario from disabling the band
/// entirely. Any move beyond 4× the base-band radius is more likely an
/// adversarial mark push than legitimate price discovery; defer to the
/// downstream `mark_change_max_bps` clamp from there.
pub fn vol_adaptive_band_bps(base_band_bps: u32, realized_vol_bps: u32) -> u32 {
    let vol_mult_num =
        (BPS_DENOM as u128).saturating_add((realized_vol_bps as u128) * 10);
    let band_capped_num = vol_mult_num.min(4 * BPS_DENOM as u128);
    let result =
        (base_band_bps as u128).saturating_mul(band_capped_num) / BPS_DENOM as u128;
    result.min(u32::MAX as u128) as u32
}

/// EMA-blend the new dampened funding rate with the prior posted rate
/// (50/50). Smoother than HL's per-block recompute — single batches of
/// toxic flow can't drive an outlier spike that traders pay before the
/// next batch corrects.
///
/// `is_first_batch` short-circuits to `new_rate` so the very first
/// settlement has no prior history to dilute against.
pub fn ema_blend_funding_rate(prior: i64, new_rate: i64, is_first_batch: bool) -> i64 {
    if is_first_batch {
        new_rate
    } else {
        ((prior as i128 + new_rate as i128) / 2) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vol_adaptive_band_at_zero_vol_is_identity() {
        assert_eq!(vol_adaptive_band_bps(100, 0), 100);
        assert_eq!(vol_adaptive_band_bps(500, 0), 500);
        assert_eq!(vol_adaptive_band_bps(0, 0), 0);
    }

    #[test]
    fn vol_adaptive_band_widens_with_volatility() {
        let base = 100u32;
        let calm = vol_adaptive_band_bps(base, 0);
        let medium = vol_adaptive_band_bps(base, 1000); // ≈2.0×
        let volatile = vol_adaptive_band_bps(base, 2000); // ≈3.0×
        assert!(calm < medium, "calm={} medium={}", calm, medium);
        assert!(medium < volatile, "medium={} volatile={}", medium, volatile);
        // ≈ 1.0× / 2.0× / 3.0× of base.
        assert!(medium >= 199 && medium <= 201, "expected ≈200, got {}", medium);
        assert!(volatile >= 299 && volatile <= 301, "expected ≈300, got {}", volatile);
    }

    #[test]
    fn vol_adaptive_band_caps_at_four_times_base() {
        let base = 100u32;
        let huge_vol = vol_adaptive_band_bps(base, 9999);
        let runaway_vol = vol_adaptive_band_bps(base, u32::MAX);
        // Both must clamp at 4× base = 400.
        assert_eq!(huge_vol, 400);
        assert_eq!(runaway_vol, 400);
    }

    #[test]
    fn vol_adaptive_band_monotone_in_vol() {
        let base = 250u32;
        let mut prev = vol_adaptive_band_bps(base, 0);
        for vol in [50u32, 100, 250, 500, 1000, 2000, 3000, 9999] {
            let now = vol_adaptive_band_bps(base, vol);
            assert!(
                now >= prev,
                "non-monotone: vol={} prev={} now={}",
                vol,
                prev,
                now,
            );
            prev = now;
        }
    }

    #[test]
    fn ema_blend_first_batch_passes_through() {
        assert_eq!(ema_blend_funding_rate(0, 50, true), 50);
        assert_eq!(ema_blend_funding_rate(9999, -25, true), -25);
    }

    #[test]
    fn ema_blend_50_50_average() {
        assert_eq!(ema_blend_funding_rate(100, 200, false), 150);
        assert_eq!(ema_blend_funding_rate(-100, 100, false), 0);
        assert_eq!(ema_blend_funding_rate(50, 50, false), 50);
    }

    #[test]
    fn ema_blend_dampens_spikes() {
        // Prior was tame; a single-batch outlier shouldn't fully propagate.
        let prior = 10i64;
        let spike = 1000i64;
        let blended = ema_blend_funding_rate(prior, spike, false);
        assert!(blended < spike);
        assert_eq!(blended, 505);
    }

    #[test]
    fn ema_blend_no_overflow_at_extremes() {
        // i64::MAX + i64::MAX would overflow i64; helper uses i128 internally.
        let blended = ema_blend_funding_rate(i64::MAX, i64::MAX, false);
        assert_eq!(blended, i64::MAX);
        let blended_neg = ema_blend_funding_rate(i64::MIN, i64::MIN, false);
        assert_eq!(blended_neg, i64::MIN);
    }

    #[test]
    fn ema_blend_opposite_signs_cancel() {
        // Funding flipping sign batch-to-batch should EMA toward zero.
        let blended = ema_blend_funding_rate(500, -500, false);
        assert_eq!(blended, 0);
    }
}
