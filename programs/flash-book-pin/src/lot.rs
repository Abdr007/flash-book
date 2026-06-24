//! Type-safe wrappers for prices, sizes, and basis points.
//!
//! The matcher operates exclusively on integer lot space — never on
//! floating-point. All conversions are explicit; mixing these types in
//! arithmetic is a compile error unless you go through one of the conversion
//! methods, which check for overflow.
//!
//! De-anchored port of `matcher/lot.rs`: arithmetic is verbatim; anchor derives
//! dropped, `error!(X)` → `X`, errors via `crate::error`.

use crate::error::{FlashBookError, OrOverflow, Result};

/// Base asset size, in base lots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct BaseLots(pub u64);

/// Quote asset size, in quote lots (USDC micro-units).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct QuoteLots(pub u64);

/// Price in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct Ticks(pub u64);

/// Basis points (1 bp = 0.0001). `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(transparent)]
pub struct Bps(pub u32);

impl BaseLots {
    pub const ZERO: Self = Self(0);
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0.checked_add(other.0).map(Self).or_overflow()
    }
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0.checked_sub(other.0).map(Self).or_underflow()
    }
    pub fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }
    pub fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl QuoteLots {
    pub const ZERO: Self = Self(0);
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0.checked_add(other.0).map(Self).or_overflow()
    }
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0.checked_sub(other.0).map(Self).or_underflow()
    }
    pub fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl Ticks {
    pub const ZERO: Self = Self(0);
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0.checked_add(other.0).map(Self).or_overflow()
    }
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0.checked_sub(other.0).map(Self).or_underflow()
    }
}

impl Bps {
    pub const ZERO: Self = Self(0);
    /// Additive tick delta for `price * (bps/10_000)`. u128 intermediate.
    pub fn ticks_delta(self, price: Ticks) -> Result<Ticks> {
        let prod = (price.0 as u128).checked_mul(self.0 as u128).or_overflow()?;
        let delta = prod
            .checked_div(crate::constants::BPS_DENOM as u128)
            .or_div_zero()?;
        if delta > u64::MAX as u128 {
            return Err(FlashBookError::ArithmeticOverflow);
        }
        Ok(Ticks(delta as u64))
    }
}

/// Notional in quote-lots: `base_lots × price_ticks × tick_size`. u128 intermediate.
pub fn notional_quote_lots(
    base: BaseLots,
    price: Ticks,
    tick_size_quote_lots_per_base_lot: u64,
) -> Result<QuoteLots> {
    let m1 = (base.0 as u128).checked_mul(price.0 as u128).or_overflow()?;
    let m2 = m1
        .checked_mul(tick_size_quote_lots_per_base_lot as u128)
        .or_overflow()?;
    if m2 > u64::MAX as u128 {
        return Err(FlashBookError::ArithmeticOverflow);
    }
    Ok(QuoteLots(m2 as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_add_sub_basic() {
        assert_eq!(BaseLots(5).checked_add(BaseLots(7)).unwrap(), BaseLots(12));
        assert_eq!(BaseLots(7).checked_sub(BaseLots(5)).unwrap(), BaseLots(2));
    }

    #[test]
    fn add_overflow_is_error() {
        assert_eq!(
            BaseLots(u64::MAX).checked_add(BaseLots(1)),
            Err(FlashBookError::ArithmeticOverflow)
        );
    }

    #[test]
    fn sub_underflow_is_error() {
        assert_eq!(
            QuoteLots(3).checked_sub(QuoteLots(5)),
            Err(FlashBookError::ArithmeticUnderflow)
        );
    }

    #[test]
    fn ticks_delta_applies_bps() {
        // 10_000 ticks × 50 bps / 10_000 = 50 ticks.
        assert_eq!(Bps(50).ticks_delta(Ticks(10_000)).unwrap(), Ticks(50));
        assert_eq!(Bps(0).ticks_delta(Ticks(10_000)).unwrap(), Ticks(0));
    }

    #[test]
    fn notional_multiplies_through() {
        // 10 base × 100 ticks × 2 tick_size = 2000 quote lots.
        assert_eq!(
            notional_quote_lots(BaseLots(10), Ticks(100), 2).unwrap(),
            QuoteLots(2000)
        );
    }

    #[test]
    fn notional_overflow_saturates_to_error() {
        assert_eq!(
            notional_quote_lots(BaseLots(u64::MAX), Ticks(u64::MAX), 2),
            Err(FlashBookError::ArithmeticOverflow)
        );
    }

    #[test]
    fn min_and_zero_helpers() {
        assert_eq!(BaseLots(3).min(BaseLots(8)), BaseLots(3));
        assert!(BaseLots::ZERO.is_zero());
        assert!(!BaseLots(1).is_zero());
    }
}
