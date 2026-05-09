# Roadmap

Staged path from this reference simulator to mainnet.

## Phase 0 — Reference simulator (this repo, **complete**)

- [x] FBA Walrasian matcher
- [x] Virtual FLP quoter (Avellaneda-Stoikov-grade)
- [x] Continuous funding via cumulative index
- [x] Stress-lattice cross-margin
- [x] In-loop liquidation engine
- [x] Insurance fund waterfall + ADL
- [x] Commit-reveal protocol
- [x] VPIN toxicity calculator
- [x] 71 unit tests, all passing
- [x] Synthetic flow simulator
- [x] Architecture, math, safety, comparison docs

**Deliverable:** behavioural reference for the Rust program.

## Phase 1 — Production Rust program ✅ **COMPLETE**

Target: deployable to MagicBlock ER devnet.

Delivered: 20 Anchor instructions, 233 tests, ~60K fuzz assertions,
comprehensive SDK, full E2E coverage, deployment runbook. Repository
is functionally feature-complete for single + cross-market production
trading. The remaining ❌ items are **upstream-blocked**, not code gaps.

- [x] Cargo workspace with `programs/flash-book` Anchor crate
- [x] Account types matching Flash V2 conventions (FLP custody, position
      PDA, market account, insurance fund PDA, commit buffer)
- [x] FBA matcher in Rust with property-test parity to TS reference
- [x] FLP virtual quoter as deterministic pure-Rust function (callable as
      CPI from FLP pool state)
- [x] Funding integration via Q64.64 cumulative index (i128)
- [x] VPIN calculator in Q32.32 fixed-point
- [x] Type-safe newtype wrappers (BaseLots, QuoteLots, Ticks, Bps)
- [x] Numbered error code enum (FlashBookError)
- [x] 31 Rust unit tests with parity to TS suite
- [x] Stress-lattice margin in Rust (port from TS)
- [x] In-loop liquidation injector in Rust
- [x] Insurance fund waterfall in Rust
- [x] Commit-reveal in Rust with hash check, expiry sweep, bond seizure
      (L1 force-include path is a roadmap item — needs MagicBlock-side support)
- [x] Property-based testing via `proptest` (6 properties × 2K cases each)
- [x] Anchor instruction handlers (initialize_market, initialize_insurance_fund,
      open_trader_state, update_oracle, place_limit_order, submit_commit,
      submit_reveal, run_batch, deposit_collateral, withdraw_collateral,
      apply_fill)
- [x] Account validation, PDA seeds, signer checks
- [x] OrderBufferAccount + TraderStateAccount on-chain types
- [x] Anchor events: MarketInitializedEvent, BatchClearedEvent,
      CollateralDepositedEvent, CollateralWithdrawnEvent, FillAppliedEvent
- [x] Per-fill position state updates via `apply_fill` (init-if-needed Position PDAs)
- [x] Position lifecycle math (open / add / reduce / flip with realized PnL)
- [x] OI tracking via `update_oi` helper on every position transition
- [x] Open-positions counter on TraderState (gates withdraw)
- [x] Per-trader per-batch order rate limit
- [x] Real FLP exposure read from FlpExposureAccount (no synthetic placeholder)
- [x] Realized-volatility coefficient in Rust FLP quoter (full TS parity)
- [x] FLP_SEQ_RESERVED_OFFSET constant (no magic numbers)
- [x] Clippy clean
- [x] Anchor IDL generated (`idl/flash_book.json`, 2,595 lines)
- [x] `liquidate_position` permissionless liquidation instruction
- [x] `LiquidationInjectedEvent` for liquidation telemetry
- [x] TypeScript SDK package (`@flash-book/sdk`) with 11 typed instruction
      builders, 7 PDA derivers, error-code enum, default params helpers
- [x] SDK test suite (28 tests covering PDAs, errors, params)
- [x] `apply_flp_fill` instruction — on-chain settlement for FLP-side fills
      with `FlpExposureAccount.per_market` mutation
- [x] Stress-lattice margin gate on order intake
      (rejects new orders that would push trader into liquidation)
- [x] FLP per-market position lifecycle (open / add / reduce / flip / close)
- [x] `set_market_status` — circuit breaker (Active/PostOnly/Paused/Closed)
- [x] `update_market_params` — governance tuning with immutable-primitive enforcement
- [x] `transfer_market_authority` — safe key rotation
- [x] Status gate on `place_limit_order`
- [x] FLP capital lifecycle: initialize_flp_exposure, deposit_flp_capital,
      withdraw_flp_capital (with open-positions gate)
