//! Unit tests for the pure matcher core.

use super::lp_quoter::{generate_quotes, LpQuoterInputs, LpQuoterParams};
use super::insurance::InsuranceFund;
use super::liquidation::{compute_shortfall, detect_liquidations, generate_liquidation_orders};
use super::lot::{BaseLots, Ticks};
use super::order::{Order, OrderType, Side};
use super::risk::{assess_margin, default_scenarios, MarketSnapshot, PositionSnapshot};
use anchor_lang::prelude::Pubkey;

#[test]
fn lp_quoter_emits_balanced_ladder_when_flat() {
    let params = LpQuoterParams {
        base_spread_bps: 5,
        alpha_bps: 5_000,
        beta_bps: 3_000,
        gamma_bps: 2_000,
        kappa_bps: 500,
        delta_bps: 20_000,
        inventory_lambda_bps: 5_000,
        depth_floor_lots: 1_000,
        max_growth_per_batch_bps: 50, // 0.5%
        levels: 5,
        tick_size: 1,
    };
    let inputs = LpQuoterInputs {
        oracle_ticks: Ticks(100_000), // arbitrary tick units
        vpin_bps: 0,
        realized_vol_bps: 0,
        pool_capital_quote_lots: 1_000_000_000,
        pool_net_quote_lots_signed: 0,
        pool_gross_utilization_bps: 0,
        oi_long_lots: 0,
        oi_short_lots: 0,
    };
    let trader = Pubkey::new_from_array([42; 32]);
    let (out, orders) = generate_quotes(params, inputs, trader, 0).unwrap();
    assert_eq!(out.skew_bps, 0);
    assert_eq!(out.fair_value, Ticks(100_000));
    assert_eq!(out.bids.len(), 5);
    assert_eq!(out.asks.len(), 5);
    assert!(out.bids[0].0 .0 < 100_000);
    assert!(out.asks[0].0 .0 > 100_000);
    assert_eq!(orders.len(), 10);
}

#[test]
fn lp_quoter_inventory_skew_short_pool_lifts_fair_value() {
    let params = LpQuoterParams {
        base_spread_bps: 5,
        alpha_bps: 5_000,
        beta_bps: 3_000,
        gamma_bps: 2_000,
        kappa_bps: 500,
        delta_bps: 20_000,
        inventory_lambda_bps: 5_000,
        depth_floor_lots: 1_000,
        max_growth_per_batch_bps: 50,
        levels: 5,
        tick_size: 1,
    };
    let inputs = LpQuoterInputs {
        oracle_ticks: Ticks(100_000),
        vpin_bps: 0,
        realized_vol_bps: 0,
        pool_capital_quote_lots: 1_000_000_000,
        pool_net_quote_lots_signed: -100_000_000, // pool is net short
        pool_gross_utilization_bps: 1_000,
        oi_long_lots: 0,
        oi_short_lots: 0,
    };
    let trader = Pubkey::new_from_array([42; 32]);
    let (out, _) = generate_quotes(params, inputs, trader, 0).unwrap();
    assert!(out.skew_bps > 0);
    assert!(out.fair_value.0 > 100_000);
}

// ─── risk + liquidation ─────────────────────────────────────────────

fn sol_market() -> MarketSnapshot {
    MarketSnapshot {
        market: Pubkey::new_from_array([1; 32]),
        mark_price: Ticks(100),
        cum_funding_index: 0,
        maintenance_margin_bps: 125, // 1.25%
        tick_size: 1,
        concentration_threshold_lots: 0,
        concentration_extra_mmr_bps: 0,
        side_oi_lots: 0,
        oi_mmr_slope_bps_per_million_lots: 0,
        oi_mmr_max_extra_bps: 0,
        paper_profit_haircut_bps: 0,
    }
}

fn long_position(market: Pubkey, size: u64, entry: u64) -> PositionSnapshot {
    PositionSnapshot {
        market,
        side: Side::Long,
        size_lots: size,
        entry_price: Ticks(entry),
        cum_funding_index_at_entry: 0,
        collateral_quote_lots: 0,
    }
}

fn short_position(market: Pubkey, size: u64, entry: u64) -> PositionSnapshot {
    PositionSnapshot {
        market,
        side: Side::Short,
        size_lots: size,
        entry_price: Ticks(entry),
        cum_funding_index_at_entry: 0,
        collateral_quote_lots: 0,
    }
}

#[test]
fn risk_healthy_long_with_collateral() {
    let m = sol_market();
    let positions = vec![long_position(m.market, 1, 100)];
    let scenarios = default_scenarios(&[m.market]);
    let a = assess_margin(&positions, &[m], &scenarios, 50).unwrap();
    assert!(a.is_healthy);
}

#[test]
fn risk_unhealthy_high_leverage() {
    let m = sol_market();
    let positions = vec![long_position(m.market, 1000, 100)];
    let scenarios = default_scenarios(&[m.market]);
    let a = assess_margin(&positions, &[m], &scenarios, 100).unwrap();
    assert!(!a.is_healthy);
    assert!(a.required_quote_lots > 0);
    assert_ne!(a.worst_scenario_idx, 0); // not the flat scenario
}

