//! VPIN — Volume-synchronized Probability of Informed Trading.
//!
//! Pure-integer fixed-point Q32.32. Each fill records a (side, size). When
//! cumulative volume crosses `bucket_size`, we compute imbalance and update the
//! EMA. The EMA value is the "VPIN" — a number in `[0, FIXED_ONE]` representing
//! toxicity; `as_bps()` projects it onto `[0, 10_000]`.
//!
//! De-anchored port of `matcher/vpin.rs`. The `record_fill`/`as_bps` math is
//! verbatim; only the framework coupling is replaced: anchor derives → plain
//! struct, `Side` is a local enum, and the `OrOverflow` helper returns a local
//! `VpinError` (so the module is pure + host-testable).

use crate::constants::{VPIN_FIXED_ONE, VPIN_FRACTIONAL_BITS};

/// Taker side. 0 = long/buy, 1 = short/sell — mirrors `matcher::order::Side`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Long,
    Short,
}

/// Arithmetic failure in `record_fill` (overflow / divide-by-zero). The Anchor
/// version maps these to `FlashBookError::{ArithmeticOverflow,DivisionByZero}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpinError {
    Overflow,
    DivByZero,
}

type VResult<T> = core::result::Result<T, VpinError>;

trait OrOverflow<T> {
    fn or_overflow(self) -> VResult<T>;
    fn or_div_zero(self) -> VResult<T>;
}
impl<T> OrOverflow<T> for Option<T> {
    #[inline]
    fn or_overflow(self) -> VResult<T> {
        self.ok_or(VpinError::Overflow)
    }
    #[inline]
    fn or_div_zero(self) -> VResult<T> {
        self.ok_or(VpinError::DivByZero)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VpinState {
    /// Pending buy volume in current open bucket.
    pub buy_pending: u64,
    /// Pending sell volume in current open bucket.
    pub sell_pending: u64,
    /// Number of buckets observed so far.
    pub buckets_observed: u64,
    /// Q32.32 fixed-point VPIN value (0 = balanced, FIXED_ONE = max imbalance).
    pub value_q32_32: u64,
}

impl VpinState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a fill, closing as many full buckets as the pending volume allows
    /// and advancing the EMA. No-op for zero size / zero bucket.
    pub fn record_fill(
        &mut self,
        taker_side: Side,
        size: u64,
        bucket_size: u64,
        ema_window: u32,
    ) -> VResult<()> {
        if size == 0 || bucket_size == 0 {
            return Ok(());
        }
        match taker_side {
            Side::Long => {
                self.buy_pending = self.buy_pending.checked_add(size).or_overflow()?;
            }
            Side::Short => {
                self.sell_pending = self.sell_pending.checked_add(size).or_overflow()?;
            }
        }

        loop {
            let total = self.buy_pending.saturating_add(self.sell_pending);
            if total < bucket_size {
                break;
            }
            let buy_chunk = (self.buy_pending as u128 * bucket_size as u128 / total as u128) as u64;
            let sell_chunk = bucket_size.saturating_sub(buy_chunk);

            let abs_diff = buy_chunk.abs_diff(sell_chunk);
            let imbalance_q = (abs_diff as u128)
                .checked_mul(VPIN_FIXED_ONE as u128)
                .or_overflow()?
                .checked_div(bucket_size as u128)
                .or_div_zero()? as u64;

            let alpha_q = if ema_window <= 1 {
                VPIN_FIXED_ONE
            } else {
                (2u128 * VPIN_FIXED_ONE as u128 / (ema_window as u128 + 1)) as u64
            };
            let one_minus_alpha = VPIN_FIXED_ONE - alpha_q;
            let part_old =
                (self.value_q32_32 as u128 * one_minus_alpha as u128) >> VPIN_FRACTIONAL_BITS;
            let part_new = (imbalance_q as u128 * alpha_q as u128) >> VPIN_FRACTIONAL_BITS;
            self.value_q32_32 = (part_old + part_new) as u64;

            self.buy_pending = self.buy_pending.saturating_sub(buy_chunk);
            self.sell_pending = self.sell_pending.saturating_sub(sell_chunk);
            self.buckets_observed = self.buckets_observed.saturating_add(1);
        }
        Ok(())
    }

    /// VPIN as a value in `[0, 10_000]` bps (for spread / toxicity-tax math).
    pub fn as_bps(self) -> u32 {
        let scaled = (self.value_q32_32 as u128 * 10_000) >> VPIN_FRACTIONAL_BITS;
        scaled.min(10_000) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_is_balanced() {
        let v = VpinState::new();
        assert_eq!(v.as_bps(), 0);
        assert_eq!(v.buckets_observed, 0);
    }

    #[test]
    fn zero_size_or_bucket_is_noop() {
        let mut v = VpinState::new();
        v.record_fill(Side::Long, 0, 100, 10).unwrap();
        v.record_fill(Side::Long, 100, 0, 10).unwrap();
        assert_eq!(v.buckets_observed, 0);
        assert_eq!(v.buy_pending, 0);
    }

    #[test]
    fn pending_accumulates_below_bucket() {
        let mut v = VpinState::new();
        v.record_fill(Side::Long, 40, 100, 10).unwrap();
        assert_eq!(v.buy_pending, 40);
        assert_eq!(v.buckets_observed, 0); // no bucket closed yet
    }

    #[test]
    fn fully_one_sided_bucket_is_max_imbalance() {
        let mut v = VpinState::new();
        // window 1 → alpha = FIXED_ONE → value jumps straight to the sample.
        v.record_fill(Side::Long, 100, 100, 1).unwrap();
        assert_eq!(v.buckets_observed, 1);
        // all-buy bucket → imbalance == 1.0 → 10_000 bps.
        assert_eq!(v.as_bps(), 10_000);
    }

    #[test]
    fn balanced_bucket_is_zero_imbalance() {
        let mut v = VpinState::new();
        v.record_fill(Side::Long, 50, 100, 1).unwrap();
        v.record_fill(Side::Short, 50, 100, 1).unwrap();
        assert_eq!(v.buckets_observed, 1);
        assert_eq!(v.as_bps(), 0); // perfectly balanced
    }

    #[test]
    fn as_bps_clamped_to_10000() {
        let mut v = VpinState::new();
        v.value_q32_32 = u64::MAX; // beyond FIXED_ONE
        assert_eq!(v.as_bps(), 10_000);
    }
}
