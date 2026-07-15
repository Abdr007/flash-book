//! Exercises the *release path* end-to-end at the algorithmic layer:
//!
//!   trader gain credited to collateral (baseline flow)
//!     → release_gain_to_haircut moves credit into reserve
//!     → mature_position drains reserve → matured
//!     → convert_position credits collateral at current h
//!     → flush_haircut_dust moves dust accounting to insurance
//!
//! Plus residual-delta accounting under `seed_residual`.
//!
//! Mirrors the on-chain handler bodies as plain mutations over the
//! same field shapes. Layered on top of the pure module
//! (`matcher::haircut`) so the proofs already established there flow
//! through.

use clober::matcher::haircut::{
    apply_convert, apply_mature, apply_release, apply_residual_delta, compute_h,
    PositionHaircutSnapshot,
};

// ─── Mirror of the on-chain state ───────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct PositionBuckets {
    /// `position.collateral_quote_lots` — isolated bucket. > 0 means
    /// isolated mode; 0 means cross.
    isolated_collateral: u64,
    /// `trader_state.collateral_quote_lots` — cross bucket.
    cross_collateral: u64,
}

#[derive(Debug, Clone, Copy)]
struct MarketHaircut {
    residual: u128,
    matured_pos_total: u128,
    dust_accrued: u128,
    h_min: u64,
    h_max: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PositionHaircut {
    reserve: u64,
    attached_at_slot: u64,
    matured: u64,
    original: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Insurance {
    balance: u64,
}

fn init_market(h_min: u64, h_max: u64, initial_residual: u128) -> MarketHaircut {
    MarketHaircut {
        residual: initial_residual,
        matured_pos_total: 0,
        dust_accrued: 0,
        h_min,
        h_max,
    }
}

// Mirror of `release_gain_to_haircut`. Same routing rule as
// compute_realized_pnl_routing: isolated bucket if > 0, else cross.
fn ix_release_gain_to_haircut(
    buckets: &mut PositionBuckets,
    pos: &mut PositionHaircut,
    gain: u64,
    now_slot: u64,
) -> Result<bool, &'static str> {
    let isolated = buckets.isolated_collateral > 0;
    if isolated {
        buckets.isolated_collateral = buckets
            .isolated_collateral
            .checked_sub(gain)
            .ok_or("insufficient isolated collateral")?;
    } else {
        buckets.cross_collateral = buckets
            .cross_collateral
            .checked_sub(gain)
            .ok_or("insufficient cross collateral")?;
    }
    let pre = PositionHaircutSnapshot {
        released_reserve_quote_lots: pos.reserve,
        released_attached_at_slot: pos.attached_at_slot,
        matured_pos_quote_lots: pos.matured,
        original_reserve_at_attach: pos.original,
    };
    let post = apply_release(pre, gain, now_slot, u64::MAX).unwrap();
    pos.reserve = post.released_reserve_quote_lots;
    pos.attached_at_slot = post.released_attached_at_slot;
    pos.matured = post.matured_pos_quote_lots;
    pos.original = post.original_reserve_at_attach;
    Ok(isolated)
}

fn ix_mature(pos: &mut PositionHaircut, market: &mut MarketHaircut, now_slot: u64) -> u64 {
    let pre = PositionHaircutSnapshot {
        released_reserve_quote_lots: pos.reserve,
        released_attached_at_slot: pos.attached_at_slot,
        matured_pos_quote_lots: pos.matured,
        original_reserve_at_attach: pos.original,
    };
    let (post, delta) = apply_mature(pre, now_slot, market.h_min, market.h_max).unwrap();
    pos.reserve = post.released_reserve_quote_lots;
    pos.attached_at_slot = post.released_attached_at_slot;
    pos.matured = post.matured_pos_quote_lots;
    pos.original = post.original_reserve_at_attach;
    market.matured_pos_total = market.matured_pos_total.checked_add(delta as u128).unwrap();
    delta
}

// Mirror of convert_position + the apply_fill wire-in that lands credit
// into the trader's collateral bucket. (The release ix itself doesn't
// land the credit yet; this test layer simulates the full intended
// flow.)
fn ix_convert_and_credit(
    buckets: &mut PositionBuckets,
    pos: &mut PositionHaircut,
    market: &mut MarketHaircut,
    isolated: bool,
) -> (u64, u64) {
    let pre = PositionHaircutSnapshot {
        released_reserve_quote_lots: pos.reserve,
        released_attached_at_slot: pos.attached_at_slot,
        matured_pos_quote_lots: pos.matured,
        original_reserve_at_attach: pos.original,
    };
    let matured_at_call = pre.matured_pos_quote_lots;
    let h = compute_h(market.residual, market.matured_pos_total);
    let (post, credit, dust) = apply_convert(pre, h);
    pos.matured = post.matured_pos_quote_lots;

    // Land the credit in the appropriate bucket.
    if isolated {
        buckets.isolated_collateral = buckets.isolated_collateral.checked_add(credit).unwrap();
    } else {
        buckets.cross_collateral = buckets.cross_collateral.checked_add(credit).unwrap();
    }

    market.matured_pos_total -= matured_at_call as u128;
    market.dust_accrued = market.dust_accrued.checked_add(dust as u128).unwrap();
    market.residual = market.residual.checked_sub(credit as u128).unwrap();
    (credit, dust)
}

fn ix_flush_dust(market: &mut MarketHaircut, ins: &mut Insurance) -> u64 {
    let dust = market.dust_accrued as u64;
    ins.balance = ins.balance.checked_add(dust).unwrap();
    market.dust_accrued -= dust as u128;
    dust
}

fn ix_seed_residual(market: &mut MarketHaircut, delta: i128) -> Result<(), &'static str> {
    let new_r = apply_residual_delta(market.residual, delta).map_err(|_| "underflow")?;
    market.residual = new_r;
    Ok(())
}

