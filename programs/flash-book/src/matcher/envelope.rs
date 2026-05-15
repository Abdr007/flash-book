//! Per-slot price/funding envelope proof (Wave 26 scaffold).
//!
//! Turns "crank often enough" from an operator preference into a hard
//! solvency boundary. At market init, the parameter set must satisfy:
//!
//!   price_funding_loss_N + liq_fee_N ≤ mm_req_N   ∀ N ∈ [1, MAX_N]
//!
//! where N is any account's risk notional, and `price_funding_loss_N`
//! is the worst-case loss an account can sustain during a single
//! `max_accrual_dt_slots` window under the configured per-slot caps.
//!
//! If this holds, then for any account whose maintenance margin is
//! healthy at slot t, the worst-case state at slot t + max_accrual_dt
//! is *also* solvent enough to cover its own liquidation. The engine
//! cannot be cranked through an arbitrary oracle or funding jump in one
//! step — the per-slot caps + `max_accrual_dt_slots` bound the damage.
//!
//! Bad-parameter markets cannot instantiate. Once a market exists, the
//! cap is enforced at every K/F advance via `gate_price_move`.
//!
//! Reference: Percolator `spec.md` v12.20.6 §1.4. Adapted to flash-book
//! lot conventions.

use crate::constants::BPS_DENOM;
use crate::matcher::side_accrual::FUNDING_DEN;

/// Max account risk notional we prove the envelope over, in quote lots.
/// Picked larger than any plausible single-position notional. Reads in
/// USD: 10^15 quote lots = $10^9 (a billion dollars notional). The
/// envelope holds for ALL N ≤ this bound.
pub const MAX_ACCOUNT_NOTIONAL_LOTS: u128 = 1_000_000_000_000_000;

/// Hard cap on `max_price_move_bps_per_slot`. Anything beyond this is
/// rejected at init — even the wildest crypto move (e.g. LUNA going to
/// zero) fits within ~500 bps/slot at 400ms slots. We pick 2_000 bps
/// (20%) as the cap — well above realistic limits.
pub const ABS_MAX_PRICE_MOVE_BPS_PER_SLOT: u32 = 2_000;

/// Hard cap on `max_accrual_dt_slots`. 10_000 slots ≈ 67 minutes at
/// 400ms; longer than this and the per-slot envelope no longer bounds
/// risk meaningfully because positions can drift too far before any
/// crank lands.
pub const ABS_MAX_ACCRUAL_DT_SLOTS: u64 = 10_000;

/// Hard cap on `max_abs_funding_e9_per_slot`. 10_000 (≈ 1e-5 per slot)
/// at 400ms/slot = 2.16% per day saturating. Mirrors Percolator's
/// `GLOBAL_MAX_ABS_FUNDING_E9_PER_SLOT`.
pub const ABS_MAX_FUNDING_E9_PER_SLOT: i64 = 10_000;

/// Per-market envelope parameters. All five are set at init and
/// enforced thereafter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeParams {
    /// Max per-slot oracle price move, in bps of the previous price.
    /// Live accrual aborts when |Δp| × 10_000 > cap × dt × p_last.
    pub max_price_move_bps_per_slot: u32,
    /// Max accrual window any single call can advance K/F over.
    pub max_accrual_dt_slots: u64,
    /// Max absolute funding rate per slot (scaled by 10^9).
    pub max_abs_funding_e9_per_slot: i64,
    /// Maintenance margin requirement, in bps of notional.
    pub maintenance_bps: u32,
    /// Liquidation fee, in bps of liquidation notional.
    pub liquidation_fee_bps: u32,
    /// Floor on the liquidation fee (absolute quote lots).
    pub min_liquidation_abs_lots: u64,
    /// Floor on the maintenance margin requirement (absolute quote lots).
    pub min_nonzero_mm_req_lots: u64,
}

