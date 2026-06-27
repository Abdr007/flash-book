//! Per-position leverage-cap math — pure, host-tested.
//!
//! A position's leverage is `notional / margin`. The `leverage_cap` (set via
//! `set_position_leverage`) is the max multiple allowed; this module decides when
//! a position exceeds it, without division (compare `notional` to `cap × margin`).

/// True iff `notional` exceeds `cap × collateral` (i.e. leverage > cap). `cap == 0`
/// is "unset" → never exceeds. Zero collateral with any positive notional is
/// infinite leverage → exceeds any finite cap. `cap × collateral` is computed in
/// `u128` so it cannot overflow (`u32 × u64` ⊂ `u128`).
pub fn exceeds_leverage_cap(notional: u128, cap: u32, collateral: u64) -> bool {
    if cap == 0 {
        return false; // unset
    }
    if collateral == 0 {
        return notional > 0; // infinite leverage
    }
    notional > (cap as u128) * (collateral as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_cap_never_exceeds() {
        assert!(!exceeds_leverage_cap(u128::MAX, 0, 1));
        assert!(!exceeds_leverage_cap(0, 0, 0));
    }

    #[test]
    fn at_or_below_cap_is_ok_above_is_exceeded() {
        // cap 10, collateral 100 → allowed notional up to 1000.
        assert!(!exceeds_leverage_cap(1_000, 10, 100)); // exactly 10x
        assert!(!exceeds_leverage_cap(500, 10, 100)); // 5x
        assert!(exceeds_leverage_cap(1_001, 10, 100)); // just over 10x
    }

    #[test]
    fn zero_collateral_with_notional_is_infinite_leverage() {
        assert!(exceeds_leverage_cap(1, 10, 0));
        // but zero notional on zero collateral is fine (a flat/empty position).
        assert!(!exceeds_leverage_cap(0, 10, 0));
    }

    #[test]
    fn no_overflow_at_extremes() {
        // u32::MAX × u64::MAX fits in u128; a huge cap admits a huge notional.
        assert!(!exceeds_leverage_cap(
            (u32::MAX as u128) * (u64::MAX as u128),
            u32::MAX,
            u64::MAX
        ));
        assert!(exceeds_leverage_cap(
            (u32::MAX as u128) * (u64::MAX as u128) + 1,
            u32::MAX,
            u64::MAX
        ));
    }
}
