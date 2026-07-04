//! Wave 24d integration tests.
//!
//! Verifies the routing behaviour of `apply_realized_pnl_delta_v2` —
//! the dispatcher that replaces `apply_realized_pnl_delta`'s direct
//! credit with H-haircut reserve routing whenever the position has
//! haircut state attached.
//!
//! Three flow scenarios:
//!
//!  1. Legacy market (haircut state absent): bit-for-bit identical to
//!     the v1 routing — positive credits to collateral immediately.
//!  2. Opted-in market with positive gain: pushes into reserve, leaves
//!     collateral unchanged.
//!  3. Opted-in market with loss: bypasses haircut entirely, debits
//!     collateral directly (loss seniority).
//!
//! Mirrors the on-chain dispatcher body as plain Rust over the same
//! shape that `apply_realized_pnl_delta_v2` consumes. Layered on top
//! of the proven `matcher::haircut` math.

use flash_book::matcher::haircut::{apply_release, PositionHaircutSnapshot};

#[derive(Debug, Clone, Copy)]
struct Buckets {
    iso: u64,
    cross: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PosHaircut {
    reserve: u64,
    attached: u64,
    matured: u64,
    original: u64,
}

/// Mirror of `apply_realized_pnl_delta_v2`. Signature exactly tracks
/// the on-chain function — `position_haircut: Option<&mut ...>` toggles
/// between legacy and haircut routing.
fn dispatch(
    delta: i128,
    isolated: bool,
    buckets: &mut Buckets,
    position_haircut: Option<&mut PosHaircut>,
    now_slot: u64,
) -> Result<(), &'static str> {
    if delta > 0 {
        if let Some(ph) = position_haircut {
            let gain = if delta > u64::MAX as i128 {
                u64::MAX
            } else {
                delta as u64
            };
            let pre = PositionHaircutSnapshot {
                released_reserve_quote_lots: ph.reserve,
                released_attached_at_slot: ph.attached,
                matured_pos_quote_lots: ph.matured,
                original_reserve_at_attach: ph.original,
            };
            let post = apply_release(pre, gain, now_slot, u64::MAX).map_err(|_| "release")?;
            ph.reserve = post.released_reserve_quote_lots;
            ph.attached = post.released_attached_at_slot;
            ph.matured = post.matured_pos_quote_lots;
            ph.original = post.original_reserve_at_attach;
            return Ok(());
        }
    }
    // Legacy v1 routing.
    if delta > 0 {
        let credit = if delta > u64::MAX as i128 {
            u64::MAX
        } else {
            delta as u64
        };
        if isolated {
            buckets.iso = buckets.iso.checked_add(credit).ok_or("overflow")?;
        } else {
            buckets.cross = buckets.cross.checked_add(credit).ok_or("overflow")?;
        }
    } else if delta < 0 {
        let debit = if delta < -(u64::MAX as i128) {
            u64::MAX
        } else {
            (-delta) as u64
        };
        if isolated {
            buckets.iso = buckets.iso.saturating_sub(debit);
        } else {
            buckets.cross = buckets
                .cross
                .checked_sub(debit)
                .ok_or("insufficient cross")?;
        }
    }
    Ok(())
}

// ─── Scenarios ──────────────────────────────────────────────────────

#[test]
fn legacy_market_gain_credits_collateral_directly() {
    // No haircut state → v1 behavior. Positive delta credits the
    // appropriate bucket immediately.
    let mut b = Buckets { iso: 1_000, cross: 0 };
    dispatch(500, true, &mut b, None, 0).unwrap();
    assert_eq!(b.iso, 1_500);
    assert_eq!(b.cross, 0);
}

#[test]
fn legacy_market_loss_debits_collateral_directly() {
    let mut b = Buckets { iso: 1_000, cross: 5_000 };
    dispatch(-300, false, &mut b, None, 0).unwrap();
    assert_eq!(b.iso, 1_000);
    assert_eq!(b.cross, 4_700);
}

#[test]
fn opted_in_market_gain_routes_to_reserve() {
    // Haircut state present → positive delta goes to reserve, NO
    // collateral mutation.
    let mut b = Buckets { iso: 1_000, cross: 0 };
    let mut ph = PosHaircut::default();
    dispatch(500, true, &mut b, Some(&mut ph), 42).unwrap();
    assert_eq!(b.iso, 1_000, "collateral untouched on opted-in market");
    assert_eq!(ph.reserve, 500);
    assert_eq!(ph.original, 500);
    assert_eq!(ph.attached, 42);
}

