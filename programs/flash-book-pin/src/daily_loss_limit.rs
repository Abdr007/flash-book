//! Daily loss limit per trader (Wave 53).
//!
//! Auto-halt new position opens once a trader's cumulative session
//! loss exceeds `loss_limit_bps` of their starting collateral.
//!
//! "Daily" is operationally configurable: the wire-in resets the
//! tracker at a session boundary (slot-aligned). Default = 24h
//! rolling window via slot count.
//!
//! Pure decision helper. The on-chain state lives on TraderState
//! (added in Wave 53b).

use crate::constants::BPS_DENOM;

/// Pure: should new position opens be blocked for this trader?
///
/// `cumulative_loss_quote_lots` is the absolute session loss (always
/// non-negative; positive PnL doesn't reset it). `starting_collateral`
/// is the session-start collateral baseline. `limit_bps` is the
/// fraction of starting collateral after which opens halt.
///
/// `limit_bps == 0` disables the check.
pub fn should_halt_opens(
    cumulative_loss_quote_lots: u64,
    starting_collateral_quote_lots: u64,
    limit_bps: u32,
) -> bool {
    if limit_bps == 0 || starting_collateral_quote_lots == 0 {
        return false;
    }
    let threshold = (starting_collateral_quote_lots as u128)
        .saturating_mul(limit_bps as u128)
        / (BPS_DENOM as u128);
    (cumulative_loss_quote_lots as u128) >= threshold
}

/// Update the cumulative loss tracker on a realized-PnL event.
/// Only adds to cumulative_loss on negative deltas; positives are no-op
/// (we don't subtract — the limit is loss-since-session-start, not
/// drawdown-from-peak).
#[inline]
pub fn record_realized_delta(
    cumulative_loss: u64,
    delta_quote_lots: i128,
) -> u64 {
    if delta_quote_lots >= 0 {
        return cumulative_loss;
    }
    let loss_u64: u64 = if delta_quote_lots < -(u64::MAX as i128) {
        u64::MAX
    } else {
        (-delta_quote_lots) as u64
    };
    cumulative_loss.saturating_add(loss_u64)
}

/// Reset on session boundary. Wire-in calls this when slot crosses
/// the session window.
#[inline]
pub fn reset_session(current_collateral: u64, _now_slot: u64) -> u64 {
    let _ = current_collateral;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_zero_disables() {
        assert!(!should_halt_opens(u64::MAX, 1_000, 0));
    }

    #[test]
    fn halts_at_threshold() {
        // 10% limit, 1000 starting → threshold 100. Loss 99 → no halt.
        assert!(!should_halt_opens(99, 1_000, 1_000));
        assert!(should_halt_opens(100, 1_000, 1_000));
        assert!(should_halt_opens(500, 1_000, 1_000));
    }

    #[test]
    fn record_only_adds_losses() {
        assert_eq!(record_realized_delta(50, -10), 60);
        assert_eq!(record_realized_delta(50, 10), 50, "gain doesn't reduce loss");
        assert_eq!(record_realized_delta(50, 0), 50);
    }

    #[test]
    fn record_clamps_extreme_negative() {
        // Negative beyond u64::MAX → saturate to u64::MAX add.
        let r = record_realized_delta(0, i128::MIN);
        assert_eq!(r, u64::MAX);
    }

    #[test]
    fn reset_clears_to_zero() {
        assert_eq!(reset_session(500, 1_000), 0);
    }

    #[test]
    fn zero_collateral_disables() {
        // Edge case: trader started with no collateral → can't halt
        // (would always halt at any loss).
        assert!(!should_halt_opens(100, 0, 1_000));
    }
}