// ─── Scenarios ──────────────────────────────────────────────────────

#[test]
fn full_release_lifecycle_isolated_position_fully_backed() {
    // Trader has an isolated position with $10,000 collateral. They
    // realize +$1,000 PnL → it credits to the isolated bucket. The
    // sequencer then releases that gain into the haircut reserve.
    // Market is fully solvent → trader gets 100% of it back after
    // warmup.
    let mut market = init_market(10, 100, 10_000);
    let mut buckets = PositionBuckets {
        isolated_collateral: 10_000 + 1_000, // pre-existing + realized gain
        cross_collateral: 0,
    };
    let mut pos = PositionHaircut::default();
    let mut ins = Insurance::default();

    let isolated = ix_release_gain_to_haircut(&mut buckets, &mut pos, 1_000, 0).unwrap();
    assert!(isolated);
    assert_eq!(
        buckets.isolated_collateral, 10_000,
        "gain moved out of bucket"
    );
    assert_eq!(pos.reserve, 1_000);

    // Wait through warmup, mature.
    ix_mature(&mut pos, &mut market, 100);
    assert_eq!(pos.matured, 1_000);
    assert_eq!(market.matured_pos_total, 1_000);

    // Convert: residual is $10k vs matured $1k → h = 1 → full credit.
    let (credit, dust) = ix_convert_and_credit(&mut buckets, &mut pos, &mut market, true);
    assert_eq!(credit, 1_000);
    assert_eq!(dust, 0);
    assert_eq!(buckets.isolated_collateral, 11_000, "credit landed back");
    assert_eq!(market.residual, 9_000);
    assert_eq!(market.dust_accrued, 0);

    // No dust to flush.
    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 0);
}

