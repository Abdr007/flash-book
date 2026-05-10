# Wave 19h — v1 Deletion Checklist

Mechanical deletion plan for the v1 surface, queued for a focused
session that can dedicate uninterrupted time to test migration. This
doc is an executable checklist: each box is a discrete file edit
with a verification step.

## Pre-flight

  - [ ] All v2 ixs functional (waves 18d–19g): `cargo test --release` 100% green
  - [ ] Bot/MM/keepers migrated to v2 (wave 19h-pt1, commit `04445a4`)
  - [ ] No production deployments depending on v1 ixs (verify via on-chain
        caller analysis)

## Phase A — Delete v1 program ixs (lib.rs)

For each of these 13 ixs, delete the `pub fn` body:

  - [ ] `initialize_order_buffer` (line ~91)
  - [ ] `place_limit_order` (line ~2963)
  - [ ] `execute_trigger_order` (line ~3610)
  - [ ] `place_bracket_order` (line ~3898)
  - [ ] `execute_twap_slice` (line ~4449)
  - [ ] `place_iceberg_order` (line ~4675)
  - [ ] `replenish_iceberg` (line ~4893)
  - [ ] `cancel_iceberg` (line ~5093)
  - [ ] `cancel_order` (line ~5477)
  - [ ] `cancel_all_orders_in_market` (line ~5521)
  - [ ] `run_batch` (line ~5639)
  - [ ] `liquidate_position` (line ~6215)
  - [ ] `liquidate_portfolio` (line ~7082)

After deletion, run `cargo build --release` — expect compile errors
about missing `*Account` contexts (those get deleted in Phase B).

## Phase B — Delete v1 account contexts (lib.rs, end-of-file region)

  - [ ] `InitializeOrderBuffer`
  - [ ] `PlaceOrder`
  - [ ] `CancelOrder`
  - [ ] `CancelAllOrders`
  - [ ] `RunBatch`
  - [ ] `PlaceBracketOrder`
  - [ ] `ExecuteTriggerOrder`
  - [ ] `ExecuteTwapSlice`
  - [ ] `PlaceIcebergOrder`
  - [ ] `ReplenishIceberg`
  - [ ] `CancelIceberg`
  - [ ] `LiquidatePosition`
  - [ ] `LiquidatePortfolio`

## Phase C — Delete v1-only events (lib.rs)

Delete events with v2 equivalents:

  - [ ] `OrderCancelledEvent` → use `OrderCancelledV2Event`
  - [ ] `OrdersMassCancelledEvent` (no v2 equivalent — drop or keep as legacy)
  - [ ] `BracketOrderPlacedEvent` → use `BracketOrderPlacedV2Event`
  - [ ] `TriggerOrderExecutedEvent` → use `TriggerOrderExecutedV2Event`
  - [ ] `TwapSliceExecutedEvent` → use `TwapSliceExecutedV2Event`
  - [ ] `IcebergOrderPlacedEvent` → use `IcebergOrderPlacedV2Event`
  - [ ] `IcebergReplenishedEvent` → use `IcebergReplenishedV2Event`
  - [ ] `LiquidationInjectedEvent` → use `LiquidationInjectedV2Event`

KEEP these (shared between v1 and v2):

  - `BatchClearedEvent` (run_batch_v2 still emits)
  - `IcebergCancelledEvent` (cancel_iceberg_v2 reuses)
  - `LiquidatorRewardedEvent` (liquidate_position_v2 reuses)
  - `FillAppliedEvent`, `FlpFillAppliedEvent` (apply_fill / apply_flp_fill stay)

## Phase D — Delete v1 state types (state.rs + constants.rs)

  - [ ] `OrderBufferAccount` struct + impl
  - [ ] `OrderSlot` struct + Default impl
  - [ ] `ORDER_BUFFER_CAP` const (constants.rs)

## Phase E — Delete v1 helpers (lib.rs)

  - [ ] `slot_to_order` (only used by deleted run_batch)

## Phase F — Lift crate-root deprecation suppression (lib.rs)

  - [ ] Remove `#![allow(deprecated)]` (no more deprecated ixs)
  - [ ] Remove the explanatory comment block above it

## Phase G — Delete v1 SDK builders (sdk-ts/src/client.ts)

  - [ ] `initializeOrderBufferIx`
  - [ ] `placeLimitOrderIx`
  - [ ] `cancelOrderIx`
  - [ ] `cancelAllOrdersInMarketIx`
  - [ ] `runBatchIx`
  - [ ] `placeBracketOrderIx`
  - [ ] `executeTriggerOrderIx`
  - [ ] `executeTwapSliceIx`
  - [ ] `placeIcebergOrderIx`
  - [ ] `replenishIcebergIx`
  - [ ] `cancelIcebergIx`
  - [ ] `liquidatePositionIx`
  - [ ] `liquidatePortfolioIx`
  - [ ] `this.orderBuffer(market)` helper method

## Phase H — Delete v1 SDK types (sdk-ts/src/accounts.ts)

  - [ ] `OrderBufferAccount` type export
  - [ ] `OrderSlot` type export
  - [ ] `fetchOrderBuffer` function

## Phase I — Delete v1 SDK PDAs (sdk-ts/src/pdas.ts)

  - [ ] `ORDER_BUFFER_SEED` const
  - [ ] `orderBufferPda` function
  - [ ] Remove from `sdk-ts/src/index.ts` re-export list

## Phase J — Delete v1 SDK events (sdk-ts/src/events.ts)

  - [ ] All v1 event interfaces with v2 equivalents (mirror Phase C)
  - [ ] Update `FlashBookEvent` discriminated union accordingly

