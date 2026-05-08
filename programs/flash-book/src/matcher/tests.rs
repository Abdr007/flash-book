//! Pure-Rust unit tests for the matcher core. Mirror the TypeScript test
//! suite in `tests/matcher.test.ts` etc.

use super::commit_reveal::{redeem_reveal, register_commit, sweep_expired, RevealPayload};
use super::fba::{clear_batch, Fill};
use super::flp_quoter::{generate_quotes, FlpQuoterInputs, FlpQuoterParams};
use super::funding::advance;
use super::insurance::InsuranceFund;
use super::liquidation::{
    compute_shortfall, detect_liquidations, generate_liquidation_orders,
};
use super::lot::{BaseLots, Ticks};
use super::order::{Order, OrderType, Side};
use super::risk::{
    assess_margin, default_scenarios, MarketSnapshot, PositionSnapshot,
};
use super::vpin::VpinState;
use crate::state::CommitRow;
use anchor_lang::prelude::Pubkey;

fn ord(
    id: u64,
    side: Side,
    size: u64,
    price: u64,
    ot: OrderType,
    trader_seed: u8,
    seq: u64,
) -> Order {
    Order {
        id,
        trader: Pubkey::new_from_array([trader_seed; 32]),
        side,
        order_type: ot,
        size: BaseLots(size),
        limit_price: Ticks(price),
        seq,
        post_only: false,
    }
}

#[test]
fn fba_empty_returns_no_fills() {
    let r = clear_batch(&[], Ticks(100)).unwrap();
    assert_eq!(r.clearing_volume, BaseLots::ZERO);
    assert!(r.fills.is_empty());
}

#[test]
fn fba_non_crossing_no_fills() {
    let orders = vec![
        ord(1, Side::Long, 1, 99, OrderType::Limit, 1, 0),
        ord(2, Side::Short, 1, 101, OrderType::Limit, 2, 1),
    ];
    let r = clear_batch(&orders, Ticks(100)).unwrap();
    assert_eq!(r.clearing_volume, BaseLots::ZERO);
}

#[test]
fn fba_crossing_uniform_price() {
    let orders = vec![
        ord(1, Side::Long, 1, 101, OrderType::Limit, 1, 0),
        ord(2, Side::Short, 1, 99, OrderType::Limit, 2, 1),
    ];
    let r = clear_batch(&orders, Ticks(100)).unwrap();
    assert_eq!(r.clearing_volume, BaseLots(1));
    assert_eq!(r.fills.len(), 1);
    assert!(r.fills[0].price.0 >= 99 && r.fills[0].price.0 <= 101);
}

#[test]
fn fba_liquidation_priority() {
    let orders = vec![
        ord(1, Side::Long, 1, 105, OrderType::Taker, 1, 0),
        ord(2, Side::Long, 1, 105, OrderType::Liquidation, 2, 1),
        ord(3, Side::Short, 1, 95, OrderType::Limit, 3, 2),
    ];
    let r = clear_batch(&orders, Ticks(100)).unwrap();
    assert_eq!(r.clearing_volume, BaseLots(1));
    // Liquidation should fill first.
    let liq_trader = Pubkey::new_from_array([2; 32]);
    assert_eq!(r.fills[0].taker_trader, liq_trader);
}

#[test]
fn fba_fifo_within_priority() {
    let orders = vec![
        ord(1, Side::Long, 1, 105, OrderType::Taker, 1, 100),
        ord(2, Side::Long, 1, 105, OrderType::Taker, 2, 50),
        ord(3, Side::Short, 1, 95, OrderType::Limit, 3, 1),
    ];
    let r = clear_batch(&orders, Ticks(100)).unwrap();
    let earlier = Pubkey::new_from_array([2; 32]);
    assert_eq!(r.fills[0].taker_trader, earlier);
}

#[test]
fn fba_self_trade_prevention() {
    // Same trader on both sides — no fill.
    let orders = vec![
        ord(1, Side::Long, 1, 105, OrderType::Limit, 7, 0),
        ord(2, Side::Short, 1, 95, OrderType::Limit, 7, 1),
    ];
    let r = clear_batch(&orders, Ticks(100)).unwrap();
    assert_eq!(r.fills.len(), 0);
}

