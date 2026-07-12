# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] — 2026-05-15 — Wave 24-65 + internal audit

Major risk-engine + primitive-library expansion. 23 new pure-math
modules covering every primitive a top-tier perp DEX uses, plus
14 new on-chain ix and a comprehensive internal audit pass.

### Added — Risk engine (Waves 24, 25, 26)

- **H-haircut (Wave 24)** — junior-claim PnL gating.
  `MarketHaircutStateAccount` + `PositionHaircutStateAccount` sibling
  PDAs. Pure math in `matcher/haircut.rs` with 22 unit tests + 10
  proptest properties. New ix:
  `initialize_haircut_state`, `init_position_haircut_state`,
  `mature_position`, `convert_position`, `flush_haircut_dust`,
  `release_gain_to_haircut`, `seed_residual`,
  `verify_haircut_invariants`. Wired into `apply_fill` +
  `apply_flp_fill` via optional accounts (opt-in per market).
- **A/K/F/B side indices (Wave 25a/b)** — lazy O(1) per-position
  settlement helpers. `MarketSideAccrualAccount` sibling PDA. Pure
  math in `matcher/side_accrual.rs` with 19 unit tests. Wire-in into
  `settle_funding` queued as Wave 25c.
- **Per-slot envelope (Wave 26)** — init-time proof of
  `price_funding_loss_N + liq_fee_N ≤ mm_req_N` for every notional.
  `MarketEnvelopeConfigAccount` sibling PDA. Pure math in
  `matcher/envelope.rs` with 13 unit tests + 7 proptest properties.
  Runtime gate `gate_oracle_update` wired into all three oracle paths
  (`update_oracle`, `update_oracle_quorum`, `update_oracle_from_pyth`).

### Added — Order types (Waves 27, 32, 51, 52, 55, 62, 63, 64)

- **`acceptable_price` slippage cap (Wave 27a/b)** — added to v3
  triggers + TWAP slices + bracket legs. GMX V2-style anti-gap-fill.
  New error `TriggerSlippageExceeded` (2100). New event
  `TriggerOrderV3SlippageCancelledEvent`. 14 unit tests.
- **Peg orders (Wave 32)** — primary + mid-peg pricing helpers in
  `matcher/peg_pricing.rs`. 10 unit tests.
- **MIT order pricing (Wave 51)** — Market-If-Touched in
  `matcher/mit_order.rs`. 7 unit tests.
- **Trailing stop (Wave 52)** — HWM/LWM-based trailing in
  `matcher/trailing_stop.rs`. 7 unit tests.
- **Stop-limit composite (Wave 55)** — `matcher/stop_limit.rs`. 9 tests.
- **Min-fill-size / FOK (Wave 62)** — `matcher/min_fill_size.rs`. 5 tests.
- **Reduce-only on limit orders (Wave 63)** — `matcher/reduce_only.rs`.
  6 tests.
- **Conditional cancel (Wave 64)** — `matcher/conditional_cancel.rs`.
  3 tests.

### Added — Anti-MEV / fairness (Waves 29, 50, 59, 61)

- **ARG / Aggressor Roundtrip Guard (Wave 29)** — within-batch
  sandwich tax. `matcher/arg.rs` with 10 unit tests + 4 proptest
  properties.
- **Self-trade prevention policies (Wave 50)** — 4-policy decision
  helper in `matcher/self_trade.rs`. 6 tests. (Matcher walk already
  implements 3 policies; pure module documents the decision logic in
  isolation.)
- **Pro-rata fill split (Wave 59)** — `matcher/pro_rata.rs`. 7 tests.
- **Cancel-on-disconnect (Wave 61)** — `matcher/cancel_on_disconnect.rs`.
  4 tests.

### Added — LP economics (Waves 35, 36, 37, 38, 56, 57)

- **Borrow fee (Wave 35)** — utilization-based, GMX V2 pattern.
  `matcher/borrow_fee.rs`. 9 tests.
- **Pending claim / soft-fail (Wave 36)** — `matcher/pending_claim.rs`.
  8 tests.
- **Funding velocity smoothing (Wave 37)** — PID-style ramp.
  `matcher/funding_velocity.rs`. 11 tests.
- **Insurance auto-replenish (Wave 38)** — `matcher/insurance_replenish.rs`.
  7 tests.
- **Tiered LP rewards / duration-weighted (Wave 56)** —
  `matcher/tiered_lp_rewards.rs`. 6 tests.
- **JIT LP defense / min-hold (Wave 57)** — `matcher/jit_lp_defense.rs`.
  6 tests.

### Added — Trader / market risk controls (Waves 28, 31, 53, 54, 58, 60, 65)

- **OI-scaled MMR (Wave 28a/b)** — GMX V2 pattern composed onto
  flash-book's stress lattice. Helpers + integration into
  `MarketSnapshot::effective_mmr_bps`. 7 unit tests + 8 proptest
  properties.
- **Per-trader position cap (Wave 31)** — `matcher/position_cap.rs`.
  8 tests.
- **Daily loss limit (Wave 53)** — `matcher/daily_loss_limit.rs`.
  6 tests.
- **Volume rate limit / token bucket (Wave 54)** —
  `matcher/volume_rate_limit.rs`. 7 tests.
- **Per-trader concentration cap (Wave 58)** —
  `matcher/concentration.rs`. 5 tests.
- **Cross-margin asset weights (Wave 60)** — joint margin with
  correlation. `matcher/cross_margin_weights.rs` with isqrt + 8 unit
  tests + 8 proptest properties.
- **Stable cross-collateral weighting (Wave 65)** —
  `matcher/stable_collateral.rs`. 7 tests.

### Added — Decentralization + operations (Waves 24f, 30)

- **`burn_market_authority` (Wave 30)** — permanent per-market
  authority burn. One-way state change.
- **`verify_protocol_solvency` (Wave 24f)** — permissionless probe
  checking `vault.amount ≥ insurance.balance + flp.total_capital`.
  Hard-reverts on insolvency.

### Changed — Audit fixes

- `execute_trigger_order_v2` + `execute_trigger_order_v3` now gate on
  oracle staleness before reading `oracle_price_ticks` (Audit 9.1, 9.2;
  HIGH severity, fixed).
- `execute_twap_slice_v3` now gates on oracle staleness before the
  acceptable_price slippage check (Audit 9.3; MEDIUM severity, fixed).
- `update_market_params` now enforces `maintenance_margin_ratio_bps <
  5000` and `initial_margin_ratio_bps < 5000`, plus the combined
  MMR + concentration_extra cap. Prevents authority from spiking MMR
  above 50% (Audit 11.1; MEDIUM severity, fixed).

### Added — Documentation

- `docs/AUDIT.md` (since consolidated into `SECURITY.md`) — 19-audit internal review report
- `docs/AUDIT_BRIEF.md` — external auditor handoff brief
- `docs/ARCHITECTURE_FULL.md` — end-to-end system diagrams
- `docs/FEATURES.md` (since consolidated into `docs/ARCHITECTURE.md`) — complete primitive matrix
- `docs/HAIRCUT_MATH.md` — H-haircut formal spec
- `docs/SDK_ALIGNMENT.md` — Flash V2 beta SDK alignment
- `docs/INCIDENT_RESPONSE.md` — operational playbook
- `docs/PARAMETER_PLAYBOOK.md` — per-asset-class tuning guide

### Test counts

| Suite | v0.2.0 | v0.3.0 |
|---|---|---|
| Rust lib + matcher | 168 | **372** |
| Rust integration | 37 | 37 |
| Property tests | 52 | **91** |
| Wave-integration | 0 | **71** |
| TypeScript | 135 | 236 (carried) |
| **Total** | 392 | **807** |

Zero failures. Clean build.

---

## [0.2.0] — 2026-05-14 — Phase 2 complete

Tagged as `v0.2.0` on `main` at commit `66bde61`. Combines Phase 2
(0.32.0 below) with the Phase 2g/2h/2i/2j follow-ups that closed every
remaining gap in `docs/MARGIN_MATH.md §8`.

### Added (Phase 2g — realized-PnL materialisation)

`apply_fill` and `apply_flp_fill` now drain the realized-PnL delta
into the correct collateral bucket on every fill. Pre-2g, closing a
profitable position incremented `pos.realized_pnl_quote_lots` but no
code path drained it into `trader_state.collateral_quote_lots` — the
trader's spendable collateral stayed unchanged. Hyperliquid / dYdX /
Drift all materialise on close; Flash Book now does too.

Routing helpers `apply_realized_pnl_delta` + pure-math
`compute_realized_pnl_routing` with 11 unit tests in
`mod realized_pnl_routing_tests`. Isolated path saturates losses at
the per-position bucket (preserves I-3); cross path uses checked_sub
(`InsufficientCollateral` rather than going negative).

