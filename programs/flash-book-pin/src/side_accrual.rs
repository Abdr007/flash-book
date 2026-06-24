//! A/K/F/B per-side lazy accrual indices (Wave 25 scaffold).
//!
//! Pure types and math. Wave 24 lands the haircut module; Wave 25
//! rewires the funding / mark / ADL settle paths onto these indices.
//! This file ships the data structures + the elementary index-advance
//! and snapshot-settle primitives so Wave 25 begins from a tested core.
//!
//! ## The four side indices
//!
//! Per market, per side, the engine maintains four cumulative indices
//! that advance lazily on every accrual call:
//!
//! - **A** — ADL multiplier. Starts at `ADL_ONE` (= 10^15). Reductions
//!   are pro-rata across the side. `effective_pos_i = basis_i × A / A_at_attach_i`.
//! - **K** — Mark-price effect. Accumulates `Δprice × A` per slot.
//!   `pnl_delta_i = basis_i × (K_now − K_attach_i) × sign / (A_attach_i × POS_SCALE)`.
//! - **F** — Funding effect. Accumulates `funding_rate × price × dt × A`.
//! - **B** — Bankruptcy residual. Accumulates socialized loss when a
//!   bankrupt close routes deficit to the opposing side. Settles
//!   pro-rata on touch.
//!
//! Each Position carries `(a_snap, k_snap, f_snap, b_snap)` taken at
//! attach time. Touching a position settles deltas in O(1).
//!
//! ## Why this matters for flash-book
//!
//! Today every funding / mark / ADL update either iterates open
//! positions or leaves drift for the next per-position settle. The
//! A/K/F/B pattern lets the matcher tick advance the four indices in
//! constant time, and every position settles itself the next time it
//! touches the chain. Closes the throughput ceiling on the ER's
//! sub-second tick.
//!
//! See Percolator `spec.md` v12.20.6 §3 (Invariant 2) for the formal
//! definitions and `percolator.rs::accrue_market_to` for the reference
//! implementation. The flash-book port adapts the scaling to our
//! `(USD_UNIT = 10^6, BPS_DENOM = 10^4, FUNDING_INDEX_FRACTIONAL_BITS
//! = 64)` conventions.

/// Multiplier denominator for `A`. Starting value of every side is
/// `ADL_ONE`; reductions scale all opposing positions pro-rata.
/// Matches Percolator's `ADL_ONE = 10^15` for compatibility with the
/// reference proofs and to give 15 decimals of headroom against
/// repeated ADL passes.
pub const ADL_ONE: u128 = 1_000_000_000_000_000;

/// Precision threshold below which a side enters DrainOnly mode (no
/// new OI; existing positions can only close). Mirrors Percolator's
/// `MIN_A_SIDE = 10^14`. Once A drops here, the side state machine
/// transitions to DrainOnly → ResetPending → Normal over time.
pub const MIN_A_SIDE: u128 = 100_000_000_000_000;

/// Position-quantity scale. `basis_i` is the position's lot size at
/// attach, multiplied by `POS_SCALE` for fixed-point K/F math.
/// Matches Percolator's `POS_SCALE = 10^6`.
pub const POS_SCALE: u128 = 1_000_000;

/// Funding denominator. Funding rates are accumulated as
/// `funding_rate_e9 × price × dt × A` and divided by
/// `FUNDING_DEN × A_attach × POS_SCALE` on settle. Matches
/// Percolator's `FUNDING_DEN = 10^9`.
pub const FUNDING_DEN: u128 = 1_000_000_000;

/// Side state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SideMode {
    /// Trades freely. The healthy steady state.
    Normal = 0,
    /// `A < MIN_A_SIDE`. No new OI allowed; existing positions can
    /// only close. Transitions to `ResetPending` once OI hits zero.
    DrainOnly = 1,
    /// OI is zero. Engine snapshots `(K, F, B)`, increments epoch,
    /// resets `A := ADL_ONE`. Stale positions settle against the
    /// epoch-start snapshots once on next touch, then become normal.
    ResetPending = 2,
}

