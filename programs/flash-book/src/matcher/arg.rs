//! ARG — Aggressor Roundtrip Guard (Wave 29).
//!
//! Protocol-level sandwich tax: within a single batch, a trader who
//! aggresses on overlapping buy + sell legs cannot realize non-negative
//! PnL without paying a tax to the insurance fund.
//!
//! ## The attack this defends
//!
//! Classic sandwich:
//!   1. MEV bot sees a large victim order in the mempool.
//!   2. Bot front-runs with an aggressor BUY (lifts the offer).
//!   3. Victim's order fills at the inflated price.
//!   4. Bot back-runs with an aggressor SELL (hits the bid).
//!   5. Bot captures `victim_size × bot_price_pressure` profit.
//!
//! In a batch-matched orderbook (or even a continuous one with
//! sub-second batch windows), the bot's BUY and SELL appear in the
//! same batch. ARG observes that the bot has aggressed on both sides
//! of the batch and taxes the round-trip realized PnL.
//!
//! ## What ARG is NOT
//!
//! - **Not** a market-maker / arbitrage deterrent. Legitimate MM
//!   strategies post **passively** (maker side), not aggressively
//!   (taker side). ARG only watches the aggressor (taker) leg.
//! - **Not** a tax on losses. Roundtrip with negative PnL pays nothing
//!   — sandwich attempts that fail naturally aren't punished.
//! - **Not** a per-tx check. The state lives per-batch (gets reset on
//!   batch advance). Cross-batch sandwiches are out of scope (they're
//!   detectable via slower mempool defenses).
//!
//! ## State model
//!
//! Per-trader, within the current batch, track:
//! - `agg_long_lots` — cumulative aggressor BUY size this batch
//! - `agg_long_avg_price` — weighted average entry price for that BUY
//! - `agg_short_lots` — cumulative aggressor SELL size
//! - `agg_short_avg_price` — weighted average entry price for SELL
//! - `arg_tax_paid_this_batch` — accumulator, prevents double-tax
//!
//! On each new aggressor fill:
//!  - Update the appropriate side's lot/avg-price accumulators.
//!  - If both sides have non-zero lots, compute the overlap-PnL and
//!    extract a tax of `min(overlap_pnl × tax_bps, overlap_pnl)`.
//!  - Tax already taken this batch is subtracted from the new tax
//!    obligation (only the *incremental* roundtrip pays).
//!
//! On batch advance (sequencer's responsibility): zero all four
//! accumulators on every trader that aggressed last batch.
//!
//! Pure math. No Solana types. Wire-in lives in `lib.rs::apply_fill`
//! once Wave 29b lands.

use crate::constants::BPS_DENOM;

/// Per-trader, per-batch ARG state. One copy per `(trader, market)`
/// per batch. Lives in a sibling PDA or on TraderState in the wire-in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArgBatchState {
    /// Cumulative aggressor BUY size this batch (lots).
    pub agg_long_lots: u64,
    /// Weighted-average entry price for the aggressor BUY (ticks).
    pub agg_long_avg_price_ticks: u64,
    /// Cumulative aggressor SELL size this batch (lots).
    pub agg_short_lots: u64,
    /// Weighted-average entry price for the aggressor SELL (ticks).
    pub agg_short_avg_price_ticks: u64,
    /// Tax paid to insurance this batch so far.
    pub tax_paid_this_batch_quote_lots: u64,
    /// Batch number this state belongs to. The wire-in resets state
    /// when the current batch advances past this.
    pub batch_seq: u64,
}

/// One aggressor (taker) leg of a fill.
#[derive(Debug, Clone, Copy)]
pub struct AggressorLeg {
    /// 0 = aggressor BUY (long), 1 = aggressor SELL (short).
    pub side: u8,
    pub size_lots: u64,
    pub price_ticks: u64,
}

/// Tax computation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgTaxOutcome {
    /// Total round-trip realized PnL across overlapping aggressor legs
    /// (quote lots, conservative — uses `min(long_lots, short_lots)`).
    pub overlap_pnl_quote_lots: i128,
    /// Cumulative tax that should be paid by end of this fill
    /// (quote lots). Already accounts for prior tax taken in the batch.
    pub incremental_tax_quote_lots: u64,
    /// Updated batch state after applying the leg.
    pub new_state: ArgBatchState,
}

/// Reset the batch state on a new batch. The wire-in calls this
/// before applying the first aggressor leg of a fresh batch.
#[inline]
pub fn reset_on_batch_advance(state: ArgBatchState, new_batch_seq: u64) -> ArgBatchState {
    if state.batch_seq == new_batch_seq {
        return state;
    }
    ArgBatchState {
        agg_long_lots: 0,
        agg_long_avg_price_ticks: 0,
        agg_short_lots: 0,
        agg_short_avg_price_ticks: 0,
        tax_paid_this_batch_quote_lots: 0,
        batch_seq: new_batch_seq,
    }
}