### Added (Phase 2h — ADL settlement routing for isolated positions)

`auto_deleverage` previously debited the underwater trader's cross
pool with the bankruptcy-price loss regardless of whether the
position was isolated — a violation of I-3. Phase 2h routes the
loss to the per-position bucket when isolated (saturating to 0; the
insurance fund waterfall absorbs the remainder) and the counter
gain to the per-position bucket when the counter is isolated. The
bankruptcy-price computation also sources `C` from the right bucket
so `bp` reflects the actual backing collateral, not the cross pool's
unrelated balance.

Routing helpers `route_adl_loss` + `route_adl_gain` with 11 unit
tests in `mod adl_routing_tests`, including the critical
`isolated_adl_leaves_both_cross_pools_untouched` invariant
lock-in.

### Added (Phase 2i — sub_index PDA verification in ApplyFill)

Closes the 1-byte routing-attack surface left by Phase 2d's seed
relaxation on `taker_trader_state` / `maker_trader_state`. The
handler now re-derives the expected TraderState PDA from
`(order.trader, order.sub_index)` and asserts against the passed
account key. `apply_fill` and `apply_flp_fill` gain `taker_sub_index`
/ `maker_sub_index` ix params (the sub_index values arrive via the
Phase 2e events, so an honest sequencer already had them).

New helper `verify_trader_state_pda(sub_index, trader, actual,
program_id)`.

### Added (Phase 2j — end-to-end ApplyFill integration tests)

Three new on-chain integration tests covering the fill path that
previously had ZERO E2E coverage:

* `apply_fill_opens_both_positions_and_moves_oi` — bedrock test;
  a single apply_fill init's both positions and updates OI.
* `apply_fill_materialises_realized_pnl_on_winning_close` — Phase 2g
  coverage on-chain. Opens long at 100_000, closes at 110_000,
  asserts trader_state.collateral_quote_lots ends at the exact
  expected value (+10_000 PnL credit - 55 close fee).
* `apply_fill_rejects_wrong_sub_index_trader_state` — Phase 2i
  coverage on-chain. Hostile sequencer attempt to pass sub-state
  while claiming sub_index=0 fails with WrongTrader.

### Final test counts (this release)

```
122 lib unit tests          (matcher + pure-math routing + state)
 37 integration tests       (was 34, +3 from Phase 2j)
  6 isolated-margin proptests (2000 cases each)
  6 risk proptests
 14 module proptests
 19 new-features proptests
  7 liquidation proptests
───
211 tests, all green
cargo build-sbf clean
```

### Closed gaps

After this release, `docs/MARGIN_MATH.md §8` has no remaining open
items. The previous v0.32.0 known-gap section listed:

* Realized PnL materialisation — **fixed (Phase 2g)**
* ADL settlement routing for isolated positions — **fixed (Phase 2h)**
* ApplyFill sequencer trust — **fixed (Phase 2i)**

The sub-account status section (now part of `docs/ARCHITECTURE.md`) is current as
of this release.

## [0.32.0] — 2026-05-14 — Phase 2: isolated margin + sub-account trading

Eight commits implementing isolated-margin end-to-end and enabling
sub-accounts to trade through every primitive. See `docs/MARGIN_MATH.md`
and the sub-account sections of `docs/ARCHITECTURE.md` / `docs/MARGIN_MATH.md` for the formal specs.

### Added

- **Isolated-margin Phase 2 (split risk + per-bucket routing).**
  `matcher/risk.rs::assess_margin_split` evaluates each isolated
  position as a singleton against its own `collateral_quote_lots`. The
  cross set is evaluated separately against the pooled collateral.
  Healthy iff every bucket is independently healthy. New
  `assess_margin_unified` dispatcher wires this into all 8 trade-path
  call sites + both liquidation health checks. Liquidator reward
  routing now debits the per-position bucket on isolated paths.
  `settle_funding` likewise routes per-position. New `set_position_isolated`
  / `set_position_cross` ixs; new `migrate_position_to_trader_state_key`
  ix. `apply_fill` fee routing for isolated positions. 5 unit + 6
  proptests covering the bucket independence invariants.
- **Position PDA migration to trader_state.key()-based seeds**
  (Phase 2c). `[POS_SEED, market, wallet]` →
  `[POS_SEED, market, trader_state]`. Sub-accounts now have distinct
  positions per market.
- **Sub-account trading enabled across all primitives** (Phase 2d–2f).
  `trader_state` seeds relaxed on 18 trade-path Accounts structs;
  `RestingOrderV2.sub_index` repurposed from `_pad`;
  `FillEntry.maker_sub_index` + `FillBatchEvent.taker_sub_index` so
  the sequencer can route fills correctly. `TraderStateAccount` and
  every secondary-order state struct (`TriggerOrderAccount{,V3}`,
  `TwapOrderAccount{,V3}`, `IcebergOrderAccount{,V3}`,
  `JitLiquidationOfferAccount`) carry sub_index; place / execute
  paths propagate it. Synthetic close orders in `liquidate_position_v2`
  + `liquidate_portfolio_v2` carry the liquidatee's sub_index.
- **`docs/MARGIN_MATH.md`** — audit-grade margin model spec with §9
  invariants table cross-referenced to handlers + tests.
- **Sub-account scope doc** (since consolidated into `docs/ARCHITECTURE.md`) — scope, design
  choices, what's done vs pending.
- **6 isolated-margin proptests** in `tests/proptest_isolated.rs`,
  2000 random cases each.
- **3 sub-account integration tests** verifying the deposit /
  wrong-trader-rejection / position-migration paths.

### Fixed

- `tests/integration.rs` hardcoded program ID aligned to
  `declare_id!()` — unblocks all 31 integration tests (previously
  100% failing with DeclaredProgramIdMismatch).

### Documentation

- `README.md` rewritten to reflect on-chain reality (continuous CLOB,
  not batch auction — the batch auction / commit-reveal pieces are in the TypeScript
  reference simulator only).
- `docs/COMPARISON.md` rewritten with file:line-cited claims and a
  dedicated "honest weaknesses" section.

### Known gaps (documented, not fixed in this release)

- Realized PnL doesn't materialise to `trader_state.collateral_quote_lots`
  on the normal fill close path. `auto_deleverage` is the only code
  path that materialises. Tracked in `MARGIN_MATH.md §8.1`.
- ApplyFill's `taker_trader_state` / `maker_trader_state` accept any
  TraderState whose `.trader` matches the order's `.trader`; the
  handler does not verify the trader_state PDA matches
  `[STATE_SEED, trader, &[sub_index]]`. Honest-sequencer-trust model.

### Stats

```
8 commits, ~21,500 lines (mostly IDL regen + COMPARISON.md rewrite).
cargo test -p flash-book — 186 tests pass (was 95).
bun test — 257 tests pass.
cargo build-sbf — clean.
```

## [0.31.0] — 2026-05-08

### Added — multi-oracle quorum (median-of-3 + dispersion gate)

Defense in depth against the JELLY/POPCAT class of attacks where an
attacker manipulates a single upstream price source. With three
independent sources (e.g. Pyth + Switchboard + internal TWAP), an
attacker would have to corrupt the majority simultaneously to move
the median.

#### `update_oracle_quorum` instruction

Takes three independent oracle observations (price, confidence,
publish_time):

```rust
update_oracle_quorum(
    prices_ticks: [u64; 3],
    confidences: [u64; 3],
    published_at_unix_seconds: [u64; 3],
)
```

Per-call validation:
1. Each source individually passes the existing staleness +
   confidence gates (params unchanged from `update_oracle`).
2. **Dispersion check**: `(max − min) / median × 10_000 ≤
   oracle_quorum_max_dispersion_bps`. If sources disagree by more
   than the configured threshold, the update is rejected — they're
   clearly seeing different markets, no single source is safe to use.

Conservative aggregation written to chain:
- `oracle_price_ticks` = median (most robust to one corrupt source).
- `oracle_confidence` = max of the three (most pessimistic).
- `oracle_published_at_unix_seconds` = min of the three (oldest, so
  staleness gates remain conservative for downstream readers).

#### Configuration

- New `MarketParams.oracle_quorum_max_dispersion_bps`
  (default 50 = 0.5%; 0 = disable check).
- Default in `defaultMajorMarketParams()` set to 50.

#### New error

- `OracleQuorumDispersionTooWide (1803)` — sources disagree.

#### SDK

- New `updateOracleQuorumIx({ authority, market, pricesTicks,
  confidences, publishedAtUnixSeconds })` builder.
- Coverage tripwire updated to **22** expected instruction builders.
- `MarketParamsRaw` extended with `oracleQuorumMaxDispersionBps`.

#### 2 new E2E integration tests

- `update_oracle_quorum_writes_median_with_three_close_sources` —
  three sources within tolerance, median accepted.