impl Default for SideMode {
    fn default() -> Self {
        Self::Normal
    }
}

/// Per-side accrual state. One pair (long, short) per market.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideAccrual {
    /// ADL multiplier. Starts at `ADL_ONE`. Monotonically non-increasing
    /// within an epoch (resets to ADL_ONE on epoch advance).
    pub a: u128,
    /// Mark-price effect. Signed (i128 store, but we represent via
    /// `(k_num, k_neg)` to stay friendly to Anchor zero-copy when this
    /// is later moved into an account). For the scaffold we use i128.
    pub k: i128,
    /// Funding effect.
    pub f: i128,
    /// Bankruptcy residual.
    pub b: i128,
    /// Side mode (Normal / DrainOnly / ResetPending).
    pub mode: SideMode,
    /// Monotonic epoch counter. Bumps on every Drain → Normal cycle.
    pub epoch: u32,
    /// `slot_last`: last slot at which K/F advanced. Used to gate
    /// per-slot price-move caps (Wave 26).
    pub slot_last: u64,
    /// Last oracle price observed at `slot_last`. Needed to compute
    /// `Δprice` deltas without re-reading the oracle on every accrual.
    pub price_last: u64,
}

impl Default for SideAccrual {
    fn default() -> Self {
        Self {
            a: ADL_ONE,
            k: 0,
            f: 0,
            b: 0,
            mode: SideMode::Normal,
            epoch: 0,
            slot_last: 0,
            price_last: 0,
        }
    }
}

/// A Position's snapshot of the side indices at attach time. Stored on
/// the per-Position account; consumed on settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PositionSnapshot {
    pub a_snap: u128,
    pub k_snap: i128,
    pub f_snap: i128,
    pub b_snap: i128,
    pub epoch_snap: u32,
}

/// Side-mode transition output of `step_mode`. Lets the wire-in handler
/// react (e.g. emit events on `DrainOnly` entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideModeTransition {
    NoChange,
    EnteredDrain,
    EnteredResetPending,
    EnteredNormal,
}

/// Advance the side-mode state machine after every A-changing operation.
/// Pure: takes the current accrual + the side's open-interest signal.
#[inline]
pub fn step_mode(side: &mut SideAccrual, oi_lots: u64) -> SideModeTransition {
    use SideMode::*;
    match side.mode {
        Normal => {
            if side.a < MIN_A_SIDE {
                side.mode = DrainOnly;
                SideModeTransition::EnteredDrain
            } else {
                SideModeTransition::NoChange
            }
        }
        DrainOnly => {
            if oi_lots == 0 {
                side.mode = ResetPending;
                SideModeTransition::EnteredResetPending
            } else {
                SideModeTransition::NoChange
            }
        }
        ResetPending => {
            // Wire-in will call `epoch_advance` exactly once; once that
            // happens we drop back to Normal here on the next step.
            if side.a == ADL_ONE {
                side.mode = Normal;
                SideModeTransition::EnteredNormal
            } else {
                SideModeTransition::NoChange
            }
        }
    }
}

/// Reset side to a fresh epoch. Called once when `step_mode` reports
/// `EnteredResetPending` and the wire-in handler is ready to advance.
/// Bumps epoch, resets A, zeroes K/F/B. Stale positions still snapshot
/// the *previous-epoch* indices and will settle against them once.
#[inline]
pub fn epoch_advance(side: &mut SideAccrual) {
    side.epoch = side.epoch.wrapping_add(1);
    side.a = ADL_ONE;
    side.k = 0;
    side.f = 0;
    side.b = 0;
    // Note: slot_last / price_last carry forward — they describe the
    // chain's view of the oracle, not the side's accrual epoch.
}

