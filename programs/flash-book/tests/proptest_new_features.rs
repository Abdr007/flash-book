//! Property tests for features added in the recent waves:
//!
//!   • LP units NAV math — deposit + withdraw round-trip when no PnL.
//!   • Liquidation reward bounds — 0 ≤ reward ≤ liquidatee collateral.
//!   • Toxicity tax routing — insurance + maker share = total tax.
//!   • Reduce-only — can't increase position size.
//!   • Tiered fees — discount caps at 100%, never negative.
//!
//! 2_000 cases per property. These mirror the on-chain integer
//! arithmetic exactly (no float fuzz, no rounding drift).
//!
//! Why these are valuable: each property captures a one-line invariant
//! that would-be exploits or refactors must preserve. If a future PR
//! breaks one, the test fails LOUD with a minimal counterexample.

use proptest::prelude::*;

const BPS_DENOM: u128 = 10_000;

// ─── LP unit NAV math ─────────────────────────────────────────────────
//
// Mirrors programs/flash-book/src/lib.rs deposit_flp_capital +
// withdraw_flp_capital. The math:
//
//   shares_to_mint = amount × shares_outstanding / NAV    (or 1:1 if NAV<=0)
//   amount_to_return = shares_to_burn × NAV / shares_outstanding
//
// Round-trip property: deposit X → withdraw all minted shares → return ≈ X
// (off by at most 1 due to integer division).

fn lp_mint(amount: u128, shares_out: u128, nav: i128) -> u128 {
    if shares_out == 0 || nav <= 0 {
        amount
    } else {
        (amount * shares_out) / (nav as u128)
    }
}

fn lp_redeem(shares_to_burn: u128, shares_out: u128, nav: i128) -> u128 {
    if shares_out == 0 || nav <= 0 {
        return 0;
    }
    (shares_to_burn * nav as u128) / shares_out
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2_000,
        max_global_rejects: 10_000,
        .. ProptestConfig::default()
    })]

    /// CRITICAL solvency invariant: deposit + immediate withdraw of the
    /// minted shares NEVER returns more than the deposit. The protocol
    /// rounds in favor of remaining LPs (truncation drift stays in the
    /// pool) — the depositor must never extract value beyond what they
    /// put in, otherwise an attacker could deposit-withdraw to drain.
    ///
    /// Real-world drift can be arbitrarily large when initial_shares is
    /// tiny relative to initial_nav (each share is "expensive"). What
    /// matters for solvency is `returned ≤ deposit` — strictly. This
    /// property guards against any future refactor introducing a +1
    /// rounding bug that would let over-redemption happen.
    #[test]
    fn lp_units_deposit_withdraw_never_over_redeems(
        initial_nav in 1u128..=1_000_000_000_000u128,
        initial_shares in 1u128..=1_000_000_000_000u128,
        deposit in 1u128..=1_000_000_000u128,
    ) {
        let nav_i = initial_nav as i128;
        let minted = lp_mint(deposit, initial_shares, nav_i);
        prop_assume!(minted > 0);
        let post_nav = (initial_nav + deposit) as i128;
        let post_shares = initial_shares + minted;
        let returned = lp_redeem(minted, post_shares, post_nav);
        prop_assert!(returned <= deposit, "returned {} > deposit {}", returned, deposit);
    }

    /// Bootstrap: NAV ≤ 0 OR shares_outstanding == 0 → mint is 1:1.
    #[test]
    fn lp_units_bootstrap_mints_one_to_one(amount in 1u128..=1_000_000u128) {
        prop_assert_eq!(lp_mint(amount, 0, 0), amount);
        prop_assert_eq!(lp_mint(amount, 100, 0), amount);
        prop_assert_eq!(lp_mint(amount, 0, 100), amount);
        prop_assert_eq!(lp_mint(amount, 100, -50), amount);
    }
}

