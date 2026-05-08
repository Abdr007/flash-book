# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
