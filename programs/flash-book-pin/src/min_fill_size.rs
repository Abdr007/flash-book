//! Minimum-fill-size guard for limit orders (Wave 62).
//!
//! Trader specifies a minimum acceptable fill amount; the matcher
//! refuses partial fills below this threshold. Useful for:
//! - **Institutional traders** who don't want to be picked apart by
//!   dust fills (creates many small positions to manage).
//! - **RFQ-style flow** where atomic block-size fills are required.
//!
//! `min_fill_lots == 0` means "no minimum" (legacy / default).
//! `min_fill_lots == size_lots` means "FOK" (Fill-or-Kill).
//! Anything in between is partial OK above the threshold.

/// Decide whether a candidate fill respects the minimum-fill-size rule.
///
/// `fill_size_lots`: the size the matcher is about to fill on this round.
/// `min_fill_lots`: the trader's configured minimum.
/// `remaining_after_fill`: how much of the original order remains AFTER
///   this fill. Used for the "OK if the order is finished" case.
///
/// Returns `true` if the fill should proceed.
pub fn fill_ok(
    fill_size_lots: u64,
    min_fill_lots: u64,
    remaining_after_fill: u64,
) -> bool {
    if min_fill_lots == 0 {
        return true; // no constraint
    }
    // Allow if either:
    //   (a) the fill itself is at or above the minimum, OR
    //   (b) the fill completes the order (no remainder).
    fill_size_lots >= min_fill_lots || remaining_after_fill == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_min_admits_all() {
        assert!(fill_ok(1, 0, 99));
        assert!(fill_ok(100, 0, 0));
    }

    #[test]
    fn admits_above_threshold() {
        assert!(fill_ok(100, 50, 0));
        assert!(fill_ok(50, 50, 100));
    }

    #[test]
    fn rejects_below_threshold_with_remainder() {
        assert!(!fill_ok(49, 50, 100));
    }

    #[test]
    fn admits_completion_below_threshold() {
        // Final fill completes the order — admit even if below min.
        assert!(fill_ok(10, 100, 0));
    }

    #[test]
    fn fok_semantics() {
        // min == total size → FOK. Fill must complete in one go.
        let total = 100;
        // Full fill, no remainder → OK.
        assert!(fill_ok(100, total, 0));
        // Partial fill leaves remainder → reject.
        assert!(!fill_ok(50, total, 50));
    }
}
