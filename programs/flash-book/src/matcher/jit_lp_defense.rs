//! JIT LP defense.
//!
//! Prevent flash JIT liquidity attacks against the FLP: a depositor
//! who tries to sneak in front of a big trader-loss event, capture the
//! windfall, and exit immediately gets blocked by a minimum hold time.
//!
//! Each LP position carries `deposited_at_slot`. Withdrawal requires
//! `now_slot - deposited_at_slot ≥ min_hold_slots`. The min-hold
//! resets on EACH deposit (any new top-up extends the lock).
//!
//! Pure math.

/// Check whether a withdrawal is admissible given the hold-time rule.
#[inline]
pub fn can_withdraw(deposited_at_slot: u64, now_slot: u64, min_hold_slots: u64) -> bool {
    if min_hold_slots == 0 {
        return true;
    }
    now_slot.saturating_sub(deposited_at_slot) >= min_hold_slots
}

/// On a new deposit, the lock resets to NOW (the most-recently
/// deposited block of capital is what extends the cap). Returns the
/// new `deposited_at_slot`.
#[inline]
pub fn extend_lock_on_deposit(now_slot: u64) -> u64 {
    now_slot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_hold_always_allows() {
        assert!(can_withdraw(100, 100, 0));
        assert!(can_withdraw(0, 0, 0));
    }

    #[test]
    fn cannot_withdraw_before_hold_elapses() {
        assert!(!can_withdraw(100, 150, 100));
        assert!(!can_withdraw(100, 199, 100));
    }

    #[test]
    fn can_withdraw_at_exact_boundary() {
        assert!(can_withdraw(100, 200, 100));
    }

    #[test]
    fn can_withdraw_after_boundary() {
        assert!(can_withdraw(100, 500, 100));
    }

    #[test]
    fn extend_lock_returns_now() {
        assert_eq!(extend_lock_on_deposit(12_345), 12_345);
    }
}
