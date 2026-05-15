//! Conditional cancel (Wave 64).
//!
//! Cancel an order if oracle crosses a configured threshold. Lets
//! traders attach a "cancel-if" rule to their limits: e.g., "cancel
//! my buy at $100 if oracle drops below $80" (the market has gapped
//! against you; you don't want the fill anymore).
//!
//! Per-order state (added to RestingOrderV2 in Wave 64b via a new
//! flag bit + extra u64 field, or stored in a sibling PDA).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalCancelDirection {
    /// Cancel if oracle ≤ threshold.
    CancelBelow,
    /// Cancel if oracle ≥ threshold.
    CancelAbove,
}

/// Check whether the conditional cancel rule has fired. Pure.
///
/// `threshold_ticks == 0` means "no rule" (legacy).
pub fn should_cancel(
    threshold_ticks: u64,
    direction: ConditionalCancelDirection,
    oracle_ticks: u64,
) -> bool {
    if threshold_ticks == 0 {
        return false;
    }
    match direction {
        ConditionalCancelDirection::CancelBelow => oracle_ticks <= threshold_ticks,
        ConditionalCancelDirection::CancelAbove => oracle_ticks >= threshold_ticks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_zero_never_fires() {
        assert!(!should_cancel(0, ConditionalCancelDirection::CancelBelow, 100));
        assert!(!should_cancel(0, ConditionalCancelDirection::CancelAbove, 100));
    }

    #[test]
    fn cancel_below_fires_at_or_below_threshold() {
        assert!(!should_cancel(100, ConditionalCancelDirection::CancelBelow, 101));
        assert!(should_cancel(100, ConditionalCancelDirection::CancelBelow, 100));
        assert!(should_cancel(100, ConditionalCancelDirection::CancelBelow, 50));
    }

    #[test]
    fn cancel_above_fires_at_or_above_threshold() {
        assert!(!should_cancel(100, ConditionalCancelDirection::CancelAbove, 99));
        assert!(should_cancel(100, ConditionalCancelDirection::CancelAbove, 100));
        assert!(should_cancel(100, ConditionalCancelDirection::CancelAbove, 200));
    }
}