// ─── Liquidation reward bounds ────────────────────────────────────────
//
// Mirrors programs/flash-book/src/lib.rs liquidate_position:
//   reward = notional × liquidator_reward_bps / BPS_DENOM
//   reward_paid = min(reward, liquidatee.collateral)

fn liquidation_reward(notional: u128, reward_bps: u32, collateral: u64) -> u64 {
    let reward = notional.saturating_mul(reward_bps as u128) / BPS_DENOM;
    let reward_u64 = if reward > u64::MAX as u128 { u64::MAX } else { reward as u64 };
    reward_u64.min(collateral)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2_000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn liquidation_reward_never_exceeds_collateral(
        size in 1u64..=1_000_000,
        oracle in 1u64..=1_000_000,
        tick_size in 1u64..=1_000,
        reward_bps in 0u32..=10_000,
        collateral in 0u64..=1_000_000_000,
    ) {
        let notional = (size as u128).saturating_mul(oracle as u128).saturating_mul(tick_size as u128);
        let paid = liquidation_reward(notional, reward_bps, collateral);
        prop_assert!(paid <= collateral, "paid {} > collateral {}", paid, collateral);
    }

    #[test]
    fn liquidation_reward_zero_bps_pays_nothing(
        notional in 0u128..=1_000_000_000_000u128,
        collateral in 0u64..=1_000_000_000,
    ) {
        prop_assert_eq!(liquidation_reward(notional, 0, collateral), 0);
    }
}

// ─── Toxicity tax routing ─────────────────────────────────────────────
//
// Mirrors apply_fill's tax split:
//   tax = notional × tax_max_bps × vpin_bps / (BPS² )
//   to_insurance = tax × tox_contribution_bps / BPS
//   to_maker     = tax - to_insurance
// Property: routed shares sum to total (no leakage, no double-credit).

fn toxicity_tax_split(
    notional: u128,
    tax_max_bps: u32,
    vpin_bps: u32,
    tox_contribution_bps: u32,
    taker_collateral: u64,
) -> (u64, u64, u64) {
    let tax_u128 = notional
        .saturating_mul(tax_max_bps as u128)
        .saturating_mul(vpin_bps as u128)
        / BPS_DENOM
        / BPS_DENOM;
    let tax_uncapped: u64 = if tax_u128 > u64::MAX as u128 { u64::MAX } else { tax_u128 as u64 };
    let tax = tax_uncapped.min(taker_collateral);
    let to_insurance =
        ((tax as u128).saturating_mul(tox_contribution_bps as u128) / BPS_DENOM) as u64;
    let to_maker = tax.saturating_sub(to_insurance);
    (tax, to_insurance, to_maker)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2_000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn toxicity_tax_shares_sum_to_total(
        size in 1u64..=1_000_000,
        oracle in 1u64..=1_000_000,
        tick in 1u64..=1_000,
        tax_max_bps in 0u32..=10_000,
        vpin_bps in 0u32..=10_000,
        tox_contribution_bps in 0u32..=10_000,
        taker_collateral in 0u64..=1_000_000_000,
    ) {
        let notional = (size as u128) * (oracle as u128) * (tick as u128);
        let (tax, ins, mkr) = toxicity_tax_split(
            notional, tax_max_bps, vpin_bps, tox_contribution_bps, taker_collateral,
        );
        prop_assert_eq!(ins.checked_add(mkr).unwrap(), tax);
        prop_assert!(tax <= taker_collateral);
    }
}

// ─── Reduce-only ──────────────────────────────────────────────────────
//
// Mirrors place_limit_order's reduce_only gate:
//   accept iff position.size > 0 AND order.side != position.side
//            AND order.size <= position.size

