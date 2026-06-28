//! ADL (auto-deleverage) collateral routing — the pure isolated-vs-cross bucket
//! math the `auto_deleverage` instruction calls. De-anchored port of
//! `lib.rs::route_adl_loss` / `route_adl_gain` (verbatim semantics).
//!
//! **Invariant I-3 (bucket insulation):** an ISOLATED position's ADL loss/gain
//! moves ONLY the per-position bucket and NEVER the trader's cross pool (and a
//! cross loss/gain never touches a position bucket). That insulation is the
//! whole point of isolated margin — a runaway ADL on one isolated position can
//! never drain the trader's other (cross) positions.
//!
//! Losses **saturate at zero** (ADL is the last-resort waterfall step and must
//! always make progress; any shortfall is absorbed by the insurance + ADL
//! waterfall). Gains are **checked** (overflow → error). Pure → host-tested +
//! Kani-proven.

use crate::error::{OrOverflow, Result};

/// Route an ADL **loss**. Returns `(new_position_collateral,
/// new_trader_state_collateral)`. Isolated → debit the per-position bucket;
/// cross → debit the cross pool. Both saturate at zero; the OTHER bucket is
/// never touched (I-3).
#[inline]
pub fn route_adl_loss(
    isolated: bool,
    loss_quote_lots: u64,
    pos_collateral: u64,
    trader_state_collateral: u64,
) -> (u64, u64) {
    if isolated {
        (
            pos_collateral.saturating_sub(loss_quote_lots),
            trader_state_collateral,
        )
    } else {
        (
            pos_collateral,
            trader_state_collateral.saturating_sub(loss_quote_lots),
        )
    }
}

/// Route an ADL **gain** (the counterparty credit). Isolated → credit the
/// per-position bucket; cross → credit the cross pool. Both are checked
/// (overflow → `ArithmeticOverflow`); the OTHER bucket is never touched (I-3).
#[inline]
pub fn route_adl_gain(
    isolated: bool,
    gain_quote_lots: u64,
    pos_collateral: u64,
    trader_state_collateral: u64,
) -> Result<(u64, u64)> {
    if isolated {
        let new_pos = pos_collateral.checked_add(gain_quote_lots).or_overflow()?;
        Ok((new_pos, trader_state_collateral))
    } else {
        let new_ts = trader_state_collateral
            .checked_add(gain_quote_lots)
            .or_overflow()?;
        Ok((pos_collateral, new_ts))
    }
}

/// FV: machine-checked I-3 insulation (Kani, comparison/add-only → fast).
#[cfg(kani)]
mod kani_proofs {
    use super::{route_adl_gain, route_adl_loss};

    /// An ADL loss moves ONLY the routed bucket: the other is byte-for-byte
    /// unchanged, and the routed bucket never grows.
    #[kani::proof]
    fn adl_loss_insulates_other_bucket() {
        let loss: u64 = kani::any();
        let pos: u64 = kani::any();
        let ts: u64 = kani::any();
        let isolated: bool = kani::any();
        let (np, nts) = route_adl_loss(isolated, loss, pos, ts);
        if isolated {
            assert!(nts == ts); // cross pool untouched
            assert!(np <= pos); // position bucket only debited
        } else {
            assert!(np == pos); // position bucket untouched
            assert!(nts <= ts); // cross pool only debited
        }
    }

    /// An ADL gain (when it doesn't overflow) credits ONLY the routed bucket:
    /// the other is unchanged, and the routed bucket never shrinks.
    #[kani::proof]
    fn adl_gain_insulates_other_bucket() {
        let gain: u64 = kani::any();
        let pos: u64 = kani::any();
        let ts: u64 = kani::any();
        let isolated: bool = kani::any();
        if let Ok((np, nts)) = route_adl_gain(isolated, gain, pos, ts) {
            if isolated {
                assert!(nts == ts); // cross pool untouched
                assert!(np >= pos); // position bucket only credited
            } else {
                assert!(np == pos); // position bucket untouched
                assert!(nts >= ts); // cross pool only credited
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{route_adl_gain, route_adl_loss};

    // ── loss side ───────────────────────────────────────────────────────
    #[test]
    fn loss_isolated_debits_position_bucket() {
        let (pos, ts) = route_adl_loss(true, 100, 500, 10_000);
        assert_eq!(pos, 400);
        assert_eq!(ts, 10_000, "cross pool must NOT be touched on isolated loss");
    }

    #[test]
    fn loss_isolated_saturates_at_zero_never_touches_cross() {
        // Loss larger than the isolated bucket saturates to 0; the remainder is
        // absorbed by the insurance + ADL waterfall. Cross is NEVER debited (I-3).
        let (pos, ts) = route_adl_loss(true, 1_000, 500, 10_000);
        assert_eq!(pos, 0);
        assert_eq!(ts, 10_000, "MUST NOT bleed into cross pool on isolated loss");
    }

    #[test]
    fn loss_cross_debits_cross_pool() {
        let (pos, ts) = route_adl_loss(false, 250, 0, 1_000);
        assert_eq!(pos, 0, "position bucket untouched on cross path");
        assert_eq!(ts, 750);
    }

    #[test]
    fn loss_cross_saturates_at_zero() {
        let (pos, ts) = route_adl_loss(false, 5_000, 0, 1_000);
        assert_eq!(pos, 0);
        assert_eq!(ts, 0);
    }

    #[test]
    fn loss_zero_amount_is_no_op() {
        assert_eq!(route_adl_loss(true, 0, 500, 10_000), (500, 10_000));
        assert_eq!(route_adl_loss(false, 0, 500, 10_000), (500, 10_000));
    }

    // ── gain side ───────────────────────────────────────────────────────
    #[test]
    fn gain_isolated_credits_position_bucket() {
        let (pos, ts) = route_adl_gain(true, 250, 500, 10_000).unwrap();
        assert_eq!(pos, 750);
        assert_eq!(ts, 10_000, "cross pool untouched on isolated gain");
    }

    #[test]
    fn gain_cross_credits_cross_pool() {
        let (pos, ts) = route_adl_gain(false, 250, 500, 10_000).unwrap();
        assert_eq!(pos, 500, "position bucket untouched on cross path");
        assert_eq!(ts, 10_250);
    }

    #[test]
    fn gain_isolated_overflow_errors() {
        assert!(route_adl_gain(true, 10, u64::MAX, 0).is_err());
    }

    #[test]
    fn gain_cross_overflow_errors() {
        assert!(route_adl_gain(false, 10, 0, u64::MAX).is_err());
    }

    #[test]
    fn gain_zero_amount_is_no_op() {
        assert_eq!(route_adl_gain(true, 0, 500, 10_000).unwrap(), (500, 10_000));
        assert_eq!(route_adl_gain(false, 0, 500, 10_000).unwrap(), (500, 10_000));
    }
}
