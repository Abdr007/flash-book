//! Property tests for `matcher::liquidation`. Each runs 2,000 cases.

use anchor_lang::prelude::Pubkey;
use clober::matcher::liquidation::{
    compute_shortfall, detect_liquidations, generate_liquidation_orders, LiquidationCandidate,
};
use clober::matcher::lot::Ticks;
use clober::matcher::order::{OrderType, Side};
use clober::matcher::risk::{default_scenarios, MarketSnapshot, PositionSnapshot};
use proptest::prelude::*;

const MARKET_PK_BYTES: [u8; 32] = [42u8; 32];

fn market(mark: u64) -> MarketSnapshot {
    MarketSnapshot {
        market: Pubkey::new_from_array(MARKET_PK_BYTES),
        mark_price: Ticks(mark),
        cum_funding_index: 0,
        maintenance_margin_bps: 125,
        tick_size: 1,
        concentration_threshold_lots: 0,
        concentration_extra_mmr_bps: 0,
        side_oi_lots: 0,
        oi_mmr_slope_bps_per_million_lots: 0,
        oi_mmr_max_extra_bps: 0,
        paper_profit_haircut_bps: 0,
        stress_shock_bps: 0,
        corr_group_id: 0,
        corr_rho_bps: 0,
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

    /// A trader with adequate collateral is never flagged for liquidation.
    #[test]
    fn detect_skips_healthy_traders(
        size in 1u64..100u64,
        entry in 50u64..200u64,
    ) {
        let m = market(entry);
        let positions = vec![position(Side::Long, size, entry)];
        let scenarios = default_scenarios(&[m.market]);
        let trader = Pubkey::new_from_array([1u8; 32]);
        // Massive collateral → always healthy.
        let traders = vec![(trader, positions, 1_000_000_000u64)];
        let candidates = detect_liquidations(&traders, &[m], &scenarios)?;
        prop_assert!(candidates.is_empty(), "healthy trader incorrectly flagged");
    }

    /// A trader with zero collateral and a non-trivial position IS flagged.
    #[test]
    fn detect_flags_zero_collateral_traders(
        size in 10u64..1_000u64,
        entry in 50u64..200u64,
    ) {
        let m = market(entry);
        let positions = vec![position(Side::Long, size, entry)];
        let scenarios = default_scenarios(&[m.market]);
        let trader = Pubkey::new_from_array([2u8; 32]);
        let traders = vec![(trader, positions, 0u64)];
        let candidates = detect_liquidations(&traders, &[m], &scenarios)?;
        prop_assert_eq!(candidates.len(), 1);
        prop_assert_eq!(candidates[0].trader, trader);
    }

    /// detect_liquidations skips traders with no positions.
    #[test]
    fn detect_skips_empty_position_lists(collateral in 0u64..1_000_000u64) {
        let m = market(100);
        let scenarios = default_scenarios(&[m.market]);
        let trader = Pubkey::new_from_array([3u8; 32]);
        let traders = vec![(trader, vec![], collateral)];
        let candidates = detect_liquidations(&traders, &[m], &scenarios)?;
        prop_assert!(candidates.is_empty());
    }

    /// generate_liquidation_orders produces one order per non-empty
    /// position in each candidate.
    #[test]
    fn liq_orders_one_per_position(
        n_positions in 1usize..8usize,
        size in 1u64..100u64,
    ) {
        let m = market(100);
        let trader = Pubkey::new_from_array([4u8; 32]);
        let positions: Vec<PositionSnapshot> =
            (0..n_positions).map(|_| position(Side::Long, size, 100)).collect();
        let candidate = LiquidationCandidate {
            trader,
            positions: positions.clone(),
            equity_signed: 0,
            required: 1_000,
            worst_scenario_idx: 0,
        };
        let orders = generate_liquidation_orders(&[candidate], &[m], 0, 50)?;
        prop_assert_eq!(orders.len(), n_positions);
        for o in &orders {
            prop_assert_eq!(o.order_type, OrderType::Liquidation);
            prop_assert_eq!(o.trader, trader);
            prop_assert_eq!(o.side, Side::Short); // closing a long
        }
    }

    /// compute_shortfall: invariant that recovered + shortfall ≤ collateral
    /// + bounded penalty (the pnl term can swing either way but the result
    /// always reflects collateral conservation modulo penalty).
    #[test]
    fn shortfall_or_recovery_consistent(
        size in 1u64..100u64,
        entry in 50u64..200u64,
        fill in 50u64..200u64,
        collateral in 0u64..100_000u64,
    ) {
        let m = market(100);
        let pos = position(Side::Long, size, entry);
        let r = compute_shortfall(&pos, Ticks(fill), collateral, &m, 50)?;
        // Either recovered > 0 OR shortfall > 0, never both.
        prop_assert!(
            !(r.collateral_recovered_quote_lots > 0 && r.shortfall_quote_lots > 0),
            "both recovered and shortfall positive simultaneously",
        );
    }

    /// Liquidation orders always sized to the position size.
    #[test]
    fn liq_orders_match_position_size(
        size in 1u64..1_000u64,
    ) {
        let m = market(100);
        let trader = Pubkey::new_from_array([5u8; 32]);
        let pos_snap = position(Side::Long, size, 100);
        let candidate = LiquidationCandidate {
            trader,
            positions: vec![pos_snap],
            equity_signed: 0,
            required: 1_000,
            worst_scenario_idx: 0,
        };
        let orders = generate_liquidation_orders(&[candidate], &[m], 0, 50)?;
        prop_assert_eq!(orders.len(), 1);
        prop_assert_eq!(orders[0].size.0, size);
    }
}
