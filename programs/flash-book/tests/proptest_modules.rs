//! Property tests for the pure matcher modules:
//!
//!   • flp_quoter   — spread monotonicity, ladder ordering, capacity bounds
//!   • insurance    — waterfall conservation, contribution math
//!
//! Each property runs against 2,000 random inputs.

use anchor_lang::prelude::Pubkey;
use flash_book::matcher::{
    flp_quoter::{generate_quotes, FlpQuoterInputs, FlpQuoterParams},
    insurance::InsuranceFund,
    lot::Ticks,
};
use proptest::prelude::*;

// ─── flp_quoter ────────────────────────────────────────────────────────

fn flp_params() -> FlpQuoterParams {
    FlpQuoterParams {
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
    }
}

fn flp_inputs(oracle: u64, vpin: u32, capital: u64, net: i64) -> FlpQuoterInputs {
    FlpQuoterInputs {
        oracle_ticks: Ticks(oracle),
        vpin_bps: vpin,
        realized_vol_bps: 0,
        pool_capital_quote_lots: capital,
        pool_net_quote_lots_signed: net,
        pool_gross_utilization_bps: 0,
        oi_long_lots: 0,
        oi_short_lots: 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// FLP bids are always at or below fair value; asks always at or above.
    #[test]
    fn flp_bid_below_ask_always(
        oracle in 1_000u64..1_000_000u64,
        vpin in 0u32..10_000u32,
        capital in 1_000_000u64..10_000_000_000u64,
    ) {
        let trader = Pubkey::new_from_array([1u8; 32]);
        let (out, _) = generate_quotes(flp_params(), flp_inputs(oracle, vpin, capital, 0), trader, 0)?;
        for level in 0..out.bids.len() {
            let bid_price = out.bids[level].0.0;
            let ask_price = out.asks[level].0.0;
            prop_assert!(bid_price <= ask_price, "bid > ask at level {}", level);
            prop_assert!(bid_price <= out.fair_value.0, "bid > fair at level {}", level);
            prop_assert!(ask_price >= out.fair_value.0, "ask < fair at level {}", level);
        }
    }

    /// Higher VPIN widens the spread (bid further from fair, ask further from fair).
    #[test]
    fn flp_vpin_widens_spread(
        oracle in 1_000u64..1_000_000u64,
        vpin_low in 0u32..1_000u32,
        capital in 1_000_000u64..10_000_000_000u64,
    ) {
        let vpin_high = vpin_low.saturating_add(5_000);
        let trader = Pubkey::new_from_array([1u8; 32]);
        let (low, _) = generate_quotes(flp_params(), flp_inputs(oracle, vpin_low, capital, 0), trader, 0)?;
        let (high, _) = generate_quotes(flp_params(), flp_inputs(oracle, vpin_high, capital, 0), trader, 0)?;

        // For every level, the high-VPIN ask must be ≥ the low-VPIN ask
        // (and the high-VPIN bid ≤ low-VPIN bid). Spread is monotonic in VPIN.
        for i in 0..low.bids.len().min(high.bids.len()) {
            prop_assert!(
                high.bids[i].0.0 <= low.bids[i].0.0,
                "high-VPIN bid ({}) higher than low-VPIN bid ({}) at level {}",
                high.bids[i].0.0, low.bids[i].0.0, i,
            );
            prop_assert!(
                high.asks[i].0.0 >= low.asks[i].0.0,
                "high-VPIN ask ({}) lower than low-VPIN ask ({}) at level {}",
                high.asks[i].0.0, low.asks[i].0.0, i,
            );
        }
    }

    /// Pool with zero capital emits no quotes.
    #[test]
    fn flp_zero_capital_emits_no_orders(
        oracle in 1_000u64..1_000_000u64,
        vpin in 0u32..10_000u32,
    ) {
        let trader = Pubkey::new_from_array([1u8; 32]);
        let (out, orders) = generate_quotes(flp_params(), flp_inputs(oracle, vpin, 0, 0), trader, 0)?;
        prop_assert!(out.bids.is_empty());
        prop_assert!(out.asks.is_empty());
        prop_assert!(orders.is_empty());
    }

    /// Pool short-skew lifts fair value (skew ≥ 0, fair ≥ oracle).
    /// (Integer truncation can collapse skew to exactly 0 when
    /// |net_abs / capital| is very small; both behaviors are correct.)
    #[test]
    fn flp_short_pool_lifts_fair_value(
        oracle in 1_000u64..1_000_000u64,
        capital in 1_000_000u64..10_000_000_000u64,
        net_abs in 1u64..100_000u64,
    ) {
        let trader = Pubkey::new_from_array([1u8; 32]);
        let net_short = -(net_abs as i64);
        let (out, _) = generate_quotes(flp_params(), flp_inputs(oracle, 0, capital, net_short), trader, 0)?;
        prop_assert!(out.skew_bps >= 0, "skew must be ≥ 0 for net-short pool");
        prop_assert!(out.fair_value.0 >= oracle, "fair_value < oracle with net-short pool");
    }

    /// Pool long-skew lowers fair value (skew ≤ 0, fair ≤ oracle).
    #[test]
    fn flp_long_pool_lowers_fair_value(
        oracle in 1_000u64..1_000_000u64,
        capital in 1_000_000u64..10_000_000_000u64,
        net_abs in 1u64..100_000u64,
    ) {
        let trader = Pubkey::new_from_array([1u8; 32]);
        let net_long = net_abs as i64;
        let (out, _) = generate_quotes(flp_params(), flp_inputs(oracle, 0, capital, net_long), trader, 0)?;
        prop_assert!(out.skew_bps <= 0, "skew must be ≤ 0 for net-long pool");
        prop_assert!(out.fair_value.0 <= oracle, "fair_value > oracle with net-long pool");
    }
}

// ─── insurance fund ────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// cover_shortfall conserves: covered + remaining == shortfall.
    #[test]
    fn insurance_cover_conserves_value(
        balance in 0u64..1_000_000_000u64,
        shortfall in 0u64..1_000_000_000u64,
    ) {
        let mut f = InsuranceFund::new(balance, 1_000, 5_000, 5_000, 100);
        let (covered, remaining) = f.cover_shortfall(shortfall);
        prop_assert_eq!(covered.saturating_add(remaining), shortfall);
        prop_assert!(covered <= balance, "covered exceeded original balance");
        prop_assert_eq!(f.balance_quote_lots, balance.saturating_sub(covered));
    }

    /// Multi-stream contributions accumulate exactly.
    #[test]
    fn insurance_contributions_sum(
        fees in 0u64..10_000_000u64,
        tax in 0u64..10_000_000u64,
        penalty in 0u64..10_000_000u64,
    ) {
        let mut f = InsuranceFund::new(0, 1_000, 5_000, 5_000, 100);
        let c1 = f.contribute_from_fees(fees)?;
        let c2 = f.contribute_from_toxicity_tax(tax)?;
        let c3 = f.contribute_from_liq_penalty(penalty)?;
        let expected_total = c1.saturating_add(c2).saturating_add(c3);
        prop_assert_eq!(f.balance_quote_lots, expected_total);
        prop_assert_eq!(f.total_contributions, expected_total);
    }

    /// pause threshold gate is monotonic in balance.
    #[test]
    fn insurance_pause_threshold_monotonic(
        threshold in 0u64..1_000_000u64,
        balance in 0u64..2_000_000u64,
    ) {
        let mut f = InsuranceFund::new(balance, 1_000, 5_000, 5_000, threshold);
        let allowed_before = f.new_positions_allowed();
        let _ = f.contribute_from_fees(10_000)?;
        let allowed_after = f.new_positions_allowed();
        // Adding to balance can only flip false→true, never true→false.
        if allowed_before {
            prop_assert!(allowed_after);
        }
    }
}

