//! Property tests for ARG (Wave 29).

use flash_book::matcher::arg::{
    apply_aggressor_leg, AggressorLeg, ArgBatchState,
};
use proptest::prelude::*;

prop_compose! {
    fn arb_leg()(side in 0u8..2, size in 1u64..1_000_000, price in 1u64..10_000_000)
        -> AggressorLeg
    {
        AggressorLeg { side, size_lots: size, price_ticks: price }
    }
}

proptest! {
    /// Tax is never negative.
    #[test]
    fn tax_non_negative(
        legs in prop::collection::vec(arb_leg(), 1..10),
        tax_bps in 0u32..15_000,
    ) {
        let mut state = ArgBatchState::default();
        for leg in legs {
            let out = apply_aggressor_leg(state, leg, 1, tax_bps);
            prop_assert!(out.incremental_tax_quote_lots < u64::MAX); // sanity
            state = out.new_state;
        }
    }

    /// Cumulative tax in state is monotone non-decreasing within a batch.
    #[test]
    fn tax_monotone_within_batch(
        legs in prop::collection::vec(arb_leg(), 1..15),
        tax_bps in 0u32..10_000,
    ) {
        let mut state = ArgBatchState::default();
        let mut prev_tax = 0u64;
        for leg in legs {
            let out = apply_aggressor_leg(state, leg, 1, tax_bps);
            prop_assert!(out.new_state.tax_paid_this_batch_quote_lots >= prev_tax);
            prev_tax = out.new_state.tax_paid_this_batch_quote_lots;
            state = out.new_state;
        }
    }

    /// Negative round-trip pays no tax.
    #[test]
    fn losing_roundtrip_no_tax(
        side_a_size in 1u64..10_000,
        side_b_size in 1u64..10_000,
        price_a in 1_000u64..2_000,
        price_b in 1_000u64..2_000,
        tax_bps in 1u32..10_000,
    ) {
        // Side 0 buys at price_a; side 1 sells at price_b. PnL = (B - A) × overlap.
        // We want a losing trade: arrange so A > B.
        let buy_price = price_a.max(price_b);
        let sell_price = price_a.min(price_b);
        // Only meaningful when buy != sell.
        if buy_price == sell_price { return Ok(()); }

        let state = ArgBatchState::default();
        let out1 = apply_aggressor_leg(state, AggressorLeg { side: 0, size_lots: side_a_size, price_ticks: buy_price }, 1, tax_bps);
        let out2 = apply_aggressor_leg(out1.new_state, AggressorLeg { side: 1, size_lots: side_b_size, price_ticks: sell_price }, 1, tax_bps);

        prop_assert!(out2.overlap_pnl_quote_lots <= 0, "buy_high sell_low is a loss");
        prop_assert_eq!(out2.incremental_tax_quote_lots, 0, "no tax on loss");
    }

    /// Batch advance resets all state.
    #[test]
    fn batch_advance_resets(
        legs in prop::collection::vec(arb_leg(), 1..10),
        tax_bps in 1u32..10_000,
    ) {
        let mut state = ArgBatchState::default();
        for leg in legs {
            let out = apply_aggressor_leg(state, leg, 1, tax_bps);
            state = out.new_state;
        }
        // Advance to batch 2.
        let after = apply_aggressor_leg(state, AggressorLeg { side: 0, size_lots: 1, price_ticks: 1 }, 2, tax_bps);
        // Long lots in new state should equal just the new leg's size.
        prop_assert_eq!(after.new_state.agg_long_lots, 1);
        prop_assert_eq!(after.new_state.tax_paid_this_batch_quote_lots, 0);
        prop_assert_eq!(after.new_state.batch_seq, 2);
    }
}