/// Apply one aggressor leg and compute the resulting tax obligation.
///
/// `arg_tax_bps` is the rate; typical value `5_000` = 50% of the
/// round-trip overlap PnL paid as tax. The full overlap PnL is the
/// upper bound (the system can't take more than the trader made).
///
/// Pure function. Returns the new state + the incremental tax to debit
/// from the trader's collateral and credit to the insurance fund.
pub fn apply_aggressor_leg(
    mut state: ArgBatchState,
    leg: AggressorLeg,
    current_batch_seq: u64,
    arg_tax_bps: u32,
) -> ArgTaxOutcome {
    state = reset_on_batch_advance(state, current_batch_seq);

    // Update the appropriate side's accumulators with weighted-average
    // price math: avg = (avg_old × size_old + price × size_new) / size_total.
    let (new_lots, new_avg_price) = match leg.side {
        0 => weighted_update(state.agg_long_lots, state.agg_long_avg_price_ticks, leg.size_lots, leg.price_ticks),
        1 => weighted_update(state.agg_short_lots, state.agg_short_avg_price_ticks, leg.size_lots, leg.price_ticks),
        _ => (0, 0),
    };
    if leg.side == 0 {
        state.agg_long_lots = new_lots;
        state.agg_long_avg_price_ticks = new_avg_price;
    } else if leg.side == 1 {
        state.agg_short_lots = new_lots;
        state.agg_short_avg_price_ticks = new_avg_price;
    }

    // Compute round-trip overlap PnL. Conservative: only `min(long, short)`
    // lots overlap. Profit = (short_price - long_price) × overlap_lots.
    let overlap_lots = state.agg_long_lots.min(state.agg_short_lots);
    let overlap_pnl: i128 = if overlap_lots == 0 {
        0
    } else {
        let sp = state.agg_short_avg_price_ticks as i128;
        let lp = state.agg_long_avg_price_ticks as i128;
        (sp - lp).saturating_mul(overlap_lots as i128)
    };

    // Tax only applies to non-negative round-trip PnL. Losses → no tax.
    let new_tax_total: u64 = if overlap_pnl <= 0 {
        0
    } else {
        let pnl_u128 = overlap_pnl as u128;
        let raw_tax = pnl_u128
            .saturating_mul(arg_tax_bps as u128)
            .checked_div(BPS_DENOM as u128)
            .unwrap_or(0);
        raw_tax.min(pnl_u128).min(u64::MAX as u128) as u64
    };

    // Incremental tax = new total − already paid (only the new portion
    // owed this fill).
    let incremental = new_tax_total.saturating_sub(state.tax_paid_this_batch_quote_lots);
    state.tax_paid_this_batch_quote_lots = state
        .tax_paid_this_batch_quote_lots
        .saturating_add(incremental);

    ArgTaxOutcome {
        overlap_pnl_quote_lots: overlap_pnl,
        incremental_tax_quote_lots: incremental,
        new_state: state,
    }
}