#[test]
fn fba_mev_neutral_within_batch() {
    // Same orders in different submission order → identical clearing.
    let a = vec![
        ord(1, Side::Long, 2, 102, OrderType::Limit, 1, 10),
        ord(2, Side::Short, 2, 98, OrderType::Limit, 2, 20),
        ord(3, Side::Long, 1, 101, OrderType::Limit, 3, 30),
    ];
    let b = vec![a[2], a[0], a[1]];
    let ra = clear_batch(&a, Ticks(100)).unwrap();
    let rb = clear_batch(&b, Ticks(100)).unwrap();
    assert_eq!(ra.clearing_price, rb.clearing_price);
    assert_eq!(ra.clearing_volume, rb.clearing_volume);
}

#[test]
fn flp_quoter_emits_balanced_ladder_when_flat() {
    let params = FlpQuoterParams {
        base_spread_bps: 5,
        alpha_bps: 5_000,
        beta_bps: 3_000,
        gamma_bps: 2_000,
        kappa_bps: 500,
        inventory_lambda_bps: 5_000,
        depth_floor_lots: 1_000,
        max_growth_per_batch_bps: 50, // 0.5%
        levels: 5,
        tick_size: 1,
    };
    let inputs = FlpQuoterInputs {
        oracle_ticks: Ticks(100_000), // arbitrary tick units
        vpin_bps: 0,
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
fn flp_quoter_inventory_skew_short_pool_lifts_fair_value() {
    let params = FlpQuoterParams {
        base_spread_bps: 5,
        alpha_bps: 5_000,
        beta_bps: 3_000,
        gamma_bps: 2_000,
        kappa_bps: 500,
        inventory_lambda_bps: 5_000,
        depth_floor_lots: 1_000,
        max_growth_per_batch_bps: 50,
        levels: 5,
        tick_size: 1,
    };
    let inputs = FlpQuoterInputs {
        oracle_ticks: Ticks(100_000),
        vpin_bps: 0,
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

// K is in "bps per 1 bp of premium per second"; realistic prod values are
// sub-bps so production stores K in micro-bps. For unit tests we use K that
// produces a visible rate after integer division.

#[test]
fn funding_premium_drives_rate_sign() {
    // mark > oracle → positive rate (longs pay)
    let (_, t) = advance(0, Ticks(101), Ticks(100), 1000, 100_000, 10_000).unwrap();
    assert!(t.rate_bps_per_sec > 0);
    assert!(t.index_delta > 0);

    // mark < oracle → negative
    let (_, t) = advance(0, Ticks(99), Ticks(100), 1000, 100_000, 10_000).unwrap();
    assert!(t.rate_bps_per_sec < 0);
    assert!(t.index_delta < 0);
}

#[test]
fn funding_zero_delta_no_change() {
    let (idx, t) = advance(123, Ticks(101), Ticks(100), 0, 100_000, 10_000).unwrap();
    assert_eq!(idx, 123);
    assert_eq!(t.index_delta, 0);
}

#[test]
fn funding_rate_clamped() {
    let (_, t) = advance(0, Ticks(1_000_000), Ticks(100), 1000, 1_000_000_000, 100).unwrap();
    assert!(t.rate_bps_per_sec.abs() <= 100);
}

#[test]
fn vpin_balanced_flow_low() {
    let mut v = VpinState::new();
    for _ in 0..50 {
        v.record_fill(Side::Long, 10, 100, 5).unwrap();
        v.record_fill(Side::Short, 10, 100, 5).unwrap();
    }
    assert!(v.as_bps() < 2_000); // < 20%
}

#[test]
fn vpin_one_sided_flow_high() {
    let mut v = VpinState::new();
    for _ in 0..50 {
        v.record_fill(Side::Long, 100, 100, 5).unwrap();
    }
    assert!(v.as_bps() > 8_000); // > 80%
}

#[test]
fn vpin_zero_before_first_bucket() {
    let mut v = VpinState::new();
    v.record_fill(Side::Long, 50, 100, 5).unwrap();
    assert_eq!(v.as_bps(), 0);
}

// ─── risk + liquidation ─────────────────────────────────────────────

fn sol_market() -> MarketSnapshot {
    MarketSnapshot {
        market: Pubkey::new_from_array([1; 32]),
        mark_price: Ticks(100),
        cum_funding_index: 0,
        maintenance_margin_bps: 125, // 1.25%
        tick_size: 1,
    }
}

fn long_position(market: Pubkey, size: u64, entry: u64) -> PositionSnapshot {
    PositionSnapshot {
        market,
        side: Side::Long,
        size_lots: size,
        entry_price: Ticks(entry),
        cum_funding_index_at_entry: 0,
    }
}

fn short_position(market: Pubkey, size: u64, entry: u64) -> PositionSnapshot {
    PositionSnapshot {
        market,
        side: Side::Short,
        size_lots: size,
        entry_price: Ticks(entry),
        cum_funding_index_at_entry: 0,
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
    let hedged = vec![long_position(m.market, 100, 100), short_position(m.market, 100, 100)];

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

// ─── commit-reveal ──────────────────────────────────────────────────

fn empty_commits() -> Vec<CommitRow> {
    vec![CommitRow::default(); 8]
}

fn payload_for(trader: Pubkey, side: Side, size: u64, limit: u64) -> RevealPayload {
    RevealPayload {
        trader,
        side,
        size: BaseLots(size),
        limit: Ticks(limit),
        nonce: [7u8; 32],
    }
}

#[test]
fn commit_reveal_roundtrip() {
    let mut commits = empty_commits();
    let trader = Pubkey::new_from_array([42; 32]);
    let p = payload_for(trader, Side::Long, 1, 100);
    let h = p.hash();
    register_commit(&mut commits, h, trader, 1000, 1, 5).unwrap();

    let order = redeem_reveal(&mut commits, &p, 2, 999).unwrap();
    assert_eq!(order.trader, trader);
    assert_eq!(order.side, Side::Long);
    assert_eq!(order.size, BaseLots(1));
    assert_eq!(order.order_type, OrderType::Taker);
}

#[test]
fn commit_reveal_mismatch_rejected() {
    let mut commits = empty_commits();
    let trader = Pubkey::new_from_array([42; 32]);
    let p = payload_for(trader, Side::Long, 1, 100);
    register_commit(&mut commits, p.hash(), trader, 1000, 1, 5).unwrap();

    // Tamper.
    let p_tampered = payload_for(trader, Side::Long, 2, 100);
    let r = redeem_reveal(&mut commits, &p_tampered, 2, 999);
    assert!(r.is_err());
}

#[test]
fn commit_reveal_expired_rejected_and_swept() {
    let mut commits = empty_commits();
    let trader = Pubkey::new_from_array([42; 32]);
    let p = payload_for(trader, Side::Long, 1, 100);
    register_commit(&mut commits, p.hash(), trader, 1000, 1, 2).unwrap();

    let r = redeem_reveal(&mut commits, &p, 10, 999);
    assert!(r.is_err());

    let seized = sweep_expired(&mut commits, 10);
    assert_eq!(seized, 1000);
}

#[test]
fn commit_duplicate_rejected() {
    let mut commits = empty_commits();
    let trader = Pubkey::new_from_array([42; 32]);
    let p = payload_for(trader, Side::Long, 1, 100);
    let h = p.hash();
    register_commit(&mut commits, h, trader, 1000, 1, 5).unwrap();
    let r = register_commit(&mut commits, h, trader, 1000, 1, 5);
    assert!(r.is_err());
}

const _: Fill = Fill {
    taker_id: 0,
    maker_id: 0,
    taker_trader: Pubkey::new_from_array([0; 32]),
    maker_trader: Pubkey::new_from_array([0; 32]),
    taker_side: Side::Long,
    size: BaseLots(0),
    price: Ticks(0),
};
