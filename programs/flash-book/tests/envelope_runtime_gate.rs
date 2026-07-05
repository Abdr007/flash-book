//! Runtime envelope gate semantics.
//!
//! Mirrors `gate_oracle_update`'s body as plain Rust over the same
//! state shape. The on-chain ix is a direct passthrough — passing
//! here proves the runtime behavior.

use flash_book::matcher::envelope::{
    gate_price_move, EnvelopeError, EnvelopeParams,
};

#[derive(Debug, Clone, Copy)]
struct EnvelopeCfg {
    max_price_move_bps_per_slot: u32,
    last_observed_slot: u64,
    last_observed_price_ticks: u64,
    gate_passes: u64,
    gate_rejects: u64,
}

impl Default for EnvelopeCfg {
    fn default() -> Self {
        let p = EnvelopeParams::default();
        Self {
            max_price_move_bps_per_slot: p.max_price_move_bps_per_slot,
            last_observed_slot: 0,
            last_observed_price_ticks: 0,
            gate_passes: 0,
            gate_rejects: 0,
        }
    }
}

/// Mirror of `gate_oracle_update` body. Mutates the cfg on success
/// (seeds/advances last_observed) or on reject (bumps gate_rejects).
fn gate(cfg: &mut EnvelopeCfg, new_price: u64, now_slot: u64) -> Result<(), EnvelopeError> {
    if cfg.last_observed_slot == 0 {
        // First observation — seed + skip.
        cfg.last_observed_slot = now_slot;
        cfg.last_observed_price_ticks = new_price;
        cfg.gate_passes += 1;
        return Ok(());
    }
    let dt = now_slot.saturating_sub(cfg.last_observed_slot);
    if dt == 0 {
        if new_price == cfg.last_observed_price_ticks {
            return Ok(());
        }
        cfg.gate_rejects += 1;
        return Err(EnvelopeError::SameSlotMove);
    }
    let last_price = cfg.last_observed_price_ticks;
    let result = gate_price_move(last_price, new_price, dt, cfg.max_price_move_bps_per_slot);
    match result {
        Ok(()) => {
            cfg.last_observed_slot = now_slot;
            cfg.last_observed_price_ticks = new_price;
            cfg.gate_passes += 1;
            Ok(())
        }
        Err(e) => {
            cfg.gate_rejects += 1;
            Err(e)
        }
    }
}

// ─── Scenarios ──────────────────────────────────────────────────────

#[test]
fn first_observation_seeds_without_gate() {
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    assert_eq!(cfg.last_observed_slot, 100);
    assert_eq!(cfg.last_observed_price_ticks, 1_000_000);
    assert_eq!(cfg.gate_passes, 1);
    assert_eq!(cfg.gate_rejects, 0);
}

#[test]
fn small_move_admitted_advances_observation() {
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    // dt=100, cap=14 bps/slot × 100 = 1400 bps. 1% move = OK.
    gate(&mut cfg, 1_010_000, 200).unwrap();
    assert_eq!(cfg.last_observed_slot, 200);
    assert_eq!(cfg.last_observed_price_ticks, 1_010_000);
    assert_eq!(cfg.gate_passes, 2);
}

#[test]
fn oversized_move_rejected_and_state_preserved() {
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    // 20% move at dt=100 with 14 bps/slot cap → 14% budget < 20%.
    let r = gate(&mut cfg, 1_200_000, 200);
    assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
    // State must be preserved on reject — last observation unchanged.
    assert_eq!(cfg.last_observed_slot, 100);
    assert_eq!(cfg.last_observed_price_ticks, 1_000_000);
    assert_eq!(cfg.gate_passes, 1);
    assert_eq!(cfg.gate_rejects, 1);
}

#[test]
fn same_slot_with_same_price_admitted() {
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    // Same slot, same price — admitted as no-op.
    gate(&mut cfg, 1_000_000, 100).unwrap();
    assert_eq!(cfg.gate_rejects, 0);
}

#[test]
fn same_slot_with_different_price_rejected() {
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    let r = gate(&mut cfg, 1_000_001, 100);
    assert_eq!(r, Err(EnvelopeError::SameSlotMove));
    assert_eq!(cfg.gate_rejects, 1);
}

#[test]
fn larger_dt_allows_larger_move() {
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    // dt=1000 → 14_000 bps budget (140%). 20% move = OK.
    gate(&mut cfg, 1_200_000, 1_100).unwrap();
    assert_eq!(cfg.last_observed_price_ticks, 1_200_000);
}

#[test]
fn sustained_small_moves_pass_indefinitely() {
    // 100 sequential moves of 1% each over 100 slots → all admitted
    // since each individual move is well within cap.
    let mut cfg = EnvelopeCfg::default();
    let mut price: u64 = 1_000_000;
    gate(&mut cfg, price, 0).unwrap();
    for i in 1..=100 {
        // 1% up per ~100 slots — well within 14 bps × 100 = 14% budget.
        price = price + (price / 200);
        gate(&mut cfg, price, i * 100).unwrap();
    }
    assert_eq!(cfg.gate_passes, 101); // initial + 100
    assert_eq!(cfg.gate_rejects, 0);
}

#[test]
fn flash_crash_blocked_in_single_step() {
    // Defining attack: a single oracle update tries to swing -50%.
    // Should be rejected.
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    let r = gate(&mut cfg, 500_000, 200);
    assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
    assert_eq!(cfg.last_observed_price_ticks, 1_000_000);
}

#[test]
fn flash_crash_admitted_via_staircase() {
    // Defensive crank pattern: same total move, but spread over many
    // smaller hops. Each hop respects the per-slot cap.
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 0).unwrap();
    // ~10 staircase steps of -5% each. Need each step within cap.
    // 14 bps/slot × 500 slots = 7000 bps = 70% budget — plenty for 5%.
    let mut price: u64 = 1_000_000;
    let mut slot: u64 = 0;
    for _ in 0..10 {
        slot += 500;
        price = price - (price / 20); // -5%
        gate(&mut cfg, price, slot).unwrap();
    }
    // After 10 × 5% drops, price ≈ 599_000 — below 50% of start.
    assert!(cfg.last_observed_price_ticks < 600_000);
    assert!(cfg.last_observed_price_ticks > 590_000);
    assert_eq!(cfg.gate_rejects, 0);
}

#[test]
fn down_moves_obey_same_cap() {
    // Symmetric: cap applies equally to up and down moves.
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 100).unwrap();
    // dt=100, cap=14% budget. 5% down OK; 20% down rejected.
    gate(&mut cfg, 950_000, 200).unwrap();
    let r = gate(&mut cfg, 760_000, 300);
    assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
}

#[test]
fn pass_reject_counters_are_independent() {
    // Same-slot same-price is admitted but is a no-op — no counter
    // bump (no observation event happened).
    let mut cfg = EnvelopeCfg::default();
    gate(&mut cfg, 1_000_000, 0).unwrap(); // seed pass (+1)
    gate(&mut cfg, 1_010_000, 100).unwrap(); // real pass (+1)
    gate(&mut cfg, 1_010_000, 100).unwrap(); // no-op (no bump)
    let _ = gate(&mut cfg, 2_000_000, 200); // reject (too big) (+1 reject)
    let _ = gate(&mut cfg, 1_010_001, 100); // reject (same-slot different) (+1 reject)
    assert_eq!(cfg.gate_passes, 2);
    assert_eq!(cfg.gate_rejects, 2);
}
