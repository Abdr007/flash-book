//! State-transition tests for the four haircut instruction handlers,
//! exercised at the algorithmic layer.
//!
//! These tests don't spin up `solana-program-test`. They model the ix
//! bodies as plain mutations on the same account-struct types the
//! handlers operate on, asserting the haircut conservation laws
//! end-to-end:
//!
//!   - initialize_haircut_state seeds (residual, h_min, h_max) correctly
//!   - apply_release accumulates to reserve
//!   - mature_position drains reserve → matured + bumps market total
//!   - convert_position floor-credits + dust + decrements residual
//!   - flush_haircut_dust moves accounting to insurance
//!
//! Cross-checks against the proven math in `matcher::haircut`:
//! conservation, monotonicity, solvency, flat-account safety remain.

use clober::matcher::haircut::{
    apply_convert, apply_mature, apply_release, compute_h, PositionHaircutSnapshot,
};

/// Mirror of the on-chain `MarketHaircutStateAccount` body (no Anchor
/// disc, no padding) — just the fields the ix mutate.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct MarketHaircutState {
    residual: u128,
    matured_pos_total: u128,
    realized_loss_total: u128,
    dust_accrued: u128,
    h_min: u64,
    h_max: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PositionHaircutState {
    reserve: u64,
    attached_at_slot: u64,
    matured: u64,
    original_reserve_at_attach: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct InsuranceFund {
    balance: u64,
    total_contributions: u64,
}

// ─── Ix-body mirrors ────────────────────────────────────────────────

fn ix_initialize(h_min: u64, h_max: u64, initial_residual: u128) -> MarketHaircutState {
    // Same validation the real ix runs.
    clober::matcher::haircut::validate_market_params(h_min, h_max).unwrap();
    MarketHaircutState {
        residual: initial_residual,
        matured_pos_total: 0,
        realized_loss_total: 0,
        dust_accrued: 0,
        h_min,
        h_max,
    }
}

/// Release-path proxy: positive realized PnL bypasses
/// `apply_realized_pnl_direct`'s collateral credit and adds to the
/// position's reserve instead (the algorithm `apply_fill` routes
/// through on haircut-enabled markets).
fn ix_release(pos: &mut PositionHaircutState, gain: u64, now_slot: u64) {
    let pre = PositionHaircutSnapshot {
        released_reserve_quote_lots: pos.reserve,
        released_attached_at_slot: pos.attached_at_slot,
        matured_pos_quote_lots: pos.matured,
        original_reserve_at_attach: pos.original_reserve_at_attach,
    };
    let post = apply_release(pre, gain, now_slot, u64::MAX).unwrap();
    pos.reserve = post.released_reserve_quote_lots;
    pos.attached_at_slot = post.released_attached_at_slot;
    pos.matured = post.matured_pos_quote_lots;
    pos.original_reserve_at_attach = post.original_reserve_at_attach;
}

fn ix_mature(
    pos: &mut PositionHaircutState,
    market: &mut MarketHaircutState,
    now_slot: u64,
) -> u64 {
    let pre = PositionHaircutSnapshot {
        released_reserve_quote_lots: pos.reserve,
        released_attached_at_slot: pos.attached_at_slot,
        matured_pos_quote_lots: pos.matured,
        original_reserve_at_attach: pos.original_reserve_at_attach,
    };
    let (post, delta) = apply_mature(pre, now_slot, market.h_min, market.h_max).unwrap();
    pos.reserve = post.released_reserve_quote_lots;
    pos.attached_at_slot = post.released_attached_at_slot;
    pos.matured = post.matured_pos_quote_lots;
    pos.original_reserve_at_attach = post.original_reserve_at_attach;
    market.matured_pos_total = market.matured_pos_total.checked_add(delta as u128).unwrap();
    delta
}

fn ix_convert(pos: &mut PositionHaircutState, market: &mut MarketHaircutState) -> (u64, u64) {
    let pre = PositionHaircutSnapshot {
        released_reserve_quote_lots: pos.reserve,
        released_attached_at_slot: pos.attached_at_slot,
        matured_pos_quote_lots: pos.matured,
        original_reserve_at_attach: pos.original_reserve_at_attach,
    };
    let matured_at_call = pre.matured_pos_quote_lots;
    let h_scaled = compute_h(market.residual, market.matured_pos_total);
    let (post, credit, dust) = apply_convert(pre, h_scaled);
    pos.matured = post.matured_pos_quote_lots;
    market.matured_pos_total -= matured_at_call as u128;
    market.dust_accrued = market.dust_accrued.checked_add(dust as u128).unwrap();
    market.residual = market.residual.checked_sub(credit as u128).unwrap();
    (credit, dust)
}

fn ix_flush_dust(market: &mut MarketHaircutState, insurance: &mut InsuranceFund) -> u64 {
    let dust = market.dust_accrued;
    let dust_u64 = if dust > u64::MAX as u128 {
        u64::MAX
    } else {
        dust as u64
    };
    insurance.balance = insurance.balance.checked_add(dust_u64).unwrap();
    insurance.total_contributions = insurance.total_contributions.checked_add(dust_u64).unwrap();
    market.dust_accrued -= dust_u64 as u128;
    dust_u64
}

// ─── Scenarios ──────────────────────────────────────────────────────

#[test]
fn happy_path_full_lifecycle_fully_backed() {
    // Initialize, release, mature, convert, flush. Residual fully
    // backs the matured profit ⇒ trader gets 100% credit, no dust.
    let mut market = ix_initialize(10, 100, 10_000);
    let mut pos = PositionHaircutState::default();
    let mut ins = InsuranceFund::default();

    ix_release(&mut pos, 1_000, 0);
    assert_eq!(pos.reserve, 1_000);
    assert_eq!(pos.attached_at_slot, 0);

    // Cannot mature before warmup starts.
    let pre_mature_delta = ix_mature(&mut pos, &mut market, 5);
    assert_eq!(pre_mature_delta, 0);

    // Fully matured at slot 100 (attached=0, h_max=100).
    let delta = ix_mature(&mut pos, &mut market, 100);
    assert_eq!(delta, 1_000);
    assert_eq!(pos.reserve, 0);
    assert_eq!(pos.matured, 1_000);
    assert_eq!(market.matured_pos_total, 1_000);

    let (credit, dust) = ix_convert(&mut pos, &mut market);
    assert_eq!(credit, 1_000, "fully backed → full credit");
    assert_eq!(dust, 0);
    assert_eq!(market.residual, 9_000);
    assert_eq!(market.matured_pos_total, 0);

    // No dust to flush.
    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 0);
}