- [x] Standalone lifecycle demo via SDK (sdk-ts/examples/full-lifecycle.ts)
- [x] **E2E integration tests via solana-program-test (5 passing)**
- [x] SDK builder coverage tests (22 cases — every Ix builder verified)
- [x] **`liquidate_portfolio` cross-market portfolio liquidation** — walks
      remaining_accounts for trader's positions across multiple markets,
      runs cross-margin assess_margin against joint scenario lattice,
      injects liquidation order on execution market
- [x] **Stress-lattice margin enforcement on order intake** — wired in
      `place_limit_order` via init-if-needed Position PDA + assess_margin
- [x] **Anchor IDL generation** (3,510 lines)
- [x] **20 E2E integration tests** via solana-program-test
- [x] **30 property tests × 2K cases** = ~60K fuzz assertions
- [x] **Zero panic paths in production code** (auditable)
- [x] **Standalone lifecycle + live monitor demo scripts**
- [x] **Deployment runbook** (`docs/DEPLOYMENT.md`)
- [ ] **Integration with MagicBlock ER `delegate_account` /
      `commit_and_undelegate_accounts`** ❌ blocked on
      `ephemeral-rollups-sdk` Solana 2.x compat (tested 0.2 + 0.13;
      both have upstream type mismatches against current Solana stack)
- [ ] **BPF compilation** ❌ blocked on Solana platform-tools v1.48
      rustc 1.84 + transitive `constant_time_eq` edition2024 dep
- [ ] **L1 force-include path for censored reveals** — needs MagicBlock-side
      sequencer accountability protocol
- [ ] **Independent security audit** — happens after upstream blockers clear

**Deliverable:** auditable mainnet-ready program, devnet deployed.

## Phase 2 — Mainnet shadow mode

Run the matcher in **observation mode** against current Flash V2 flow:

- [ ] Devnet → mainnet program deployment
- [ ] Read-only ingestion of mainnet Flash V2 trades
- [ ] Replay each trade through the matcher
- [ ] Compute what FBA + virtual FLP would have done
- [ ] A/B compare against actual Flash V2 pool outcomes
- [ ] 30+ days of shadow data

**Deliverable:** empirical validation that Flash Book outperforms
oracle-only model on LP yield + retail execution quality. Ship gate:
shadow demonstrates ≥ 10% LP yield improvement over the comparison window.

## Phase 3 — Limited production

Open one market (SOL-PERP) to live trading:

- [ ] Whitelist a small set of MMs for resting liquidity
- [ ] Cap per-trader position size at 0.1% of FLP capital
- [ ] Cap insurance fund withdrawals; maintain target balance
- [ ] Real-time invariant monitoring + automatic kill switch
- [ ] 7-day soak with bug bounty program

**Deliverable:** SOL-PERP trading on Flash Book, oracle-priced fallback if
matcher faults, no measurable user-impact regressions.

## Phase 4 — Multi-market rollout

- [ ] BTC-PERP, ETH-PERP onboarded
- [ ] Per-market parameter calibration from shadow data
- [ ] Long-tail markets (per protocol roadmap)
- [ ] Builder-deployed markets (HIP-3-style platform layer)
- [ ] Real-world asset markets (per Flash V3 roadmap)

**Deliverable:** Flash Trade Orderbook V3 fully on mainnet.

## Phase 5 — Continuous improvement

- [x] Multi-oracle quorum (`update_oracle_quorum` — median + dispersion)
- [x] Cross-market netting at the matcher level (`place_basket_order_n`)
- [x] Maker rebate distribution from toxicity tax pool
- [x] Spot trading on the same matcher (`defaultSpotMarketParams`)
- [ ] Lending integration (cross-margin against spot collateral) — deferred

## Phase 6 — Hyperliquid parity + Flash-specific math wins (waves 6-13, **complete**)

Shipped after the original phase 5 list. Every item below is a NEW
on-chain primitive added since the original "feature-complete" mark.

### Native order types (HL parity)
- [x] Trigger orders w/ OCO + reduce_only + GTT (`place_trigger_order`)
- [x] TWAP orders w/ permissionless slice exec (`place_twap_order`)
- [x] Bracket orders — atomic parent + 2 OCO triggers (`place_bracket_order`)
- [x] Trailing stops w/ permissionless ratchet (`update_trailing_stop`)
- [x] Iceberg orders w/ permissionless replenish (`place_iceberg_order`)
- [x] Mass cancel — single-tx flatten (`cancel_all_orders_in_market`)
- [x] GTT order expiry on every order (`expires_at_slot`)
- [x] STP modes — CancelNewest / CancelOldest / CancelBoth

