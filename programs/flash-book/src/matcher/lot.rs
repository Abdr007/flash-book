//! Type-safe wrappers for prices, sizes, and basis points.
//!
//! The matcher operates exclusively on integer lot space — never on
//! floating-point. All conversions are explicit; mixing these types in
//! arithmetic is a compile error unless you go through one of the
//! conversion methods, which check for overflow.

use crate::errors::{FlashBookError, OrOverflow};
use anchor_lang::prelude::*;

/// Base asset size, in base lots. One base lot is `MarketParams.base_lot_size`
/// atoms of the base mint.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, AnchorSerialize, AnchorDeserialize,
)]
#[repr(transparent)]
pub struct BaseLots(pub u64);

/// Quote asset size, in quote lots. One quote lot is `MarketParams.quote_lot_size`
/// atoms of the quote mint (USDC).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, AnchorSerialize, AnchorDeserialize,
)]
#[repr(transparent)]
pub struct QuoteLots(pub u64);

/// Price in ticks. One tick is `MarketParams.tick_size_quote_lots_per_base_lot`
/// quote-lots-per-base-lot.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, AnchorSerialize, AnchorDeserialize,
)]
#[repr(transparent)]
pub struct Ticks(pub u64);

/// Basis points (1 bp = 0.0001). Stored as u32 — max 4.29B bps which
/// covers any sensible parameter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, AnchorSerialize, AnchorDeserialize,
)]
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
}

impl QuoteLots {
    pub const ZERO: Self = Self(0);
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0.checked_add(other.0).map(Self).or_overflow()
    }
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0.checked_sub(other.0).map(Self).or_underflow()
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
}

/// Compute notional in quote-lots from base-lots × ticks.
/// `notional_quote_lots = base_lots × ticks × quote_lots_per_tick_per_base_lot`
/// In our convention `tick_size_quote_lots_per_base_lot` is already the
/// conversion factor, so:
/// `notional = base_lots × price_ticks × tick_size`
///
/// We use u128 intermediate.
pub fn notional_quote_lots(
    base: BaseLots,
    price: Ticks,
    tick_size_quote_lots_per_base_lot: u64,
) -> Result<QuoteLots> {
    let m1 = (base.0 as u128)
        .checked_mul(price.0 as u128)
        .or_overflow()?;
    let m2 = m1
        .checked_mul(tick_size_quote_lots_per_base_lot as u128)
        .or_overflow()?;
    if m2 > u64::MAX as u128 {
        return Err(error!(FlashBookError::ArithmeticOverflow));
    }
    Ok(QuoteLots(m2 as u64))
}
