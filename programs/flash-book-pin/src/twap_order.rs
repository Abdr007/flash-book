//! TWAP (time-sliced) order parameter validation — pure, host-tested,
//! anchor-parity port of the `place_twap_order_v3` checks.

/// Validate a proposed TWAP order against the market's lot/tick rules and the
/// current slot. `Err(())` on any violation:
///  * `side` out of `0..=1`;
///  * zero slice size, or `total_size < slice_size`;
///  * zero limit price, or zero slot interval;
///  * `limit_price` not on tick, or `slice_size` below the market's `min_base_lots`;
///  * a non-zero `acceptable_price` off-tick or on the wrong side of the limit
///    (long cap ≥ limit, short cap ≤ limit);
///  * a non-zero `end_slot` that is not strictly in the future (`> now_slot`).
#[allow(clippy::too_many_arguments)]
pub fn validate_twap_params(
    side: u8,
    slice_size_lots: u64,
    total_size_lots: u64,
    limit_price_ticks: u64,
    slot_interval: u64,
    acceptable_price_ticks: u64,
    end_slot: u64,
    now_slot: u64,
    min_base_lots: u64,
    tick_size: u64,
) -> Result<(), ()> {
    if side > 1 {
        return Err(());
    }
    if slice_size_lots == 0 || total_size_lots < slice_size_lots {
        return Err(());
    }
    if limit_price_ticks == 0 || slot_interval == 0 {
        return Err(());
    }
    if tick_size == 0 || limit_price_ticks % tick_size != 0 {
        return Err(());
    }
    if slice_size_lots < min_base_lots {
        return Err(());
    }
    if acceptable_price_ticks > 0 {
        if acceptable_price_ticks % tick_size != 0 {
            return Err(());
        }
        match side {
            0 => {
                if acceptable_price_ticks < limit_price_ticks {
                    return Err(());
                }
            }
            _ => {
                if acceptable_price_ticks > limit_price_ticks {
                    return Err(());
                }
            }
        }
    }
    if end_slot > 0 && end_slot <= now_slot {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // (side, slice, total, limit, interval, accept, end, now, min, tick)
    fn call(t: (u8, u64, u64, u64, u64, u64, u64, u64, u64, u64)) -> Result<(), ()> {
        validate_twap_params(t.0, t.1, t.2, t.3, t.4, t.5, t.6, t.7, t.8, t.9)
    }
    const OK: (u8, u64, u64, u64, u64, u64, u64, u64, u64, u64) =
        (0, 10, 100, 1_000, 5, 0, 0, 50, 1, 10);

    #[test]
    fn accepts_a_well_formed_twap() {
        assert_eq!(call(OK), Ok(()));
        // long cap >= limit, future end slot.
        assert_eq!(call((0, 10, 100, 1_000, 5, 1_020, 100, 50, 1, 10)), Ok(()));
        // short cap <= limit.
        assert_eq!(call((1, 10, 100, 1_000, 5, 980, 0, 50, 1, 10)), Ok(()));
    }

    #[test]
    fn rejects_sizes_prices_interval() {
        assert_eq!(call((2, 10, 100, 1_000, 5, 0, 0, 50, 1, 10)), Err(())); // side
        assert_eq!(call((0, 0, 100, 1_000, 5, 0, 0, 50, 1, 10)), Err(())); // slice 0
        assert_eq!(call((0, 100, 10, 1_000, 5, 0, 0, 50, 1, 10)), Err(())); // total < slice
        assert_eq!(call((0, 10, 100, 0, 5, 0, 0, 50, 1, 10)), Err(())); // limit 0
        assert_eq!(call((0, 10, 100, 1_000, 0, 0, 0, 50, 1, 10)), Err(())); // interval 0
        assert_eq!(call((0, 5, 100, 1_000, 5, 0, 0, 50, 10, 10)), Err(())); // below min lot
        assert_eq!(call((0, 10, 100, 1_005, 5, 0, 0, 50, 1, 10)), Err(())); // limit off tick
    }

    #[test]
    fn rejects_bad_cap_and_past_end_slot() {
        assert_eq!(call((0, 10, 100, 1_000, 5, 990, 0, 50, 1, 10)), Err(())); // long cap < limit
        assert_eq!(call((1, 10, 100, 1_000, 5, 1_010, 0, 50, 1, 10)), Err(())); // short cap > limit
        assert_eq!(call((0, 10, 100, 1_000, 5, 1_025, 0, 50, 1, 10)), Err(())); // cap off tick
        assert_eq!(call((0, 10, 100, 1_000, 5, 0, 50, 50, 1, 10)), Err(())); // end == now
        assert_eq!(call((0, 10, 100, 1_000, 5, 0, 40, 50, 1, 10)), Err(())); // end < now
    }
}