#[test]
fn stressed_market_produces_dust() {
    // Residual = 500, MaturedPos = 1000 → h = 0.5 → half credit, half
    // dust. Dust accumulates and can be flushed to insurance.
    let mut market = ix_initialize(0, 1, 500);
    let mut pos = PositionHaircutState::default();
    let mut ins = InsuranceFund::default();

    ix_release(&mut pos, 1_000, 0);
    ix_mature(&mut pos, &mut market, 1);
    let (credit, dust) = ix_convert(&mut pos, &mut market);
    assert_eq!(credit, 500);
    assert_eq!(dust, 500);
    assert_eq!(market.residual, 0);
    assert_eq!(market.dust_accrued, 500);

    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 500);
    assert_eq!(ins.balance, 500);
    assert_eq!(ins.total_contributions, 500);
    assert_eq!(market.dust_accrued, 0);
}

#[test]
fn multiple_positions_share_haircut_uniformly() {
    // Two positions, each matures 1000 of profit. Residual is only 1000
    // total ⇒ h = 0.5 ⇒ each gets 500 credit, 500 dust.
    let mut market = ix_initialize(0, 1, 1_000);
    let mut a = PositionHaircutState::default();
    let mut b = PositionHaircutState::default();

    ix_release(&mut a, 1_000, 0);
    ix_release(&mut b, 1_000, 0);
    ix_mature(&mut a, &mut market, 1);
    ix_mature(&mut b, &mut market, 1);
    assert_eq!(market.matured_pos_total, 2_000);

    let (credit_a, _) = ix_convert(&mut a, &mut market);
    // After A converts: residual = 500, matured_total = 1000 → h = 0.5.
    // But A's convert used the h sampled BEFORE its own subtraction →
    // h = min(1000, 2000) / 2000 = 0.5. A gets 500.
    assert_eq!(credit_a, 500);

    let (credit_b, _) = ix_convert(&mut b, &mut market);
    // After A: residual = 500, matured = 1000 → h still 0.5 → B gets 500.
    assert_eq!(credit_b, 500);

    // Total extracted = initial residual.
    assert_eq!(market.residual, 0);
}

#[test]
fn solvency_under_random_release_sequence() {
    // Many releases on one position, mature/convert at the end. No
    // matter the gain sequence, total extracted never exceeds initial
    // Residual.
    let initial_residual: u128 = 5_000;
    let mut market = ix_initialize(0, 1, initial_residual);
    let mut pos = PositionHaircutState::default();

    let gains = [123u64, 4567, 89, 1000, 222, 333, 4500, 999];
    let mut total_gain: u128 = 0;
    for (i, g) in gains.iter().enumerate() {
        ix_release(&mut pos, *g, i as u64);
        total_gain += *g as u128;
    }
    ix_mature(&mut pos, &mut market, 10);
    let (credit, dust) = ix_convert(&mut pos, &mut market);

    // Solvency: credit ≤ initial_residual.
    assert!((credit as u128) <= initial_residual);
    // Conservation: credit + dust = total_gain (since fully matured).
    assert_eq!(credit as u128 + dust as u128, total_gain);
    // Residual was decremented exactly by credit.
    assert_eq!(market.residual, initial_residual - credit as u128);
}