/// Weighted-average price helper. `((lots_old × avg_old) + (lots_new × price_new)) / total_lots`.
/// Returns `(total_lots, new_avg_price)`. Saturating math; if total
/// overflows u64, returns u64::MAX with the existing average preserved.
#[inline]
fn weighted_update(lots_old: u64, avg_old: u64, lots_new: u64, price_new: u64) -> (u64, u64) {
    let total = lots_old.saturating_add(lots_new);
    if total == 0 {
        return (0, 0);
    }
    if lots_old == 0 {
        return (lots_new, price_new);
    }
    if lots_new == 0 {
        return (lots_old, avg_old);
    }
    let num: u128 = (avg_old as u128).saturating_mul(lots_old as u128)
        .saturating_add((price_new as u128).saturating_mul(lots_new as u128));
    let avg = (num / total as u128).min(u64::MAX as u128) as u64;
    (total, avg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_no_tax() {
        let s = ArgBatchState::default();
        let leg = AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 };
        let out = apply_aggressor_leg(s, leg, 1, 5_000);
        assert_eq!(out.overlap_pnl_quote_lots, 0);
        assert_eq!(out.incremental_tax_quote_lots, 0);
        assert_eq!(out.new_state.agg_long_lots, 100);
        assert_eq!(out.new_state.agg_long_avg_price_ticks, 1_000);
    }

    #[test]
    fn buy_then_sell_at_higher_price_taxed() {
        // Bot buys 100 @ 1000, then sells 100 @ 1010. Overlap 100 lots,
        // PnL = (1010 - 1000) × 100 = 1000. At 50% tax → 500.
        let s = ArgBatchState::default();
        let leg1 = AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 };
        let leg2 = AggressorLeg { side: 1, size_lots: 100, price_ticks: 1_010 };
        let after1 = apply_aggressor_leg(s, leg1, 1, 5_000);
        let after2 = apply_aggressor_leg(after1.new_state, leg2, 1, 5_000);
        assert_eq!(after2.overlap_pnl_quote_lots, 1_000);
        assert_eq!(after2.incremental_tax_quote_lots, 500);
    }

    #[test]
    fn losing_roundtrip_no_tax() {
        // Bot misjudges and sells low after buying high: no tax.
        let s = ArgBatchState::default();
        let leg1 = AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_010 };
        let leg2 = AggressorLeg { side: 1, size_lots: 100, price_ticks: 1_000 };
        let after1 = apply_aggressor_leg(s, leg1, 1, 5_000);
        let after2 = apply_aggressor_leg(after1.new_state, leg2, 1, 5_000);
        assert!(after2.overlap_pnl_quote_lots < 0);
        assert_eq!(after2.incremental_tax_quote_lots, 0);
    }

    #[test]
    fn partial_overlap_taxes_only_overlap() {
        // Buy 100, sell 60 → overlap = 60 lots. PnL = (1010 - 1000) × 60 = 600.
        // 50% tax = 300.
        let s = ArgBatchState::default();
        let after1 = apply_aggressor_leg(s, AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 }, 1, 5_000);
        let after2 = apply_aggressor_leg(after1.new_state, AggressorLeg { side: 1, size_lots: 60, price_ticks: 1_010 }, 1, 5_000);
        assert_eq!(after2.overlap_pnl_quote_lots, 600);
        assert_eq!(after2.incremental_tax_quote_lots, 300);
    }

    #[test]
    fn batch_advance_resets_state() {
        let mut s = ArgBatchState::default();
        s = apply_aggressor_leg(s, AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 }, 1, 5_000).new_state;
        // Different batch → state resets.
        let reset = reset_on_batch_advance(s, 2);
        assert_eq!(reset.agg_long_lots, 0);
        assert_eq!(reset.tax_paid_this_batch_quote_lots, 0);
        assert_eq!(reset.batch_seq, 2);
    }

    #[test]
    fn cumulative_tax_only_charges_incremental() {
        // First overlap: tax = 500 (paid).
        let s = ArgBatchState::default();
        let after1 = apply_aggressor_leg(s, AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 }, 1, 5_000);
        let after2 = apply_aggressor_leg(after1.new_state, AggressorLeg { side: 1, size_lots: 100, price_ticks: 1_010 }, 1, 5_000);
        assert_eq!(after2.incremental_tax_quote_lots, 500);

        // Second sell adds another 100 lots at 1020. New short-side avg
        // = (1010×100 + 1020×100)/200 = 1015. Long lots still 100, short
        // now 200. Overlap = min(100, 200) = 100. PnL = (1015 - 1000)×100 = 1500.
        // 50% tax = 750. Already paid 500 → incremental = 250.
        let after3 = apply_aggressor_leg(after2.new_state, AggressorLeg { side: 1, size_lots: 100, price_ticks: 1_020 }, 1, 5_000);
        assert_eq!(after3.overlap_pnl_quote_lots, 1_500);
        assert_eq!(after3.incremental_tax_quote_lots, 250);
        assert_eq!(after3.new_state.tax_paid_this_batch_quote_lots, 750);
    }

    #[test]
    fn weighted_avg_correct_for_same_side_legs() {
        // Buy 100 @ 1000 then buy 100 @ 1010 → avg 1005.
        let s = ArgBatchState::default();
        let after1 = apply_aggressor_leg(s, AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 }, 1, 5_000);
        let after2 = apply_aggressor_leg(after1.new_state, AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_010 }, 1, 5_000);
        assert_eq!(after2.new_state.agg_long_lots, 200);
        assert_eq!(after2.new_state.agg_long_avg_price_ticks, 1_005);
    }

    #[test]
    fn zero_tax_bps_is_disabled() {
        let s = ArgBatchState::default();
        let after1 = apply_aggressor_leg(s, AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 }, 1, 0);
        let after2 = apply_aggressor_leg(after1.new_state, AggressorLeg { side: 1, size_lots: 100, price_ticks: 1_010 }, 1, 0);
        assert_eq!(after2.overlap_pnl_quote_lots, 1_000);
        assert_eq!(after2.incremental_tax_quote_lots, 0, "tax_bps=0 disables");
    }

    #[test]
    fn tax_bounded_by_overlap_pnl() {
        // tax_bps > 100% caps at overlap_pnl (no overpaying).
        let s = ArgBatchState::default();
        let after1 = apply_aggressor_leg(s, AggressorLeg { side: 0, size_lots: 100, price_ticks: 1_000 }, 1, 20_000);
        let after2 = apply_aggressor_leg(after1.new_state, AggressorLeg { side: 1, size_lots: 100, price_ticks: 1_010 }, 1, 20_000);
        assert_eq!(after2.overlap_pnl_quote_lots, 1_000);
        assert_eq!(after2.incremental_tax_quote_lots, 1_000, "capped at 100% of overlap");
    }

    #[test]
    fn invalid_side_is_noop() {
        let s = ArgBatchState::default();
        let after = apply_aggressor_leg(s, AggressorLeg { side: 99, size_lots: 100, price_ticks: 1_000 }, 1, 5_000);
        // Invalid side: state unchanged (no update happens).
        assert_eq!(after.new_state.agg_long_lots, 0);
        assert_eq!(after.new_state.agg_short_lots, 0);
    }
}
