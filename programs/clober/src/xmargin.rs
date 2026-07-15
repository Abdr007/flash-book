//! Cross-domain (ER) reserved-margin attestation — opt-in, additive (#8).
//!
//! The hybrid venue splits state across two domains: a trader's COLLATERAL
//! (`TraderStateAccount`) is authoritative on L1, while their RESTING ORDERS
//! live in the order book delegated to the MagicBlock Ephemeral Rollup (ER).
//! L1 therefore cannot see the margin those resting orders will require if they
//! fill — `open_positions` only counts already-FILLED positions. Without a
//! bridge, a trader could rest large orders on the ER, withdraw the collateral
//! backing them on L1 (nothing filled yet, so the strict-withdraw gate passes),
//! and then have those orders fill into undercollateralized positions / bad debt.
//!
//! This module closes that gap with a sequencer-ATTESTED reserved-margin figure.
//! The ER's trusted margin attestor periodically writes, per trader, the total
//! initial margin reserved by that trader's live ER orders, behind a monotonic
//! epoch (the same replay-guard shape as `apply_fill`'s settlement nonce). The
//! cross-domain withdraw variants then require post-withdrawal collateral to
//! cover `max(IM_filled, floor) + er_reserved`, so collateral backing resting ER
//! orders can never be pulled.
//!
//! Everything here is a pure decision function or a plain account; the
//! instruction handlers in `lib.rs` call into it. That keeps the accounting
//! Kani-provable and host-testable independent of the (un-runnable-in-harness)
//! ER itself. Defaults are inert: a trader with no attestation / `er_active == 0`
//! behaves EXACTLY as before, so this is fully backward compatible.

use anchor_lang::prelude::*;

use crate::errors::CloberError;

/// PDA seed for a trader's ER reserved-margin attestation.
/// Address: `[ER_MARGIN_SEED, trader_state_key]`.
pub const ER_MARGIN_SEED: &[u8] = b"er_margin";

/// Per-trader ER reserved-margin attestation. Created once by the protocol
/// authority (which pins the trusted `attestor`), then maintained by that
/// attestor. `reserved_margin_quote_lots` is the total initial margin the
/// trader's LIVE ER orders require; `epoch` is a strictly-increasing replay
/// guard. 81 bytes + 8 disc.
#[account]
pub struct ErMarginAttestation {
    /// The `trader_state` PDA this attestation is bound to (NOT the wallet —
    /// supports sub-accounts, which key by trader_state).
    pub trader_state: Pubkey,
    /// The only key allowed to update this attestation (the ER margin
    /// sequencer). Pinned at init by the protocol authority.
    pub attestor: Pubkey,
    /// Total initial margin (quote lots) reserved by the trader's live ER
    /// resting orders. Withdrawals must leave at least this much behind.
    pub reserved_margin_quote_lots: u64,
    /// Strictly-increasing attestation epoch (monotonic replay guard).
    pub epoch: u64,
    pub bump: u8,
}

impl ErMarginAttestation {
    /// 32 + 32 + 8 + 8 + 1.
    pub const LEN: usize = 32 + 32 + 8 + 8 + 1;
}

/// Advance the attestation epoch. STRICTLY increasing only — a replayed or
/// stale attestation (epoch ≤ current) is rejected. Mirror of
/// `matcher::fill_commitment::advance_settlement_seq`, kept here so the
/// reserved-margin path has its own proven monotonic guard.
#[inline]
pub fn advance_epoch(current: u64, proposed: u64) -> Result<u64> {
    require!(proposed > current, CloberError::ErEpochReplay);
    Ok(proposed)
}

/// Cross-domain gate for the simple withdraw path (trader has NO filled
/// positions, so `open_positions == 0` is enforced separately by the handler).
/// Post-withdrawal collateral must still cover the ER reserved margin.
///
/// Returns Ok iff `amount <= collateral` AND `collateral - amount >= er_reserved`.
/// With `er_reserved == 0` this reduces to the original balance check, so the
/// non-ER path is unaffected.
#[inline]
pub fn check_simple_withdraw(collateral: u64, amount: u64, er_reserved: u64) -> Result<()> {
    require!(amount <= collateral, CloberError::InsufficientCollateral);
    let remaining = collateral - amount;
    require!(remaining >= er_reserved, CloberError::ErMarginReserved);
    Ok(())
}

/// Pure core for an INTERNAL collateral transfer between two of the same
/// trader's accounts: move `amount` quote-lots from
/// `src` to `dst`, leaving at least `er_reserved` behind on the source. Returns
/// the new `(src, dst)` balances.
///
/// This IS the transfer handlers' code path — `transfer_main_to_sub` /
/// `transfer_sub_to_main` mutate the two `collateral_quote_lots` fields ONLY via
/// this function (enforced by `proven_wrapper_enforcement`), so the balance
/// arithmetic is proven here rather than inline. Reuses the already-proven
/// `check_simple_withdraw` gate — no duplicated guard. TOTAL collateral is
/// conserved: `src_after + dst_after == src + dst` (proven in
/// `collateral_transfer_conserves_total`).
#[inline]
pub fn apply_collateral_transfer(
    src: u64,
    dst: u64,
    amount: u64,
    er_reserved: u64,
) -> Result<(u64, u64)> {
    check_simple_withdraw(src, amount, er_reserved)?; // amount ≤ src, src − amount ≥ er_reserved
    let src_after = src - amount; // gate ⇒ no underflow
    let dst_after = dst
        .checked_add(amount)
        .ok_or_else(|| error!(CloberError::ArithmeticOverflow))?;
    Ok((src_after, dst_after))
}

/// Pure core for the CROSS→ISOLATED margin conversion: move `amount`
/// collateral from the trader's pooled cross balance into a fresh isolated
/// position (which held 0). Returns `(cross_after, isolated_after)`. Preserves
/// the exact `ArithmeticUnderflow` error the inline path used. Conserves total:
/// `cross_after + isolated_after == cross` (proven in `split_to_isolated_conserves`).
#[inline]
pub fn split_to_isolated(cross: u64, amount: u64) -> Result<(u64, u64)> {
    let cross_after = cross
        .checked_sub(amount)
        .ok_or_else(|| error!(CloberError::ArithmeticUnderflow))?;
    Ok((cross_after, amount)) // fresh isolated position started at 0
}

/// Pure core for the ISOLATED→CROSS margin conversion: return all of
/// an isolated position's `isolated` collateral to the pooled cross balance.
/// Returns `cross_after`. Preserves the exact `ArithmeticOverflow` error.
/// Conserves total: `cross_after == cross + isolated` (proven in
/// `merge_to_cross_conserves`); the isolated position is then zeroed by the caller.
#[inline]
pub fn merge_to_cross(cross: u64, isolated: u64) -> Result<u64> {
    cross
        .checked_add(isolated)
        .ok_or_else(|| error!(CloberError::ArithmeticOverflow))
}

