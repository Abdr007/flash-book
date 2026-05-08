//! Pure-Rust unit tests for the matcher core. Mirror the TypeScript test
//! suite in `tests/matcher.test.ts` etc.

use super::fba::{clear_batch, Fill};
use super::flp_quoter::{generate_quotes, FlpQuoterInputs, FlpQuoterParams};
use super::funding::advance;
use super::lot::{BaseLots, Ticks};
use super::order::{Order, OrderType, Side};
use super::vpin::VpinState;
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

const _: Fill = Fill {
    taker_id: 0,
    maker_id: 0,
    taker_trader: Pubkey::new_from_array([0; 32]),
    maker_trader: Pubkey::new_from_array([0; 32]),
    taker_side: Side::Long,
    size: BaseLots(0),
    price: Ticks(0),
};