#[test]
fn opted_in_market_loss_still_debits_collateral_directly() {
    // Loss seniority: even on opted-in markets, losses bypass the
    // haircut and debit collateral.
    let mut b = Buckets { iso: 1_000, cross: 5_000 };
    let mut ph = PosHaircut::default();
    dispatch(-300, true, &mut b, Some(&mut ph), 42).unwrap();
    assert_eq!(b.iso, 700, "loss debits isolated bucket directly");
    assert_eq!(ph.reserve, 0, "reserve never touched by losses");
    assert_eq!(ph.matured, 0);
}

#[test]
fn opted_in_market_zero_delta_is_noop_with_or_without_haircut() {
    // No-op cases identical between legacy and haircut paths.
    let mut b1 = Buckets { iso: 1_000, cross: 5_000 };
    let mut b2 = Buckets { iso: 1_000, cross: 5_000 };
    let mut ph = PosHaircut::default();
    dispatch(0, true, &mut b1, None, 0).unwrap();
    dispatch(0, true, &mut b2, Some(&mut ph), 0).unwrap();
    assert_eq!(b1.iso, b2.iso);
    assert_eq!(b1.cross, b2.cross);
    assert_eq!(ph.reserve, 0);
}

#[test]
fn sequential_gains_on_opted_in_share_clock() {
    // Two consecutive fills with positive PnL on the same opted-in
    // position combine into one warmup pool; the attachment slot is pulled
    // forward (reserve-weighted) so a later gain can't inherit an elapsed
    // clock (AUDIT HIGH-9): attached = (300*10 + 700*25)/1000 = 20.
    let mut b = Buckets { iso: 1_000, cross: 0 };
    let mut ph = PosHaircut::default();
    dispatch(300, true, &mut b, Some(&mut ph), 10).unwrap();
    dispatch(700, true, &mut b, Some(&mut ph), 25).unwrap();
    assert_eq!(b.iso, 1_000, "collateral untouched across both fills");
    assert_eq!(ph.reserve, 1_000);
    assert_eq!(ph.original, 1_000, "subsequent gain joins warmup pool");
    assert_eq!(ph.attached, 20, "reserve-weighted warmup clock");
}

#[test]
fn gain_then_loss_on_opted_in() {
    // A position can have profit in reserve AND a subsequent loss
    // that debits collateral. The reserve is unaffected by the loss.
    let mut b = Buckets { iso: 1_000, cross: 0 };
    let mut ph = PosHaircut::default();
    dispatch(400, true, &mut b, Some(&mut ph), 5).unwrap();
    assert_eq!(ph.reserve, 400);
    dispatch(-200, true, &mut b, Some(&mut ph), 6).unwrap();
    assert_eq!(b.iso, 800, "loss debited collateral");
    assert_eq!(ph.reserve, 400, "reserve survives loss intact");
}

#[test]
fn cross_isolated_routing_under_opted_in_is_orthogonal_to_haircut() {
    // The isolated/cross flag still determines collateral bucket for
    // losses on opted-in markets — only gains route to reserve, and
    // gains don't pick a bucket.
    let mut b = Buckets { iso: 0, cross: 5_000 };
    let mut ph = PosHaircut::default();
    // Cross loss debits cross.
    dispatch(-300, false, &mut b, Some(&mut ph), 0).unwrap();
    assert_eq!(b.iso, 0);
    assert_eq!(b.cross, 4_700);
    // Cross gain (would normally credit cross, but goes to reserve).
    dispatch(200, false, &mut b, Some(&mut ph), 0).unwrap();
    assert_eq!(b.cross, 4_700, "cross unchanged — gain went to reserve");
    assert_eq!(ph.reserve, 200);
}

#[test]
fn legacy_path_unchanged_under_any_isolated_cross_combination() {
    // The legacy path must behave bit-for-bit as before. Regression
    // guard against accidentally diverging the no-haircut path.
    // Isolated + gain
    let mut b = Buckets { iso: 100, cross: 200 };
    dispatch(50, true, &mut b, None, 0).unwrap();
    assert_eq!(b, Buckets { iso: 150, cross: 200 });
    // Cross + gain
    let mut b = Buckets { iso: 100, cross: 200 };
    dispatch(50, false, &mut b, None, 0).unwrap();
    assert_eq!(b, Buckets { iso: 100, cross: 250 });
    // Isolated + loss (saturating)
    let mut b = Buckets { iso: 30, cross: 200 };
    dispatch(-50, true, &mut b, None, 0).unwrap();
    assert_eq!(b, Buckets { iso: 0, cross: 200 });
    // Cross + loss
    let mut b = Buckets { iso: 30, cross: 200 };
    dispatch(-50, false, &mut b, None, 0).unwrap();
    assert_eq!(b, Buckets { iso: 30, cross: 150 });
}

impl PartialEq for Buckets {
    fn eq(&self, other: &Self) -> bool {
        self.iso == other.iso && self.cross == other.cross
    }
}
impl Eq for Buckets {}