#[test]
fn full_release_lifecycle_cross_position_stressed_market() {
    // Same shape but cross collateral + h < 1.
    // Residual = $500, matured = $1000 → h = 0.5 → trader gets half;
    // dust accrues to insurance.
    let mut market = init_market(0, 1, 500);
    let mut buckets = PositionBuckets {
        isolated_collateral: 0,
        cross_collateral: 10_000 + 1_000,
    };
    let mut pos = PositionHaircut::default();
    let mut ins = Insurance::default();

    let isolated = ix_release_gain_to_haircut(&mut buckets, &mut pos, 1_000, 0).unwrap();
    assert!(!isolated, "cross routing when isolated bucket is 0");
    assert_eq!(buckets.cross_collateral, 10_000);

    ix_mature(&mut pos, &mut market, 1);
    let (credit, dust) = ix_convert_and_credit(&mut buckets, &mut pos, &mut market, false);
    assert_eq!(credit, 500);
    assert_eq!(dust, 500);
    assert_eq!(buckets.cross_collateral, 10_500, "half credit landed");
    assert_eq!(market.residual, 0, "all residual consumed");

    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 500);
    assert_eq!(ins.balance, 500);
}

#[test]
fn release_blocks_when_collateral_insufficient() {
    let _market = init_market(0, 1, 10_000);
    let mut buckets = PositionBuckets {
        isolated_collateral: 100, // not enough to cover the requested release
        cross_collateral: 0,
    };
    let mut pos = PositionHaircut::default();
    let r = ix_release_gain_to_haircut(&mut buckets, &mut pos, 1_000, 0);
    assert!(r.is_err());
    assert_eq!(
        buckets.isolated_collateral, 100,
        "no state mutation on failure"
    );
}

#[test]
fn seed_residual_signed_deltas() {
    let mut market = init_market(0, 1, 1_000);

    ix_seed_residual(&mut market, 500).unwrap();
    assert_eq!(market.residual, 1_500);

    ix_seed_residual(&mut market, -300).unwrap();
    assert_eq!(market.residual, 1_200);

    // Underflow blocked.
    let r = ix_seed_residual(&mut market, -10_000);
    assert!(r.is_err());
    assert_eq!(market.residual, 1_200, "no mutation on underflow");
}

#[test]
fn many_releases_share_warmup_clock() {
    // Two gains released at different slots within the same warmup combine
    // into one reserve, and the attachment slot is pulled forward
    // (reserve-weighted) so a late gain cannot inherit an elapsed clock.
    // Their combined original amount matures on that clock.
    let mut market = init_market(10, 100, 100_000);
    let mut buckets = PositionBuckets {
        isolated_collateral: 5_000,
        cross_collateral: 0,
    };
    let mut pos = PositionHaircut::default();

    ix_release_gain_to_haircut(&mut buckets, &mut pos, 1_000, 50).unwrap();
    assert_eq!(pos.attached_at_slot, 50);
    assert_eq!(pos.original, 1_000);

    // A second release pulls the warmup clock FORWARD,
    // reserve-weighted, so a large late gain can't inherit an already-elapsed
    // clock and mature instantly:
    //   attached' = (1000*50 + 500*80) / (1000+500) = 90000/1500 = 60.
    ix_release_gain_to_haircut(&mut buckets, &mut pos, 500, 80).unwrap();
    assert_eq!(pos.attached_at_slot, 60, "reserve-weighted warmup clock");
    assert_eq!(pos.original, 1_500);
    assert_eq!(pos.reserve, 1_500);

    // At slot 100, elapsed since 60 = 40, fraction = (40-10)/(100-10) = 1/3.
    // Target cumulative = 1500 × 1/3 = 500.
    ix_mature(&mut pos, &mut market, 100);
    assert_eq!(pos.matured, 500);
    assert_eq!(pos.reserve, 1_500 - 500);

    // At slot 200 (well past h_max), the rest matures.
    ix_mature(&mut pos, &mut market, 200);
    assert_eq!(pos.matured, 1_500);
    assert_eq!(pos.reserve, 0);
}

#[test]
fn convert_credit_can_be_zero_when_residual_is_zero() {
    let mut market = init_market(0, 1, 0); // no residual at all
    let mut buckets = PositionBuckets {
        isolated_collateral: 5_000,
        cross_collateral: 0,
    };
    let mut pos = PositionHaircut::default();
    let mut ins = Insurance::default();

    ix_release_gain_to_haircut(&mut buckets, &mut pos, 1_000, 0).unwrap();
    ix_mature(&mut pos, &mut market, 1);
    let (credit, dust) = ix_convert_and_credit(&mut buckets, &mut pos, &mut market, true);
    assert_eq!(credit, 0, "h=0 → no credit");
    assert_eq!(dust, 1_000, "everything becomes dust");
    assert_eq!(market.residual, 0);

    // All dust → insurance.
    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 1_000);
    assert_eq!(ins.balance, 1_000);
}