/// Pure core for a LIQUIDATION-REWARD payment: pay the liquidator a
/// `reward`, capped at the liquidated source's available collateral, moving it
/// from `src` (the liquidated position or trader_state) to `caller` (the
/// liquidator's trader_state). Returns `(src_after, caller_after, paid)`.
/// Preserves the exact `ArithmeticOverflow` error. Conserves total collateral
/// (`src_after + caller_after == src + caller`) and never over-rewards
/// (`paid <= reward`), proven in `liquidation_reward_conserves`.
#[inline]
pub fn apply_liquidation_reward(src: u64, caller: u64, reward: u64) -> Result<(u64, u64, u64)> {
    let paid = reward.min(src); // capped at the source's available collateral
    let src_after = src - paid; // paid ≤ src ⇒ no underflow
    let caller_after = caller
        .checked_add(paid)
        .ok_or_else(|| error!(CloberError::ArithmeticOverflow))?;
    Ok((src_after, caller_after, paid))
}

/// Pure core for a CAPPED collateral debit: remove up to `amount`
/// from `balance`, capped at what's available (a fee that exceeds the balance
/// takes only the balance). Returns `(balance_after, debited)`. Conserves the
/// removed value exactly (`balance_after + debited == balance`) and never
/// over-charges (`debited <= amount`) — proven in `capped_debit_conserves`.
/// Used for the taker-fee deduction in `apply_fill`.
#[inline]
pub fn apply_capped_debit(balance: u64, amount: u64) -> (u64, u64) {
    let debited = amount.min(balance);
    (balance - debited, debited) // debited ≤ balance ⇒ no underflow
}

// ─────────────────────────────────────────────────────────────────────
// COMMITTED-MARGIN RESERVATION — pure arithmetic core (DORMANT).
//
// The intake initial-margin gate reserves nothing at placement, so on an
// UNDELEGATED market a trader can rest an L1 order, withdraw the backing
// collateral, and have it fill undercollateralized (the flat-start race). These
// pure ops were built as the reserve↔release core for a per-trader `reserved_im`
// accumulator to close that — proven here once so the accounting cannot drift at
// call sites.
//
// They are DELIBERATELY UNWIRED. A sound + complete on-chain reservation is
// architecturally precluded: there is no per-trader live-order anchor to prove
// completeness, and the removal sites that
// fire without the owner — bulk `reap_expired_orders` and the maker side of a
// taker walk — cannot carry the owner's `TraderState`, so any accumulator drifts
// and would permanently over-lock collateral. The residual loss is instead BOUNDED
// (insurance/ADL, Kani-proven in `matcher::insurance`); the ER-delegated path is
// already closed by the sequencer-attested `ErMarginAttestation`. `incremental_im`
// is retained (it also defines the intake gate's rounding); `reserve_add` /
// `reserve_release` are kept as proven building blocks should a future
// off-chain-attested design for the undelegated path ever want them.
// ─────────────────────────────────────────────────────────────────────

/// Incremental INITIAL margin an order commits, computed EXACTLY as the intake
/// gate does (`assert_intake_initial_margin`): `⌈notional·im_bps / BPS_DENOM⌉`
/// where `notional = size·price·tick`. Rounded UP and saturating so a reservation
/// can NEVER understate the true requirement (a stale/large order saturates to
/// `u64::MAX`, blocking the open rather than under-reserving). `im_bps == 0`
/// (market opted out of IM) reserves nothing.
#[inline]
pub fn incremental_im(size_lots: u64, price_ticks: u64, tick_size: u64, im_bps: u32) -> u64 {
    if im_bps == 0 {
        return 0;
    }
    let notional = (size_lots as u128)
        .saturating_mul(price_ticks as u128)
        .saturating_mul(tick_size as u128);
    let im = notional
        .saturating_mul(im_bps as u128)
        .div_ceil(crate::constants::BPS_DENOM as u128);
    if im > u64::MAX as u128 {
        u64::MAX
    } else {
        im as u64
    }
}

/// Reserve margin for a newly-resting order: `reserved + add`, CHECKED (an
/// overflow errors rather than wrapping, so the reservation can never silently
/// shrink). Every reserve site routes through this.
#[inline]
pub fn reserve_add(reserved: u64, add: u64) -> Result<u64> {
    reserved
        .checked_add(add)
        .ok_or_else(|| error!(CloberError::ArithmeticOverflow))
}

/// Release margin for an order leaving the book (cancel / expiry / fill):
/// `reserved − sub`, SATURATING at 0. Saturation (not checked) is REQUIRED for
/// safety: an order that was placed BEFORE this feature shipped carries no
/// reservation, so its release would otherwise underflow — saturating makes the
/// release a safe no-op for such default orders and can never lock a trader out
/// by driving `reserved` negative. It can only ever equal or over-release toward
/// 0, never under-release, so the invariant `reserved ≥ Σ live-order IM` is
/// preserved (over-release only frees the trader's own collateral). Every
/// release site routes through this.
#[inline]
pub fn reserve_release(reserved: u64, sub: u64) -> u64 {
    reserved.saturating_sub(sub)
}

/// Pure core for a CHECKED collateral credit: add `amount` to
/// `balance`, erroring on overflow with the exact `ArithmeticOverflow`. Returns
/// the new balance. `balance_after == balance + amount` when Ok (proven in
/// `collateral_credit_exact`). Used for the rebate credits in `apply_fill`.
#[inline]
pub fn apply_collateral_credit(balance: u64, amount: u64) -> Result<u64> {
    balance
        .checked_add(amount)
        .ok_or_else(|| error!(CloberError::ArithmeticOverflow))
}

/// Pure core for a CHECKED collateral debit: subtract `amount` from
/// `balance`, erroring on underflow with the exact `InsufficientCollateral`.
/// Returns the new balance. `balance_after == balance - amount` when Ok (proven
/// in `collateral_debit_exact`). Used for the maker-fee deduction in `apply_fill`.
#[inline]
pub fn apply_collateral_debit_checked(balance: u64, amount: u64) -> Result<u64> {
    balance
        .checked_sub(amount)
        .ok_or_else(|| error!(CloberError::InsufficientCollateral))
}

/// Pure core for a CHECKED debit that reports the exact `ArithmeticUnderflow`
/// error: subtract `amount` from `balance`, erroring on underflow.
/// Returns `balance - amount` when Ok (proven in `collateral_debit_underflow_exact`).
/// Used where the on-chain path historically used `checked_sub().ok_or(
/// ArithmeticUnderflow)` — withdraw / xdomain-withdraw / insurance payout.
#[inline]
pub fn apply_collateral_debit_underflow(balance: u64, amount: u64) -> Result<u64> {
    balance
        .checked_sub(amount)
        .ok_or_else(|| error!(CloberError::ArithmeticUnderflow))
}

/// Resolve the ER reserved margin a collateral-releasing path must leave
/// behind on the source trader_state. Fail-closed in both directions: an
/// ER-active source (live attested reservation) must supply its own bound
/// attestation, and a supplied attestation must belong to the source — a
/// stranger's (or stale sub-account's) attestation can never understate the
/// reservation. A source that was never attested (or attested back to zero)
/// reserves nothing.
pub fn resolve_er_reserved(
    source_state: Pubkey,
    source_er_active: u8,
    attestation: Option<&ErMarginAttestation>,
) -> Result<u64> {
    match attestation {
        Some(a) => {
            require_keys_eq!(
                a.trader_state,
                source_state,
                CloberError::ErMarginAccountMismatch
            );
            Ok(a.reserved_margin_quote_lots)
        }
        None => {
            require!(source_er_active == 0, CloberError::UseXDomainWithdraw);
            Ok(0)
        }
    }
}

