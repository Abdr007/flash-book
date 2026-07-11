//! Property tests for the Phase 2 isolated-margin invariants in
//! `matcher::risk::assess_margin_unified` / `assess_margin_split`.
//!
//! The invariants exercised here are the ones MARGIN_MATH.md §9 calls
//! out as load-bearing for isolation safety:
//!
//!   I-1/I-2  Strict bucket independence: a healthy cross set + a
//!            healthy isolated bucket = healthy trader; ANY isolated
//!            failure flips the whole assessment to unhealthy.
//!   I-3      Cross pool insulation: no matter how big the cross
//!            collateral, an under-collateralised isolated position
//!            CANNOT be rescued by it.
//!   I-4      Isolated bucket insulation: an isolated failure does not
//!            bleed into the cross set's own health — running the cross
//!            subset by itself (with the same C_T) yields the same
//!            healthy verdict as running it alongside an isolated bust.
//!   (5.4)    Dispatch invariant: unified with all-cross snapshots is
//!            byte-identical to assess_margin (required + equity +
//!            healthy + worst_scenario_idx all equal).
//!
//! Sister files: `proptest_risk.rs` (cross-only invariants) and
//! `proptest_liquidation.rs` (close-side ordering and synthetic limit).

use anchor_lang::prelude::Pubkey;
use flash_book::matcher::lot::Ticks;
use flash_book::matcher::order::Side;
use flash_book::matcher::risk::{
    assess_margin, assess_margin_split, assess_margin_unified, default_scenarios, MarketSnapshot,
    PositionSnapshot,
};
use proptest::prelude::*;

const MARKET_A_BYTES: [u8; 32] = [42u8; 32];
const MARKET_B_BYTES: [u8; 32] = [43u8; 32];

fn market(seed: [u8; 32], mark: u64, mmr_bps: u32) -> MarketSnapshot {
    MarketSnapshot {
        market: Pubkey::new_from_array(seed),
        mark_price: Ticks(mark),
        cum_funding_index: 0,
        maintenance_margin_bps: mmr_bps,
        tick_size: 1,
        concentration_threshold_lots: 0,
        concentration_extra_mmr_bps: 0,
        // OI-scaled MMR disabled by default in tests.
        side_oi_lots: 0,
        oi_mmr_slope_bps_per_million_lots: 0,
        oi_mmr_max_extra_bps: 0,
        paper_profit_haircut_bps: 0,
    }
}

