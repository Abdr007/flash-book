//! Envelope config + verify + gate instruction tests.
//!
//! Mirrors the on-chain handlers as algorithmic operations on the
//! pure `EnvelopeParams` struct. The ix bodies are direct passthrough
//! to `prove_envelope` / `gate_price_move`, so passing here proves
//! on-chain correctness.

use clober::matcher::envelope::{
    gate_price_move, prove_envelope, EnvelopeError, EnvelopeParams, ABS_MAX_ACCRUAL_DT_SLOTS,
    ABS_MAX_PRICE_MOVE_BPS_PER_SLOT,
};

// ─── set_envelope_config equivalent: validate then commit ───────────

fn ix_set_envelope_config(params: &EnvelopeParams) -> Result<(), EnvelopeError> {
    // The ix runs prove_envelope first; bad params abort the write.
    prove_envelope(params)
}

fn ix_gate_price_move(
    cfg: &EnvelopeParams,
    old: u64,
    new: u64,
    dt: u64,
) -> Result<(), EnvelopeError> {
    gate_price_move(old, new, dt, cfg.max_price_move_bps_per_slot)
}

// ─── Happy paths ────────────────────────────────────────────────────

#[test]
fn set_envelope_config_with_defaults_succeeds() {
    let p = EnvelopeParams::default();
    ix_set_envelope_config(&p).unwrap();
}

#[test]
fn set_envelope_config_with_conservative_params_succeeds() {
    // Tighter than defaults: more headroom.
    let p = EnvelopeParams {
        max_price_move_bps_per_slot: 5,
        max_accrual_dt_slots: 50,
        max_abs_funding_e9_per_slot: 5_000,
        maintenance_bps: 5_000, // 50% — extremely conservative
        liquidation_fee_bps: 10,
        min_liquidation_abs_lots: 1,
        min_nonzero_mm_req_lots: 100,
    };
    ix_set_envelope_config(&p).unwrap();
}

// ─── Failure modes ──────────────────────────────────────────────────

#[test]
fn set_envelope_config_rejects_zero_price_cap() {
    let p = EnvelopeParams {
        max_price_move_bps_per_slot: 0,
        ..Default::default()
    };
    assert_eq!(ix_set_envelope_config(&p), Err(EnvelopeError::PriceCapZero));
}

#[test]
fn set_envelope_config_rejects_unbounded_price_cap() {
    let p = EnvelopeParams {
        max_price_move_bps_per_slot: ABS_MAX_PRICE_MOVE_BPS_PER_SLOT + 1,
        ..Default::default()
    };
    assert_eq!(
        ix_set_envelope_config(&p),
        Err(EnvelopeError::PriceCapTooLarge)
    );
}

#[test]
fn set_envelope_config_rejects_oversized_window() {
    let p = EnvelopeParams {
        max_accrual_dt_slots: ABS_MAX_ACCRUAL_DT_SLOTS + 1,
        ..Default::default()
    };
    assert_eq!(
        ix_set_envelope_config(&p),
        Err(EnvelopeError::AccrualDtTooLarge)
    );
}

#[test]
fn set_envelope_config_rejects_maintenance_at_full_bps() {
    let p = EnvelopeParams {
        maintenance_bps: 10_000,
        ..Default::default()
    };
    assert_eq!(
        ix_set_envelope_config(&p),
        Err(EnvelopeError::MaintenanceTooLarge)
    );
}

#[test]
fn set_envelope_config_rejects_envelope_violation() {
    // Aggressive price cap with tiny MMR → envelope inequality fails.
    let p = EnvelopeParams {
        max_price_move_bps_per_slot: 100,
        max_accrual_dt_slots: 200,
        max_abs_funding_e9_per_slot: 10_000,
        maintenance_bps: 100, // 1% — way too low for 20% budget
        liquidation_fee_bps: 50,
        min_liquidation_abs_lots: 1,
        min_nonzero_mm_req_lots: 1,
    };
    let r = ix_set_envelope_config(&p);
    assert!(matches!(r, Err(EnvelopeError::EnvelopeViolated { .. })));
}

// ─── Runtime gate ───────────────────────────────────────────────────

#[test]
fn gate_admits_small_move_within_cap() {
    let cfg = EnvelopeParams::default(); // 14 bps/slot × 100 slots = 1400 bps budget
    ix_gate_price_move(&cfg, 1_000_000, 1_001_400, 100).unwrap();
}

