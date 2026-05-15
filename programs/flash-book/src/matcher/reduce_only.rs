//! Reduce-only flag for limit orders (Wave 63).
//!
//! When set, the order can ONLY reduce or close an existing position
//! on the configured side — it cannot open a new position or flip
//! the side. Critical for protective stops and take-profits placed
//! as limits (existing trigger v3 already has this; here we add it
//! to plain limit orders).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReduceOnlyOutcome {
    /// Order admits in full: the fill would reduce the position.
    Admit,
    /// Partial admit: the order admits up to `lots` lots before flipping.
    PartialAdmit(u64),
    /// Reject: no position to reduce on this side.
    Reject,
}

/// Decide whether a reduce-only order can fill given the current
/// position state.
///
/// `position_side`: 0 = long, 1 = short, 2 = flat (no position).
/// `position_size_lots`: current position size on the trader's side.
/// `order_side`: 0 = buy/long, 1 = sell/short. For reduce, the order's
///   side must be OPPOSITE to the position's side (sell to reduce a
///   long, buy to reduce a short).
/// `order_size_lots`: the order's intended size.
pub fn check_reduce_only(
    position_side: u8,
    position_size_lots: u64,
    order_side: u8,
    order_size_lots: u64,
) -> ReduceOnlyOutcome {
    if position_size_lots == 0 || position_side == 2 {
        return ReduceOnlyOutcome::Reject;
    }
    // Order must oppose the position.
    let opposes = (position_side == 0 && order_side == 1)
        || (position_side == 1 && order_side == 0);
    if !opposes {
        return ReduceOnlyOutcome::Reject;
    }
    if order_size_lots <= position_size_lots {
        ReduceOnlyOutcome::Admit
    } else {
        // Order is bigger than the position — admit only the
        // position-size portion; the rest would flip.
        ReduceOnlyOutcome::PartialAdmit(position_size_lots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_when_flat() {
        assert_eq!(check_reduce_only(2, 0, 1, 100), ReduceOnlyOutcome::Reject);
    }

    #[test]
    fn rejects_same_side_order() {
        // Long position, long order would grow it → reject.
        assert_eq!(check_reduce_only(0, 100, 0, 50), ReduceOnlyOutcome::Reject);
    }

    #[test]
    fn admits_full_reduce() {
        // Long 100, sell 50 → reduce.
        assert_eq!(check_reduce_only(0, 100, 1, 50), ReduceOnlyOutcome::Admit);
        // Long 100, sell exactly 100 → close.
        assert_eq!(check_reduce_only(0, 100, 1, 100), ReduceOnlyOutcome::Admit);
    }

    #[test]
    fn partial_admits_capped_at_position_size() {
        // Long 100, sell 200 → admit 100 (don't flip).
        assert_eq!(check_reduce_only(0, 100, 1, 200), ReduceOnlyOutcome::PartialAdmit(100));
    }

    #[test]
    fn short_position_reduces_with_buy() {
        assert_eq!(check_reduce_only(1, 100, 0, 50), ReduceOnlyOutcome::Admit);
        assert_eq!(check_reduce_only(1, 100, 0, 200), ReduceOnlyOutcome::PartialAdmit(100));
    }

    #[test]
    fn zero_order_size_admits_trivially() {
        assert_eq!(check_reduce_only(0, 100, 1, 0), ReduceOnlyOutcome::Admit);
    }
}
