pub const BPS_DENOM: u32 = 10_000;
pub const USD_UNIT: u64 = 1_000_000;

/// VPIN Q32.32 fixed-point (toxicity / VPIN math — see vpin.rs).
pub const VPIN_FRACTIONAL_BITS: u32 = 32;
pub const VPIN_FIXED_ONE: u64 = 1u64 << VPIN_FRACTIONAL_BITS;