- `update_oracle_quorum_rejects_dispersed_sources` — sources
  disagreeing by ~10% with 50bps cap → rejected.

### Phase 2 / future work

The current quorum takes raw price values from the authority. A future
instruction will read directly via CPI from on-chain Pyth +
Switchboard + chainlink-on-solana accounts, removing the trust-the-caller
assumption. The quorum logic itself is identical.

### Total Anchor instruction surface: **22**.
### Total test count: 273 (172 TS sim+SDK+parity + 31 Rust unit + 36 Rust property × 2K + 33 E2E + 1 Rust parity).

## [0.30.0] — 2026-05-08

### Added — apply_flp_fill fee wiring + cross-language parity test

#### apply_flp_fill fee/rebate wiring (parity with apply_fill)

Same correctness gap that apply_fill had — fees were computed in the
matcher but never collected. Now fixed for the FLP-side path too:
- Computes notional, taker_fee, maker_rebate same as apply_fill.
- Deducts taker_fee from taker collateral.
- Credits maker_rebate to **flp.total_capital_quote_lots** (FLP is the
  maker; rebate accrues to pool capital).
- Contributes net fee to insurance fund.
- Updates market.total_fees_collected.

`InsuranceFundAccount` now required (mut) in `ApplyFlpFill` context.

#### Cross-language parity test — Rust ↔ TS implementations agree

Shared fixture `tests/parity/scenarios.json` with 8 canonical scenarios.
Both Rust (`programs/flash-book/tests/parity_test.rs`) and TS
(`sdk-ts/tests/parity.test.ts`) read the same file and assert identical
outputs. **If the two implementations ever drift apart, both tests
fail loudly.**

Scenarios: non-crossing, single crossing, tie-break, liquidation
priority, FIFO within priority class, self-trade prevention,
volume maximization, empty input.

The "self-trade prevention" scenario revealed a documented semantic:
`clearing_volume` is the Walrasian-max (theoretical) while
`fills.length × size` is the actual after STP. Both implementations
agree; documented in the fixture's `_doc` annotation.

### Total test count: 271. Total fuzz assertions: ~72,000.

## [0.29.0] — 2026-05-08

### Added — Phase 1 final-final-final polish (deep audit pass)

After a second deep research pass studying every major perp DEX (Phoenix,
Drift, Mango, Hyperliquid, dYdX v4, Manifest), three real correctness
gaps closed:

#### 1. Maker rebate and taker fee actually collected on-chain

**Gap discovered**: `Fill.takerFee` and `Fill.makerRebate` were computed
in the matcher's `clearBatch`, emitted in events, but `apply_fill` never
actually deducted/credited them. Real money was being left on the floor.

**Fix**: `apply_fill` now atomically:
- Computes `notional = size × price × tick_size`.
- Computes `taker_fee = notional × taker_fee_bps / 10_000`.
- Computes `maker_rebate = notional × maker_rebate_bps / 10_000`.
- Asserts `maker_rebate ≤ taker_fee` (defense against bad governance).
- Deducts taker_fee from taker collateral.
- Credits maker_rebate to maker collateral.
- Contributes `(taker_fee - maker_rebate) × fee_contribution_bps / 10_000`
  to insurance fund balance and `total_contributions`.
- Increments `market.total_fees_collected`.

`InsuranceFundAccount` is now a required mut account in `ApplyFill`.

#### 2. `cancel_order` instruction (every real exchange has this)

Orders sat in the buffer until run_batch with no way to retract.
Manifest, Phoenix, Hyperliquid, dYdX, Drift — every CLOB has a
cancel. Now we do too.

- Walks buffer slots; finds the one with `seq == arg && trader == signer`.
- Rejects synthesized orders (FLP_virtual, Liquidation, ADL).
- Sets `valid = 0`, decrements `head`.
- Emits `OrderCancelledEvent`.
- Errors: `WrongTrader`, `OutOfRange` (synth), `LiquidationStale` (not found).

Total Anchor instruction surface: **21**.

#### 3. FLP inventory_bps clamp (defense in depth)

In `flp_quoter.rs`, `inv_bps` was cast from i128 to i32. Even though the
math couldn't realistically overflow (capital > 0 guard), a future
refactor that breaks the invariant could cause sign flip on cast.

**Fix**: `raw.clamp(-(BPS_DENOM as i128), BPS_DENOM as i128) as i32` —
clamps to ±100% before the cast. Mathematically equivalent in normal
operation; refactor-safe.

### 4 new E2E tests + SDK extensions

- `cancel_order_removes_from_buffer`
- `cancel_order_rejects_other_traders_order`
- `apply_fill_settles_two_trader_positions` updated for fee accrual
- SDK gets `cancelOrderIx` builder, `OrderCancelledEvent` type
- 21st builder coverage tripwire

### Notes from the manifest research

Manifest's HyperTree (80-byte uniform graph nodes) and "global orders"
(deferred token movement) achieve essentially what our matcher core
already does via fixed-size OrderSlot + Anchor's init-if-needed PDAs.
Their core/wrapper architecture mirrors our matcher::* + lib.rs
separation.

### Total test count: 260 (162 TS sim+SDK + 31 Rust unit + 36 Rust property × 2K + 31 E2E integration).

## [0.28.0] — 2026-05-08

### Security hardening — defenses against 2025 production attack patterns

After deep research into how every major perp DEX has actually failed
in production, three concrete attack defenses are now wired into the
program:

#### 1. JELLY/POPCAT-style mark-price manipulation (Hyperliquid 2025)

**Attack pattern**: attackers manipulate thin upstream price sources
to distort the weighted index, then trigger cascading liquidations.
JELLY (March 2025) and POPCAT (November 2025) cost Hyperliquid ~$5M
each.

**Defense**: oracle staleness + confidence gates on every update.
- `MarketParams.oracle_staleness_max_seconds` — rejects updates
  whose `published_at` is more than N seconds old.
- `MarketParams.oracle_confidence_max_bps` — rejects updates where
  `confidence / price > N bps`.
- `update_oracle(price, confidence, published_at_unix_seconds)`
  signature.
- `MarketAccount.oracle_published_at_unix_seconds` field.
- New errors: `OracleTooStale (1800)`,
  `OracleConfidenceTooWide (1801)`, `OraclePausedConfidence (1802)`.

This implements Pyth's documented best practice: clamp price updates
by confidence interval, reject stale, pause when confidence widens.

#### 2. Coordinated multi-wallet position buildup (POPCAT 2025)

**Attack pattern**: distribute capital across many wallets, each
opening sub-cap positions, accumulate massive concentrated risk that
single-wallet caps don't catch.

**Defense**: per-trader per-market position size cap.
- `MarketParams.max_position_lots_per_trader` — 0 = unlimited; non-zero
  rejects orders that would push position size past the cap.
- New error: `PositionSizeCapExceeded (1209)`.
- Enforced in `place_limit_order` *before* the stress-lattice margin gate.

#### 3. ADL trilemma documentation (arxiv 2512.01112, Dec 2025)

The recent paper proves no ADL policy can simultaneously satisfy
exchange solvency, revenue, and trader fairness. Documented in
`docs/SAFETY.md` with the explicit trade-off; our design pairs
profit-ratio × leverage ranking with insurance-fund-first to minimize
ADL frequency.

### 3 new E2E integration tests

- `update_oracle_rejects_stale_price`
- `update_oracle_rejects_wide_confidence`
- `place_limit_order_rejects_above_position_cap`
  (with positive boundary case)

### Total test count: 257 (161 TS sim+SDK + 31 Rust unit + 36 Rust property × 2K + 29 E2E integration).

## [0.27.0] — 2026-05-08

### Added — Phase 1 final-final polish (test coverage gaps closed)

#### `projectPosition` tests (9 cases)

Extracted and exported `projectPosition` from `preview-trade.ts`.
Tests cover all four lifecycle paths matching the on-chain
`apply_fill_to_position`:
- empty / null current → open new position
- same side → volume-weighted average entry (tested with symmetric
  and asymmetric weights)
- opposite side, smaller fill → reduce
- opposite side, exact size → close to zero
- opposite side, larger fill → flip side
- identity preservation (trader, market) across all cases

#### Rate-limit E2E (1 test)

`place_limit_order_per_trader_rate_limit_enforced` — places 16
orders successfully, then verifies the 17th in the same batch is
rejected with `RateLimited`. Verifies buffer head is unchanged after
rejection. The 16-order-per-batch safety gate is now proven on real
Solana runtime.

#### Liquidation property tests (6 properties × 2K = 12K assertions)

`programs/flash-book/tests/proptest_liquidation.rs`:
- `detect_skips_healthy_traders` — massive collateral always healthy
- `detect_flags_zero_collateral_traders` — non-trivial position with
  zero collateral always flagged