fn position(market_key: Pubkey, side: Side, size: u64, entry: u64, iso: u64) -> PositionSnapshot {
    PositionSnapshot {
        market: market_key,
        side,
        size_lots: size,
        entry_price: Ticks(entry),
        cum_funding_index_at_entry: 0,
        collateral_quote_lots: iso,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Dispatch identity: when no snapshot is isolated,
    /// `assess_margin_unified` is byte-identical to `assess_margin`.
    /// This is the bedrock of the "drop-in replacement" claim across
    /// the 8 trade-path call sites.
    #[test]
    fn unified_no_isolated_equals_assess_margin(
        long_size in 0u64..1_000u64,
        short_size in 0u64..1_000u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        collateral in 0u64..1_000_000u64,
    ) {
        let mkt = market(MARKET_A_BYTES, mark, 125);
        let mut positions = Vec::new();
        if long_size > 0 {
            positions.push(position(mkt.market, Side::Long, long_size, entry, 0));
        }
        if short_size > 0 {
            positions.push(position(mkt.market, Side::Short, short_size, entry, 0));
        }
        let scenarios = default_scenarios(&[mkt.market]);
        let flat = assess_margin(&positions, &[mkt], &scenarios, collateral)?;
        let unified = assess_margin_unified(&positions, &[mkt], &scenarios, collateral)?;
        prop_assert_eq!(flat.required_quote_lots, unified.required_quote_lots);
        prop_assert_eq!(flat.equity_quote_lots_signed, unified.equity_quote_lots_signed);
        prop_assert_eq!(flat.is_healthy, unified.is_healthy);
        prop_assert_eq!(flat.worst_scenario_idx, unified.worst_scenario_idx);
    }

    /// I-3 (cross pool insulation): a single isolated position is
    /// stress-tested against its OWN collateral, regardless of how
    /// large `cross_collateral` is. If the isolated bucket cannot
    /// survive the worst scenario, the trader is unhealthy — period.
    #[test]
    fn isolated_assessment_ignores_fat_cross_pool(
        size in 100u64..1_000u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        cross_pool in 1_000_000u64..u64::MAX / 4,
    ) {
        let mkt = market(MARKET_A_BYTES, mark, 125);
        // Isolate with a token-amount of per-position collateral against
        // a sizeable position — the stress lattice will trip on it.
        let positions = vec![position(mkt.market, Side::Long, size, entry, 1)];
        let scenarios = default_scenarios(&[mkt.market]);

        let unified = assess_margin_unified(&positions, &[mkt], &scenarios, cross_pool)?;
        // Reference: same position run on cross-only path with iso=0 but
        // collateral = 1 — that's what the isolated bucket sees.
        let reference_positions = vec![position(mkt.market, Side::Long, size, entry, 0)];
        let reference = assess_margin(&reference_positions, &[mkt], &scenarios, 1)?;

        // If the isolated bucket fails the reference check, the unified
        // dispatch must ALSO fail — the cross pool is invisible.
        if !reference.is_healthy {
            prop_assert!(
                !unified.is_healthy,
                "cross pool of {} rescued an isolated bust (required={}, eq_cross={})",
                cross_pool, unified.required_quote_lots, unified.equity_quote_lots_signed,
            );
        }
    }

    /// I-4 (isolated bucket insulation): an isolated failure does not
    /// retroactively make the cross set unhealthy. We construct a
    /// healthy cross set on market A and a busted isolated on market
    /// B; the cross-only health of A against `cross_pool` is the
    /// ground truth — running it alongside the isolated bust does not
    /// change whether A is solvent.
    ///
    /// Setup constraint: both positions are at-the-money (`entry ==
    /// mark`) so unrealized PnL is zero and the isolated bucket's
    /// solvency is dominated by `iso=1` vs stress-lattice maintenance
    /// margin on a 1000-lot position — guaranteed to fail.
    #[test]
    fn isolated_failure_does_not_bleed_into_cross_set(
        cross_size in 1u64..50u64,         // small, well-collateralised
        iso_size in 500u64..5_000u64,      // large, will trip stress
        mark in 50u64..200u64,
        cross_pool in 100_000u64..1_000_000u64,
    ) {
        let mkt_a = market(MARKET_A_BYTES, mark, 125);
        let mkt_b = market(MARKET_B_BYTES, mark, 125);
        let scenarios = default_scenarios(&[mkt_a.market, mkt_b.market]);

        // Entry == mark for both → upnl = 0 at the flat scenario; the
        // isolated bucket has only iso=1 against a large position.
        let cross_only = vec![position(mkt_a.market, Side::Long, cross_size, mark, 0)];
        let mixed = vec![
            position(mkt_a.market, Side::Long, cross_size, mark, 0),
            position(mkt_b.market, Side::Long, iso_size, mark, 1), // isolated, certainly busted
        ];

        let cross_alone = assess_margin(&cross_only, &[mkt_a, mkt_b], &scenarios, cross_pool)?;
        let combined = assess_margin_unified(&mixed, &[mkt_a, mkt_b], &scenarios, cross_pool)?;

        // Cross subset must remain solvent: a tiny long at fair value
        // against 100k+ of collateral cannot fail any stress scenario.
        prop_assert!(
            cross_alone.is_healthy,
            "cross-only setup should be healthy by construction \
             (cross_size={}, cross_pool={})",
            cross_size, cross_pool,
        );
        // The combined verdict is unhealthy because of the isolated
        // failure — but that's information about the isolated bucket,
        // not about the cross set.
        prop_assert!(
            !combined.is_healthy,
            "isolated position with iso=1 against size={} at fair value \
             must fail the stress lattice", iso_size,
        );
        // And the split required must be ≥ cross-only required (it
        // includes the isolated bucket's required on top).
        prop_assert!(
            combined.required_quote_lots >= cross_alone.required_quote_lots,
            "split required ({}) must be ≥ cross-only required ({})",
            combined.required_quote_lots, cross_alone.required_quote_lots,
        );
    }

    /// I-7-adjacent (transition cash conservation): the explicit
    /// isolated map override semantics of `assess_margin_split` —
    /// when the snapshot's `collateral_quote_lots` field disagrees
    /// with the external map, the external map wins. This is the
    /// contract that `set_position_isolated` relies on (it calls
    /// assess_margin_split with the POST-transition map even though
    /// the snapshot still reads the pre-transition value).
    #[test]
    fn split_external_map_overrides_snapshot_field(
        size in 10u64..500u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        iso_collateral in 1_000u64..100_000u64,
    ) {
        let mkt = market(MARKET_A_BYTES, mark, 125);
        let scenarios = default_scenarios(&[mkt.market]);
        // Snapshot field reads as cross (0); external map says isolated.
        let positions = vec![position(mkt.market, Side::Long, size, entry, 0)];
        let iso_map = [(mkt.market, iso_collateral)];

        let via_map = assess_margin_split(
            &positions, &[mkt], &scenarios, 0, &iso_map,
        )?;
        // Equivalent: same position with snapshot field = iso_collateral
        // and external map empty, but routed through unified (which
        // builds the map from the field).
        let mirror_positions = vec![position(mkt.market, Side::Long, size, entry, iso_collateral)];
        let via_field = assess_margin_unified(&mirror_positions, &[mkt], &scenarios, 0)?;

        prop_assert_eq!(via_map.required_quote_lots, via_field.required_quote_lots);
        prop_assert_eq!(via_map.equity_quote_lots_signed, via_field.equity_quote_lots_signed);
        prop_assert_eq!(via_map.is_healthy, via_field.is_healthy);
    }

    /// Monotone insulation: adding more collateral to the isolated
    /// bucket of a position can NEVER make the trader less healthy.
    /// (Mirror of `monotonic_in_collateral` from proptest_risk.rs,
    /// but on the per-position bucket instead of the cross pool.)
    #[test]
    fn monotonic_in_isolated_collateral(
        size in 10u64..500u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        iso_a in 1u64..50_000u64,
        delta in 1u64..50_000u64,
    ) {
        let mkt = market(MARKET_A_BYTES, mark, 125);
        let scenarios = default_scenarios(&[mkt.market]);
        let iso_b = iso_a.saturating_add(delta);

        let a = vec![position(mkt.market, Side::Long, size, entry, iso_a)];
        let b = vec![position(mkt.market, Side::Long, size, entry, iso_b)];

        // Cross pool 0 — only the isolated bucket can rescue or sink.
        let a_assess = assess_margin_unified(&a, &[mkt], &scenarios, 0)?;
        let b_assess = assess_margin_unified(&b, &[mkt], &scenarios, 0)?;

        if a_assess.is_healthy {
            prop_assert!(
                b_assess.is_healthy,
                "more isolated collateral made the trader less healthy: \
                 iso_a={} (healthy), iso_b={} (unhealthy)", iso_a, iso_b,
            );
        }
    }

    /// Cross pool changes do NOT alter the verdict for a trader whose
    /// only position is isolated. (The cross set is empty → vacuously
    /// healthy → required = 0; only the isolated bucket drives the
    /// outcome.)
    #[test]
    fn cross_pool_changes_dont_affect_solo_isolated(
        size in 10u64..500u64,
        entry in 50u64..200u64,
        mark in 50u64..200u64,
        iso in 100u64..100_000u64,
        cross_a in 0u64..50_000u64,
        cross_b in 0u64..50_000u64,
    ) {
        let mkt = market(MARKET_A_BYTES, mark, 125);
        let scenarios = default_scenarios(&[mkt.market]);
        let positions = vec![position(mkt.market, Side::Long, size, entry, iso)];

        let a = assess_margin_unified(&positions, &[mkt], &scenarios, cross_a)?;
        let b = assess_margin_unified(&positions, &[mkt], &scenarios, cross_b)?;

        prop_assert_eq!(a.required_quote_lots, b.required_quote_lots);
        prop_assert_eq!(a.is_healthy, b.is_healthy);
        // Equity DOES vary with cross pool (the aggregate equity is
        // pool + isolated), but the healthy verdict and required are
        // bucket-local and must match.
    }
}