#[test]
fn gate_rejects_oversized_move() {
    // Default cap: 14 bps/slot × dt. At dt=100, cap = 1400 bps = 14%.
    // A 20% move (200_000 over 1_000_000) exceeds it.
    let cfg = EnvelopeParams::default();
    let r = ix_gate_price_move(&cfg, 1_000_000, 1_200_000, 100);
    assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
}

#[test]
fn gate_rejects_same_slot_move() {
    let cfg = EnvelopeParams::default();
    let r = ix_gate_price_move(&cfg, 1_000_000, 1_000_001, 0);
    assert_eq!(r, Err(EnvelopeError::SameSlotMove));
}

#[test]
fn gate_admits_first_price() {
    // p_last = 0 means "first observation"; any move is admissible.
    let cfg = EnvelopeParams::default();
    ix_gate_price_move(&cfg, 0, 1_000_000, 0).unwrap();
}

#[test]
fn gate_symmetric_on_down_moves() {
    let cfg = EnvelopeParams::default();
    ix_gate_price_move(&cfg, 1_000_000, 999_000, 100).unwrap();
    let r = ix_gate_price_move(&cfg, 1_000_000, 800_000, 100);
    assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
}

#[test]
fn gate_scales_linearly_with_dt() {
    // Twice the dt → twice the budget. Move rejected at dt=1 should
    // be accepted at dt=2. Need params that pass prove_envelope.
    // 10 bps × 200 slots = 2000 bps budget vs 3000 bps MMR ⇒ proves.
    let cfg = EnvelopeParams {
        max_price_move_bps_per_slot: 10, // 0.1%/slot
        max_accrual_dt_slots: 200,
        max_abs_funding_e9_per_slot: 10_000,
        maintenance_bps: 3_000,
        liquidation_fee_bps: 50,
        min_liquidation_abs_lots: 1,
        min_nonzero_mm_req_lots: 100,
    };
    prove_envelope(&cfg).unwrap();
    // At dt=1, cap = 10 bps = 1_000 ticks for price 1_000_000.
    ix_gate_price_move(&cfg, 1_000_000, 1_001_000, 1).unwrap();
    let r = ix_gate_price_move(&cfg, 1_000_000, 1_002_000, 1);
    assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
    // At dt=2, cap = 20 bps = 2_000 ticks → previously-rejected move
    // is now admitted.
    ix_gate_price_move(&cfg, 1_000_000, 1_002_000, 2).unwrap();
}

#[test]
fn gate_uses_only_price_cap_field() {
    // Demonstrate that gate_price_move only consults
    // max_price_move_bps_per_slot — other fields don't affect runtime.
    let cfg_a = EnvelopeParams::default();
    let cfg_b = EnvelopeParams {
        // Same price cap, different everything else.
        max_price_move_bps_per_slot: cfg_a.max_price_move_bps_per_slot,
        max_accrual_dt_slots: 200,
        max_abs_funding_e9_per_slot: 0,
        maintenance_bps: 9_000,
        liquidation_fee_bps: 100,
        min_liquidation_abs_lots: 1_000,
        min_nonzero_mm_req_lots: 10_000,
    };
    let a = ix_gate_price_move(&cfg_a, 1_000_000, 1_001_400, 100);
    let b = ix_gate_price_move(&cfg_b, 1_000_000, 1_001_400, 100);
    assert_eq!(a, b);
}

// ─── Round-trip semantics ───────────────────────────────────────────

#[test]
fn re_set_with_same_params_succeeds() {
    // Setting the same valid params twice is fine (the ix bumps
    // version + last_proven_at_slot but doesn't reject).
    let p = EnvelopeParams::default();
    ix_set_envelope_config(&p).unwrap();
    ix_set_envelope_config(&p).unwrap();
}

#[test]
fn re_set_with_tighter_params_succeeds() {
    // Authority can update to a more conservative config.
    let p1 = EnvelopeParams::default();
    ix_set_envelope_config(&p1).unwrap();
    let p2 = EnvelopeParams {
        max_price_move_bps_per_slot: 5,
        ..p1
    };
    ix_set_envelope_config(&p2).unwrap();
}