impl Default for EnvelopeParams {
    fn default() -> Self {
        // Tuned so the envelope holds for every N ∈ [1, MAX_ACCOUNT_NOTIONAL].
        // Relation that has to bind:
        //   price_budget = price_cap × dt = 14 bps × 100 = 1400 bps = 14%
        //   maintenance  = 3000 bps = 30%
        //   ratio = 0.467 + fee_term ≈ 0.473 ≤ 1.0 ✓
        // Matches flash-book's existing v3 `max_price_move_bps_per_slot=14`
        // (from V3 doc) and Percolator's `maintenance_margin_bps=3000` test
        // default. Per-market override at init.
        Self {
            max_price_move_bps_per_slot: 14, // 0.14% per slot ≈ 0.35%/sec
            max_accrual_dt_slots: 100,       // ~40s at 400ms
            max_abs_funding_e9_per_slot: 10_000,
            maintenance_bps: 3_000, // 30% MMR (Percolator parity)
            liquidation_fee_bps: 50, // 0.5%
            min_liquidation_abs_lots: 1,
            min_nonzero_mm_req_lots: 100,
        }
    }
}

/// Verify the envelope holds for every N in [1, MAX_ACCOUNT_NOTIONAL].
/// Called exactly once at `initialize_market`. If this fails, the
/// market cannot be created.
pub fn prove_envelope(params: &EnvelopeParams) -> Result<(), EnvelopeError> {
    // 1. Range checks on raw caps.
    if params.max_price_move_bps_per_slot == 0 {
        return Err(EnvelopeError::PriceCapZero);
    }
    if params.max_price_move_bps_per_slot > ABS_MAX_PRICE_MOVE_BPS_PER_SLOT {
        return Err(EnvelopeError::PriceCapTooLarge);
    }
    if params.max_accrual_dt_slots == 0 {
        return Err(EnvelopeError::AccrualDtZero);
    }
    if params.max_accrual_dt_slots > ABS_MAX_ACCRUAL_DT_SLOTS {
        return Err(EnvelopeError::AccrualDtTooLarge);
    }
    if params.max_abs_funding_e9_per_slot.abs() > ABS_MAX_FUNDING_E9_PER_SLOT {
        return Err(EnvelopeError::FundingCapTooLarge);
    }
    if params.maintenance_bps == 0 {
        return Err(EnvelopeError::MaintenanceZero);
    }
    if params.maintenance_bps as u64 >= BPS_DENOM as u64 {
        return Err(EnvelopeError::MaintenanceTooLarge);
    }
    if params.liquidation_fee_bps as u64 >= BPS_DENOM as u64 {
        return Err(EnvelopeError::LiqFeeTooLarge);
    }

    // 2. Closed-form envelope check (Percolator spec §1.4 adapted).
    //
    //    price_budget_bps = max_price_move_bps × dt
    //    fund_budget_num  = |max_funding_e9| × dt × BPS_DENOM
    //    loss_budget_num  = price_budget × FUNDING_DEN + fund_budget_num
    //    price_funding_loss(N) = ceil(N × loss_budget / (BPS_DENOM × FUNDING_DEN))
    //    worst_liq_notional(N) = ceil(N × (BPS_DENOM + price_budget) / BPS_DENOM)
    //    liq_fee_raw(N)        = ceil(worst_liq × liq_fee_bps / BPS_DENOM)
    //    liq_fee(N)            = max(liq_fee_raw, min_liq_abs)
    //    mm_req(N)             = max(floor(N × mm_bps / BPS_DENOM), min_mm_req)
    //
    //    require: price_funding_loss(N) + liq_fee(N) ≤ mm_req(N) for all
    //             N in [1, MAX_ACCOUNT_NOTIONAL_LOTS].
    //
    // This is monotone in N once N is large enough that floor(N × mm_bps
    // / BPS_DENOM) ≥ min_mm_req, so we only need to check the boundary
    // cases:
    //   a) N = 1 (smallest meaningful)
    //   b) N = the breakpoint where mm_req transitions from min_mm_req
    //      to the proportional formula
    //   c) N = MAX_ACCOUNT_NOTIONAL_LOTS
    //
    // For correctness we evaluate all three; if any fails, the env doesn't
    // hold.

    let price_budget_bps = (params.max_price_move_bps_per_slot as u128)
        .checked_mul(params.max_accrual_dt_slots as u128)
        .ok_or(EnvelopeError::Overflow)?;
    let funding_abs = params.max_abs_funding_e9_per_slot.unsigned_abs() as u128;
    let fund_budget_num = funding_abs
        .checked_mul(params.max_accrual_dt_slots as u128)
        .ok_or(EnvelopeError::Overflow)?
        .checked_mul(BPS_DENOM as u128)
        .ok_or(EnvelopeError::Overflow)?;
    let loss_budget_num = price_budget_bps
        .checked_mul(FUNDING_DEN)
        .ok_or(EnvelopeError::Overflow)?
        .checked_add(fund_budget_num)
        .ok_or(EnvelopeError::Overflow)?;
    let loss_budget_den = (BPS_DENOM as u128).checked_mul(FUNDING_DEN).ok_or(EnvelopeError::Overflow)?;

    let breakpoint_n = breakpoint_notional(params);
    let probes: [u128; 3] = [1, breakpoint_n, MAX_ACCOUNT_NOTIONAL_LOTS];

    for &n in &probes {
        if n == 0 {
            continue;
        }
        let price_funding_loss = ceil_div(
            n.checked_mul(loss_budget_num).ok_or(EnvelopeError::Overflow)?,
            loss_budget_den,
        );
        let worst_liq_notional = ceil_div(
            n.checked_mul((BPS_DENOM as u128) + price_budget_bps)
                .ok_or(EnvelopeError::Overflow)?,
            BPS_DENOM as u128,
        );
        let liq_fee_raw = ceil_div(
            worst_liq_notional
                .checked_mul(params.liquidation_fee_bps as u128)
                .ok_or(EnvelopeError::Overflow)?,
            BPS_DENOM as u128,
        );
        let liq_fee = liq_fee_raw.max(params.min_liquidation_abs_lots as u128);
        let mm_floor = n
            .checked_mul(params.maintenance_bps as u128)
            .ok_or(EnvelopeError::Overflow)?
            / (BPS_DENOM as u128);
        let mm_req = mm_floor.max(params.min_nonzero_mm_req_lots as u128);

        if price_funding_loss + liq_fee > mm_req {
            return Err(EnvelopeError::EnvelopeViolated {
                n,
                loss: price_funding_loss,
                fee: liq_fee,
                mm: mm_req,
            });
        }
    }

    Ok(())
}