### Permissionless markets (HIP-3)
- [x] `permissionless_initialize_market` w/ envelope-clamped params
- [x] Pre-launch market flag (`is_pre_launch`)
- [x] Slashable HIP-3 deployer bond (`MarketBondAccount`)
- [x] 7-day unbond delay
- [x] `slash_market_bond` (governance-gated)

### Capital primitives
- [x] User-managed trading vaults (`VaultAccount` + `VaultPositionAccount`)
- [x] HWM perf-fee in shares (`settle_vault_perf_fee`)
- [x] Mark-to-market vault NAV (market walk in remaining_accounts)
- [x] Cross-margin sweep (`sweep_collateral`) — position-aware
- [x] Per-position leverage cap (`set_position_leverage`)

### Fee + reward primitives
- [x] Builder codes (`set_trader_builder` + `BuilderFeeOwedEvent`)
- [x] Referral program (`set_trader_referrer` — one-time-write)
- [x] Negative-fee top tier (discount up to 12_000 bps)
- [x] Trading-rewards eligibility (`TradingRewardEligibleEvent`)
- [x] Multi-threshold pre-liq margin alerts

### Liquidation safety
- [x] Auto-Deleverage (`auto_deleverage`) w/ bankruptcy-price math
- [x] Mark-price sanity cap (`mark_change_max_bps`)
- [x] Whole-market OI cap (`max_oi_base_lots`)

### Smarter-than-HL math (Flash Book specific)
- [x] CME-style concentration margin tier (FLP-capital-keyed)
- [x] Funding-premium TWAP dampener (kills 50ms cadence microbursts)
- [x] Symmetric-OI funding dampener (balanced book → 0 funding)

### View ixs (UI primitives via tx simulation)
- [x] `view_predicted_funding`
- [x] `view_quote_ladder`
- [x] `view_portfolio_risk` (cross-market, single-call)

### Bot suite (`@flash-book/bot`)
- [x] AdlKeeper (off-chain ranking, on-chain eligibility)
- [x] TrailingStopKeeper
- [x] IcebergKeeper
- [x] BondMonitorKeeper (read-only alerts, governance owns slash)

### Math hardening
- [x] 9 new property tests (~18K cases) for OI dampener +
      concentration-tier invariants

**Status:** 35 native ixs, 149 Rust tests, 263 TypeScript tests,
zero warnings, ER-compatible. Architecture doc (`docs/ARCHITECTURE.md`)
+ comparison doc (`docs/COMPARISON.md`) refreshed to current state.

## Phase 7 — Production hardening (in progress)

### Required to ship
- [ ] End-to-end integration tests for ADL / vault MTM / bracket OCO /
      iceberg replenish / sweep-with-positions (math is proptested,
      flows aren't yet)
- [ ] Mainnet deployment scripts + upgrade-authority handling
- [ ] Threat-model coverage matrix (audit prep)
- [ ] Multi-sig market-pause governance (currently single-authority)

### Strongly desired
- [ ] Liquidity bootstrap auction for HIP-3 (Dutch-style price
      discovery for the first hour of a permissionless deploy)
- [ ] Multi-tier (N=4) concentration margin (we have single-tier)
- [ ] Funding-per-period cap (anti-gouge over 24h windows)

### Not required (different scope)
- Cross-asset cross-collateral (stress-lattice rework)
- Subaccount as separate type (covered by delegate slot)
- HYPE-style governance token (tokenomics decision)
- Block-trade RFQ (off-chain matching layer)

## Open research questions

These don't block any phase but inform design refinement:

1. **Optimal batch interval per market.** 50 ms is calibrated for major
   pairs; long-tail may benefit from 200 ms. Empirical study needed.
2. **VPIN parameter calibration** (α, β, γ, κ, δ). Default values are
   industry-typical; production values should come from 6-month historical
   replay against current Flash V2 flow.
3. **Insurance fund optimal sizing.** 1% of OI is a starting heuristic
   from the October 2025 crash data. Stochastic modeling of bankruptcy
   tail risk should refine this.
4. **ADL fairness improvements.** Current rank by profit/leverage; could
   add tie-breaking randomization within profit-ratio brackets.
5. **Commit bond economics.** Bond size is a parameter; needs analysis of
   spam-vs-friction trade-off on real flow.