- `detect_skips_empty_position_lists` — no positions = no liquidation
- `liq_orders_one_per_position` — one synthesized order per position
  in each candidate; correct side (opposite of held); correct type
  (`Liquidation`)
- `shortfall_or_recovery_consistent` — `recovered > 0 ∧ shortfall > 0`
  is impossible (mutually exclusive)
- `liq_orders_match_position_size` — synthesized order size = held size

### Total fuzz assertions: 72,000 (12K batch auction + 14K risk + 34K modules + 12K liquidation).
### Total test count: 254 (161 TS sim+SDK + 31 Rust unit + 36 Rust property × 2K + 26 E2E integration).

## [0.26.0] — 2026-05-08

### Added — Phase 1 polish (composable previewTrade SDK helper)

**`sdk-ts/src/preview-trade.ts`** — the canonical pre-trade UX
composer. One call fetches all relevant on-chain state, simulates
clearing with the trader's prospective order, projects the post-fill
position, runs risk preview against it.

```ts
const result = await previewTrade(client, {
  trader: alice.publicKey,
  market: solUsdcMarket,
  side: 'long',
  sizeLots: 10,
  limitTicks: 99_950,
  orderType: 'taker',
});

// result:
// {
//   intakeAllowed: true,
//   expectedFill: { sizeLots: 10, priceTicks: 99_950 },
//   priceImprovementBps: 0,
//   postTradeRisk: {
//     equity: 50_000,
//     required: 12_500,
//     isHealthy: true,
//     healthRatio: 4.0,
//     worstScenario: 'all_down_10pct',
//     ...
//   },
//   initialMarginRequired: 25_000,
// }
```

What it does:
1. Fetches market + order buffer + trader state + current position
   (4 RPC calls in parallel via `Promise.all`).
2. Checks intake gates: market status, trader exists, rate limit.
3. Builds the simulator input from the current buffer + the
   prospective order.
4. Runs `simulateBatchClearing` for the predicted fill.
5. Projects the post-fill position state (open / add / reduce / flip
   semantics matching `apply_fill_to_position`).
6. Runs `previewPortfolioRisk` against the projected portfolio.
7. Computes `priceImprovementBps` (always ≥ 0; batch auction fills at-or-better
   than limit by construction).

This is the canonical UI feature for any frontend. TradingView-style
"limit order will fill at ~X with N% slippage; resulting position
would be Y% to liquidation" — all in one async call.

### Total test count: 238 (152 TS sim+SDK + 31 Rust unit + 30 Rust property × 2K + 25 E2E integration).

## [0.25.0] — 2026-05-08

### Added — Phase 1 closure (100% instruction E2E coverage)

Two final E2E tests covering the remaining settlement paths:

- **`apply_fill_settles_two_trader_positions`** — the canonical full
  settlement test. Two traders cross, run_batch matches them, then
  `apply_fill` is invoked with the clearing price from the market's
  TWAP buffer. Verifies both Position PDAs, both TraderState
  open_positions counters, and market OI all reflect the trade
  atomically. **The complete intake → match → settle → state-write
  pipeline runs end-to-end on real Solana runtime.**
- **`apply_flp_fill_creates_taker_position_and_flp_entry`** — FLP
  settlement path. Trader buys from FLP; verifies the taker's
  Position PDA, the FLP's per-market entry on the opposite side, and
  market OI all updated. **Proves the FLP becomes a counterparty
  on-chain.**

### 100% instruction E2E coverage milestone

All 20 Anchor instructions now have E2E tests on a real Solana runtime:

```
Setup:        initialize_insurance_fund ✓ initialize_flp_exposure ✓
              initialize_market ✓ open_trader_state ✓
Lifecycle:    deposit_collateral ✓ withdraw_collateral ✓
              deposit_flp_capital ✓ withdraw_flp_capital ✓
Order intake: place_limit_order ✓ submit_commit ✓ submit_reveal ✓
Batch:        run_batch ✓ (with verified clearing price)
Settlement:   apply_fill ✓ apply_flp_fill ✓
Liquidation:  liquidate_position ✓ liquidate_portfolio ✓
Governance:   update_oracle ✓ set_market_status ✓
              update_market_params ✓ transfer_market_authority ✓
```

### Total test count: 238 (152 TS sim+SDK + 31 Rust unit + 30 Rust property × 2K + 25 E2E integration).

## [0.24.0] — 2026-05-08

### Added — Phase 1 final polish (verified batch execution + edge case E2E)

Three new E2E integration tests covering the strongest "this works"
demonstration and order-intake edge cases:

- **`two_traders_crossing_orders_clear_in_batch`** — the canonical
  end-to-end batch execution test. Two traders place crossing orders
  (Alice buys at 100,500; Bob sells at 99,500). After `run_batch`:
  - Buffer is cleared (head = 0)
  - Market batch counter advanced (current_batch = 1)
  - Recent clearing prices contains a price ∈ [99,500, 100,500]
  - Mark price within the configured 100-bps oracle band
  This proves the whole matcher pipeline runs end-to-end on real
  Solana runtime: order intake → Walrasian batch-auction clearing →
  TWAP-banded mark update.
- **`place_limit_order_below_min_lot_rejected`** — zero-size order
  rejected by the `ZeroSize` gate.
- **`place_limit_order_off_tick_rejected`** — order with limit not
  aligned to `tick_size` rejected by the `PriceNotOnTick` gate.

### Total test count: 236 (152 TS sim+SDK + 31 Rust unit + 30 Rust property × 2K + 23 E2E integration).

## [0.23.0] — 2026-05-08

### Added — Phase 1 final hardening (60,000 fuzz assertions)

`programs/flash-book/tests/proptest_modules.rs` — 17 property tests
across the remaining matcher modules. Each runs 2,000 random cases.
Total fuzz assertions across all proptests now stands at **60,000**:

- **batch-auction matcher (the batch-auction matcher proptests)** — 6 × 2K = 12K
- **Risk (proptest_risk.rs)** — 7 × 2K = 14K
- **Modules (proptest_modules.rs)** — 17 × 2K = 34K

By module:

#### `flp_quoter` (5 properties)
- `flp_bid_below_ask_always` — bid ≤ fair_value ≤ ask at every level
- `flp_vpin_widens_spread` — high-VPIN bid ≤ low-VPIN bid; high-VPIN
  ask ≥ low-VPIN ask. **Spread is monotonic in VPIN.**
- `flp_zero_capital_emits_no_orders` — protects against div-by-zero
- `flp_short_pool_lifts_fair_value` — net-short pool has skew ≥ 0
- `flp_long_pool_lowers_fair_value` — net-long pool has skew ≤ 0

#### `funding` (3 properties)
- `funding_rate_sign_matches_premium_sign` — mark > oracle ⇒ rate ≥ 0;
  mark < oracle ⇒ rate ≤ 0; mark = oracle ⇒ rate = 0
- `funding_zero_delta_no_change` — Δt = 0 leaves cum_index unchanged
- `funding_rate_bounded_by_max` — clamping holds for any input

#### `vpin` (3 properties)
- `vpin_bounded` — `as_bps()` always in [0, 10_000]
- `vpin_one_sided_flow_high` — pure long flow → VPIN > 50% bps
- `vpin_no_fills_zero` — empty stream stays at 0

#### `insurance fund` (3 properties)
- `insurance_cover_conserves_value` — covered + remaining = shortfall
  always; balance after = balance before − covered
- `insurance_contributions_sum` — three-stream contributions add up
  exactly to balance + total_contributions
- `insurance_pause_threshold_monotonic` — adding to balance can only
  flip false→true on the pause gate (never true→false)

#### `commit_reveal` (3 properties)
- `commit_reveal_roundtrip` — committed payload → identical revealed Order
- `commit_reveal_tamper_rejected` — any tampered field fails the hash
- `commit_sweep_returns_bond` — expired commits release their bond

### Total test count: 233 (152 TS sim+SDK + 31 Rust unit + 30 Rust property × 2K cases + 20 E2E integration).
### Total fuzz assertions: **~60,000**.

## [0.22.0] — 2026-05-08

### Added — Phase 1 polish (matcher::risk property tests + integer-math findings)

`programs/flash-book/tests/proptest_risk.rs` — 7 property tests for
the stress-lattice maintenance margin engine, each running 2,000
random cases (= 14,000 fuzz assertions on top of the 12K from the
batch-auction matcher):

1. **`required_margin_non_negative`** — for any portfolio shape.
2. **`monotonic_in_collateral`** — more collateral never makes you
   less healthy. Asserts `equity_diff == delta` (exact linearity)
   AND `a.healthy ⇒ b.healthy` (healthy is preserved by extra
   collateral).
3. **`hedge_at_fair_value_reduces_required_margin`** — at mark = entry,
   long+short same market always reduces required margin.
4. **`is_healthy_consistent_with_equity_required`** — `is_healthy ↔
   equity ≥ required` (definitional invariant).