/// Pure helper: compute a position's effective lots given its basis +
/// snapshot. `basis_lots` is the lot size at attach; the function
/// returns `floor(basis × A / a_snap)`. For positions whose epoch
/// matches the side's current epoch this is the live position; for
/// stale positions (epoch_snap != side.epoch) the caller settles the
/// position against the *epoch-end* indices and then promotes.
#[inline]
pub fn effective_lots(basis_lots: u64, a_now: u128, a_snap: u128) -> u64 {
    if a_snap == 0 {
        return 0;
    }
    // basis × A ≤ u64::MAX × ADL_ONE ≤ ~10^34 → u128 safe.
    let num = (basis_lots as u128).saturating_mul(a_now);
    let lots = num / a_snap;
    lots.min(u64::MAX as u128) as u64
}

/// Pure helper: PnL delta from K (mark moves) since attach. Sign comes
/// from the side: long positions gain when K grows, short positions
/// gain when K shrinks. Caller multiplies by the side sign.
#[inline]
pub fn pnl_delta_from_k(basis_lots: u64, k_now: i128, k_snap: i128, a_snap: u128) -> i128 {
    if a_snap == 0 {
        return 0;
    }
    let dk = k_now.saturating_sub(k_snap);
    // basis × dk fits in i128 for any realistic basis × dk product.
    let scaled = (basis_lots as i128).saturating_mul(dk);
    let denom = (a_snap as i128).saturating_mul(POS_SCALE as i128);
    if denom == 0 {
        return 0;
    }
    scaled / denom
}

/// Pure helper: PnL delta from F (funding) since attach. Same shape
/// as K; the wire-in handler picks the side sign.
#[inline]
pub fn pnl_delta_from_f(basis_lots: u64, f_now: i128, f_snap: i128, a_snap: u128) -> i128 {
    if a_snap == 0 {
        return 0;
    }
    let df = f_now.saturating_sub(f_snap);
    let scaled = (basis_lots as i128).saturating_mul(df);
    let denom = (a_snap as i128)
        .saturating_mul(POS_SCALE as i128)
        .saturating_mul(FUNDING_DEN as i128);
    if denom == 0 {
        return 0;
    }
    scaled / denom
}

// ─── Wave 25b — Index advance + position settle helpers ────────────

/// Advance a side's K and F indices given a new oracle price and a
/// funding rate. Pure math.
///
/// `funding_rate_e9` is the per-slot funding rate scaled by 10^9 (the
/// FUNDING_DEN). Positive = longs pay shorts; negative = shorts pay
/// longs (zero-sum per market).
///
/// Both indices accumulate `delta_index ∝ A` so that a position's
/// settle on touch divides by `A_attach × POS_SCALE` and naturally
/// scales with any ADL reductions that happened since attach.
///
/// Returns `Ok` when the advance is admissible; the caller is expected
/// to gate `p_new` against the envelope (Wave 26b) before invoking
/// this — the helper itself doesn't re-validate.
pub fn advance_indices(
    side: &mut SideAccrual,
    p_new: u64,
    funding_rate_e9: i64,
    now_slot: u64,
) {
    if side.slot_last == 0 {
        // First observation — seed and skip the delta math.
        side.slot_last = now_slot;
        side.price_last = p_new;
        return;
    }
    let dt = now_slot.saturating_sub(side.slot_last);
    if dt == 0 {
        // Same slot — no time elapsed, no K/F advance.
        // Allow the price update though (the matcher may have moved
        // mark within a slot).
        side.price_last = p_new;
        return;
    }

    // K advances with the price delta, scaled by current A.
    //   k_delta = (p_new - p_last) × A_now
    // Saturating math throughout.
    let dp: i128 = (p_new as i128) - (side.price_last as i128);
    let k_delta: i128 = dp.saturating_mul(side.a as i128);
    side.k = side.k.saturating_add(k_delta);

    // F advances with funding × price × dt, scaled by A. Long side
    // accumulates negative F when paying longs (longs receive +funding);
    // short side gets +F. The sign here represents long-side
    // accumulation — wire-in calls with funding sign appropriate to
    // the side.
    if funding_rate_e9 != 0 {
        let f_per_slot: i128 = (side.price_last as i128)
            .saturating_mul(funding_rate_e9 as i128);
        let f_delta: i128 = f_per_slot
            .saturating_mul(dt as i128)
            .saturating_mul(side.a as i128);
        side.f = side.f.saturating_add(f_delta);
    }

    side.slot_last = now_slot;
    side.price_last = p_new;
}

