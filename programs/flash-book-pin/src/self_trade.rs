//! Self-trade prevention (Wave 50).
//!
//! Block a taker from filling against a resting order they own. Three
//! policies:
//!
//! - **`CancelTaker`**: when self-cross detected, cancel the taker's
//!   incoming order entirely.
//! - **`CancelMaker`**: cancel the conflicting resting order; taker
//!   continues to walk the next price level.
//! - **`CancelBoth`**: cancel both sides (rare; useful for
//!   risk-averse traders who want to ensure cleanup).
//! - **`Allow`**: legacy / default — self-trades are permitted. Some
//!   strategies legitimately want to repaint their book (e.g. moving
//!   liquidity to a different price level via a paired self-match).
//!
//! Pure decision logic. Wire-in: per-trader `self_trade_policy: u8`
//! field on TraderState, consulted in the matcher walk.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SelfTradePolicy {
    Allow = 0,
    CancelTaker = 1,
    CancelMaker = 2,
    CancelBoth = 3,
}

impl SelfTradePolicy {
    pub fn from_u8(b: u8) -> Self {
        match b {
            1 => Self::CancelTaker,
            2 => Self::CancelMaker,
            3 => Self::CancelBoth,
            _ => Self::Allow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfTradeAction {
    /// No conflict — let the match proceed.
    Proceed,
    /// Skip this maker but keep walking.
    SkipMaker,
    /// Abort taker order (cancel + return).
    AbortTaker,
    /// Cancel both sides; no fill, no continuation.
    AbortBoth,
}

/// Decide what to do when a taker would fill against a maker on the
/// same trader's order. Pure function.
///
/// `same_trader`: caller has already checked `taker_trader_id ==
/// maker_trader_id`. (Caller's job to handle sub-accounts: typically
/// "same wallet, same sub" counts as same-trader; cross-sub trades
/// are allowed by default but configurable per design.)
pub fn decide(same_trader: bool, policy: SelfTradePolicy) -> SelfTradeAction {
    if !same_trader {
        return SelfTradeAction::Proceed;
    }
    match policy {
        SelfTradePolicy::Allow => SelfTradeAction::Proceed,
        SelfTradePolicy::CancelTaker => SelfTradeAction::AbortTaker,
        SelfTradePolicy::CancelMaker => SelfTradeAction::SkipMaker,
        SelfTradePolicy::CancelBoth => SelfTradeAction::AbortBoth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_traders_always_proceed() {
        for p in [SelfTradePolicy::Allow, SelfTradePolicy::CancelTaker,
                  SelfTradePolicy::CancelMaker, SelfTradePolicy::CancelBoth] {
            assert_eq!(decide(false, p), SelfTradeAction::Proceed);
        }
    }

    #[test]
    fn allow_policy_lets_self_trade_proceed() {
        assert_eq!(decide(true, SelfTradePolicy::Allow), SelfTradeAction::Proceed);
    }

    #[test]
    fn cancel_taker_aborts_taker_on_self_cross() {
        assert_eq!(decide(true, SelfTradePolicy::CancelTaker), SelfTradeAction::AbortTaker);
    }

    #[test]
    fn cancel_maker_skips_and_continues() {
        assert_eq!(decide(true, SelfTradePolicy::CancelMaker), SelfTradeAction::SkipMaker);
    }

    #[test]
    fn cancel_both_aborts_everything() {
        assert_eq!(decide(true, SelfTradePolicy::CancelBoth), SelfTradeAction::AbortBoth);
    }

    #[test]
    fn from_u8_roundtrip() {
        assert_eq!(SelfTradePolicy::from_u8(0), SelfTradePolicy::Allow);
        assert_eq!(SelfTradePolicy::from_u8(1), SelfTradePolicy::CancelTaker);
        assert_eq!(SelfTradePolicy::from_u8(2), SelfTradePolicy::CancelMaker);
        assert_eq!(SelfTradePolicy::from_u8(3), SelfTradePolicy::CancelBoth);
        // Unknown values default to Allow (safest for future-proofing).
        assert_eq!(SelfTradePolicy::from_u8(99), SelfTradePolicy::Allow);
    }
}