5. **`empty_portfolio_zero_required`** — zero positions ⇒ required = 0
   ⇒ healthy ⇒ equity = collateral.
6. **`worst_scenario_idx_within_bounds`** — index always valid.
7. **`approximately_linear_scaling_in_position_size`** — doubling all
   sizes ≈ doubles required margin (with tolerance for integer
   truncation in per-scenario arithmetic).

### Real findings from the property tests

The proptest run found two **mathematical edge cases** worth documenting:

1. **Hedge does NOT always reduce required margin** — only at fair
   value (mark ≈ entry). When a long is deep in the money, adding
   a hedge "trades" the favorable directional component for double
   maintenance margin overhead. This is correct behavior; the
   property test now constrains mark = entry to capture the clean
   case.

2. **Linear scaling has integer-truncation drift** — doubling size
   doesn't always exactly double required margin because the
   maintenance margin term truncates differently across scenarios
   at size N vs 2N. Drift is bounded by ~26 (1 unit per scenario
   per term × 13 scenarios × 2 terms). Property test asserts
   approximate linearity with a generous tolerance.

These are not bugs — they're correct behaviors of integer arithmetic
on a stress lattice. But they're surprising, and the property tests
now document them for any future engineer reading the code.

### Total fuzz assertions: 26,000 (12K batch auction + 14K risk).
### Total test count: 216 (152 TS sim+SDK + 31 Rust unit + 13 Rust property × 2K cases + 20 E2E integration).

## [0.21.0] — 2026-05-08

### Security audit pass — zero panic paths in production code

Audited the Rust program for all `.unwrap()`, `.expect()`, `panic!`,
`unreachable!`, `unimplemented!`, `todo!` paths in non-test code.

Findings:
- Two `.unwrap()` calls in the batch-auction matcher module tie-break code, both
  guarded by a length check earlier in the function. Replaced with
  `.copied().unwrap_or(prior_mark)` for defense in depth — even a
  future refactor that breaks the length invariant cannot introduce
  a panic path.
- All array indexing paths audited and verified bounded:
  - `market.recent_clearing_prices[idx]` — `idx = batch_num % 16` on
    a fixed-size 16 array.
  - `remaining[i]` in `liquidate_portfolio` — guarded by
    `i + 1 < remaining.len()` loop condition.
  - `prices[i]` / `prices[i+1]` in `realized_vol_bps_from_window` —
    guarded by `for i in 0..(n - 1)` where `n ≤ prices.len()`.
  - `flp.per_market[idx]` — `idx` derived from `position()` /
    `ok_or_else(BufferFull)` so always valid.

**Production code now has ZERO panic paths.** Every error condition
returns a typed `FlashBookError`. The matcher will fail gracefully
under any input — verified by the property tests (12K random cases)
and the integration tests (20 E2E paths).

## [0.20.0] — 2026-05-08

### Added — Phase 1 polish (SDK batch auction simulator)

**`sdk-ts/src/order-simulator.ts`** — pure-TypeScript port of the
on-chain Walrasian clearing algorithm.

- `simulateBatchClearing(orders, priorMarkTicks)` returns
  `{ clearingPriceTicks, clearingVolumeLots, fills[] }` — the same
  output shape as `clear_batch` in
  `programs/flash-book/src/the batch-auction matcher module`.
- `fillForOrder(result, orderId)` — aggregate size + price filled for
  a specific order in the result.
- Mirrors all on-chain semantics: priority order
  (`liquidation > adl > taker > flp_virtual > limit`), FIFO within
  priority class, self-trade prevention, tie-breaking by proximity to
  prior mark.

Pairs with `previewPortfolioRisk` for full pre-trade analysis:
- Fetch the order buffer + market state + your position.
- Add your prospective order.
- Simulate the batch → get expected fill price + volume.
- Project new position, run risk preview against post-fill portfolio.
- Decide: submit, adjust limit, or back off.

This is the canonical UI feature for any frontend (TradingView-style
"limit order will fill at ~X with Y% slippage and result in N% to
liquidation"). It was previously impossible without re-running the
on-chain matcher in simulation.

11 tests covering: empty batch, non-crossing, crossing, ties, priority,
FIFO, self-trade, MEV-neutrality (permutation), volume maximization,
fill aggregation.

### Total test count: 209 (152 TS sim+SDK + 31 Rust unit + 6 Rust property × 2K + 20 E2E integration).

## [0.19.0] — 2026-05-08

### Added — Phase 1 polish (multi-market E2E coverage)

Two new E2E integration tests exercising the multi-market path:

- **`liquidate_portfolio_with_two_markets_and_no_positions`** —
  initializes two markets with different oracle prices, opens a
  trader, places a tiny order on market 1 (verifies init_if_needed
  Position PDA across markets), then attempts a portfolio liquidation
  — confirming the multi-market remaining_accounts walk + validation
  paths work end-to-end.
- **`second_market_initializes_at_different_oracle_price`** — verifies
  two markets coexist with distinct mints + different oracle prices,
  while sharing the same global insurance fund + FLP exposure +
  authority. Proves the initialize_market PDA seed scheme correctly
  isolates per-market state.

New helper `setup_additional_market` lets tests initialize a second
market on an already-initialized protocol without re-running protocol
setup.

The matcher's hedge-recognition property itself is exhaustively
covered by the Rust matcher unit tests
(`risk_hedged_position_collapses_required_margin`) and the SDK's
`risk-preview.test.ts` hedge tests. The on-chain E2E tests verify the
multi-market account-walking + validation paths that wire those
properties to actual program execution.

### Total test count: 198 (141 TS sim+SDK + 31 Rust unit + 6 Rust property × 2K + 20 E2E integration).

## [0.18.0] — 2026-05-08

### Added — Phase 1 closure (cross-market portfolio liquidation)

The last remaining feature item: **`liquidate_portfolio`** — a 20th
Anchor instruction that walks the trader's positions across multiple
markets via `remaining_accounts` and runs the matcher's `assess_margin`
against the joint cross-market stress lattice.

How it works:
- Named accounts: `caller`, `execution_market`, `execution_order_buffer`,
  `trader_state`, `execution_position`.
- `remaining_accounts` = alternating pairs of
  `[other_market, other_position, …]`.
- Each pair is verified: both accounts owned by this program;
  `position.trader == trader_state.trader`; `position.market == market.key()`.
- Cross-market scenario lattice generated dynamically over the union
  of all markets seen.
- `assess_margin` runs with the trader's collateral as the equity input.
- If `is_healthy = true` → reject with `NotLiquidatable`. Otherwise inject
  a `Liquidation`-priority order on the execution market (same shape
  as `liquidate_position`).
- Emits `LiquidationInjectedEvent`.

**Cross-margin recognition**: a long+short pair on different but
correlated markets cancels in correlated stress scenarios, sharply
reducing required margin. The on-chain algorithm is identical to the
SDK's off-chain `previewPortfolioRisk` — they MUST agree (and do, via
the same Rust matcher core compiled into both paths).

SDK extended:
- `liquidatePortfolioIx({ caller, executionMarket, trader, crossMargin? })`
  builder. `crossMargin` is `Array<{ market }>`; for each entry, the
  builder appends two readonly accounts to `remainingAccounts`
  (Market PDA + derived Position PDA).
- 2 new SDK builder tests: zero-cross-margin (5 keys), 2-market
  cross-margin (9 keys).
- Coverage tripwire updated to **20** expected instruction builders.

E2E test:
- `liquidate_portfolio_rejects_healthy_trader_zero_remaining` —
  degenerate case: empty position rejected with `LiquidationStale`,
  matching `liquidate_position` semantics.

IDL regenerated: 3,510 lines (was 3,356).

### Total test count: 195 (141 TS sim+SDK + 31 Rust unit + 6 Rust property × 2K + 18 E2E integration).
### Total Anchor instruction surface: **20**.

## [0.17.0] — 2026-05-08

### Added — Phase 1 continued (SDK risk preview + event subscription)

**`sdk-ts/src/risk-preview.ts`** — off-chain port of the on-chain
stress-lattice margin assessment.

- `previewPortfolioRisk(positions, markets, collateral)` — predicts
  liquidation BEFORE submitting. Returns `{ collateral, unrealizedPnl,
  equity, required, isHealthy, worstScenario, healthRatio }`. The
  `healthRatio` field gives a clean "% to liquidation" indicator for UIs.
- `defaultScenarios(markets)` — generates the same 13-scenario lattice
  as `default_scenarios()` in `programs/flash-book/src/matcher/risk.rs`
  (flat + 8 single-market shocks + 2 correlated + 2 black-swan).
- `initialMarginRequired(size, price, market)` — pre-trade IM check.
- 12 tests including hedge-recognition and corner cases.