/// Runtime gate enforced on every K/F advance. Returns `Ok` iff the
/// proposed price move respects `max_price_move_bps_per_slot × dt`.
///
/// `dt` is the number of slots since the last accrual; `0` rejects.
/// The check uses unsigned arithmetic on the absolute delta.
pub fn gate_price_move(
    p_last: u64,
    p_new: u64,
    dt_slots: u64,
    max_price_move_bps_per_slot: u32,
) -> Result<(), EnvelopeError> {
    if p_last == 0 {
        // First accrual: any price is admissible (the matcher's own
        // init bound applies).
        return Ok(());
    }
    if dt_slots == 0 {
        return Err(EnvelopeError::SameSlotMove);
    }
    let abs_delta = if p_new >= p_last {
        p_new - p_last
    } else {
        p_last - p_new
    };
    // |Δp| × BPS_DENOM ≤ cap × dt × p_last
    let lhs = (abs_delta as u128)
        .checked_mul(BPS_DENOM as u128)
        .ok_or(EnvelopeError::Overflow)?;
    let rhs = (max_price_move_bps_per_slot as u128)
        .checked_mul(dt_slots as u128)
        .ok_or(EnvelopeError::Overflow)?
        .checked_mul(p_last as u128)
        .ok_or(EnvelopeError::Overflow)?;
    if lhs > rhs {
        return Err(EnvelopeError::PriceMoveExceedsCap);
    }
    Ok(())
}

/// Where does mm_req transition from the absolute floor to the
/// proportional formula? That's where the envelope is tightest.
#[inline]
fn breakpoint_notional(params: &EnvelopeParams) -> u128 {
    // mm_floor(N) = N × mm_bps / BPS_DENOM. We want N s.t. mm_floor ==
    // min_mm_req → N = min_mm_req × BPS_DENOM / mm_bps.
    if params.maintenance_bps == 0 {
        return 0;
    }
    (params.min_nonzero_mm_req_lots as u128)
        .saturating_mul(BPS_DENOM as u128)
        / (params.maintenance_bps as u128)
}

#[inline]
fn ceil_div(num: u128, den: u128) -> u128 {
    if den == 0 {
        return 0;
    }
    num / den + if num % den != 0 { 1 } else { 0 }
}

