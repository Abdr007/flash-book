# Wave 23 — Certora Formal Verification Prep

External engagement spec for proving Flash Book v3's matcher and risk
engine correctness against a formal specification. This doc is the
input to a Certora paid engagement; it captures the invariants we want
machine-checked and the entry points where they apply.

## Why Certora vs Anchor unit tests

Unit tests + property tests catch the cases you think to test. Certora
proves invariants over the FULL state space using SMT solvers. For an
exchange holding user collateral, the difference is:

  • Unit test catches "this case I imagined".
  • Certora catches "any sequence of N ixs starting from any reachable
    state preserves no-bad-debt."

Real-world: dYdX v4, Compound, Aave, Lido all run Certora on their
risk-critical contracts. It's table stakes for a perp DEX claiming
production-grade.

## Engagement model

Certora charges by LOC of spec + complexity of proofs. Estimated:

  • Core matcher invariants (8 properties): 4-6 weeks engagement
  • Margin / liquidation invariants (5 properties): 3-4 weeks
  • Total engagement: 8-10 weeks, ~$80-120K USD

Output: a `.spec` file in Certora's CVL (Certora Verification Language)
+ proof-of-correctness reports per invariant.

## Critical invariants to verify

### Matcher (FBA Walrasian clearing — `matcher::fba::clear_batch`)

INV-M-1: **Clearing price is in the indifference interval.**
  For all order sets, the chosen clearing price `p*` satisfies:
    `min(buy_limit_prices ≥ p*) ≤ p* ≤ max(sell_limit_prices ≤ p*)`
  No order is filled at a price worse than its limit.

INV-M-2: **Volume conservation.**
  `Σ fill.size for buys = Σ fill.size for sells = clearing_volume`
  No fill is double-counted or lost.

INV-M-3: **No self-trade.**
  For all fills (buy, sell), `buy.trader ≠ sell.trader` UNLESS the
  STP mode explicitly allows it (`StpMode::CancelBoth` is the only
  exception).

INV-M-4: **Maximum-volume tiebreak.**
  If multiple candidate prices yield the same V(p*), the chosen p* is
  closest to `prior_mark` (then midpoint of indifference if tied).

INV-M-5: **FIFO within priority class.**
  For two orders at the same priority and price, the one with smaller
  `seq` fills first.

### Margin (matcher::risk + assess_margin_fn)

INV-R-1: **Healthy → no liquidation eligibility.**
  If `assess_margin_fn(...).is_healthy == true`, then
  `liquidate_position_v2` rejects with `NotLiquidatable`.

INV-R-2: **Initial margin ≥ maintenance margin.**
  For any market params, `initial_margin_ratio_bps ≥
  maintenance_margin_ratio_bps`. Enforced at init + update.

INV-R-3: **Concentration MMR is monotone.**
  If `position.size ≥ concentration_threshold`, the effective MMR
  satisfies `effective_mmr ≥ baseline_mmr +
  concentration_extra_mmr_bps`.

INV-R-4: **Cross-margin doesn't increase IM.**
  For a portfolio with hedged positions in correlated markets, the
  joint IM under `default_scenarios` is ≤ the sum of per-market IMs.

INV-R-5: **No bad debt creation.**
  After any fill, `taker.collateral + maker.collateral ≥
  insurance_fund_starting_balance`. If a fill would create negative
  equity, the loss is absorbed by the insurance fund (not transferred
  to the counterparty).

### Hypertree (state_v2 + matcher::v2_bookkeeping)

INV-H-1: **RBT order preservation.**
  `for_each_bid_best_first` yields orders in non-increasing
  `price_ticks` order; `for_each_ask_best_first` yields in
  non-decreasing `price_ticks` order.

INV-H-2: **Free-list reuse.**
  After `remove_*_node(idx)` followed by `insert_*(order)`, the new
  insert may reuse `idx` (allocator is correct) and the new node has
  the freshly-written value.

INV-H-3: **`total_orders_active` is exact.**
  At any state, `total_orders_active == |bids tree| + |asks tree|`
  (active leaves only).

INV-H-4: **Header `bids_best_index` ≡ MIN of bids tree.**
  After any insert/remove, the cached best index points at the node
  with smallest `order_id` in the tree (which corresponds to the
  highest-priced bid given inverted encoding).

### Funding (matcher::funding + matcher::v2_bookkeeping)

INV-F-1: **EMA blend bounded.**
  `ema_blend_funding_rate(prior, new, false)` returns a value in
  `[min(prior, new), max(prior, new)]` for all i64 inputs.

INV-F-2: **Per-period cap honored.**
  After `run_batch_v2` advances funding, the absolute bps charged in
  the current period ≤ `funding_per_period_max_bps`.

INV-F-3: **Vol-adaptive band capped.**
  `vol_adaptive_band_bps(base, vol)` returns a value in
  `[base, 4 × base]` for all u32 inputs.

## Pre-Certora work (in-house, ~2 weeks)

Before opening an engagement, internal team:

  1. Refactor the matcher to make state transitions explicit.
     Currently `clear_batch` returns a `ClearResult` value; Certora
     prefers state-transition predicates (`stateAfter = f(stateBefore,
     orders)`). Wrap the matcher in a `MatcherStep` predicate.

  2. Tag all integer arithmetic ops in v2 ix bodies with explicit
     overflow contracts. Currently we use `checked_*` / `saturating_*`
     consistently; Certora needs the contract spelled out.

  3. Add ghost variables for invariants Certora needs to track but
     aren't in production state (e.g., `total_volume_cleared`,
     `total_fees_collected_invariant`).

  4. Compile a regression suite of "known-bad" inputs that should
     trigger each invariant violation. Ensures the spec catches
     them.

## Out of scope

- Verifying the FLP virtual-quote generator (`matcher::flp_quoter`).
  Heuristic; correctness is best-effort, not provable.
- Verifying the off-chain SDK simulators.
- Verifying the MagicBlock ER delegation CPI surface (Certora doesn't
  spec cross-program CPI semantics).

## Vendor selection

Certora is the de facto standard but alternatives include:

  • **Certora** — most established, used by Aave/Compound/dYdX.
    CVL is stable, tooling mature. **Recommended.**
  • **Halmos** — newer, Solidity-focused (would need Anchor adapter).
  • **K Framework** — academic, steep learning curve.

## Deliverables (Certora engagement)

  1. `.spec` file with all 13 invariants formalized in CVL.
  2. Per-invariant proof report (PASS / FAIL with counterexample).
  3. Bug-class report — for each FAIL, classification (severity,
     impact, fix recommendation).
  4. Coverage report — which functions / state-transitions are
     covered.
  5. Maintenance handoff: 2 hours of training so internal team can
     re-run proofs after code changes.

## Contracts to engage

  • [Certora](https://www.certora.com) — request quote referencing
    "Solana Anchor program, ~10K LOC, 13 critical invariants,
    perpetual DEX matcher + risk".
  • Lead time: 6-8 weeks from initial contact to engagement start.