/// Settle a Position's funding+mark PnL owed using the current side
/// indices vs its attach-time snapshot. Returns the realized PnL
/// delta in quote lots; positive = trader gained, negative = trader
/// owes the protocol.
///
/// The position's snapshot is **not** mutated here — the wire-in
/// updates it on the call site after the delta is settled to
/// collateral (so a single settle uses the same snapshot for both K
/// and F before refresh).
pub fn settle_position_pnl(
    basis_lots: u64,
    side: &SideAccrual,
    pos: &PositionSnapshot,
    side_sign: i128, // +1 for long, -1 for short
) -> i128 {
    if pos.a_snap == 0 || basis_lots == 0 {
        return 0;
    }
    let from_k = pnl_delta_from_k(basis_lots, side.k, pos.k_snap, pos.a_snap);
    let from_f = pnl_delta_from_f(basis_lots, side.f, pos.f_snap, pos.a_snap);
    // Long: gains when K grows, loses when F grows (longs pay).
    // Short: opposite (handled by side_sign).
    let combined = from_k.saturating_sub(from_f);
    combined.saturating_mul(side_sign)
}

/// Refresh a Position's snapshot to the current side indices after a
/// settle. The wire-in calls this AFTER `settle_position_pnl` so the
/// next touch starts a fresh delta period.
pub fn refresh_position_snapshot(pos: &mut PositionSnapshot, side: &SideAccrual) {
    pos.a_snap = side.a;
    pos.k_snap = side.k;
    pos.f_snap = side.f;
    pos.b_snap = side.b;
    pos.epoch_snap = side.epoch;
}