/// Errors from envelope validation. Wire-in maps to `FlashBookError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    PriceCapZero,
    PriceCapTooLarge,
    AccrualDtZero,
    AccrualDtTooLarge,
    FundingCapTooLarge,
    MaintenanceZero,
    MaintenanceTooLarge,
    LiqFeeTooLarge,
    Overflow,
    SameSlotMove,
    PriceMoveExceedsCap,
    EnvelopeViolated {
        n: u128,
        loss: u128,
        fee: u128,
        mm: u128,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_satisfy_envelope() {
        prove_envelope(&EnvelopeParams::default()).expect("default params must prove");
    }

    #[test]
    fn zero_price_cap_rejected() {
        let p = EnvelopeParams {
            max_price_move_bps_per_slot: 0,
            ..Default::default()
        };
        assert_eq!(prove_envelope(&p), Err(EnvelopeError::PriceCapZero));
    }

    #[test]
    fn unbounded_price_cap_rejected() {
        let p = EnvelopeParams {
            max_price_move_bps_per_slot: ABS_MAX_PRICE_MOVE_BPS_PER_SLOT + 1,
            ..Default::default()
        };
        assert_eq!(prove_envelope(&p), Err(EnvelopeError::PriceCapTooLarge));
    }

    #[test]
    fn excessive_window_rejected() {
        let p = EnvelopeParams {
            max_accrual_dt_slots: ABS_MAX_ACCRUAL_DT_SLOTS + 1,
            ..Default::default()
        };
        assert_eq!(prove_envelope(&p), Err(EnvelopeError::AccrualDtTooLarge));
    }

    #[test]
    fn maintenance_too_low_fails_envelope() {
        // Tiny MMR cannot absorb the worst-case price move + liq fee.
        let p = EnvelopeParams {
            maintenance_bps: 5, // 0.05%
            max_price_move_bps_per_slot: 100,
            max_accrual_dt_slots: 200,
            ..Default::default()
        };
        let res = prove_envelope(&p);
        assert!(matches!(res, Err(EnvelopeError::EnvelopeViolated { .. })),
            "expected envelope violation, got {res:?}");
    }

    #[test]
    fn maintenance_at_full_bps_rejected() {
        let p = EnvelopeParams {
            maintenance_bps: BPS_DENOM,
            ..Default::default()
        };
        assert_eq!(prove_envelope(&p), Err(EnvelopeError::MaintenanceTooLarge));
    }

    #[test]
    fn gate_accepts_move_within_cap() {
        // 100 bps/slot × 5 slots × p_last=1000 = 5000 (in BPS_DENOM units)
        // |Δp| × BPS_DENOM = 50 × 10_000 = 500_000 ≤ 100 × 5 × 1000 × 1 = 500_000
        // Wait: rhs = cap × dt × p_last = 100 × 5 × 1000 = 500_000
        //       lhs = |Δp| × BPS_DENOM = 50 × 10_000 = 500_000 ≤ 500_000 ✓
        gate_price_move(1000, 1050, 5, 100).unwrap();
    }

    #[test]
    fn gate_rejects_overshoot() {
        // 100 bps/slot × 5 slots cap → 5% allowed. 1000 → 1100 is +10%, reject.
        let r = gate_price_move(1000, 1100, 5, 100);
        assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
    }

    #[test]
    fn gate_rejects_same_slot() {
        let r = gate_price_move(1000, 1010, 0, 100);
        assert_eq!(r, Err(EnvelopeError::SameSlotMove));
    }

    #[test]
    fn gate_first_accrual_always_admits() {
        // p_last = 0 means "first observation"; any p_new is fine.
        gate_price_move(0, 1_000_000, 0, 1).unwrap();
    }

    #[test]
    fn gate_symmetric_on_down_moves() {
        // Same cap applies to down moves.
        gate_price_move(1000, 950, 5, 100).unwrap();
        let r = gate_price_move(1000, 900, 5, 100);
        assert_eq!(r, Err(EnvelopeError::PriceMoveExceedsCap));
    }

    #[test]
    fn breakpoint_notional_is_sensible() {
        let p = EnvelopeParams {
            min_nonzero_mm_req_lots: 1_000,
            maintenance_bps: 300,
            ..Default::default()
        };
        // N where N × 300 / 10_000 = 1_000 → N = 33_333 (and change).
        assert_eq!(breakpoint_notional(&p), 33_333);
    }

    #[test]
    fn ceil_div_works() {
        assert_eq!(ceil_div(10, 3), 4);
        assert_eq!(ceil_div(9, 3), 3);
        assert_eq!(ceil_div(0, 5), 0);
        assert_eq!(ceil_div(7, 0), 0);
    }
}