This unlocks three downstream consumers:
- **Liquidation bots** can decide "is this trader actually liquidatable"
  client-side before paying for a tx that the matcher would reject.
- **UIs** can render "you're at X% to liquidation" in real time.
- **Backtests** can replay historical state without re-running the
  full Anchor program.

**`sdk-ts/src/event-stream.ts`** — `subscribeToProgramEvents(connection,
callback)` wraps `logsSubscribe` + `decodeEventsFromLogs` so callers
get typed `FlashBookEvent`s with slot + signature, no log parsing.

### Total test count: 193 (139 TS sim+SDK + 31 Rust unit + 6 Rust property × 2K + 17 E2E integration).

## [0.16.0] — 2026-05-08

### Added — Phase 1 continued (SDK becomes a full read+write client)

The SDK was previously write-only — only instruction builders. Now it's
a complete client with typed account readers + event decoders.

**`sdk-ts/src/accounts.ts`** — typed fetchers + decoders for all 7
on-chain account types:

- `fetchMarket`, `fetchOrderBuffer`, `fetchCommitBuffer`,
  `fetchInsuranceFund`, `fetchFlpExposure`, `fetchTraderState`,
  `fetchPosition` — each returns `T | null` via Anchor's
  `fetchNullable`.
- `decodeAccount<T>(name, data)` — for raw buffers from
  `getProgramAccounts` / RPC subscriptions / snapshots. Backed by
  `BorshAccountsCoder`.
- Full TypeScript shape definitions mirroring `programs/flash-book/src/state.rs`:
  `MarketAccount`, `OrderBufferAccount`, `CommitBufferAccount`,
  `InsuranceFundAccount`, `FlpExposureAccount`, `TraderStateAccount`,
  `PositionAccount`, plus nested types `OrderSlot`, `CommitRow`,
  `FlpMarketExposure`, `VpinState`, `MarketParamsAccount`.

**`sdk-ts/src/event-decoder.ts`** — event log decoder using
`BorshEventCoder`:

- `decodeEventsFromLogs(logs)` — pass a transaction's `logMessages`,
  get a typed array of `FlashBookEvent`s in emission order.
- `decodeOne(line)` — single-line decoder; returns `null` for
  non-program-data lines or malformed payloads.
- `EventSubscription` / `EventStreamCallback` types for the
  `logsSubscribe` integration path.

This is the canonical path for indexers, off-chain monitors, the
risk engine bot, and the front-end — anyone who needs to read program
state or react to events.

**8 new SDK tests** covering decoder error paths.

### Total test count: 181 (127 TS sim+SDK + 31 Rust unit + 6 Rust property × 2K + 17 E2E integration).

## [0.15.0] — 2026-05-08

### Added — Phase 1 continued (E2E coverage at 17 tests)

Six more E2E integration tests covering FLP capital, commit/reveal, and
authority management:

- **`deposit_flp_capital_grows_pool`** — verifies LP capital injection
  increments `total_capital_quote_lots`.
- **`withdraw_flp_capital_blocked_with_open_positions`** — verifies the
  empty-pool path succeeds (open-positions block tested separately).
- **`submit_commit_and_reveal_full_flow`** — end-to-end: build keccak
  hash on the client side matching the program's exact rule, commit,
  verify the slot is active, reveal, verify the slot is consumed and
  a Taker-priority order materializes in the buffer with the right
  side / size / limit / trader.
- **`submit_reveal_with_wrong_payload_fails`** — proves
  `CommitMismatch` fires when the reveal's `size_lots` is tampered
  vs the committed hash. The MEV-resistance property is enforced
  on-chain.
- **`update_oracle_authority_only`** — proves only the market
  authority can write oracle. Random caller is rejected with
  `Unauthorized`. Verifies oracle state unchanged after the
  rejected attempt.
- **`transfer_market_authority_rotates_keys`** — verifies authority
  swap, then proves the OLD authority is revoked (old key can't
  update oracle anymore).

### Total test count: 173 (119 TS sim+SDK + 31 Rust unit + 6 Rust property × 2K + 17 E2E integration).

## [0.14.0] — 2026-05-08

### Added — Phase 1 continued (E2E coverage doubled to 11 tests)

Six additional E2E integration tests covering the harder paths:

- **`initialize_market_writes_state`** — verifies all market header
  fields (oracle, mark, params, status, OI counters) plus that the
  freshly-init OrderBuffer is empty.
- **`place_limit_order_lands_in_buffer`** — full chain: init protocol
  → init market → open trader → deposit → place_limit_order. Verifies
  the order materializes in slot 0 of OrderBuffer with the right
  side/size/limit, and TraderState's per-batch counter increments.
- **`run_batch_advances_counter_and_clears_buffer`** — places an order,
  runs a batch, verifies `current_batch` advances and the buffer is
  cleared (all slots back to `valid = 0`).
- **`set_market_status_blocks_orders_when_paused`** — proves the
  circuit breaker actually fires: paused market rejects
  place_limit_order with an error.
- **`update_market_params_rejects_immutable_primitive_change`** —
  proves the immutability invariant on `tick_size`. Mutable changes
  (taker_fee_bps) succeed.
- **`liquidate_position_rejects_healthy_trader`** — proves the
  matcher's `assess_margin` gate fires; can't force-close someone
  who's not actually unhealthy.

### Bug fix: `CommitBufferAccount` size

The 256-slot CommitBufferAccount was 22 KB, exceeding Solana's 10 KiB
single-call init limit. The program could never have been deployed
end-to-end. Reduced to 64 slots (matches OrderBuffer cap; ~5.7 KB).
New constant: `COMMIT_BUFFER_CAP = 64`. Future expansion via Anchor's
zero-copy + multi-step realloc.

### Total test count: 167 (119 TS sim+SDK + 31 Rust unit + 6 Rust property × 2K + 11 E2E integration).

## [0.13.0] — 2026-05-08

### Added — Phase 1 continued (E2E integration tests + builder coverage)

**🎉 Integration tests via solana-program-test now work.**

Bridged Anchor 0.31's `entry` (which uses `'info` for both the slice
ref and `AccountInfo` items) to solana-program-test's HRTB
`BuiltinFunctionWithContext` via a documented, sound unsafe-transmute
wrapper:

```rust
fn anchor_entry_wrapper<'a, 'b, 'c, 'd>(
    program_id: &'a Pubkey,
    accounts: &'b [AccountInfo<'c>],
    instruction_data: &'d [u8],
) -> Result<(), ProgramError> {
    let accounts_unified: &'c [AccountInfo<'c>] =
        unsafe { std::mem::transmute(accounts) };
    flash_book::entry(program_id, accounts_unified, instruction_data)
}
```

The transmute changes only the type-level lifetime parameter; runtime
layout is identical. Sound because solana-program-test owns the
AccountInfo Vec for the duration of the instruction call.

**5 E2E integration tests** (in `programs/flash-book/tests/integration.rs`),
each running the program through a real Solana runtime:

- `initialize_insurance_fund_writes_state`
- `initialize_flp_exposure_writes_state_and_empty_slots` —
  verifies all 16 slots come up empty (`side = 255`)
- `open_trader_state_initializes_zero_balance`
- `deposit_collateral_credits_balance_and_emits_event` — verifies
  successive deposits accumulate
- `withdraw_collateral_reduces_balance`

**SDK builder coverage**: 20 new tests in
`sdk-ts/tests/builders.test.ts`, asserting every one of the 19
instruction builders produces a valid TransactionInstruction with
correct programId, non-empty data, and the expected account count.
A coverage tripwire test catches when the program adds a new
instruction without updating the SDK.

### Total test count: 161 (71 TS sim + 48 TS SDK + 31 Rust unit + 6 Rust property × 2K cases + 5 E2E integration).

## [0.12.0] — 2026-05-08

### Added — Phase 1 continued (FLP capital lifecycle + lifecycle demo)

**Three new authority-gated FLP instructions, fixing a real init gap.**

The `FlpExposureAccount` PDA was referenced in `initialize_market`'s
context as a required existing account, but no instruction existed to
create it — the program could not actually be deployed end-to-end.

- **`initialize_flp_exposure(initial_capital_quote_lots)`** — creates
  the global FLP exposure PDA. Initializes all 16 per-market slots to
  empty (`side = 255`). Emits `FlpExposureInitializedEvent`.
- **`deposit_flp_capital(amount)`** — LP adds capital. Authority-gated.
  Emits `FlpCapitalUpdatedEvent` with delta.
- **`withdraw_flp_capital(amount)`** — LP removes capital. Blocked
  while pool has any open positions (`markets_count > 0`). Phase 2
  version takes `remaining_accounts` to verify against actual mark
  prices. Emits `FlpCapitalUpdatedEvent`.

**SDK extended.**
- `initializeFlpExposureIx`, `depositFlpCapitalIx`,
  `withdrawFlpCapitalIx` builders.
- `FlpExposureInitializedEvent`, `FlpCapitalUpdatedEvent` types.

**Standalone lifecycle demo** at `sdk-ts/examples/full-lifecycle.ts`.

Builds and prints all 9 instructions for the full single-market
lifecycle (insurance fund init, FLP exposure init, market init,
trader onboarding, deposit, place limit, run batch, apply fill,
governance status change). Total: 290 bytes across 9 instructions, 44
account references. Runnable with `bun run examples/full-lifecycle.ts`
without any external dependencies — proves the SDK constructs
syntactically valid transactions against the IDL with reproducible
PDA seeds. Includes a (commented) send block for executing against a
deployed program.

IDL regenerated: 3,356 lines (was 3,129).
Total Anchor instruction surface: **19**.

## [0.11.0] — 2026-05-08

### Added — Phase 1 continued (documentation pass)

- **`docs/INSTRUCTIONS.md`** — full reference for the 16 Anchor
  instructions. Each entry lists accounts (mode + seed), arguments,
  gates, errors, and emitted events. The IDL remains the canonical
  machine-readable source; this is its human-readable companion.
- **`docs/SAFETY.md` updated** — invariants table now lists 14
  solvency invariants, each annotated with where it's enforced (TS sim
  vs Rust program). Audit checklist split into "already in code" (12
  items, checked) and "pending for production audit" (10 items).
- **`README.md`** — points to `INSTRUCTIONS.md` in the docs index.

### Investigated and parked — `solana-program-test` integration tests

Attempted integration tests via `solana-program-test` 2.1 hit an upstream
lifetime-signature mismatch: Anchor 0.31's `entry` function uses `'info`
for both the `&[AccountInfo<'info>]` slice ref and the `AccountInfo<'info>`
items, while `solana-program-test`'s `BuiltinFunctionWithContext` HRTB
expects two independent lifetimes. No safe (non-`unsafe`-transmute)
workaround in current versions. Tracked as a known gap; revisit when
Anchor releases a compat shim or BPF build is unblocked.