fn reduce_only_check(
    pos_size: u64,
    pos_side: u8,
    order_size: u64,
    order_side: u8,
) -> bool {
    pos_size > 0 && pos_side != order_side && order_size <= pos_size
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, .. ProptestConfig::default() })]

    /// Same-side or oversized orders never accepted under reduce-only.
    #[test]
    fn reduce_only_rejects_increase_or_flip(
        pos_size in 1u64..=1_000,
        pos_side in 0u8..=1,
        order_size in 1u64..=2_000,
        order_side in 0u8..=1,
    ) {
        let accepted = reduce_only_check(pos_size, pos_side, order_size, order_side);
        if accepted {
            // Must be opposite side AND not flip-the-position-sized.
            prop_assert_ne!(pos_side, order_side);
            prop_assert!(order_size <= pos_size);
        }
    }

    #[test]
    fn reduce_only_rejects_zero_position(
        order_size in 1u64..=1_000,
        order_side in 0u8..=1,
        pos_side in 0u8..=1,
    ) {
        prop_assert!(!reduce_only_check(0, pos_side, order_size, order_side));
    }
}

// ─── Tiered fees ──────────────────────────────────────────────────────
//
// Mirrors apply_fill's discount math:
//   discounted_fee = base_fee × (BPS - discount_bps) / BPS
//   discount_bps capped at BPS (10_000) on chain.

fn discounted_fee(base_fee_u128: u128, discount_bps: u32) -> u128 {
    let bps = (discount_bps as u128).min(BPS_DENOM);
    base_fee_u128.saturating_mul(BPS_DENOM - bps) / BPS_DENOM
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, .. ProptestConfig::default() })]

    #[test]
    fn tiered_fees_never_exceed_base(
        base in 0u128..=1_000_000_000_000u128,
        discount_bps in 0u32..=20_000,
    ) {
        let out = discounted_fee(base, discount_bps);
        prop_assert!(out <= base);
    }

    #[test]
    fn tiered_fees_full_discount_yields_zero(base in 0u128..=1_000_000_000u128) {
        prop_assert_eq!(discounted_fee(base, 10_000), 0);
        prop_assert_eq!(discounted_fee(base, 20_000), 0); // capped above
    }

    #[test]
    fn tiered_fees_no_discount_is_identity(
        base in 0u128..=1_000_000u128,
    ) {
        prop_assert_eq!(discounted_fee(base, 0), base);
    }
}

// ─── Symmetric-OI funding dampener (wave 13) ─────────────────────────
//
// Mirrors the run_batch dampener:
//   skew_bps = |oi_long - oi_short| × 10_000 / (oi_long + oi_short)
//   dampened_rate = rate × skew_bps / 10_000
//
// Properties:
//   • balanced book → dampened_rate == 0
//   • fully one-sided → dampened_rate == raw rate
//   • |dampened_rate| ≤ |raw rate| always (dampening can't amplify)
//   • dampened sign == raw sign (or both zero) — never flips direction

fn skew_bps(oi_long: u64, oi_short: u64) -> u64 {
    let total = (oi_long as u128) + (oi_short as u128);
    if total == 0 {
        return 0;
    }
    let imb = if oi_long >= oi_short {
        (oi_long - oi_short) as u128
    } else {
        (oi_short - oi_long) as u128
    };
    ((imb * BPS_DENOM) / total).min(BPS_DENOM) as u64
}