#[test]
fn multi_position_solvency_under_partial_backing() {
    // Two positions release $1000 each. Residual is $1500 ⇒ h = 0.75.
    // Each gets 750 (total credit 1500 = residual). Dust = 500 total.
    let mut market = init_market(0, 1, 1_500);
    let mut buckets_a = PositionBuckets {
        isolated_collateral: 1_000,
        cross_collateral: 0,
    };
    let mut buckets_b = PositionBuckets {
        isolated_collateral: 1_000,
        cross_collateral: 0,
    };
    let mut pos_a = PositionHaircut::default();
    let mut pos_b = PositionHaircut::default();
    let mut ins = Insurance::default();

    ix_release_gain_to_haircut(&mut buckets_a, &mut pos_a, 1_000, 0).unwrap();
    ix_release_gain_to_haircut(&mut buckets_b, &mut pos_b, 1_000, 0).unwrap();
    ix_mature(&mut pos_a, &mut market, 1);
    ix_mature(&mut pos_b, &mut market, 1);

    // Both convert in sequence. h is computed against the global matured.
    let (credit_a, _) = ix_convert_and_credit(&mut buckets_a, &mut pos_a, &mut market, true);
    let (credit_b, _) = ix_convert_and_credit(&mut buckets_b, &mut pos_b, &mut market, true);

    // h = 1500/2000 = 0.75, applied to each 1000 → 750 each.
    assert_eq!(credit_a, 750);
    assert_eq!(credit_b, 750);
    // Total extracted equals initial residual.
    assert_eq!(credit_a + credit_b, 1_500);
    assert_eq!(market.residual, 0);

    // Dust = 250 + 250 = 500.
    assert_eq!(market.dust_accrued, 500);
    let flushed = ix_flush_dust(&mut market, &mut ins);
    assert_eq!(flushed, 500);
}

#[test]
fn residual_grows_back_when_seeded_during_warmup() {
    // Mid-warmup, authority seeds more residual (e.g. fees accrued).
    // The trader gets a better h at convert time.
    let mut market = init_market(10, 100, 500);
    let mut buckets = PositionBuckets {
        isolated_collateral: 1_000,
        cross_collateral: 0,
    };
    let mut pos = PositionHaircut::default();

    ix_release_gain_to_haircut(&mut buckets, &mut pos, 1_000, 0).unwrap();
    ix_mature(&mut pos, &mut market, 100);
    // h would be 0.5 if we converted now.

    // Seed more residual (simulating fee accrual to LP).
    ix_seed_residual(&mut market, 500).unwrap();
    // h is now 1.0.

    let (credit, dust) = ix_convert_and_credit(&mut buckets, &mut pos, &mut market, true);
    assert_eq!(credit, 1_000);
    assert_eq!(dust, 0);
}

#[test]
fn convert_isolated_lands_credit_in_isolated_bucket() {
    let mut market = init_market(0, 1, 10_000);
    let mut buckets = PositionBuckets {
        isolated_collateral: 5_000, // isolated mode
        cross_collateral: 100,      // cross has some, but isolated picks first
    };
    let mut pos = PositionHaircut::default();

    let isolated = ix_release_gain_to_haircut(&mut buckets, &mut pos, 1_000, 0).unwrap();
    assert!(isolated);
    ix_mature(&mut pos, &mut market, 1);
    let (credit, _) = ix_convert_and_credit(&mut buckets, &mut pos, &mut market, isolated);
    // Credit goes back to isolated bucket (not cross).
    assert_eq!(buckets.isolated_collateral, 4_000 + credit);
    assert_eq!(buckets.cross_collateral, 100, "cross untouched");
}
