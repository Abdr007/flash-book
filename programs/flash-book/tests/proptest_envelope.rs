//! Property tests for the per-slot envelope (Wave 26).
//!
//! Each property runs over 2000 random cases.

use flash_book::matcher::envelope::{
    gate_price_move, prove_envelope, EnvelopeError, EnvelopeParams,
    ABS_MAX_ACCRUAL_DT_SLOTS, ABS_MAX_PRICE_MOVE_BPS_PER_SLOT,
};
use proptest::prelude::*;

prop_compose! {
    fn arb_valid_params()
        (price_bps in 1u32..50,
         dt in 1u64..500,
         funding_e9 in 0i64..10_000,
         mm_bps in 2_000u32..5_000,
         liq_bps in 1u32..200)
        -> EnvelopeParams
    {
        EnvelopeParams {
            max_price_move_bps_per_slot: price_bps,
            max_accrual_dt_slots: dt,
            max_abs_funding_e9_per_slot: funding_e9,
            maintenance_bps: mm_bps,
            liquidation_fee_bps: liq_bps,
            min_liquidation_abs_lots: 1,
            min_nonzero_mm_req_lots: 100,
        }
    }
}

proptest! {
    /// gate_price_move is monotone in dt: any move that passes at
    /// dt=N also passes at dt=N+1 (more time, more budget).
    #[test]
    fn gate_monotone_in_dt(
        p_last in 1u64..1_000_000_000,
        delta in 0u64..1_000_000,
        dt in 1u64..500,
        cap_bps in 1u32..200,
    ) {
        let p_new = p_last.saturating_add(delta);
        let a = gate_price_move(p_last, p_new, dt, cap_bps);
        let b = gate_price_move(p_last, p_new, dt + 1, cap_bps);
        if a.is_ok() {
            prop_assert!(b.is_ok(), "more time should never reject");
        }
    }

    /// Symmetric: up and down moves of equal magnitude either both
    /// pass or both fail.
    #[test]
    fn gate_symmetric_up_down(
        p in 1_000_000u64..10_000_000,
        delta in 0u64..500_000,
        dt in 1u64..500,
        cap_bps in 1u32..500,
    ) {
        let up_result = gate_price_move(p, p.saturating_add(delta), dt, cap_bps);
        let down_result = gate_price_move(p, p.saturating_sub(delta), dt, cap_bps);
        prop_assert_eq!(up_result.is_ok(), down_result.is_ok());
    }

    /// prove_envelope is deterministic: same params → same result.
    #[test]
    fn prove_envelope_deterministic(p in arb_valid_params()) {
        let a = prove_envelope(&p);
        let b = prove_envelope(&p);
        prop_assert_eq!(a.is_ok(), b.is_ok());
    }

    /// Tightening the price cap doesn't make a passing envelope fail.
    #[test]
    fn prove_envelope_monotone_in_price_cap(p in arb_valid_params()) {
        if prove_envelope(&p).is_ok() && p.max_price_move_bps_per_slot > 1 {
            let tighter = EnvelopeParams {
                max_price_move_bps_per_slot: p.max_price_move_bps_per_slot - 1,
                ..p
            };
            prop_assert!(prove_envelope(&tighter).is_ok(), "tighter price cap should still prove");
        }
    }

    /// Increasing maintenance margin doesn't make a passing envelope fail.
    #[test]
    fn prove_envelope_monotone_in_mm(p in arb_valid_params()) {
        if prove_envelope(&p).is_ok() && p.maintenance_bps < 9_000 {
            let stronger = EnvelopeParams {
                maintenance_bps: p.maintenance_bps + 100,
                ..p
            };
            prop_assert!(prove_envelope(&stronger).is_ok(), "stronger MM should still prove");
        }
    }

    /// Same-slot non-zero move always rejects (when cap > 0).
    #[test]
    fn gate_same_slot_rejects(
        p in 1u64..1_000_000,
        delta in 1u64..1_000_000,
        cap_bps in 1u32..200,
    ) {
        let r = gate_price_move(p, p + delta, 0, cap_bps);
        prop_assert_eq!(r, Err(EnvelopeError::SameSlotMove));
    }

    /// p_last == 0 means "first observation" — any new price admits.
    #[test]
    fn gate_first_observation_admits(
        p_new in 1u64..u64::MAX / 2,
        dt in 0u64..1_000,
        cap_bps in 1u32..200,
    ) {
        let r = gate_price_move(0, p_new, dt, cap_bps);
        prop_assert!(r.is_ok());
    }
}

#[test]
fn envelope_const_caps_dont_overflow() {
    // The absolute caps × max notional shouldn't overflow u128.
    let max_lot = 10u128.pow(15);
    let max_budget = ABS_MAX_PRICE_MOVE_BPS_PER_SLOT as u128 * ABS_MAX_ACCRUAL_DT_SLOTS as u128;
    let max_product = max_lot.saturating_mul(max_budget);
    assert!(max_product < u128::MAX / 2);
}
