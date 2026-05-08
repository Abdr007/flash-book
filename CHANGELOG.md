# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
    virtual quotes, runs FBA Walrasian clearing, updates mark via
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
  - `matcher::fba` — Walrasian uniform-price clearing in integer space, with
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
- 16 Rust unit tests covering FBA, FLP quoter, funding, VPIN.
- All financial arithmetic uses checked u128 / i128 with overflow propagation
  via `OrOverflow` trait — zero integer wraparound paths.

### Total test count: 87 (71 TypeScript + 16 Rust).

## [0.1.0] — 2026-05-08

### Added
- Initial reference design + simulator.
- FBA matcher with Walrasian uniform-price clearing.
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
