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

// ── ADL settlement math (bankruptcy price / loss / counter gain) ──────────────
// Pure transcription of the `auto_deleverage` settlement arithmetic
// (lib.rs:6992..7065). The `auto_deleverage` instruction (a later batch) wires
// these with account validation + the routing above. No funds, no accounts.

/// The bankruptcy price in ticks — the mark at which the underwater position's
/// equity is exactly zero. `long: bp = entry − C/(S·tick)` (clamped ≥ 1);
/// `short: bp = entry + C/(S·tick)`. `collateral` is the bucket BACKING this
/// position (per-position if isolated, else cross — the caller picks, mirroring
/// the isolated-vs-cross routing). `None` if `size·tick == 0`. The short side
/// saturates to `u64::MAX` (matches the anchor clamp).
#[inline]
pub fn bankruptcy_price(
    side: u8,
    entry_ticks: u64,
    collateral: u64,
    size_lots: u64,
    tick_size: u64,
) -> Option<u64> {
    let denom = (size_lots as u128).checked_mul(tick_size as u128)?;
    if denom == 0 {
        return None;
    }
    let collateral_per_lot = (collateral as u128) / denom;
    let bp = if side == 0 {
        (entry_ticks as u128).saturating_sub(collateral_per_lot).max(1)
    } else {
        // entry + cpl: both < 2^64 ⇒ the u128 sum can't overflow.
        (entry_ticks as u128) + collateral_per_lot
    };
    Some(if bp > u64::MAX as u128 { u64::MAX } else { bp as u64 })
}

/// The underwater trader's realized loss for closing `close_size_lots` of a
/// `size_lots` position: `collateral · close/size` (proportional collateral
/// wiped). Saturating; `0` when `size_lots == 0`.
#[inline]
pub fn adl_underwater_loss(collateral: u64, close_size_lots: u64, size_lots: u64) -> u64 {
    if size_lots == 0 {
        return 0;
    }
    let v = (collateral as u128).saturating_mul(close_size_lots as u128) / (size_lots as u128);
    if v > u64::MAX as u128 { u64::MAX } else { v as u64 }
}

/// The counter (winning) trader's positive PnL at the bankruptcy price.
/// `long counter: (bp − entry_c)·close·tick`; `short counter: (entry_c − bp)·
/// close·tick`. The per-lot diff saturates at 0, so an INELIGIBLE counter (see
/// [`counter_eligible_at_bp`]) yields `0`. Saturating throughout.
#[inline]
pub fn adl_counter_gain(
    counter_side: u8,
    counter_entry: u64,
    bp_ticks: u64,
    close_size_lots: u64,
    tick_size: u64,
) -> u64 {
    let per_lot = if counter_side == 0 {
        (bp_ticks as u128).saturating_sub(counter_entry as u128)
    } else {
        (counter_entry as u128).saturating_sub(bp_ticks as u128)
    };
    let v = per_lot
        .saturating_mul(close_size_lots as u128)
        .saturating_mul(tick_size as u128);
    if v > u64::MAX as u128 { u64::MAX } else { v as u64 }
}

/// Counter eligibility: the counter must have POSITIVE PnL at the bankruptcy
/// price to be deleveraged. `long counter: bp > entry_c`; `short: bp < entry_c`.
#[inline]
pub fn counter_eligible_at_bp(counter_side: u8, counter_entry: u64, bp_ticks: u64) -> bool {
    if counter_side == 0 {
        bp_ticks > counter_entry
    } else {
        bp_ticks < counter_entry
    }
}

/// FV: machine-checked I-3 insulation (Kani, comparison/add-only → fast).
#[cfg(kani)]
mod kani_proofs {
    use super::{
        adl_counter_gain, counter_eligible_at_bp, route_adl_gain, route_adl_loss,
    };

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

    /// Eligibility ⇔ gain: a counter that is eligible at the bankruptcy price
    /// (positive PnL) realizes a STRICTLY POSITIVE gain when there is a real
    /// fill (`close > 0`, `tick > 0`); an ineligible counter realizes ZERO.
    /// Bounded to u16 so the `per_lot · close · tick` mul terminates in CBMC.
    #[kani::proof]
    fn counter_gain_matches_eligibility() {
        let side: u8 = kani::any();
        kani::assume(side <= 1);
        let entry = kani::any::<u16>() as u64;
        let bp = kani::any::<u16>() as u64;
        let close = kani::any::<u16>() as u64;
        let tick = kani::any::<u16>() as u64;
        let eligible = counter_eligible_at_bp(side, entry, bp);
        let gain = adl_counter_gain(side, entry, bp, close, tick);
        if eligible && close > 0 && tick > 0 {
            assert!(gain > 0);
        }
        if !eligible {
            assert!(gain == 0); // per-lot diff saturates to 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        adl_counter_gain, adl_underwater_loss, bankruptcy_price, counter_eligible_at_bp,
        route_adl_gain, route_adl_loss,
    };

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

    // ── settlement math ─────────────────────────────────────────────────
    #[test]
    fn bankruptcy_price_long_and_short() {
        // long 10 @100, collat 50, tick 1: cpl = 50/(10*1) = 5 → bp = 95.
        assert_eq!(bankruptcy_price(0, 100, 50, 10, 1), Some(95));
        // short same inputs → bp = 105.
        assert_eq!(bankruptcy_price(1, 100, 50, 10, 1), Some(105));
        // long where collateral overshoots entry → clamps to 1 (never 0).
        assert_eq!(bankruptcy_price(0, 100, 100_000, 10, 1), Some(1));
        // size·tick == 0 ⇒ None.
        assert_eq!(bankruptcy_price(0, 100, 50, 0, 1), None);
        assert_eq!(bankruptcy_price(0, 100, 50, 10, 0), None);
    }

    #[test]
    fn underwater_loss_is_proportional() {
        // collat 50, close 4 of size 10 → 50*4/10 = 20.
        assert_eq!(adl_underwater_loss(50, 4, 10), 20);
        // full close → full collateral.
        assert_eq!(adl_underwater_loss(50, 10, 10), 50);
        assert_eq!(adl_underwater_loss(50, 4, 0), 0);
    }

    #[test]
    fn counter_gain_and_eligibility() {
        // long counter @90, bp 95, close 4, tick 1 → (95-90)*4 = 20, eligible.
        assert!(counter_eligible_at_bp(0, 90, 95));
        assert_eq!(adl_counter_gain(0, 90, 95, 4, 1), 20);
        // short counter @110, bp 105 → (110-105)*4 = 20, eligible (bp < entry).
        assert!(counter_eligible_at_bp(1, 110, 105));
        assert_eq!(adl_counter_gain(1, 110, 105, 4, 1), 20);
        // ineligible long counter (bp <= entry) ⇒ no gain.
        assert!(!counter_eligible_at_bp(0, 95, 95));
        assert_eq!(adl_counter_gain(0, 95, 95, 4, 1), 0);
        assert_eq!(adl_counter_gain(0, 100, 95, 4, 1), 0);
    }
}