/// Reduce A on a side by a factor `numerator / denominator`. Wave 25b
/// replacement for `auto_deleverage`'s bankruptcy-price math: shrink
/// every opposing position pro-rata by scaling A. Idempotent through
/// the settle math — opposing positions read the new A on their next
/// touch and self-shrink via `effective_lots`.
///
/// Caller ensures `numerator <= denominator` (otherwise A grows,
/// which is a logic bug). Returns the new A value.
pub fn reduce_a_pro_rata(side: &mut SideAccrual, numerator: u128, denominator: u128) -> u128 {
    if denominator == 0 || numerator >= denominator {
        return side.a;
    }
    let new_a = side.a.saturating_mul(numerator) / denominator;
    side.a = new_a;
    new_a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unit_state() {
        let s = SideAccrual::default();
        assert_eq!(s.a, ADL_ONE);
        assert_eq!(s.k, 0);
        assert_eq!(s.f, 0);
        assert_eq!(s.b, 0);
        assert_eq!(s.mode, SideMode::Normal);
    }

    #[test]
    fn effective_lots_unscaled_at_attach() {
        // Position attached when A == ADL_ONE and A hasn't moved.
        assert_eq!(effective_lots(1_000, ADL_ONE, ADL_ONE), 1_000);
    }

    #[test]
    fn effective_lots_shrinks_pro_rata_on_adl() {
        // Side ADL'd to half: every opposing position halves.
        assert_eq!(effective_lots(1_000, ADL_ONE / 2, ADL_ONE), 500);
    }

    #[test]
    fn step_mode_enters_drain_on_precision_collapse() {
        let mut s = SideAccrual {
            a: MIN_A_SIDE - 1,
            ..Default::default()
        };
        let t = step_mode(&mut s, 1_000);
        assert_eq!(t, SideModeTransition::EnteredDrain);
        assert_eq!(s.mode, SideMode::DrainOnly);
    }

    #[test]
    fn step_mode_reset_pending_once_oi_zeros() {
        let mut s = SideAccrual {
            mode: SideMode::DrainOnly,
            ..Default::default()
        };
        let t = step_mode(&mut s, 0);
        assert_eq!(t, SideModeTransition::EnteredResetPending);
        assert_eq!(s.mode, SideMode::ResetPending);
    }

    #[test]
    fn epoch_advance_resets_state() {
        let mut s = SideAccrual {
            a: MIN_A_SIDE / 2,
            k: 12_345,
            f: -678,
            b: 99,
            mode: SideMode::ResetPending,
            epoch: 5,
            slot_last: 1_000,
            price_last: 500,
        };
        epoch_advance(&mut s);
        assert_eq!(s.epoch, 6);
        assert_eq!(s.a, ADL_ONE);
        assert_eq!(s.k, 0);
        assert_eq!(s.f, 0);
        assert_eq!(s.b, 0);
        assert_eq!(s.slot_last, 1_000, "slot/price carry forward");
        // mode is *not* changed by epoch_advance; the next step_mode call promotes.
        assert_eq!(s.mode, SideMode::ResetPending);
        let t = step_mode(&mut s, 0);
        assert_eq!(t, SideModeTransition::EnteredNormal);
        assert_eq!(s.mode, SideMode::Normal);
    }

    #[test]
    fn pnl_delta_from_k_signed() {
        // Denominator is a_snap × POS_SCALE = 10^15 × 10^6 = 10^21.
        // To produce a non-zero delta from a 1_000-lot basis we need
        // dk ≥ 10^21 / 1_000 = 10^18. K accumulates `Δprice × A` over
        // many slots, so at market level this grows fast; on a real
        // market K = 10^21 is reached after ~1_000_000 ticks of price
        // movement (well within an hour of normal trading).
        let big_k: i128 = 1_000_000_000_000_000_000_000; // 10^21
        let pos = pnl_delta_from_k(1_000, big_k, 0, ADL_ONE);
        assert!(pos > 0, "k grew → long gains positive (got {pos})");
        let neg = pnl_delta_from_k(1_000, -big_k, 0, ADL_ONE);
        assert!(neg < 0, "k shrank → long gains negative (got {neg})");
        // Symmetry: equal-magnitude moves produce equal-magnitude deltas.
        assert_eq!(pos, -neg);
    }

    #[test]
    fn pnl_delta_floors_to_zero_at_small_k() {
        // Documented behaviour: at small K values relative to the
        // denominator, the floor is 0. This is fine — K is meant to
        // accumulate at market scale, not per-tick.
        assert_eq!(pnl_delta_from_k(1_000, 100, 0, ADL_ONE), 0);
    }

    #[test]
    fn zero_a_snap_returns_zero_safely() {
        assert_eq!(effective_lots(1_000, ADL_ONE, 0), 0);
        assert_eq!(pnl_delta_from_k(1_000, 100, 0, 0), 0);
        assert_eq!(pnl_delta_from_f(1_000, 100, 0, 0), 0);
    }

    // ─── Wave 25b tests ─────────────────────────────────────────────

    #[test]
    fn advance_indices_first_observation_seeds_only() {
        let mut s = SideAccrual::default();
        advance_indices(&mut s, 1_000_000, 0, 100);
        assert_eq!(s.slot_last, 100);
        assert_eq!(s.price_last, 1_000_000);
        assert_eq!(s.k, 0);
        assert_eq!(s.f, 0);
    }

    #[test]
    fn advance_indices_k_grows_with_price_delta() {
        let mut s = SideAccrual::default();
        advance_indices(&mut s, 1_000_000, 0, 100);
        advance_indices(&mut s, 1_001_000, 0, 200);
        assert!(s.k > 0, "K must grow on up-price move");
        // K = dp × A = 1000 × 10^15 = 10^18.
        assert_eq!(s.k, 1_000 * (ADL_ONE as i128));
    }

    #[test]
    fn advance_indices_f_accumulates_with_funding() {
        let mut s = SideAccrual::default();
        advance_indices(&mut s, 1_000_000, 0, 100);
        // dt=100 slots, funding=10_000 e-9, price=1_000_000, A=10^15.
        // f_delta = 1_000_000 × 10_000 × 100 × 10^15 = 10^30.
        advance_indices(&mut s, 1_000_000, 10_000, 200);
        assert!(s.f > 0);
    }

    #[test]
    fn advance_indices_same_slot_only_updates_price() {
        let mut s = SideAccrual::default();
        advance_indices(&mut s, 1_000_000, 0, 100);
        advance_indices(&mut s, 1_010_000, 10_000, 100); // same slot
        assert_eq!(s.slot_last, 100);
        assert_eq!(s.price_last, 1_010_000, "price updated within slot");
        assert_eq!(s.k, 0, "K does not advance same-slot");
        assert_eq!(s.f, 0, "F does not advance same-slot");
    }

    #[test]
    fn settle_position_pnl_zero_basis_returns_zero() {
        let s = SideAccrual::default();
        let p = PositionSnapshot::default();
        assert_eq!(settle_position_pnl(0, &s, &p, 1), 0);
    }

    #[test]
    fn settle_position_pnl_no_drift_returns_zero() {
        let mut s = SideAccrual::default();
        advance_indices(&mut s, 1_000_000, 0, 100);
        let p = PositionSnapshot {
            a_snap: s.a,
            k_snap: s.k,
            f_snap: s.f,
            b_snap: s.b,
            epoch_snap: s.epoch,
        };
        // Snapshot equal to current state → no PnL.
        assert_eq!(settle_position_pnl(1_000, &s, &p, 1), 0);
    }

    #[test]
    fn refresh_position_snapshot_captures_full_state() {
        let mut s = SideAccrual::default();
        s.a = MIN_A_SIDE / 2;
        s.k = 12_345;
        s.f = -678;
        s.b = 99;
        s.epoch = 7;
        let mut p = PositionSnapshot::default();
        refresh_position_snapshot(&mut p, &s);
        assert_eq!(p.a_snap, s.a);
        assert_eq!(p.k_snap, s.k);
        assert_eq!(p.f_snap, s.f);
        assert_eq!(p.b_snap, s.b);
        assert_eq!(p.epoch_snap, s.epoch);
    }

    #[test]
    fn reduce_a_pro_rata_halves_a() {
        let mut s = SideAccrual::default();
        let initial = s.a;
        let new_a = reduce_a_pro_rata(&mut s, 1, 2);
        assert_eq!(new_a, initial / 2);
        assert_eq!(s.a, initial / 2);
    }

    #[test]
    fn reduce_a_pro_rata_rejects_growth() {
        let mut s = SideAccrual::default();
        let initial = s.a;
        // numerator ≥ denominator → no-op (caller bug).
        reduce_a_pro_rata(&mut s, 2, 1);
        assert_eq!(s.a, initial);
        reduce_a_pro_rata(&mut s, 0, 0);
        assert_eq!(s.a, initial);
    }

    #[test]
    fn reduce_a_pro_rata_shrinks_effective_lots() {
        let mut s = SideAccrual::default();
        let basis = 1_000;
        // Before reduce: effective = 1000.
        let before = effective_lots(basis, s.a, ADL_ONE);
        assert_eq!(before, 1_000);
        // Half A.
        reduce_a_pro_rata(&mut s, 1, 2);
        let after = effective_lots(basis, s.a, ADL_ONE);
        assert_eq!(after, 500);
    }
}