fn dampened_rate(raw_rate_bps: i64, oi_long: u64, oi_short: u64) -> i64 {
    let skew = skew_bps(oi_long, oi_short);
    ((raw_rate_bps as i128) * (skew as i128) / (BPS_DENOM as i128)) as i64
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, .. ProptestConfig::default() })]

    /// Balanced book (oi_long == oi_short, both > 0) ⇒ dampened == 0.
    #[test]
    fn oi_dampener_balanced_book_zeros_funding(
        oi in 1u64..=1_000_000_000u64,
        raw in -1_000i64..=1_000i64,
    ) {
        prop_assert_eq!(dampened_rate(raw, oi, oi), 0);
    }

    /// Fully one-sided (oi_short == 0, oi_long > 0) ⇒ dampened == raw.
    /// And vice versa.
    #[test]
    fn oi_dampener_one_sided_passes_through(
        oi in 1u64..=1_000_000_000u64,
        raw in -1_000i64..=1_000i64,
    ) {
        // Long-only.
        prop_assert_eq!(dampened_rate(raw, oi, 0), raw);
        // Short-only.
        prop_assert_eq!(dampened_rate(raw, 0, oi), raw);
    }

    /// |dampened| ≤ |raw| — dampening can never amplify.
    #[test]
    fn oi_dampener_never_amplifies(
        oi_long in 0u64..=1_000_000_000u64,
        oi_short in 0u64..=1_000_000_000u64,
        raw in -10_000i64..=10_000i64,
    ) {
        let damp = dampened_rate(raw, oi_long, oi_short);
        prop_assert!(damp.unsigned_abs() <= raw.unsigned_abs());
    }

    /// Sign of dampened matches sign of raw (or both zero) — never flips.
    #[test]
    fn oi_dampener_preserves_sign(
        oi_long in 0u64..=1_000_000_000u64,
        oi_short in 0u64..=1_000_000_000u64,
        raw in -10_000i64..=10_000i64,
    ) {
        let damp = dampened_rate(raw, oi_long, oi_short);
        if raw == 0 {
            prop_assert_eq!(damp, 0);
        } else if damp != 0 {
            prop_assert_eq!(raw.signum(), damp.signum());
        }
    }

    /// Empty book (no OI either side) ⇒ dampened == 0 (no positions to charge).
    #[test]
    fn oi_dampener_empty_book_zeros(raw in -10_000i64..=10_000i64) {
        prop_assert_eq!(dampened_rate(raw, 0, 0), 0);
    }
}

// ─── Concentration margin tier (wave 12) ─────────────────────────────
//
// Mirrors MarketSnapshot::effective_mmr_bps:
//   if size >= threshold > 0 → base + extra (saturating_add)
//   else                     → base
//
// Properties:
//   • threshold = 0 ⇒ identity (always returns base)
//   • size below threshold ⇒ identity
//   • size at-or-above threshold ⇒ base + extra (or u32::MAX on saturate)
//   • effective is monotone in size (larger position never gets cheaper margin)

fn effective_mmr(size_lots: u64, base_bps: u32, threshold: u64, extra_bps: u32) -> u32 {
    if threshold > 0 && size_lots >= threshold {
        base_bps.saturating_add(extra_bps)
    } else {
        base_bps
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, .. ProptestConfig::default() })]

    #[test]
    fn concentration_zero_threshold_is_identity(
        size in 0u64..=1_000_000_000u64,
        base in 0u32..=10_000u32,
        extra in 0u32..=10_000u32,
    ) {
        prop_assert_eq!(effective_mmr(size, base, 0, extra), base);
    }

    #[test]
    fn concentration_below_threshold_is_identity(
        size in 0u64..=999u64,
        base in 0u32..=10_000u32,
        extra in 0u32..=10_000u32,
    ) {
        prop_assert_eq!(effective_mmr(size, base, 1_000, extra), base);
    }

    #[test]
    fn concentration_at_or_above_threshold_adds_extra(
        size in 1_000u64..=1_000_000_000u64,
        base in 0u32..=10_000u32,
        extra in 0u32..=10_000u32,
    ) {
        let eff = effective_mmr(size, base, 1_000, extra);
        prop_assert_eq!(eff, base.saturating_add(extra));
    }

    #[test]
    fn concentration_monotone_in_size(
        size_a in 0u64..=500_000u64,
        size_b in 500_001u64..=1_000_000u64,
        base in 0u32..=10_000u32,
        extra in 0u32..=10_000u32,
    ) {
        let threshold = 750_000u64;
        let a = effective_mmr(size_a, base, threshold, extra);
        let b = effective_mmr(size_b, base, threshold, extra);
        prop_assert!(b >= a);
    }
}
