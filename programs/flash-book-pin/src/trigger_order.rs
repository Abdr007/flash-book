//! Conditional (trigger) order parameter validation — pure, host-tested,
//! anchor-parity port of the `place_trigger_order_v3` checks.

/// Validate a proposed trigger order against the market's lot/tick rules.
/// `Err(())` on any violation:
///  * `side` or `kind` out of `0..=1`;
///  * zero size / trigger / limit price;
///  * `size_lots` below the market's `min_base_lots`;
///  * `tick_size == 0`, or trigger/limit price not on a tick;
///  * a non-zero `acceptable_price` that is off-tick, or on the WRONG side of the
///    trigger — for a long (`side 0`) the slippage cap must be ≥ trigger, for a
///    short (`side 1`) it must be ≤ trigger; otherwise the trigger could never
///    fire (it would always breach its own cap).
#[allow(clippy::too_many_arguments)]
pub fn validate_trigger_params(
    side: u8,
    kind: u8,
    size_lots: u64,
    trigger_price_ticks: u64,
    limit_price_ticks: u64,
    acceptable_price_ticks: u64,
    min_base_lots: u64,
    tick_size: u64,
) -> Result<(), ()> {
    if side > 1 || kind > 1 {
        return Err(());
    }
    if size_lots == 0 || trigger_price_ticks == 0 || limit_price_ticks == 0 {
        return Err(());
    }
    if size_lots < min_base_lots {
        return Err(());
    }
    if tick_size == 0 {
        return Err(());
    }
    if trigger_price_ticks % tick_size != 0 || limit_price_ticks % tick_size != 0 {
        return Err(());
    }
    if acceptable_price_ticks > 0 {
        if acceptable_price_ticks % tick_size != 0 {
            return Err(());
        }
        match side {
            0 => {
                if acceptable_price_ticks < trigger_price_ticks {
                    return Err(());
                }
            }
            _ => {
                if acceptable_price_ticks > trigger_price_ticks {
                    return Err(());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // (side, kind, size, trigger, limit, accept, min_lots, tick)
    const OK: (u8, u8, u64, u64, u64, u64, u64, u64) = (0, 0, 10, 1_000, 1_010, 0, 1, 10);

    fn call(t: (u8, u8, u64, u64, u64, u64, u64, u64)) -> Result<(), ()> {
        validate_trigger_params(t.0, t.1, t.2, t.3, t.4, t.5, t.6, t.7)
    }

    #[test]
    fn accepts_a_well_formed_order() {
        assert_eq!(call(OK), Ok(()));
        // long with a cap on the correct (>= trigger) side, on tick.
        assert_eq!(call((0, 1, 10, 1_000, 1_010, 1_020, 1, 10)), Ok(()));
        // short with a cap <= trigger.
        assert_eq!(call((1, 0, 10, 1_000, 990, 980, 1, 10)), Ok(()));
    }

    #[test]
    fn rejects_bad_enums_and_zeros() {
        assert_eq!(call((2, 0, 10, 1_000, 1_010, 0, 1, 10)), Err(())); // side
        assert_eq!(call((0, 2, 10, 1_000, 1_010, 0, 1, 10)), Err(())); // kind
        assert_eq!(call((0, 0, 0, 1_000, 1_010, 0, 1, 10)), Err(())); // size
        assert_eq!(call((0, 0, 10, 0, 1_010, 0, 1, 10)), Err(())); // trigger
        assert_eq!(call((0, 0, 10, 1_000, 0, 0, 1, 10)), Err(())); // limit
    }

    #[test]
    fn rejects_below_min_lot_and_off_tick() {
        assert_eq!(call((0, 0, 5, 1_000, 1_010, 0, 10, 10)), Err(())); // below min lot
        assert_eq!(call((0, 0, 10, 1_005, 1_010, 0, 1, 10)), Err(())); // trigger off tick
        assert_eq!(call((0, 0, 10, 1_000, 1_015, 0, 1, 10)), Err(())); // limit off tick
        assert_eq!(call((0, 0, 10, 1_000, 1_010, 0, 1, 0)), Err(())); // tick_size 0
    }

    #[test]
    fn rejects_cap_on_wrong_side_or_off_tick() {
        // long cap below trigger — un-fireable.
        assert_eq!(call((0, 0, 10, 1_000, 1_010, 990, 1, 10)), Err(()));
        // short cap above trigger — un-fireable.
        assert_eq!(call((1, 0, 10, 1_000, 990, 1_010, 1, 10)), Err(()));
        // cap off tick.
        assert_eq!(call((0, 0, 10, 1_000, 1_010, 1_025, 1, 10)), Err(()));
    }
}
