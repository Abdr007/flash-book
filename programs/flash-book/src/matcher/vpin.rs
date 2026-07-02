//! VPIN — Volume-synchronized Probability of Informed Trading.
//!
//! ⚠️ AUDIT M-11 (2026-07): THIS MODULE IS INERT IN PRODUCTION. `record_fill`
//! has NO on-chain caller (it is never invoked from `apply_fill` or anywhere
//! else), so a market's `VpinState` never advances past `default()` and
//! `as_bps()` is always 0. Consequently the VPIN-scaled toxicity tax in
//! `apply_fill` / `apply_flp_fill` NEVER fires. The `MarketAccount.vpin` field
//! is retained only because removing an on-chain field is a state migration;
//! do NOT treat VPIN or the toxicity tax as an active protection. To activate:
//! call `record_fill` on every fill and migrate the (already-present) state.
//!
//! Pure-integer fixed-point Q32.32. Each fill records a (side, size). When
//! cumulative volume crosses `bucket_size`, we compute imbalance and update
//! the EMA. The EMA value is the "VPIN" — a number in [0, Q32.32_one]
//! representing toxicity.

use super::order::Side;
use crate::constants::{VPIN_FIXED_ONE, VPIN_FRACTIONAL_BITS};
use crate::errors::OrOverflow;
use anchor_lang::prelude::*;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, AnchorSerialize, AnchorDeserialize, Default,
)]
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

    /// Record a fill. Returns Ok if state was advanced; bucket-close events
    /// are computed internally (no return needed).
    pub fn record_fill(
        &mut self,
        taker_side: Side,
        size: u64,
        bucket_size: u64,
        ema_window: u32,
    ) -> Result<()> {
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

        // Close as many full buckets as fit in the pending volume.
        loop {
            let total = self.buy_pending.saturating_add(self.sell_pending);
            if total < bucket_size {
                break;
            }
            // Proportional carve-out.
            let buy_chunk = (self.buy_pending as u128 * bucket_size as u128 / total as u128) as u64;
            let sell_chunk = bucket_size.saturating_sub(buy_chunk);

            // imbalance = |buy - sell| / bucket_size, expressed in Q32.32.
            let abs_diff = buy_chunk.abs_diff(sell_chunk);
            // imbalance_q = abs_diff * FIXED_ONE / bucket_size
            let imbalance_q = (abs_diff as u128)
                .checked_mul(VPIN_FIXED_ONE as u128)
                .or_overflow()?
                .checked_div(bucket_size as u128)
                .or_div_zero()? as u64;

            // EMA: alpha = 2 / (window + 1), in Q32.32 → alpha_q = 2*FIXED_ONE / (window+1)
            let alpha_q = if ema_window <= 1 {
                VPIN_FIXED_ONE
            } else {
                (2u128 * VPIN_FIXED_ONE as u128 / (ema_window as u128 + 1)) as u64
            };
            // value = value*(1-alpha) + sample*alpha
            let one_minus_alpha = VPIN_FIXED_ONE - alpha_q;
            let part_old = (self.value_q32_32 as u128 * one_minus_alpha as u128) >> VPIN_FRACTIONAL_BITS;
            let part_new = (imbalance_q as u128 * alpha_q as u128) >> VPIN_FRACTIONAL_BITS;
            self.value_q32_32 = (part_old + part_new) as u64;

            self.buy_pending = self.buy_pending.saturating_sub(buy_chunk);
            self.sell_pending = self.sell_pending.saturating_sub(sell_chunk);
            self.buckets_observed = self.buckets_observed.saturating_add(1);
        }
        Ok(())
    }

    /// VPIN as a value in [0, 10_000] bps for use in spread calculations.
    pub fn as_bps(self) -> u32 {
        // bps = value_q32_32 * 10_000 / FIXED_ONE
        let scaled = (self.value_q32_32 as u128 * 10_000) >> VPIN_FRACTIONAL_BITS;
        scaled.min(10_000) as u32
    }
}
