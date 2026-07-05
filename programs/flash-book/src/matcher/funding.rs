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

use crate::constants::FUNDING_INDEX_FRACTIONAL_BITS;
use crate::errors::OrOverflow;
use anchor_lang::prelude::*;

/// Q64.64 fixed-point cumulative funding index (signed).
pub type FundingIndex = i128;

pub const FUNDING_INDEX_ONE: i128 = 1i128 << FUNDING_INDEX_FRACTIONAL_BITS;

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