#[test]
fn risk_hedged_position_collapses_required_margin() {
    let m = sol_market();
    let scenarios = default_scenarios(&[m.market]);
    let unhedged = vec![long_position(m.market, 100, 100)];
    let hedged = vec![
        long_position(m.market, 100, 100),
        short_position(m.market, 100, 100),
    ];

    let a_unhedged = assess_margin(&unhedged, &[m], &scenarios, 0).unwrap();
    let a_hedged = assess_margin(&hedged, &[m], &scenarios, 0).unwrap();

    // Hedged required margin should be << unhedged.
    assert!(a_hedged.required_quote_lots < a_unhedged.required_quote_lots / 5);
}

#[test]
fn liquidation_detect_unhealthy_traders() {
    let m = sol_market();
    let trader_a = Pubkey::new_from_array([10; 32]);
    let trader_b = Pubkey::new_from_array([11; 32]);
    let traders = vec![
        (trader_a, vec![long_position(m.market, 1000, 100)], 100u64),
        (trader_b, vec![long_position(m.market, 1, 100)], 1000u64), // healthy
    ];
    let scenarios = default_scenarios(&[m.market]);
    let candidates = detect_liquidations(&traders, &[m], &scenarios).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].trader, trader_a);
}

#[test]
fn liquidation_orders_have_correct_side_and_priority() {
    let m = sol_market();
    let trader = Pubkey::new_from_array([20; 32]);
    let candidates = vec![super::liquidation::LiquidationCandidate {
        trader,
        positions: vec![long_position(m.market, 5, 100)],
        equity_signed: -10,
        required: 10,
        worst_scenario_idx: 1,
    }];
    let orders = generate_liquidation_orders(&candidates, &[m], 0, 50).unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].side, Side::Short); // closing a long → sell
    assert_eq!(orders[0].order_type, OrderType::Liquidation);
    assert_eq!(orders[0].size, BaseLots(5));
}

#[test]
fn shortfall_sufficient_collateral_covers() {
    let m = sol_market();
    let pos = long_position(m.market, 1, 100);
    // Liquidated at 99 (price moved against the long); penalty 50bps; lot=1.
    let r = compute_shortfall(&pos, Ticks(99), 100, &m, 50).unwrap();
    // realized PnL = 1 * (99 - 100) * 1 = -1; penalty = 1*99*1*50/10000 = 0 (integer trunc)
    // remaining = 100 - 1 - 0 = 99, recovered.
    assert_eq!(r.shortfall_quote_lots, 0);
    assert!(r.collateral_recovered_quote_lots > 0);
}

#[test]
fn shortfall_bankruptcy_when_collateral_insufficient() {
    let m = sol_market();
    let pos = long_position(m.market, 100, 100);
    // Liquidated at 50 — massive loss.
    let r = compute_shortfall(&pos, Ticks(50), 100, &m, 50).unwrap();
    assert!(r.shortfall_quote_lots > 0);
    assert_eq!(r.collateral_recovered_quote_lots, 0);
}

// ─── insurance fund ─────────────────────────────────────────────────

#[test]
fn insurance_contributions_accumulate() {
    let mut f = InsuranceFund::new(0, 1000, 5000, 5000, 100);
    let c = f.contribute_from_fees(1000).unwrap();
    assert_eq!(c, 100);
    assert_eq!(f.balance_quote_lots, 100);
    let c = f.contribute_from_toxicity_tax(200).unwrap();
    assert_eq!(c, 100);
    let c = f.contribute_from_liq_penalty(200).unwrap();
    assert_eq!(c, 100);
    assert_eq!(f.balance_quote_lots, 300);
    assert_eq!(f.total_contributions, 300);
}

#[test]
fn insurance_cover_full_when_balance_sufficient() {
    let mut f = InsuranceFund::new(500, 1000, 5000, 5000, 100);
    let (c, r) = f.cover_shortfall(200);
    assert_eq!(c, 200);
    assert_eq!(r, 0);
    assert_eq!(f.balance_quote_lots, 300);
}

#[test]
fn insurance_partial_when_underfunded() {
    let mut f = InsuranceFund::new(100, 1000, 5000, 5000, 100);
    let (c, r) = f.cover_shortfall(500);
    assert_eq!(c, 100);
    assert_eq!(r, 400);
    assert_eq!(f.balance_quote_lots, 0);
}

#[test]
fn insurance_pause_threshold_gates_new_positions() {
    let mut f = InsuranceFund::new(50, 1000, 5000, 5000, 100);
    assert!(!f.new_positions_allowed());
    f.contribute_from_fees(1000).unwrap();
    assert!(f.new_positions_allowed());
}

// Order data structure marker — preserved so refactors that touch
// the matcher's Order/Side/OrderType types are surfaced via this module's
// test compile.
const _: Order = Order {
    id: 0,
    trader: Pubkey::new_from_array([0; 32]),
    side: Side::Long,
    order_type: OrderType::Limit,
    size: BaseLots(0),
    limit_price: Ticks(0),
    seq: 0,
    post_only: false,
    stp_mode: crate::matcher::order::StpMode::CancelNewest,
};
