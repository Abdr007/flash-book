//! Peg order pricing helpers (Wave 32).
//!
//! Peg orders rest on the book at `oracle ± offset`. As the oracle
//! moves, the limit price moves with it. Useful for market makers
//! who want passive flow tracking the mid without active requoting.
//!
//! Two variants:
//! - **Primary peg**: rests on the trader's own side (long peg buy
//!   sits on the bid side). Offset is **negative** for bids
//!   (`oracle - offset`), **positive** for asks.
//! - **Mid peg**: rests at the protected midpoint between best bid
//!   and best ask. Useful for crossing the spread without
//!   front-running.
//!
//! Pure pricing math. Wire-in (Wave 32b) repeges resting orders on
//! every `settle_mark` call by walking the peg-order list and
//! invoking these helpers.

/// Compute the limit price for a primary peg order.
///
/// `side`: 0 = bid (buying), 1 = ask (selling).
/// `offset_ticks`: distance from oracle (always positive).
/// - Bid pegs sit BELOW oracle (oracle - offset).
/// - Ask pegs sit ABOVE oracle (oracle + offset).
///
/// Returns `None` if the result would underflow (bid offset > oracle).
pub fn primary_peg_price_ticks(
    oracle_price_ticks: u64,
    side: u8,
    offset_ticks: u64,
) -> Option<u64> {
    match side {
        0 => oracle_price_ticks.checked_sub(offset_ticks),
        1 => oracle_price_ticks.checked_add(offset_ticks),
        _ => None,
    }
}

/// Compute the limit price for a mid-peg order: protected midpoint
/// between best bid and best ask.
///
/// `side`: 0 = bid (so we sit at mid or slightly below), 1 = ask
/// (sit at mid or slightly above). The `side_offset_ticks` is added
/// or subtracted to nudge away from mid for safety.
///
/// Returns `None` when best_bid > best_ask (degenerate / crossed book).
pub fn mid_peg_price_ticks(
    best_bid_ticks: u64,
    best_ask_ticks: u64,
    side: u8,
    side_offset_ticks: u64,
) -> Option<u64> {
    if best_bid_ticks > best_ask_ticks {
        return None;
    }
    let mid = (best_bid_ticks as u128 + best_ask_ticks as u128) / 2;
    let mid_u64 = mid.min(u64::MAX as u128) as u64;
    match side {
        0 => mid_u64.checked_sub(side_offset_ticks),
        1 => mid_u64.checked_add(side_offset_ticks),
        _ => None,
    }
}

/// Align a price to the market's tick size (floor for bids, ceil for
/// asks — conservative for the trader's side).
#[inline]
pub fn align_to_tick(price_ticks: u64, side: u8, tick_size: u64) -> u64 {
    if tick_size <= 1 {
        return price_ticks;
    }
    let remainder = price_ticks % tick_size;
    if remainder == 0 {
        return price_ticks;
    }
    match side {
        0 => price_ticks - remainder, // bid floors
        1 => price_ticks + (tick_size - remainder), // ask ceils
        _ => price_ticks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_bid_sits_below_oracle() {
        assert_eq!(primary_peg_price_ticks(1_000_000, 0, 100), Some(999_900));
    }

    #[test]
    fn primary_ask_sits_above_oracle() {
        assert_eq!(primary_peg_price_ticks(1_000_000, 1, 100), Some(1_000_100));
    }

    #[test]
    fn primary_bid_underflow_returns_none() {
        assert_eq!(primary_peg_price_ticks(50, 0, 100), None);
    }

    #[test]
    fn primary_invalid_side_returns_none() {
        assert_eq!(primary_peg_price_ticks(1_000_000, 99, 100), None);
    }

    #[test]
    fn mid_peg_at_mid_when_offset_zero() {
        assert_eq!(mid_peg_price_ticks(1_000, 1_010, 0, 0), Some(1_005));
        assert_eq!(mid_peg_price_ticks(1_000, 1_010, 1, 0), Some(1_005));
    }

    #[test]
    fn mid_peg_with_safety_offset() {
        // Bid side, offset 2 → mid - 2 = 1003.
        assert_eq!(mid_peg_price_ticks(1_000, 1_010, 0, 2), Some(1_003));
        assert_eq!(mid_peg_price_ticks(1_000, 1_010, 1, 2), Some(1_007));
    }

    #[test]
    fn mid_peg_rejects_crossed_book() {
        assert_eq!(mid_peg_price_ticks(1_010, 1_000, 0, 0), None);
    }

    #[test]
    fn align_floor_for_bid() {
        assert_eq!(align_to_tick(1_007, 0, 10), 1_000);
        assert_eq!(align_to_tick(1_000, 0, 10), 1_000);
    }

    #[test]
    fn align_ceil_for_ask() {
        assert_eq!(align_to_tick(1_007, 1, 10), 1_010);
        assert_eq!(align_to_tick(1_000, 1, 10), 1_000);
    }

    #[test]
    fn align_tick_size_one_is_noop() {
        assert_eq!(align_to_tick(1_007, 0, 1), 1_007);
        assert_eq!(align_to_tick(1_007, 1, 1), 1_007);
    }
}
