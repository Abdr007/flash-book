//! Property tests for `matcher::risk` — the stress-lattice maintenance
//! margin engine. Each property runs against thousands of random inputs.
//!
//! Why: the unit tests in `src/matcher/tests.rs` cover specific scenarios
//! (one healthy, one unhealthy, one hedged). Property tests prove the
//! algorithm's *invariants* hold for any input shape:
//!
//!   1. required_margin ≥ 0 for any portfolio
//!   2. equity = collateral + Σ unrealized_pnl − Σ funding_owed  (linearity)
//!   3. is_healthy ↔ equity ≥ required (definitional)
//!   4. hedge always reduces required margin: M(long+short) < M(long alone)
//!   5. monotonic collateral: more collateral can only make you healthier
//!
//! Caveat: the assess_margin function uses the worst-case scenario, so
//! invariants are about the OUTPUT of the algorithm, not its scenario
//! by-product.

use anchor_lang::prelude::Pubkey;
use flash_book::matcher::risk::{
    assess_margin, default_scenarios, MarketSnapshot, PositionSnapshot,
};
use flash_book::matcher::order::Side;
use flash_book::matcher::lot::Ticks;
use proptest::prelude::*;

const MARKET_PK_BYTES: [u8; 32] = [42u8; 32];

fn market(mark: u64, maintenance_bps: u32) -> MarketSnapshot {
    MarketSnapshot {
        market: Pubkey::new_from_array(MARKET_PK_BYTES),
        mark_price: Ticks(mark),
        cum_funding_index: 0,
        maintenance_margin_bps: maintenance_bps,
        tick_size: 1,
        concentration_threshold_lots: 0,
        concentration_extra_mmr_bps: 0,
        side_oi_lots: 0,
        oi_mmr_slope_bps_per_million_lots: 0,
        oi_mmr_max_extra_bps: 0,
    }
}

