//! Cancel-on-disconnect (CoD) (Wave 61).
//!
//! Hyperliquid / Binance pattern: traders set a heartbeat. If the
//! heartbeat is stale, their resting orders auto-cancel on the next
//! matcher tick. Critical for HFT — keeps stale limit orders out of
//! the book during disconnect / outage.
//!
//! Per-trader state on TraderState (Wave 61b layout):
//! - `cod_enabled: bool`
//! - `last_heartbeat_slot: u64`
//! - `cod_timeout_slots: u64`
//!
//! Pure decision helper. Caller is the matcher walk: on every order
//! traversal, check `should_cancel(trader_state)`. If true, skip the
//! order (treat as auto-cancelled) and emit a CoD event for indexers.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodState {
    pub enabled: bool,
    pub last_heartbeat_slot: u64,
    pub timeout_slots: u64,
}

/// Should this trader's orders be auto-cancelled? Pure.
///
/// Returns true iff:
///   1. CoD is enabled.
///   2. `now_slot - last_heartbeat_slot ≥ timeout_slots`.
#[inline]
pub fn should_cancel(state: CodState, now_slot: u64) -> bool {
    if !state.enabled || state.timeout_slots == 0 {
        return false;
    }
    now_slot.saturating_sub(state.last_heartbeat_slot) >= state.timeout_slots
}

/// Update heartbeat. Wire-in calls this when the trader sends any tx.
#[inline]
pub fn update_heartbeat(state: CodState, now_slot: u64) -> CodState {
    CodState {
        enabled: state.enabled,
        last_heartbeat_slot: now_slot,
        timeout_slots: state.timeout_slots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_never_cancels() {
        let s = CodState { enabled: false, last_heartbeat_slot: 0, timeout_slots: 100 };
        assert!(!should_cancel(s, 1_000_000));
    }

    #[test]
    fn zero_timeout_never_cancels() {
        let s = CodState { enabled: true, last_heartbeat_slot: 0, timeout_slots: 0 };
        assert!(!should_cancel(s, 1_000_000));
    }

    #[test]
    fn cancels_after_timeout() {
        let s = CodState { enabled: true, last_heartbeat_slot: 100, timeout_slots: 100 };
        assert!(!should_cancel(s, 199));
        assert!(should_cancel(s, 200));
        assert!(should_cancel(s, 1_000));
    }

    #[test]
    fn heartbeat_resets_clock() {
        let s = CodState { enabled: true, last_heartbeat_slot: 100, timeout_slots: 100 };
        let s2 = update_heartbeat(s, 500);
        assert_eq!(s2.last_heartbeat_slot, 500);
        assert!(!should_cancel(s2, 550));
    }
}