/// Cross-domain required-collateral floor for the PARTIAL withdraw path (trader
/// has filled positions). The existing gate requires post-withdrawal collateral
/// `>= max(im_required, notional_floor)`; the cross-domain variant adds the ER
/// reserved margin on top (saturating), so collateral must cover BOTH the filled
/// positions' requirement AND the resting ER orders' reservation.
///
/// `required = max(im_required, notional_floor) + er_reserved` (saturating).
#[inline]
pub fn required_collateral_with_er(im_required: u64, notional_floor: u64, er_reserved: u64) -> u64 {
    let base = if im_required > notional_floor {
        im_required
    } else {
        notional_floor
    };
    base.saturating_add(er_reserved)
}

/// OI-vs-insurance circuit-breaker predicate. TRUE iff the market's GROSS
/// open-interest notional exceeds the effective cap
/// `max(insurance_balance · multiple_bps / BPS_DENOM, floor_notional)`.
/// `multiple_bps == 0` DISABLES the breaker (returns false), so default markets
/// that never opted in are unaffected.
///
/// BOOTSTRAP FLOOR (`floor_notional`): the pure insurance-scaled cap collapses to
/// ~0 when the fund is empty, so enabling the breaker on a FRESH (near-zero-
/// insurance) market would auto-pause it on the very first fill — the cap is
/// un-turn-on-able at launch. `floor_notional` is an absolute gross-notional the
/// market may always carry regardless of insurance; the effective cap is the MAX
/// of the two, so OI up to the floor is tolerated while the fund is thin and the
/// insurance-scaled term takes over once it grows past
/// `floor_notional · BPS_DENOM / multiple_bps`. Because the cap is `max(scaled,
/// floor) >= scaled`, adding a floor can only ever RAISE the ceiling — it never
/// trips more often than the floorless breaker (`oi_breaker_floor_only_loosens`),
/// and a market whose gross OI is within its floor can never be bricked
/// (`oi_breaker_floor_never_bricks_bootstrap`). `floor_notional == 0` = no floor
/// (pure insurance-scaled), the default behaviour.
///
/// All arithmetic is 128-bit SATURATING: it can never overflow or panic, and an
/// overflowed notional saturates to the max and trips the breaker — fail-safe
/// (pause rather than silently permit unbounded OI). Pure, so the settlement path
/// gains only a status-flag write, never new fallible math. Proven in
/// `oi_breaker_disabled_is_false` / `oi_breaker_no_overflow` /
/// `oi_breaker_floor_never_bricks_bootstrap` / `oi_breaker_floor_only_loosens`.
pub fn oi_exceeds_insurance_cap(
    oi_long_lots: u64,
    oi_short_lots: u64,
    mark_ticks: u64,
    tick_size: u64,
    insurance_balance: u64,
    multiple_bps: u64,
    floor_notional: u64,
) -> bool {
    if multiple_bps == 0 {
        return false;
    }
    let gross_notional = (oi_long_lots as u128)
        .saturating_add(oi_short_lots as u128)
        .saturating_mul(mark_ticks as u128)
        .saturating_mul(tick_size as u128);
    let insurance_cap = (insurance_balance as u128).saturating_mul(multiple_bps as u128)
        / crate::constants::BPS_DENOM as u128;
    // Effective cap = max(insurance-scaled, absolute bootstrap floor). Never below
    // the floor ⇒ a thin fund can't brick a market whose OI is within its floor.
    notional_exceeds_effective_cap(gross_notional, insurance_cap, floor_notional)
}

/// Division-free core of the OI-breaker cap comparison: TRUE iff `gross_notional`
/// exceeds the effective cap `max(insurance_cap, floor_notional)`. Factored out of
/// [`oi_exceeds_insurance_cap`] so the floor's bootstrap-safety and loosen-only
/// properties can be machine-proven (Kani) over a SYMBOLIC `insurance_cap` — which
/// sidesteps the u128 `/ BPS_DENOM` non-power-of-2 division the full predicate
/// performs. CBMC bit-blasts that divider by the u128 TYPE width (not the value
/// range), so a proof that keeps the division live does not terminate in a
/// practical bound (the same limitation documented for `incremental_im`). The
/// division-dependent behaviour is instead pinned by the host test
/// `oi_insurance_breaker_predicate`. Pure, total, no division.
#[inline]
pub fn notional_exceeds_effective_cap(
    gross_notional: u128,
    insurance_cap: u128,
    floor_notional: u64,
) -> bool {
    gross_notional > insurance_cap.max(floor_notional as u128)
}

/// Backstop gate (Phase 1): TRUE iff the market's worst-case TAIL gap loss —
/// the bad debt if the price gaps to the per-market underwritten `tail_bps`
/// BEYOND maintenance — exceeds the insurance fund. Leverage is thereby bounded
/// by the fund: a stress tier whose worst gap loss the fund cannot absorb is
/// refused by `set_market_stress_tier`.
///
///   gap_bps = max(0, tail_bps − mm_bps)          // gap PAST maintenance
///   loss    = oi_cap_notional · gap_bps / BPS    // conservative: full OI, zero recovery
///   trips iff loss > insurance_balance
///
/// `tail_bps` is the per-market black-swan tail the fund underwrites — DISTINCT
/// from (and ≥) the margin `stress_shock_bps`; the setter enforces `tail ≥
/// max(BASELINE_STRESS_SHOCK_BPS, stress_shock)` so `tail > mm` for any leverage-
/// unlocking tier ⇒ the gate is never vacuous (`backstop_gate_is_non_vacuous`).
/// All arithmetic is 128-bit SATURATING — an overflowed loss trips (fail-safe,
/// never panics). The `/ BPS_DENOM` divide is pinned by the host grid-test
/// `backstop_gap_loss_predicate`; the comparison is proven division-free over a
/// symbolic pre-divided `loss` in `gap_loss_exceeds_insurance`.
pub fn worst_gap_loss_exceeds_insurance(
    oi_cap_notional: u128,
    tail_bps: u32,
    mm_bps: u32,
    insurance_balance: u64,
) -> bool {
    let gap_bps = tail_gap_bps(tail_bps, mm_bps) as u128;
    let loss = oi_cap_notional.saturating_mul(gap_bps) / crate::constants::BPS_DENOM as u128;
    gap_loss_exceeds_insurance(loss, insurance_balance)
}

/// Division-free core of the backstop comparison: TRUE iff the (already-divided)
/// tail-gap `loss` exceeds `insurance_balance`. Factored out so the bound is
/// Kani-provable over a SYMBOLIC `loss`, sidestepping the u128 `/ BPS_DENOM`
/// divide CBMC cannot discharge (same discipline as
/// `notional_exceeds_effective_cap`). Pure, total, no division.
#[inline]
pub fn gap_loss_exceeds_insurance(loss: u128, insurance_balance: u64) -> bool {
    loss > insurance_balance as u128
}