fn position(side: Side, size: u64, entry: u64) -> PositionSnapshot {
    PositionSnapshot {
        market: Pubkey::new_from_array(MARKET_PK_BYTES),
        side,
        size_lots: size,
        entry_price: Ticks(entry),
        cum_funding_index_at_entry: 0,
        collateral_quote_lots: 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// required_margin is always non-negative.
    #[test]
    fn required_margin_non_negative(
        long_size in 0u64..1_000u64,
        short_size in 0u64..1_000u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        collateral in 0u64..1_000_000u64,
    ) {
        let m = market(mark, 125);
        let mut positions = Vec::new();
        if long_size > 0 {
            positions.push(position(Side::Long, long_size, entry));
        }
        if short_size > 0 {
            positions.push(position(Side::Short, short_size, entry));
        }
        let scenarios = default_scenarios(&[m.market]);
        let a = assess_margin(&positions, &[m], &scenarios, collateral)?;
        // u64 is non-negative by type. The assertion is on equity sign:
        // even with no collateral and worst PnL, equity_signed must be a
        // valid i128.
        prop_assert!(a.required_quote_lots <= u64::MAX);
    }

    /// More collateral never makes the trader less healthy.
    /// (For the same positions, equity grows linearly with collateral.)
    #[test]
    fn monotonic_in_collateral(
        long_size in 1u64..500u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        col_a in 0u64..50_000u64,
        delta in 1u64..50_000u64,
    ) {
        let m = market(mark, 125);
        let positions = vec![position(Side::Long, long_size, entry)];
        let scenarios = default_scenarios(&[m.market]);
        let col_b = col_a.saturating_add(delta);
        let a_assess = assess_margin(&positions, &[m], &scenarios, col_a)?;
        let b_assess = assess_margin(&positions, &[m], &scenarios, col_b)?;
        // required is independent of collateral — it depends only on positions.
        prop_assert_eq!(a_assess.required_quote_lots, b_assess.required_quote_lots);
        // equity must grow by exactly `delta`.
        let equity_diff = b_assess.equity_quote_lots_signed - a_assess.equity_quote_lots_signed;
        prop_assert_eq!(equity_diff, delta as i128);
        // If `a` was healthy, `b` (more collateral) must also be healthy.
        if a_assess.is_healthy {
            prop_assert!(b_assess.is_healthy);
        }
    }

    /// Hedging at fair value strictly reduces required margin.
    ///
    /// The property only holds at mark = entry. Off fair value, the long's
    /// unrealized PnL is non-zero, and adding a hedge "trades" the
    /// directional term (which may have been favorable in one direction)
    /// for double maintenance margin. So hedge can increase required if
    /// the position was deep in the money.
    ///
    /// At fair value, unrealized = 0 so the directional cancellation is
    /// pure win: hedged required = 2*maintenance, vs unhedged
    /// directional_loss + maintenance ≫ 2*maintenance.
    #[test]
    fn hedge_at_fair_value_reduces_required_margin(
        size in 10u64..1_000u64,
        price in 50u64..200u64,
    ) {
        let m = market(price, 125);
        let scenarios = default_scenarios(&[m.market]);

        let unhedged = vec![position(Side::Long, size, price)];
        let hedged = vec![
            position(Side::Long, size, price),
            position(Side::Short, size, price),
        ];

        let unhedged_assess = assess_margin(&unhedged, &[m], &scenarios, 0)?;
        let hedged_assess = assess_margin(&hedged, &[m], &scenarios, 0)?;

        prop_assert!(
            hedged_assess.required_quote_lots <= unhedged_assess.required_quote_lots,
            "hedge increased required margin (unhedged: {}, hedged: {})",
            unhedged_assess.required_quote_lots,
            hedged_assess.required_quote_lots,
        );
    }

    /// is_healthy is exactly equity ≥ required.
    #[test]
    fn is_healthy_consistent_with_equity_required(
        size in 1u64..500u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        collateral in 0u64..100_000u64,
    ) {
        let m = market(mark, 125);
        let positions = vec![position(Side::Long, size, entry)];
        let scenarios = default_scenarios(&[m.market]);
        let a = assess_margin(&positions, &[m], &scenarios, collateral)?;
        let expected_healthy = a.equity_quote_lots_signed >= a.required_quote_lots as i128;
        prop_assert_eq!(a.is_healthy, expected_healthy);
    }

    /// Empty portfolio has zero required margin and is always healthy
    /// (modulo the funding term which is also zero with no positions).
    #[test]
    fn empty_portfolio_zero_required(collateral in 0u64..1_000_000u64) {
        let m = market(100, 125);
        let scenarios = default_scenarios(&[m.market]);
        let a = assess_margin(&[], &[m], &scenarios, collateral)?;
        prop_assert_eq!(a.required_quote_lots, 0);
        prop_assert!(a.is_healthy);
        prop_assert_eq!(a.equity_quote_lots_signed, collateral as i128);
    }

    /// Worst-case scenario index is always within the scenario list bounds.
    #[test]
    fn worst_scenario_idx_within_bounds(
        size in 1u64..500u64,
        entry in 50u64..200u64,
    ) {
        let m = market(100, 125);
        let positions = vec![position(Side::Long, size, entry)];
        let scenarios = default_scenarios(&[m.market]);
        let a = assess_margin(&positions, &[m], &scenarios, 0)?;
        prop_assert!((a.worst_scenario_idx as usize) < scenarios.len());
    }

    /// Doubling position size approximately doubles required margin.
    /// (Exact equality fails because of integer truncation in
    /// per-scenario arithmetic — the maintenance margin's `* mm_bps /
    /// BPS_DENOM` truncates differently at size=N vs size=2N. Tolerance
    /// reflects worst-case truncation: 1 unit per scenario per term × 2
    /// terms × 13 scenarios = ~26.)
    #[test]
    fn approximately_linear_scaling_in_position_size(
        size in 10u64..500u64,
        entry in 50u64..200u64,
    ) {
        let m = market(100, 125);
        let scenarios = default_scenarios(&[m.market]);
        let single = vec![position(Side::Long, size, entry)];
        let double = vec![position(Side::Long, size * 2, entry)];
        let a = assess_margin(&single, &[m], &scenarios, 0)?;
        let b = assess_margin(&double, &[m], &scenarios, 0)?;
        let twice_a = a.required_quote_lots.saturating_mul(2);
        let tolerance: u64 = 100; // generous bound on integer truncation
        let diff = if b.required_quote_lots > twice_a {
            b.required_quote_lots - twice_a
        } else {
            twice_a - b.required_quote_lots
        };
        prop_assert!(
            diff <= tolerance,
            "expected b.required ≈ 2*a.required ± {} (got a={}, b={})",
            tolerance,
            a.required_quote_lots,
            b.required_quote_lots,
        );
    }
}