#[test]
fn mature_idempotent_at_same_slot() {
    // Calling mature twice at the same slot must not double-credit.
    let mut market = ix_initialize(0, 100, 10_000);
    let mut pos = PositionHaircutState::default();

    ix_release(&mut pos, 1_000, 0);
    let d1 = ix_mature(&mut pos, &mut market, 50);
    let d2 = ix_mature(&mut pos, &mut market, 50);
    assert!(d1 > 0);
    assert_eq!(d2, 0, "second mature at same slot is a no-op");
}

#[test]
fn convert_idempotent_after_drain() {
    // Convert twice — second call has nothing to do.
    let mut market = ix_initialize(0, 1, 10_000);
    let mut pos = PositionHaircutState::default();

    ix_release(&mut pos, 1_000, 0);
    ix_mature(&mut pos, &mut market, 1);
    let (c1, _) = ix_convert(&mut pos, &mut market);
    let (c2, d2) = ix_convert(&mut pos, &mut market);
    assert_eq!(c1, 1_000);
    assert_eq!(c2, 0);
    assert_eq!(d2, 0);
}

#[test]
fn dust_can_be_flushed_in_chunks_across_ix_calls() {
    // Dust accrues across multiple converts; flushed in one tx-like call.
    let mut market = ix_initialize(0, 1, 100);
    let mut a = PositionHaircutState::default();
    let mut b = PositionHaircutState::default();
    let mut ins = InsuranceFund::default();

    ix_release(&mut a, 1_000, 0);
    ix_mature(&mut a, &mut market, 1);
    // After A matures: matured_total=1000, residual=100, h = 0.1
    let (ca, da) = ix_convert(&mut a, &mut market);
    assert_eq!(ca, 100);
    assert_eq!(da, 900);

    // B's turn — but residual is now 0; h = 0; everything is dust.
    ix_release(&mut b, 500, 0);
    ix_mature(&mut b, &mut market, 1);
    let (cb, db) = ix_convert(&mut b, &mut market);
    assert_eq!(cb, 0);
    assert_eq!(db, 500);

    // Flush all accumulated dust.
    assert_eq!(market.dust_accrued, 900 + 500);
    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 1_400);
    assert_eq!(ins.balance, 1_400);
}

#[test]
fn flat_account_never_extracts_anything() {
    // A position that never had a positive PnL never affects anything.
    let mut market = ix_initialize(10, 100, 10_000);
    let mut pos = PositionHaircutState::default();
    let mut ins = InsuranceFund::default();

    // No release. Mature is a no-op.
    let d = ix_mature(&mut pos, &mut market, 1_000);
    assert_eq!(d, 0);

    // Convert is also a no-op (no matured).
    // We'd need to handle the require!(matured > 0) error path in the
    // real ix; here we just check the state stays clean.
    assert_eq!(pos.reserve, 0);
    assert_eq!(pos.matured, 0);
    assert_eq!(market.residual, 10_000);
    assert_eq!(market.dust_accrued, 0);

    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 0);
}

#[test]
fn partial_warmup_keeps_residual_consistent() {
    // Halfway through the warmup, only part of the reserve has
    // matured. Convert at that point credits the matured portion only;
    // the rest stays in reserve for later.
    let mut market = ix_initialize(10, 100, 100_000);
    let mut pos = PositionHaircutState::default();

    ix_release(&mut pos, 1_000, 100);
    // At slot 155, elapsed = 55, fraction = (55-10)/(100-10) = 0.5
    let delta_half = ix_mature(&mut pos, &mut market, 155);
    assert_eq!(delta_half, 500);
    assert_eq!(pos.reserve, 500);
    assert_eq!(pos.matured, 500);

    // Convert the matured half.
    let (credit_half, dust_half) = ix_convert(&mut pos, &mut market);
    assert_eq!(credit_half, 500);
    assert_eq!(dust_half, 0);
    assert_eq!(market.residual, 99_500);

    // Later: rest matures.
    let delta_rest = ix_mature(&mut pos, &mut market, 1_000);
    assert_eq!(delta_rest, 500);
    let (credit_rest, _) = ix_convert(&mut pos, &mut market);
    assert_eq!(credit_rest, 500);
    assert_eq!(market.residual, 99_000);
}