/// The tail gap PAST maintenance, in bps: `max(0, tail_bps − mm_bps)`.
/// Division-free; the non-vacuity guarantee (`tail > mm ⇒ gap > 0`) is proven
/// on this in `backstop_gate_is_non_vacuous`.
#[inline]
pub fn tail_gap_bps(tail_bps: u32, mm_bps: u32) -> u32 {
    tail_bps.saturating_sub(mm_bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_im_matches_the_intake_gate_rounding() {
        // notional = 10 · 100_000 · 1 = 1_000_000; im 250bps ⇒ 25_000 (exact).
        assert_eq!(incremental_im(10, 100_000, 1, 250), 25_000);
        // Rounds UP: notional 3, im 1bps ⇒ 3·1/10000 = 0.0003 ⇒ ceil = 1.
        assert_eq!(incremental_im(1, 3, 1, 1), 1);
        // im_bps == 0 ⇒ market opted out ⇒ reserves nothing.
        assert_eq!(incremental_im(1_000_000, 100_000, 1, 0), 0);
        // Saturates rather than wrapping at extreme size.
        assert_eq!(
            incremental_im(u64::MAX, u64::MAX, u64::MAX, 10_000),
            u64::MAX
        );
    }

    /// IM NEVER UNDERSTATES (exhaustive grid): over a dense grid of realistic
    /// inputs, `incremental_im` equals the exact ceiling `ceil(notional·im_bps /
    /// BPS_DENOM)` (saturated to u64) — so the reservation is never LESS than the
    /// true requirement (no under-charge) and never more than one unit over (matches
    /// the intake gate's `div_ceil`). Covers what the u128 `div_ceil` makes
    /// intractable for the bounded model checker; the two conservation properties
    /// (round-trip exactness, release saturation) remain machine-proved in Kani.
    #[test]
    fn incremental_im_never_understates_over_grid() {
        let bps_denom = crate::constants::BPS_DENOM as u128;
        for &size in &[0u64, 1, 3, 10, 999, 100_000, 1 << 20] {
            for &price in &[0u64, 1, 7, 250, 100_000, 1 << 20] {
                for &tick in &[1u64, 2, 8, 15] {
                    for &im_bps in &[0u32, 1, 25, 250, 1_000, 5_000, 10_000] {
                        let reserved = incremental_im(size, price, tick, im_bps);
                        let notional = (size as u128) * (price as u128) * (tick as u128);
                        let numerator = notional * (im_bps as u128);
                        let exact_ceil = numerator.div_ceil(bps_denom);
                        let expected = if exact_ceil > u64::MAX as u128 {
                            u64::MAX
                        } else {
                            exact_ceil as u64
                        };
                        // Exact match ⇒ never understates AND matches the gate rounding.
                        assert_eq!(
                            reserved, expected,
                            "size={size} price={price} tick={tick} im_bps={im_bps}"
                        );
                        // The defining ceiling property, checked division-free too.
                        assert!((reserved as u128) * bps_denom >= numerator);
                    }
                }
            }
        }
    }

    #[test]
    fn oi_insurance_breaker_predicate() {
        let bps = crate::constants::BPS_DENOM as u64; // 10_000
                                                      // Disabled (multiple 0) never trips, whatever the OI/floor.
        assert!(!oi_exceeds_insurance_cap(
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            0,
            0,
            0
        ));
        // multiple = 10_000 bps = 1×, no floor: cap == insurance balance (quote lots).
        // gross_notional = (long+short)·mark·tick. long=short=5, mark=100, tick=1
        // ⇒ 10·100·1 = 1_000. insurance 1_000, 1× cap = 1_000 ⇒ NOT exceeded (>).
        assert!(!oi_exceeds_insurance_cap(5, 5, 100, 1, 1_000, bps, 0));
        // insurance 999 ⇒ cap 999 < 1_000 ⇒ tripped.
        assert!(oi_exceeds_insurance_cap(5, 5, 100, 1, 999, bps, 0));
        // 10× multiple (100_000 bps): cap = insurance·10. notional 1_000 vs
        // insurance 100 → cap 1_000 ⇒ not exceeded; insurance 99 → cap 990 ⇒ tripped.
        assert!(!oi_exceeds_insurance_cap(5, 5, 100, 1, 100, 10 * bps, 0));
        assert!(oi_exceeds_insurance_cap(5, 5, 100, 1, 99, 10 * bps, 0));
        // Saturation is fail-safe: an overflowing notional trips (never panics).
        assert!(oi_exceeds_insurance_cap(
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            1,
            bps,
            0
        ));

        // ── Bootstrap floor ──────────────────────────────────────────────
        // Zero insurance + enabled breaker (1×) would give cap 0 and brick the
        // market at any OI. A floor of 1_000 lets gross OI up to 1_000 stand.
        // notional 1_000, insurance 0, floor 1_000 ⇒ cap max(0, 1_000)=1_000 ⇒ OK.
        assert!(!oi_exceeds_insurance_cap(5, 5, 100, 1, 0, bps, 1_000));
        // One tick over the floor with a still-empty fund ⇒ tripped.
        assert!(oi_exceeds_insurance_cap(5, 5, 100, 1, 0, bps, 999));
        // The floor is a LOWER bound on the cap: once insurance·multiple exceeds
        // the floor, the insurance-scaled term governs. insurance 5_000 (1× ⇒ cap
        // 5_000) with a small floor 100 ⇒ notional 1_000 well under 5_000 ⇒ OK.
        assert!(!oi_exceeds_insurance_cap(5, 5, 100, 1, 5_000, bps, 100));
        // A floor never trips MORE than floorless: with insurance 999 (cap 999) the
        // floorless breaker trips on notional 1_000, but floor 2_000 rescues it.
        assert!(oi_exceeds_insurance_cap(5, 5, 100, 1, 999, bps, 0));
        assert!(!oi_exceeds_insurance_cap(5, 5, 100, 1, 999, bps, 2_000));
    }

    /// BACKSTOP end-to-end (pins the `/ BPS_DENOM` divide the Kani core omits):
    /// over a grid, `worst_gap_loss_exceeds_insurance` trips iff the exact
    /// conservative loss `OI_cap · max(0, tail−mm) / BPS` exceeds insurance, is
    /// fail-safe on overflow (saturates ⇒ trips), and is NON-VACUOUS whenever
    /// tail > mm with a nonzero OI cap.
    #[test]
    fn backstop_gap_loss_predicate() {
        let bps = crate::constants::BPS_DENOM as u128; // 10_000
                                                       // tail 30%, mm 1.5% ⇒ gap 28.5%. OI cap 1_000_000 ⇒ loss 285_000.
        let oi = 1_000_000u128;
        let loss = oi * (3000 - 150) / bps; // 285_000
        assert_eq!(loss, 285_000);
        assert!(!worst_gap_loss_exceeds_insurance(oi, 3000, 150, 285_000)); // == cap ⇒ not exceeded
        assert!(worst_gap_loss_exceeds_insurance(oi, 3000, 150, 284_999)); // one under ⇒ tripped
                                                                           // tail == mm ⇒ zero gap ⇒ zero loss ⇒ never trips (fund 0 ok).
        assert!(!worst_gap_loss_exceeds_insurance(oi, 3000, 3000, 0));
        assert!(!worst_gap_loss_exceeds_insurance(oi, 150, 150, 0));
        // Non-vacuity: tail > mm with a real OI ⇒ requires strictly-positive fund.
        assert!(worst_gap_loss_exceeds_insurance(oi, 3000, 150, 0));
        // Fail-safe: an overflowing OI·gap saturates and trips (never panics).
        assert!(worst_gap_loss_exceeds_insurance(
            u128::MAX,
            3000,
            150,
            u64::MAX
        ));
        // Grid: matches the exact conservative formula.
        for &oi in &[0u128, 1, 1_000, 1_000_000, 1u128 << 60] {
            for &tail in &[150u32, 500, 1_000, 3_000, 6_000] {
                for &mm in &[25u32, 150, 500, 3_000] {
                    for &ins in &[0u64, 1, 285_000, u64::MAX] {
                        let gap = tail.saturating_sub(mm) as u128;
                        let exact = oi.saturating_mul(gap) / bps;
                        assert_eq!(
                            worst_gap_loss_exceeds_insurance(oi, tail, mm, ins),
                            exact > ins as u128,
                            "oi={oi} tail={tail} mm={mm} ins={ins}"
                        );
                    }
                }
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(8000))]
        /// BACKSTOP NON-VACUITY (host-pinned; the symbolic u128×u128 product CBMC
        /// cannot discharge): whenever the underwritten tail exceeds the
        /// maintenance floor and the OI cap is nonzero, the worst-case required
        /// loss is STRICTLY positive — so with an empty fund the gate TRIPS. Hence
        /// the backstop genuinely bounds leverage by the fund (it is not a no-op).
        #[test]
        fn backstop_nonzero_gap_forces_nonzero_loss_proptest(
            oi in 1u128..=(u128::MAX / crate::constants::BPS_DENOM as u128),
            mm in 0u32..=500_000u32,
            gap_bps in 1u32..=500_000u32,
        ) {
            // Construct tail = mm + gap (gap ≥ 1) so tail > mm holds BY
            // CONSTRUCTION (no rejects). The gap PAST maintenance is strictly
            // positive; when oi·gap ≥ BPS the floored loss is ≥ 1 > 0, so an empty
            // fund is provably uncovered (the gate trips) — the backstop is not a
            // no-op.
            let tail = mm + gap_bps; // ≤ 1_000_000, no overflow
            let gap = gap_bps as u128;
            if oi.saturating_mul(gap) >= crate::constants::BPS_DENOM as u128 {
                proptest::prelude::prop_assert!(
                    worst_gap_loss_exceeds_insurance(oi, tail, mm, 0),
                    "oi={oi} tail={tail} mm={mm}: nonzero gap+OI must require a nonzero fund"
                );
            }
        }
    }

    #[test]
    fn reservation_round_trip_and_saturation() {
        let r = reserve_add(0, 25_000).unwrap();
        assert_eq!(r, 25_000);
        let r = reserve_add(r, 5_000).unwrap();
        assert_eq!(r, 30_000);
        assert_eq!(reserve_release(r, 5_000), 25_000);
        assert_eq!(reserve_release(25_000, 25_000), 0); // place→cancel nets to 0
                                                        // Default (unreserved) order release saturates, never underflows.
        assert_eq!(reserve_release(0, 25_000), 0);
        // Reserve overflow errors rather than wrapping.
        assert!(reserve_add(u64::MAX, 1).is_err());
    }

    #[test]
    fn epoch_must_strictly_increase() {
        assert_eq!(advance_epoch(0, 1).unwrap(), 1);
        assert_eq!(advance_epoch(5, 9).unwrap(), 9);
        // Replays and stale epochs are rejected.
        assert!(advance_epoch(5, 5).is_err());
        assert!(advance_epoch(5, 4).is_err());
        assert!(advance_epoch(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn simple_withdraw_respects_reserved_margin() {
        // No ER reservation ⇒ pure balance check (original behavior).
        assert!(check_simple_withdraw(100, 100, 0).is_ok());
        assert!(check_simple_withdraw(100, 101, 0).is_err());
        // With reservation: can pull down to exactly the reserved floor...
        assert!(check_simple_withdraw(100, 40, 60).is_ok());
        // ...but not one lot past it.
        assert!(check_simple_withdraw(100, 41, 60).is_err());
        // Reserved == full collateral ⇒ nothing withdrawable.
        assert!(check_simple_withdraw(100, 1, 100).is_err());
        assert!(check_simple_withdraw(100, 0, 100).is_ok());
    }

    #[test]
    fn resolve_er_reserved_binds_attestation_and_fails_closed() {
        let state = Pubkey::new_from_array([7u8; 32]);
        let other = Pubkey::new_from_array([8u8; 32]);
        let att = |ts: Pubkey, reserved: u64| ErMarginAttestation {
            trader_state: ts,
            attestor: Pubkey::default(),
            reserved_margin_quote_lots: reserved,
            epoch: 1,
            bump: 0,
        };
        // Bound attestation ⇒ its reservation, verbatim (even when zero).
        assert_eq!(
            resolve_er_reserved(state, 1, Some(&att(state, 42))).unwrap(),
            42
        );
        assert_eq!(
            resolve_er_reserved(state, 0, Some(&att(state, 0))).unwrap(),
            0
        );
        // A stranger's attestation can never understate the reservation.
        assert!(resolve_er_reserved(state, 1, Some(&att(other, 0))).is_err());
        // ER-active with no attestation supplied ⇒ fail closed.
        assert!(resolve_er_reserved(state, 1, None).is_err());
        // Never attested ⇒ nothing reserved.
        assert_eq!(resolve_er_reserved(state, 0, None).unwrap(), 0);
    }

    #[test]
    fn required_floor_adds_reserved_on_top_of_max() {
        // base = max(im, floor); +er_reserved.
        assert_eq!(required_collateral_with_er(30, 50, 20), 70);
        assert_eq!(required_collateral_with_er(80, 50, 20), 100);
        assert_eq!(required_collateral_with_er(0, 0, 0), 0);
        // Saturating — never wraps.
        assert_eq!(required_collateral_with_er(u64::MAX, 0, 10), u64::MAX);
    }
}

/// FV: the cross-domain gate never lets collateral fall below the ER reservation,
/// and the epoch guard is strictly monotonic — the two invariants that make the
/// reserved-margin bridge safe against bad debt and replay.
#[cfg(kani)]
mod xmargin_kani_proofs {
    use super::*;

    /// BACKSTOP (T4): the division-free backstop comparison is EXACT and
    /// MONOTONE over the whole domain (no division). A larger tail-gap loss can
    /// only make the gate MORE likely to trip; a larger fund only LESS likely —
    /// so the gate is a sound one-sided bound (it never lets leverage through
    /// that the fund cannot cover).
    #[kani::proof]
    fn backstop_gap_gate_is_monotone_and_exact() {
        let loss: u128 = kani::any();
        let insurance: u64 = kani::any();
        let trips = gap_loss_exceeds_insurance(loss, insurance);
        assert!(trips == (loss > insurance as u128));
        let more: u128 = kani::any();
        if more >= loss && trips {
            assert!(gap_loss_exceeds_insurance(more, insurance));
        }
        let more_ins: u64 = kani::any();
        if more_ins >= insurance && !trips {
            assert!(!gap_loss_exceeds_insurance(loss, more_ins));
        }
    }

    /// BACKSTOP NON-VACUITY: whenever the underwritten tail exceeds the
    /// maintenance floor — the setter enforces `tail ≥ max(3000, shock)` and
    /// (decision A) `mm = max(shock, 25)`, so `tail > mm` for every leverage-
    /// unlocking tier — the gap PAST maintenance is STRICTLY positive, and a
    /// nonzero OI cap forces a strictly-positive worst-case loss. Hence the gate
    /// CAN trip: leverage is genuinely bounded by the fund (not a no-op). This is
    /// the proof that the resolved model is not vacuous. Division-free.
    #[kani::proof]
    fn backstop_gate_is_non_vacuous() {
        // Non-vacuity reduces to: whenever the underwritten tail exceeds the
        // maintenance floor (the setter enforces tail ≥ max(3000, shock) and,
        // decision A, mm = max(shock, 25), so tail > mm for every leverage-
        // unlocking tier), the gap PAST maintenance is STRICTLY positive — so a
        // nonzero OI cap forces a strictly-positive required loss and the gate
        // CAN trip (leverage is genuinely fund-bounded, not a no-op). Only this
        // `gap > 0` step is proven in Kani (division-free, no multiply). The
        // remaining `oi > 0 ∧ gap > 0 ⇒ oi·gap > 0` is a symbolic u128×u128
        // product CBMC cannot discharge (same class as symbolic division, Law 5);
        // it is pinned by the host proptest
        // `backstop_nonzero_gap_forces_nonzero_loss_proptest`.
        let tail: u32 = kani::any();
        let mm: u32 = kani::any();
        kani::assume(tail > mm);
        assert!(tail_gap_bps(tail, mm) > 0);
    }

    /// SAFETY: if the simple-withdraw gate passes, the post-withdrawal collateral
    /// is provably ≥ the ER reserved margin (collateral backing resting ER orders
    /// can never be withdrawn).
    #[kani::proof]
    fn simple_withdraw_preserves_reserved_margin() {
        let collateral: u64 = kani::any();
        let amount: u64 = kani::any();
        let er_reserved: u64 = kani::any();
        if check_simple_withdraw(collateral, amount, er_reserved).is_ok() {
            // amount <= collateral was required, so this does not underflow.
            assert!(amount <= collateral);
            assert!(collateral - amount >= er_reserved);
        }
    }

    /// CONSERVATION: the internal collateral transfer core neither
    /// mints nor burns collateral — the total across the two accounts is
    /// invariant, the source keeps its ER reservation, and exactly `amount`
    /// moves. Proven on the REAL `apply_collateral_transfer` symbol over all
    /// `u64`, so the `transfer_*` handlers that route through it preserve
    /// V = Σ collateral + … .
    #[kani::proof]
    fn collateral_transfer_conserves_total() {
        let src: u64 = kani::any();
        let dst: u64 = kani::any();
        let amount: u64 = kani::any();
        let er_reserved: u64 = kani::any();
        if let Ok((src_after, dst_after)) = apply_collateral_transfer(src, dst, amount, er_reserved)
        {
            // total collateral conserved — nothing minted or burned
            assert!(src_after as u128 + dst_after as u128 == src as u128 + dst as u128);
            // exactly `amount` moved from src to dst
            assert!(src_after == src - amount);
            assert!(dst_after == dst + amount);
            // the source still covers its ER reservation
            assert!(src_after >= er_reserved);
        }
    }

    /// CONSERVATION: the cross→isolated margin conversion moves
    /// `amount` from the cross pool to a fresh isolated position without minting
    /// or burning — `cross_after + isolated_after == cross`, on the real
    /// `split_to_isolated` symbol over all `u64`.
    #[kani::proof]
    fn split_to_isolated_conserves() {
        let cross: u64 = kani::any();
        let amount: u64 = kani::any();
        if let Ok((cross_after, isolated_after)) = split_to_isolated(cross, amount) {
            assert!(cross_after as u128 + isolated_after as u128 == cross as u128);
            assert!(cross_after == cross - amount);
            assert!(isolated_after == amount);
        }
    }

    /// CONSERVATION: the isolated→cross margin conversion returns all
    /// isolated collateral to the cross pool without minting or burning —
    /// `cross_after == cross + isolated`, on the real `merge_to_cross` symbol.
    #[kani::proof]
    fn merge_to_cross_conserves() {
        let cross: u64 = kani::any();
        let isolated: u64 = kani::any();
        if let Ok(cross_after) = merge_to_cross(cross, isolated) {
            assert!(cross_after as u128 == cross as u128 + isolated as u128);
        }
    }

    /// CONSERVATION: the liquidation-reward payment moves the capped
    /// reward from the liquidated source to the liquidator without minting or
    /// burning — `src_after + caller_after == src + caller`, `paid <= reward`,
    /// on the real `apply_liquidation_reward` symbol over all `u64`.
    #[kani::proof]
    fn liquidation_reward_conserves() {
        let src: u64 = kani::any();
        let caller: u64 = kani::any();
        let reward: u64 = kani::any();
        if let Ok((src_after, caller_after, paid)) = apply_liquidation_reward(src, caller, reward) {
            assert!(src_after as u128 + caller_after as u128 == src as u128 + caller as u128);
            assert!(paid <= reward); // never over-rewards
            assert!(src_after == src - paid); // exact debit, capped at src
        }
    }

    /// CONSERVATION: the capped fee debit removes exactly the debited
    /// amount and never more than is available — `balance_after + debited ==
    /// balance`, `debited <= amount`, on the real `apply_capped_debit` symbol.
    #[kani::proof]
    fn capped_debit_conserves() {
        let balance: u64 = kani::any();
        let amount: u64 = kani::any();
        let (after, debited) = apply_capped_debit(balance, amount);
        assert!(after as u128 + debited as u128 == balance as u128);
        assert!(debited <= amount);
        assert!(after <= balance); // never mints collateral
    }

    /// RESERVATION CONSERVATION: reserving an order's IM then releasing the
    /// SAME amount returns `reserved` to its original value exactly (when the add
    /// did not overflow) — so a place→cancel/fill round-trip nets to zero and can
    /// never leak reserved margin (which would lock a trader out of their own
    /// collateral). Over the whole (reserved, im) domain.
    #[kani::proof]
    fn reservation_round_trip_is_exact() {
        let reserved: u64 = kani::any();
        let im: u64 = kani::any();
        if let Ok(after_reserve) = reserve_add(reserved, im) {
            assert!(after_reserve >= reserved); // reserving never shrinks
            let after_release = reserve_release(after_reserve, im);
            assert!(after_release == reserved); // exact round-trip, no leak
        }
    }

    /// RELEASE NEVER UNDERFLOWS: releasing any amount saturates at 0 — a
    /// default order (placed before reservations existed, so unreserved) whose
    /// release exceeds `reserved` is a safe no-op, never a panic or wrap. Release
    /// only ever moves `reserved` toward 0 (frees the trader's own collateral),
    /// so the invariant `reserved >= Σ live-order IM` is preserved.
    #[kani::proof]
    fn reservation_release_saturates() {
        let reserved: u64 = kani::any();
        let sub: u64 = kani::any();
        let after = reserve_release(reserved, sub);
        assert!(after <= reserved); // only ever decreases (or holds), never wraps up
    }

    /// the breaker is DISABLED when `multiple_bps == 0` — it never trips, so
    /// a default market that never opted in is byte-for-byte unaffected. Over the
    /// whole input domain (no assumptions), including ANY floor value — a floor
    /// alone never activates a disabled breaker.
    #[kani::proof]
    fn oi_breaker_disabled_is_false() {
        let oi_long: u64 = kani::any();
        let oi_short: u64 = kani::any();
        let mark: u64 = kani::any();
        let tick: u64 = kani::any();
        let insurance: u64 = kani::any();
        let floor: u64 = kani::any();
        assert!(!oi_exceeds_insurance_cap(
            oi_long, oi_short, mark, tick, insurance, 0, floor
        ));
    }

    /// the predicate is TOTAL — it never panics/overflows for ANY u64 inputs
    /// (all arithmetic is 128-bit saturating). Reaching the assertion at all proves
    /// no arithmetic trap on the way. Whole domain, no assumptions.
    #[kani::proof]
    fn oi_breaker_no_overflow() {
        let oi_long: u64 = kani::any();
        let oi_short: u64 = kani::any();
        let mark: u64 = kani::any();
        let tick: u64 = kani::any();
        let insurance: u64 = kani::any();
        let multiple: u64 = kani::any();
        let floor: u64 = kani::any();
        let _ = oi_exceeds_insurance_cap(oi_long, oi_short, mark, tick, insurance, multiple, floor);
        assert!(true); // reached ⇒ no panic/overflow for any inputs
    }

    /// BOOTSTRAP SAFETY: a market whose GROSS OI notional is within its absolute
    /// floor can NEVER trip the breaker — regardless of the insurance-scaled cap.
    /// This is the property that makes the breaker safely enable-able on a fresh
    /// market: `gross <= floor ⇒ !trips`. Proven over the DIVISION-FREE core
    /// (`notional_exceeds_effective_cap`) with a SYMBOLIC `insurance_cap` — so it
    /// holds for EVERY value the real `insurance · multiple / BPS_DENOM` could take,
    /// including 0 (empty fund) — while the real saturating `gross` computation is
    /// kept intact. No division ⇒ CBMC discharges it fast. Whole domain.
    #[kani::proof]
    fn oi_breaker_floor_never_bricks_bootstrap() {
        let oi_long: u64 = kani::any();
        let oi_short: u64 = kani::any();
        let mark: u64 = kani::any();
        let tick: u64 = kani::any();
        let insurance_cap: u128 = kani::any(); // abstracts insurance·multiple/BPS (any value)
        let floor: u64 = kani::any();
        // Real gross OI notional (same saturating form the predicate uses).
        let gross = (oi_long as u128)
            .saturating_add(oi_short as u128)
            .saturating_mul(mark as u128)
            .saturating_mul(tick as u128);
        // Precondition: the market's OI is within the absolute floor.
        kani::assume(gross <= floor as u128);
        assert!(!notional_exceeds_effective_cap(gross, insurance_cap, floor));
    }

    /// LIVENESS MONOTONICITY: adding (or raising) a floor can only ever LOOSEN the
    /// breaker — it never trips more often than the floorless (pure insurance-
    /// scaled) breaker. Formally: if the FLOORED comparison trips, the FLOORLESS one
    /// (floor 0) trips too. Proven over the division-free core with SYMBOLIC
    /// `gross`/`insurance_cap` (so it covers every real division result); no
    /// division ⇒ fast. A market fine under the plain breaker stays fine with a
    /// floor added; the floor never introduces a new pause.
    #[kani::proof]
    fn oi_breaker_floor_only_loosens() {
        let gross: u128 = kani::any();
        let insurance_cap: u128 = kani::any();
        let floor: u64 = kani::any();
        let floored = notional_exceeds_effective_cap(gross, insurance_cap, floor);
        let floorless = notional_exceeds_effective_cap(gross, insurance_cap, 0);
        // floored ⇒ floorless (a floor only removes trips, never adds them).
        assert!(!floored || floorless);
    }

    // NOTE: `incremental_im`'s "never understates" property is verified exhaustively
    // in the host test `incremental_im_never_understates_over_grid` rather than in
    // Kani: the function's u128 `div_ceil` by BPS_DENOM (a non-power-of-2) forces
    // CBMC to bit-blast a full 128-bit divider circuit — sized by the u128 type, not
    // the value range — which does not terminate in a practical bound. The grid test
    // pins exact equality to the ground-truth ceiling across a dense input grid. The
    // two conservation properties below/above (round-trip exactness, release
    // saturation) contain no such division and remain fully machine-proved.

    /// EXACT: the checked credit adds exactly `amount` when it does not
    /// overflow — on the real `apply_collateral_credit` symbol over all `u64`.
    #[kani::proof]
    fn collateral_credit_exact() {
        let balance: u64 = kani::any();
        let amount: u64 = kani::any();
        if let Ok(after) = apply_collateral_credit(balance, amount) {
            assert!(after as u128 == balance as u128 + amount as u128);
        }
    }

    /// EXACT: the checked debit subtracts exactly `amount` when the
    /// balance covers it — on the real `apply_collateral_debit_checked` symbol.
    #[kani::proof]
    fn collateral_debit_exact() {
        let balance: u64 = kani::any();
        let amount: u64 = kani::any();
        if let Ok(after) = apply_collateral_debit_checked(balance, amount) {
            assert!(after == balance - amount);
            assert!(amount <= balance);
        }
    }

    /// EXACT: the underflow-erroring checked debit subtracts exactly
    /// `amount` when the balance covers it — on the real
    /// `apply_collateral_debit_underflow` symbol over all `u64`.
    #[kani::proof]
    fn collateral_debit_underflow_exact() {
        let balance: u64 = kani::any();
        let amount: u64 = kani::any();
        if let Ok(after) = apply_collateral_debit_underflow(balance, amount) {
            assert!(after == balance - amount);
            assert!(amount <= balance);
        }
    }

    /// MONOTONIC: the epoch guard only accepts a strictly-greater epoch, so a
    /// replayed/stale attestation can never be applied.
    #[kani::proof]
    fn epoch_strictly_increases() {
        let current: u64 = kani::any();
        let proposed: u64 = kani::any();
        if let Ok(next) = advance_epoch(current, proposed) {
            assert!(next > current);
            assert!(next == proposed);
        }
    }

    /// FAIL-CLOSED: the reservation resolver never understates the attested
    /// reserve — with an attestation it returns the bound reservation verbatim
    /// (or rejects a foreign one), and without one it only ever returns 0 for a
    /// source that is provably not ER-active.
    #[kani::proof]
    fn resolve_er_reserved_never_understates() {
        let state = Pubkey::new_from_array(kani::any());
        let er_active: u8 = kani::any();
        let supplied: bool = kani::any();
        let att = ErMarginAttestation {
            trader_state: Pubkey::new_from_array(kani::any()),
            attestor: Pubkey::new_from_array([0u8; 32]),
            reserved_margin_quote_lots: kani::any(),
            epoch: kani::any(),
            bump: kani::any(),
        };
        let attestation = if supplied { Some(&att) } else { None };
        if let Ok(reserved) = resolve_er_reserved(state, er_active, attestation) {
            match attestation {
                Some(a) => {
                    assert!(a.trader_state == state);
                    assert!(reserved == a.reserved_margin_quote_lots);
                }
                None => {
                    assert!(er_active == 0);
                    assert!(reserved == 0);
                }
            }
        }
    }

    /// CONSERVATIVE: the cross-domain required floor is never LESS than either the
    /// filled-position requirement or the ER reservation alone — adding ER margin
    /// only ever tightens the withdraw gate, never loosens it.
    #[kani::proof]
    fn required_floor_is_conservative() {
        let im: u64 = kani::any();
        let floor: u64 = kani::any();
        let er: u64 = kani::any();
        let req = required_collateral_with_er(im, floor, er);
        let base = core::cmp::max(im, floor);
        assert!(req >= base);
        assert!(req >= er);
    }

    /// Rejects self-liquidation by construction: no withdrawal the reserve-margin
    /// gate allows can leave the
    /// account below maintenance margin — so a trader can never withdraw
    /// themselves into a liquidatable state and dump the resulting loss onto the
    /// insurance fund. The partial-withdraw gate admits `amount` only if the
    /// remainder covers `required_collateral_with_er(im, floor, er) =
    /// max(im,floor)+er`. Because initial margin is never below maintenance margin
    /// (`im >= mm`, a protocol invariant), the remainder is provably `>= mm`, and
    /// it still covers the ER reservation. This is the adversarial theorem behind
    /// the launch claim.
    #[kani::proof]
    fn withdraw_cannot_self_liquidate_below_maintenance() {
        let collateral: u64 = kani::any();
        let amount: u64 = kani::any();
        let im: u64 = kani::any();
        let floor: u64 = kani::any();
        let mm: u64 = kani::any();
        let er: u64 = kani::any();
        // Initial margin dominates maintenance margin — always true on-chain.
        kani::assume(im >= mm);
        let required = required_collateral_with_er(im, floor, er);
        // The gate an adversary must pass to release `amount`.
        kani::assume(amount <= collateral);
        kani::assume(collateral - amount >= required);
        let remaining = collateral - amount;
        // The account cannot be pushed below maintenance margin...
        assert!(remaining >= mm);
        // ...nor below the ER reservation (no cross-domain loss dump).
        assert!(remaining >= er);
    }

    /// ROUND-TRIP — moving collateral to isolated and back to cross returns EXACTLY
    /// the original cross balance (no value created or destroyed by the split/merge
    /// pair), for every reachable `(cross, amount)`.
    #[kani::proof]
    fn split_then_merge_is_identity() {
        let cross: u64 = kani::any();
        let amount: u64 = kani::any();
        // split only succeeds when the cross pool can fund it.
        kani::assume(amount <= cross);
        let (cross_after, isolated) = split_to_isolated(cross, amount).unwrap();
        // isolated bucket got exactly `amount`; cross fell by exactly `amount`.
        assert!(isolated == amount);
        assert!(cross_after == cross - amount);
        // merging the isolated bucket back restores the original cross.
        let restored = merge_to_cross(cross_after, isolated).unwrap();
        assert!(restored == cross);
    }

    /// CAPPED DEBIT bounds — `apply_capped_debit` pays at most what is owed AND at
    /// most what the balance holds, and the post-balance is exactly `balance −
    /// paid` (never underflows, never over-charges). This is the fee-debit
    /// primitive on an under-collateralized taker.
    #[kani::proof]
    fn capped_debit_bounds() {
        let balance: u64 = kani::any();
        let amount: u64 = kani::any();
        let (after, paid) = apply_capped_debit(balance, amount);
        assert!(paid <= amount); // never charges more than owed
        assert!(paid <= balance); // never charges more than held
        assert!(after == balance - paid); // exact remainder, no underflow
        assert!(after <= balance); // balance never grows on a debit
    }

    /// LIQUIDATION REWARD is bounded by the source — the reward paid to the caller
    /// never exceeds what the liquidatee's bucket can fund, and the three legs
    /// (source-out, caller-in, leftover) conserve. Complements the existing
    /// `liquidation_reward_conserves` with the non-creation bound.
    #[kani::proof]
    fn liquidation_reward_bounded_by_source() {
        let src: u64 = kani::any();
        let caller: u64 = kani::any();
        let reward: u64 = kani::any();
        if let Ok((src_after, caller_after, paid)) = apply_liquidation_reward(src, caller, reward) {
            assert!(paid <= src); // can't pay a reward the source can't fund
            assert!(src_after == src - paid); // exact debit
            assert!(caller_after >= caller); // caller only ever gains
        }
    }

    /// TRANSFER never mints — `apply_collateral_transfer` moves value between two
    /// balances with the destination gaining exactly what the source loses, and
    /// neither the sum overflows nor the source underflows.
    #[kani::proof]
    fn transfer_moves_exactly_and_never_mints() {
        let from: u64 = kani::any();
        let to: u64 = kani::any();
        let amount: u64 = kani::any();
        let er: u64 = kani::any();
        if let Ok((from_after, to_after)) = apply_collateral_transfer(from, to, amount, er) {
            assert!(from_after == from - amount);
            assert!(to_after == to + amount);
            // total is invariant across the move.
            assert!((from_after as u128) + (to_after as u128) == (from as u128) + (to as u128));
        }
    }

    /// FLOOR MONOTONICITY — the ER-aware required collateral is monotone
    /// non-decreasing in each of its inputs, so a larger margin need / notional
    /// floor / ER reservation can only ever RAISE the requirement (an adversary
    /// can never lower their own requirement by inflating an input).
    #[kani::proof]
    fn required_floor_monotone_in_inputs() {
        let im: u64 = kani::any();
        let floor: u64 = kani::any();
        let er: u64 = kani::any();
        let d: u64 = kani::any();
        let base = required_collateral_with_er(im, floor, er);
        // raising IM never lowers the requirement
        if let Some(im2) = im.checked_add(d) {
            assert!(required_collateral_with_er(im2, floor, er) >= base);
        }
        // raising the ER reservation never lowers the requirement
        if let Some(er2) = er.checked_add(d) {
            assert!(required_collateral_with_er(im, floor, er2) >= base);
        }
    }
}