## [0.10.0] — 2026-05-08

### Added — Phase 1 continued (governance + circuit breaker)

Three new authority-gated instructions completing the operational surface:

- **`set_market_status(new_status)`** — circuit breaker. Status enum:
  Inactive(0) / Active(1) / PostOnly(2) / Paused(3) / Closed(4).
  Closed is terminal; cannot be reopened. Emits `MarketStatusChangedEvent`.
- **`update_market_params(new_params)`** — governance parameter tuning.
  Enforces immutability of measurement primitives (`tick_size`,
  `base_lot_size`, `quote_lot_size`, `min_base_lots` — changing these
  would silently invalidate every existing order and position). All
  other fields (fees, margins, FLP coefficients, funding rates, oracle
  band, VPIN, batch interval) can be retuned. Sanity bounds enforced
  on the mutable fields. Emits `MarketParamsUpdatedEvent`.
- **`transfer_market_authority(new_authority)`** — for safe key
  rotation. Emits `MarketAuthorityTransferredEvent`.

Wire-up:
- `place_limit_order` now gates by status: blocked unless market is
  Active or PostOnly. (Liquidation, fills, and run_batch remain
  available in any status so positions can be closed during pause.)

SDK extended:
- `setMarketStatusIx`, `updateMarketParamsIx`,
  `transferMarketAuthorityIx`, `updateOracleIx` builders.
- `MarketStatus` enum re-exported.
- Three new event type definitions.

IDL regenerated: 3,129 lines (was 2,848).

The Anchor program now exposes 16 instructions covering the full
production lifecycle: setup, lifecycle, order intake, batch execution,
settlement, liquidation, and governance.

## [0.9.0] — 2026-05-08

### Added — Phase 1 continued (FLP fill bookkeeping + stress-lattice gate)

**`apply_flp_fill` instruction.**
- The on-chain settlement path for fills where the FLP pool is the maker.
- Mutates `FlpExposureAccount.per_market` for this market via the new
  `apply_fill_to_flp_market()` helper, mirroring `apply_fill_to_position`
  semantics on a `FlpMarketExposure` slot (open / add / reduce / flip
  with realized PnL accumulated globally on `flp.realized_pnl`).
- Updates the taker's `PositionAccount` on the opposite side via the
  existing `apply_fill_to_position()` helper.
- Updates market OI counters for both sides via `update_oi`.
- Updates `TraderState.open_positions` on transitions.
- New `FlpFillAppliedEvent` carries the FLP's post-fill side+size for
  off-chain monitors.

**Stress-lattice margin gate on order intake.**
- `place_limit_order` now requires the caller's `PositionAccount` PDA
  (init-if-needed). If the trader has an existing non-empty position on
  this market, the matcher's `assess_margin` runs against the default
  stress lattice. If the position is unhealthy, the order is rejected
  with `TraderLiquidatable` (1400) — preventing unhealthy traders from
  digging deeper.
- Empty positions (first-ever order on a market) skip the gate
  trivially.
- Cost: one rent-exempt account creation on first order per
  (trader, market) pair; subsequent orders pay zero rent (Anchor's
  `init_if_needed` is a no-op when already initialized).

**SDK extended.**
- `applyFlpFillIx` builder.
- `FlpFillAppliedEvent` type definition.
- `placeLimitOrderIx` updated to pass the position PDA.
- IDL regenerated (2,848 lines, was 2,595).

## [0.8.0] — 2026-05-08

### Added — Phase 1 continued (liquidation entry point + SDK tests)

**`liquidate_position` instruction.**
- Permissionless: any caller can trigger liquidation against any position.
- Validates the trader is *actually* unhealthy via the matcher's
  `assess_margin` against the configured stress lattice; healthy traders
  cannot be force-closed (returns `NotLiquidatable` error).
- On success: synthesizes a `Liquidation`-priority order on the opposite
  side at oracle ± liq_penalty and appends to the order buffer. The next
  `run_batch` clears it at the batch uniform price; `apply_fill` then
  settles the position.
- Single-market scope: assesses against this market's position only.
  Cross-market portfolio liquidation is a Phase 2 instruction.
- New `LiquidationInjectedEvent` for off-chain consumers.
- New error code `NotLiquidatable = 1403` to gate against unjustified
  liquidation calls.

**SDK extensions.**
- `liquidatePositionIx` builder added to `FlashBookClient`.
- `LiquidationInjectedEvent` type definition.
- `NotLiquidatable` error code.
- IDL regenerated (2,595 lines).

**SDK tests (28 cases across 3 files).**
- `tests/pdas.test.ts` — PDA derivation determinism + uniqueness across
  10 cases (market deterministic, market differs on swap, order/commit
  buffers derive from market, insurance + flp are global, trader_state
  per-trader, position per-(market, trader), all bumps in valid range).
- `tests/errors.test.ts` — error code family classification + name
  lookup across all 8 families.
- `tests/params.test.ts` — sanity-checks default parameters
  (fee structure, margin tiers, FLP coefficients ≥ 0, batch interval
  fits ER block range, contribution rates ≤ 10000 bps).

### Total test count: 136 (71 TS sim + 31 Rust unit + 6 Rust property × 2K cases + 28 TS SDK).

## [0.7.0] — 2026-05-08

### Added — Phase 1 continued (TypeScript SDK)

New package `@flash-book/sdk` at `sdk-ts/`. Wraps the Anchor program
for downstream TypeScript clients (e.g. `flash-mobile`).

- **PDA derivation helpers**: `marketPda`, `orderBufferPda`,
  `commitBufferPda`, `insuranceFundPda`, `flpExposurePda`,
  `traderStatePda`, `positionPda`.
- **Typed parameters**: `MarketParamsRaw`, `InsuranceFundInitParams`,
  with `defaultMajorMarketParams()` / `defaultInsuranceFundParams()`
  calibrated to the Rust program's defaults.
- **`FlashBookClient`** — Anchor `Program<Idl>` wrapper with one
  `*Ix()` builder per program instruction (10 builders covering the
  full instruction surface: initializeInsuranceFund, initializeMarket,
  openTraderState, depositCollateral, withdrawCollateral,
  placeLimitOrder, submitCommit, submitReveal, runBatch, applyFill).
- **Event type definitions** matching Anchor `#[event]` shapes
  (BatchCleared, FillApplied, MarketInitialized, etc).
- **`FlashBookErrorCode` enum** + `errorFamily()` / `errorName()`
  helpers for client-side error classification.
- IDL bundled at `sdk-ts/idl.json` (sourced from `idl/flash_book.json`).

Strict TypeScript with `exactOptionalPropertyTypes`, `verbatimModuleSyntax`.

## [0.6.0] — 2026-05-08

### Added — Phase 1 continued (audit pass + IDL + advanced market making)

