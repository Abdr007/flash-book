//! Continuous funding via cumulative index — fixed-point Q64.64.
//!
//! Funding integral: ΔI = clamp(K·premium, ±r_max) · Δt, accumulated per
//! ER block. Position charge on settlement: sign·notional·(I_now − I_entry).
//!
//! We represent the index as i128 in Q64.64. Range: ±2^63 in the integer
//! part — at any reasonable rate (≤ 1% per block) this never overflows
//! within the lifetime of the universe.

use super::lot::{BaseLots, Bps, Ticks};
use crate::constants::{BPS_DENOM, FUNDING_INDEX_FRACTIONAL_BITS};
use crate::errors::OrOverflow;
use anchor_lang::prelude::*;

/// Q64.64 fixed-point cumulative funding index (signed).
pub type FundingIndex = i128;

pub const FUNDING_INDEX_ONE: i128 = 1i128 << FUNDING_INDEX_FRACTIONAL_BITS;

#[derive(Debug, Clone, Copy)]
pub struct FundingTick {
    pub index_delta: FundingIndex,
    pub rate_bps_per_sec: i64,
    pub premium_bps: i64,
}

/// Advance the cumulative funding index for a market.
///
/// `mark_ticks` and `oracle_ticks` are in tick space (must use same units).
/// `block_delta_ms` is the time since last advance.
/// `funding_rate_k_bps` is the K coefficient in bps (e.g. 28 for ~1% per hour).
/// `rate_max_bps_per_sec` caps the rate.
pub fn advance(
    cum_index: FundingIndex,
    mark_ticks: Ticks,
    oracle_ticks: Ticks,
    block_delta_ms: u64,
    funding_rate_k_bps: u32,
    rate_max_bps_per_sec: u32,
) -> Result<(FundingIndex, FundingTick)> {
    if oracle_ticks.0 == 0 || block_delta_ms == 0 {
        return Ok((
            cum_index,
            FundingTick {
                index_delta: 0,
                rate_bps_per_sec: 0,
                premium_bps: 0,
            },
        ));
    }

    // premium_bps = (mark - oracle) * 10_000 / oracle  (signed)
    let mark = mark_ticks.0 as i128;
    let oracle = oracle_ticks.0 as i128;
    let diff = mark - oracle;
    let premium_bps_i128 = diff
        .checked_mul(BPS_DENOM as i128)
        .or_overflow()?
        .checked_div(oracle)
        .or_div_zero()?;
    let premium_bps = clamp_i128(premium_bps_i128, i64::MIN as i128, i64::MAX as i128) as i64;

    // rate = K * premium  (in bps · bps space → divide by 10_000)
    let raw_rate_i128 = (funding_rate_k_bps as i128)
        .checked_mul(premium_bps as i128)
        .or_overflow()?
        / (BPS_DENOM as i128);
    let max = rate_max_bps_per_sec as i128;
    let rate_bps_per_sec = clamp_i128(raw_rate_i128, -max, max) as i64;

    // ΔI = rate(bps/sec) * Δt(sec) — converted to Q64.64.
    // rate / 10_000 * (delta_ms / 1000) → use Q64.64 by:
    //   ΔI = (rate * delta_ms / 1000 / 10_000) * 2^64
    //      = (rate * delta_ms * 2^64) / (1000 * 10_000)
    let numer = (rate_bps_per_sec as i128)
        .checked_mul(block_delta_ms as i128)
        .or_overflow()?
        .checked_mul(FUNDING_INDEX_ONE)
        .or_overflow()?;
    let denom = 1000i128 * BPS_DENOM as i128;
    let index_delta = numer.checked_div(denom).or_div_zero()?;

    let new_index = cum_index.checked_add(index_delta).or_overflow()?;
    Ok((
        new_index,
        FundingTick {
            index_delta,
            rate_bps_per_sec,
            premium_bps,
        },
    ))
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
    // owed = sign * notional * delta / 2^64  (Q64.64 → linear)
    let prod = (notional_quote_lots as i128)
        .checked_mul(delta)
        .or_overflow()?;
    let scaled = prod >> FUNDING_INDEX_FRACTIONAL_BITS;
    Ok(sign * scaled)
}

fn clamp_i128(x: i128, lo: i128, hi: i128) -> i128 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

const _: BaseLots = BaseLots(0);
const _: Bps = Bps(0);