## Phase K — Delete `detectOrderbookVersion` SDK helper

  - [ ] Remove from `sdk-ts/src/index.ts` (the v1-vs-v2 detection
        helper from wave 18h is no longer meaningful — only v2 exists)
  - [ ] Remove `tests/orderbook-version.test.ts` (6 tests that exercise
        the helper)
  - [ ] Remove `PREFERRED_ORDERBOOK_VERSION` const (only one version exists now)

## Phase L — Migrate Rust integration tests (programs/flash-book/tests/integration.rs)

70+ refs to v1 ixs. Two paths:

**Path 1 (recommended) — Migrate each test:**

  - [ ] `place_limit_order_lands_in_buffer` → `place_limit_order_v2_lands_in_hypertree`
  - [ ] `run_batch_advances_counter_and_clears_buffer` → `run_batch_v2_*`
  - [ ] `cancel_order_removes_from_buffer` → `cancel_order_v2_*`
  - [ ] `cancel_order_rejects_other_traders_order` → v2 equivalent
  - [ ] `liquidate_position_rejects_healthy_trader` → `liquidate_position_v2_*`
  - [ ] `liquidate_portfolio_rejects_healthy_trader_zero_remaining` → v2
  - [ ] `liquidate_portfolio_with_two_markets_and_no_positions` → v2
  - [ ] `place_limit_order_rejects_*` (5 tests) → `place_limit_order_v2_rejects_*`
  - [ ] `place_basket_order_*` (4 tests) → these are bracket order tests — use v2
  - [ ] `two_traders_crossing_orders_clear_in_batch` → v2
  - [ ] `place_limit_order_below_min_lot_rejected` → v2
  - [ ] `place_limit_order_off_tick_rejected` → v2
  - [ ] `apply_fill_*` (3 tests) → these don't depend on v1 directly; just
        the test setup uses v1 to create position state. Update setup to v2.
  - [ ] `place_limit_order_per_trader_rate_limit_enforced` → v2
  - [ ] `place_limit_order_rejects_above_position_cap` → v2
  - [ ] `place_limit_order_rejects_above_capital_ratio_cap` → v2

  Estimated effort: 30 min/test × ~25 v1-touching tests = ~12 hours

**Path 2 (faster, less coverage) — Delete v1 tests, rely on:**

  - state_v2 unit tests (12 tests, in `state_v2.rs`)
  - matcher proptests (5 modules, ~50 tests)
  - SDK builder tests (135 tests in `sdk-ts/tests/`)
  - The risk module proptest invariants

  Loses ~25 integration tests covering ix-level wiring. Acceptable IF
  coverage analysis shows the unit/proptest suites cover the same logic.

## Phase M — Update preview-trade.ts (sdk-ts/src/preview-trade.ts)

Currently uses `fetchOrderBuffer` to cross-sim against pre-existing
orders. Migration options:

  - [ ] **Option A**: drop the cross-sim (just project post-trade
        position + margin against current mark)
  - [ ] **Option B**: invoke `view_book_depth_v2` via `simulateTransaction`
        to fetch top-4 levels per side, feed into sim
  - [ ] Update `tests/preview-trade.test.ts` accordingly

## Phase N — Update bot fetchOpenOrders (bot/src/market-maker.ts)

The v2 venue's in-memory order tracking is correct for the constant-
churn quoter pattern but DOESN'T survive process restart. For
production:

  - [ ] Add an event-stream subscription using `subscribeToProgramEvents`
  - [ ] Subscribe to `OrderPlacedV2Event`, `OrderCancelledV2Event`,
        `BatchClearedEvent` for the (market, trader) pair
  - [ ] On startup, replay events from a stored slot to hydrate the
        `placedOrders` map
  - [ ] On placement, append to map (existing logic)
  - [ ] On `OrderCancelledV2Event` matching the trader, remove from map
  - [ ] On `BatchClearedEvent`, walk fills (need per-fill event subscription
        — currently `BatchClearedEvent` carries fill_count only, not per-fill
        data; consider emitting `FillAppliedEvent` on the matcher tick)

## Verification gates

After each phase:

  - [ ] `cargo build --release` clean
  - [ ] `cargo test --release` 100% pass
  - [ ] `cd sdk-ts && bun run typecheck` clean
  - [ ] `bun test` 100% pass
  - [ ] `anchor build` IDL regenerates cleanly
  - [ ] `git diff --stat` shows expected file changes only

## Final state target

After all phases complete:

  - flash-book program is v2-only (one orderbook, not two)
  - `OrderBufferAccount` does not exist anywhere
  - SDK exports only v2 builders
  - `bot/` runs on v2 with event-stream-derived order tracking
  - All integration tests target v2 ixs
  - Total LOC reduction: ~2000 (estimated)
  - Cleanup commit count: 5-7 commits (one per phase batch)

## Time estimate

  - Path 1 (full migration): 16-20 focused hours, 1 dedicated session
    over 2-3 days
  - Path 2 (delete old tests): 4-6 hours, single session

## Risk

  - **Low** if executed phase-by-phase with verification gates
  - **High** if attempted as one mega-commit (cascading test failures
    impossible to debug)

## Why not done in autonomous session

This deletion was repeatedly attempted in the autonomous session that
shipped waves 18a–19g. Each attempt hit the same wall: 70+ integration
test references to v1 ixs that need careful migration. Done in haste
they introduce subtle bugs (e.g., forgetting that `place_limit_order_v2`
takes `flags: u8` not `postOnly: bool`); done carefully they take
4-6+ hours. That's a focused human-driven session, not an autonomous
side-effect.
