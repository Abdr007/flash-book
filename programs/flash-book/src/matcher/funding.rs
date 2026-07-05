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
}