**Realized-volatility coefficient in FLP quoter (Avellaneda-Stoikov parity).**
- New `flp_spread_delta_bps` parameter in `MarketParams` and `FlpQuoterParams`.
- New `realized_vol_bps` input field to the FLP quoter.
- New `realized_vol_bps_from_window()` helper computes std-dev of returns
  in pure integer arithmetic over the recent clearing-price window
  (uses `u128::isqrt`, no floats).
- Spread function now: `s = base + α·VPIN + β·u + γ·|oi_imb| + κ·Q/D + δ·σ`
  — full parity with the TS reference implementation.

**Wired previously-dead state into real semantics.**
- `place_limit_order` now enforces per-trader per-batch rate limit
  (`MAX_ORDERS_PER_TRADER_PER_BATCH = 16`) using `TraderState.last_batch_seen`
  and `orders_this_batch` counters that were previously initialized but unused.
- `apply_fill` now updates Open Interest counters via new `update_oi()` helper
  with full pre→post transition handling (no double-counting on flips).
- `apply_fill` now updates `TraderState.open_positions` on
  open/close transitions, gating `withdraw_collateral` correctly.
- `run_batch` now reads real signed FLP exposure for the market from
  `FlpExposureAccount.per_market` (replaces the previous hardcoded `0`).

**Removed dead code.**
- Removed `delegate_market` / `undelegate_market` instruction stubs that
  returned errors. Replaced with a comment documenting that the integration
  is purely additive — no misleading instruction surface.
- Removed associated `DelegateMarket` / `UndelegateMarket` Account contexts.

**Cleanup.**
- Replaced magic-number `1_000_000` FLP seq base with the explicit
  `FLP_SEQ_RESERVED_OFFSET = 1 << 56` constant; user orders strictly below,
  FLP virtual orders strictly above.
- Place_limit_order rejects user orders that would collide with the FLP
  reserved range.
- Clippy clean (zero non-upstream warnings).

**IDL** — `idl/flash_book.json` (2,389 lines) committed; downstream TS
clients can now consume the Anchor instruction surface.

### Test status: 108 (71 TS + 31 Rust unit + 6 Rust property × 2K cases). All green.

## [0.5.0] — 2026-05-08

### Added — Phase 1 continued (lifecycle instructions)

- `deposit_collateral` — credit a trader's quote-lot balance.
- `withdraw_collateral` — debit, blocked while trader has open positions.
- `apply_fill` — applies a Fill against the taker's and maker's `PositionAccount`
  PDAs (init-if-needed), with full position lifecycle:
    * empty → open (side, size, entry = price)
    * same-side → volume-weighted average entry
    * opposite-side, ≤ existing → reduce, realize PnL on closed portion
    * opposite-side, > existing → flip side, realize PnL on full close,
      remaining size opens at fill price
- New events: `CollateralDepositedEvent`, `CollateralWithdrawnEvent`,
  `FillAppliedEvent`.
- `init-if-needed` feature enabled on anchor-lang for Position PDAs that
  are created on first fill.

### Known issue

- `cargo build-sbf` (BPF target) currently fails on Solana platform-tools
  v1.48 (rustc 1.84) due to a transitive `constant_time_eq` dep that
  requires edition2024. This is an upstream toolchain alignment issue —
  newer platform-tools releases will resolve it. Native `cargo check` /
  `cargo test` are unaffected.

## [0.4.0] — 2026-05-08

### Added — Phase 1 continued (Anchor instruction handlers wired)

- `OrderBufferAccount` — per-market 64-slot pending order buffer.
- `TraderStateAccount` — per-trader collateral, toxicity score, rate-limit
  counter.
- Full instruction handlers in `lib.rs`:
  - `initialize_market` — creates Market, OrderBuffer, CommitBuffer PDAs
    with full constraints (seeds: `["market", base_mint, quote_mint]`).
  - `initialize_insurance_fund` — creates the global InsuranceFund PDA.
  - `open_trader_state` — creates a per-trader state PDA.
  - `update_oracle` — authority-gated oracle price write.
  - `place_limit_order` — validates size/price/lot/tick, finds first empty
    slot, writes with monotonic seq counter.
  - `submit_commit` — registers a hash in the per-market commit buffer.
  - `submit_reveal` — verifies hash, synthesizes a taker order in the
    next batch's buffer.
  - `run_batch` — the heart: advances funding index, generates FLP
    virtual quotes, runs Walrasian batch-auction clearing, updates mark via
    TWAP-with-oracle-band, updates VPIN per fill, sweeps expired commits,
    clears buffer, emits `BatchClearedEvent`.
- Anchor events: `MarketInitializedEvent`, `BatchClearedEvent`.
- Numbered error codes consistently propagated through `require!` /
  `require_keys_eq!` / `error!()` macros.
- All instruction handlers use `Box<Account<>>` for large accounts to
  keep stack pressure low.

## [0.3.0] — 2026-05-08

### Added — Phase 1 continued

- `matcher::risk` — stress-lattice maintenance margin in Rust, with
  `default_scenarios()` generator (per-market ±2/5/10/20%, correlated ±10%,
  black-swan ±30%).
- `matcher::liquidation` — `detect_liquidations()`, `generate_liquidation_orders()`,
  and `compute_shortfall()` for the in-loop liquidation pipeline.
- `matcher::insurance` — `InsuranceFund` type with three-stream contribution
  methods + `cover_shortfall()` waterfall + `new_positions_allowed()` gate.
- `matcher::commit_reveal` — Solana-keccak-based hash protocol with
  `register_commit()` / `redeem_reveal()` / `sweep_expired()`, operating
  over the on-chain `CommitRow` array stored in `CommitBufferAccount`.
- 15 additional Rust unit tests covering all four new modules.
- 6 property-based tests via `proptest` (2,000 cases each = **12,000 fuzz
  assertions** on matcher safety):
  - MEV-neutrality under input permutation
  - Volume conservation
  - Self-trade prevention in fills
  - Eligibility (limit price respects clearing price)
  - Fills never exceed input order size
  - Matcher never panics on any input

### Total test count: 108 (71 TypeScript + 31 Rust unit + 6 Rust property × 2K cases).

## [0.2.0] — 2026-05-08

### Added — Phase 1 begin (Rust on-chain matcher core)

- Cargo workspace with `programs/flash-book` Anchor crate.
- Pure-Rust matcher core (no Solana runtime dep), tested standalone:
  - `matcher::lot` — type-safe `BaseLots` / `QuoteLots` / `Ticks` / `Bps` newtypes
    with checked arithmetic.
  - `matcher::order` — `Order` / `OrderType` / `Side` with FIFO priority keys.
  - the batch-auction matcher module — Walrasian uniform-price clearing in integer space, with
    self-trade prevention and within-batch MEV-neutrality property test.
  - `matcher::flp_quoter` — virtual FLP quote ladder using bps integer math.
  - `matcher::funding` — Q64.64 cumulative funding index with rate clamping.
  - `matcher::vpin` — Q32.32 fixed-point VPIN calculator.
- `state` module with on-chain account types (Market, Position, InsuranceFund,
  FlpExposure, CommitBuffer).
- `errors` module with numbered error families (1000–1799).
- Anchor program skeleton in `lib.rs` with seven instruction shells:
  `initialize_market`, `place_limit_order`, `submit_commit`, `submit_reveal`,
  `run_batch`, `delegate_market`, `undelegate_market`.
- 16 Rust unit tests covering batch auction, FLP quoter, funding, VPIN.
- All financial arithmetic uses checked u128 / i128 with overflow propagation
  via `OrOverflow` trait — zero integer wraparound paths.

### Total test count: 87 (71 TypeScript + 16 Rust).

## [0.1.0] — 2026-05-08

### Added
- Initial reference design + simulator.
- batch-auction matcher with Walrasian uniform-price clearing.
- Virtual FLP quoter — Avellaneda-Stoikov-grade inventory-aware quoting,
  VPIN-driven adverse-selection widening, depth amortization, realized-vol
  spread term.
- Continuous funding via cumulative index (per-block accrual, eliminates
  funding sniping).
- Stress-lattice cross-margin (single-asset shocks ±2/5/10/20%, correlated
  shocks, black-swan ±30%; recognizes hedges).
- In-loop liquidation engine: detection from prior-batch mark, order
  injection into current batch, deterministic clearing.
- Insurance fund with three contribution streams (fees / toxicity tax /
  liq penalty) and bankruptcy waterfall.
- ADL (auto-deleveraging) by profit/leverage rank when insurance is
  exhausted.
- Commit-reveal taker protocol with bond + expiry sweep.
- VPIN volume-synchronized toxicity calculator with EMA over buckets.
- Synthetic flow simulator demonstrating end-to-end behaviour at
  ~42 K batches/sec wall-clock on Apple Silicon.
- 71 unit tests across all modules.
- Architecture, math, safety, comparison, roadmap docs.
