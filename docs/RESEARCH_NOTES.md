# Research Notes — What we took from Percolator + GMX V2

Internal briefing for the Flash team on the design lineage of Waves
24–65. Two external protocols were studied at depth; this document
maps each idea borrowed back to its source, then to the flash-book
artifact (module + ix) that implements it.

---

## Percolator (Anatoly Yakovenko's research engine)

**Source**: `github.com/aeyakovenko/percolator` — `spec.md` v12.20.6, the
`src/percolator.rs` library (~10.6k lines), and the `aeyakovenko/percolator-prog`
wrapper. Status: **EDUCATIONAL RESEARCH — NOT AUDITED**. Live STOXX50/SOL
bounty market on mainnet but not battle-tested.

Percolator's core insight is the **three-invariant risk engine**:
H (haircut), A/K/F/B (lazy side indices), and a per-slot envelope
proof. Each is a separately reasoned mathematical invariant; together
they produce an engine that can be cranked permissionlessly without
admin intervention.

### Invariant 1 — H (Haircut Ratio)

**What it is**: a single global ratio

```text
h = min(Residual, MaturedPos_total) / MaturedPos_total
```

applied uniformly to every profitable position's released positive
PnL. Capital is senior (never haircut); profit is junior. By
construction, `Σ credits ≤ Residual`.

**Why it matters**: replaces ranked ADL queues with a fair, math-only
mechanism. Hyperliquid's Oct 10 2025 ADL overshoot ($45–51M extra
loss vs theoretical minimum per Chitra's arXiv:2512.01112 paper) is
the failure mode H is designed to prevent.

**What flash-book ships (Wave 24, complete)**:
- `programs/flash-book/src/matcher/haircut.rs` — pure math: `compute_h`,
  `apply_release`, `apply_mature`, `apply_convert`, `apply_residual_delta`.
- `state_v3.rs::MarketHaircutStateAccount` + `PositionHaircutStateAccount`
  sibling PDAs.
- 8 on-chain ix: `initialize_haircut_state`, `init_position_haircut_state`,
  `mature_position`, `convert_position`, `flush_haircut_dust`,
  `release_gain_to_haircut`, `seed_residual`, `verify_haircut_invariants`.
- Wire-in into `apply_fill` + `apply_flp_fill` via optional accounts
  (opt-in per market — pre-existing markets keep working bit-for-bit).
- Formal spec: `docs/HAIRCUT_MATH.md`.
- 22 unit tests + 10 proptest properties × 2000 cases each.

**Bug found and fixed during port**: the initial implementation wasn't
idempotent at the same slot — two cranks at slot N would drain more
than one slot's worth of reserve. Fixed by tracking
`original_reserve_at_attach` and computing `delta = target_cumulative
− already_drained`. Caught by `wave24b_haircut_ix::mature_idempotent_at_same_slot`.

### Invariant 2 — A/K/F/B (lazy side indices)

**What it is**: per-side cumulative indices for ADL multiplier (A),
mark accrual (K), funding accrual (F), and bankruptcy residual (B).
Each Position carries a snapshot `(a_snap, k_snap, f_snap, b_snap)`
at attach. Settling on touch is O(1).

**Why it matters**: replaces per-position iteration in `settle_funding`
and ADL with constant-time index advance. Critical for matcher-tick
throughput on the ER (~50 ms cadence).

**What flash-book ships (Wave 25, helpers complete; wire-in queued)**:
- `programs/flash-book/src/matcher/side_accrual.rs` — pure math:
  `advance_indices`, `settle_position_pnl`, `refresh_position_snapshot`,
  `reduce_a_pro_rata`, `step_mode`, `epoch_advance`.
- `state_v3.rs::MarketSideAccrualAccount` sibling PDA with hydrate /
  write helpers.
- `initialize_side_accrual` ix.
- Side state machine: Normal → DrainOnly (A < MIN_A_SIDE) → ResetPending
  (OI = 0) → Normal (epoch++).
- 19 unit tests covering all transitions.

**Status**: pure helpers ready. The `settle_funding` + `auto_deleverage`
rewrite onto these indices is queued as **Wave 25c** — a future focused
PR. The current `settle_funding` (per-position iteration) remains
functional in the meantime.

### Invariant 3 — Per-slot envelope

**What it is**: an init-time wide-arithmetic proof that

```text
price_funding_loss_N + liq_fee_N ≤ mm_req_N
```

for every notional `N ∈ [1, MAX_ACCOUNT_NOTIONAL_LOTS]`. Bad-parameter
markets cannot instantiate. At runtime, every oracle update gates on
`|Δp| × BPS ≤ max_price_move_bps × dt × p_last`.

**Why it matters**: turns "crank often enough" from operator
preference into a hard solvency boundary. An attacker who compromises
the oracle can move price by at most `cap × 1` per call, bounded.

**What flash-book ships (Wave 26, complete)**:
- `programs/flash-book/src/matcher/envelope.rs` — pure math:
  `prove_envelope`, `gate_price_move`.
- `state_v3.rs::MarketEnvelopeConfigAccount` sibling PDA with version
  counter + last-observed (slot, price) gate state.
- 3 on-chain ix: `set_envelope_config`, `verify_envelope_config`,
  `gate_envelope_price_move`.
- Runtime gate `gate_oracle_update` wired into all three oracle
  update paths: `update_oracle`, `update_oracle_quorum`,
  `update_oracle_from_pyth`.
- 13 unit tests + 7 proptest properties × 2000 cases.

### Other Percolator ideas

| Idea | Source | flash-book artifact |
|---|---|---|
| ARG (Aggressor Roundtrip Guard) | Percolator Phase 1 `plan.md` | `matcher/arg.rs` (Wave 29) + tests |
| Authority burn / immutability ladder | Percolator's authority-`[0;32]` pattern | `lib.rs::burn_market_authority` (Wave 30) |
| Active bankrupt-close phase machine | Percolator §6 | spec'd in `docs/HAIRCUT_MATH.md §12`; queued |
| Bilateral consent at any exec_price | Percolator's TradeNoCpi / TradeCpi | **Rejected** — kills CLOB price-time priority |
| `h_min = 0` instant-PnL lane | Percolator §3 | Inherited via warmup window; we keep non-zero |

---

## GMX V2 / GM Markets

**Source**: `github.com/gmx-io/gmx-synthetics` — `MarketUtils.sol`,
`PositionPricingUtils.sol`, `Oracle.sol`, `AdlHandler.sol`. Status:
**$3.5B+ TVL on Arbitrum + Avalanche mainnet, multiple audits
(Sherlock, GuardianAudits, ABDK)**.

GMX V2 is a pool-counterparty perp DEX (no CLOB). Despite being
architecturally different from flash-book, its design contains many
primitives that map cleanly onto a CLOB.

### Adopted ideas (with attribution)

| Idea | GMX V2 | flash-book artifact |
|---|---|---|
| **`acceptable_price` slippage cap on triggers** | `Order.acceptablePrice` (every order type) | Wave 27a/b: field on `TriggerOrderAccountV3` + `TwapOrderAccountV3`; runtime check in `execute_trigger_order_v3` + `execute_twap_slice_v3` |
| **OI-scaled MMR** | `getMinCollateralFactorForOpenInterest` | Wave 28a/b: `risk.rs::oi_scaled_mmr_extra_bps` + `effective_mmr_bps_full`, wired into `MarketSnapshot::effective_mmr_bps` |
| **Borrow fee (utilization-based)** | `MarketUtils.getNextBorrowingFees` | Wave 35: `matcher/borrow_fee.rs` (pure math; wire-in Wave 35b queued) |
| **Funding velocity smoothing (PID)** | `MarketUtils.getNextFundingFactorPerSecond` | Wave 37: `matcher/funding_velocity.rs` (pure math; wire-in queued) |
| **`claimableCollateralAmount` soft-fail** | `MarketUtils.applyDeltaToClaimable*` | Wave 36: `matcher/pending_claim.rs` (pure math; wire-in queued) |
| **min/max asymmetric oracle prices** | `Price.Props { min, max }` | **Conceptually adopted** — flash-book uses `worse-of(mark, oracle)` per side (line `lib.rs:5599`) with `oracle_band_bps` providing the spread |
| **MIT order pricing** | GMX limit-with-slippage shape | Wave 51: `matcher/mit_order.rs` |
| **Per-market isolation** | One pool per (longToken, shortToken, indexToken) | Inherited via flash-book's per-market PDA architecture |
| **Pool-PnL-driven ADL trigger** | `maxPnlFactorForAdl` | Mentioned in `docs/HLP_BACKSTOP_VAULT.md`; not yet shipped |

### Rejected ideas (with rationale)

| GMX V2 idea | Why we didn't adopt |
|---|---|
| **Pool-as-counterparty model** | flash-book's defining feature is the CLOB; pool model would dilute it |
| **Two-step request → execute pattern** | Solana's continuous CLOB doesn't need it; price discovery is intrinsic to the orderbook |
| **Chainlink Data Streams** | Solana has Pyth Solana Receiver (pull oracle, same shape); already integrated |
| **Single-keeper full-close liquidation** | flash-book's JIT auction + Dutch reward is strictly more competitive |

---

## Other research touchpoints

| Source | Concept | flash-book artifact |
|---|---|---|
| Hyperliquid | Tiered MMR by notional | `matcher/risk.rs::tiered_mmr_bps` (Wave 20a) |
| Hyperliquid | Cancel-on-disconnect | Wave 61: `matcher/cancel_on_disconnect.rs` |
| Hyperliquid | Builder codes / referral fees | Pre-existing in `state.rs` |
| Hyperliquid | Tiered LP rewards | Wave 56: `matcher/tiered_lp_rewards.rs` |
| Drift v2 | Stress-lattice risk math | Pre-existing `matcher/risk.rs::assess_margin` |
| dYdX v4 | Aggressor-side fee tier | Pre-existing fee tier table (Wave 22) |
| CME | Pro-rata fill split | Wave 59: `matcher/pro_rata.rs` |
| CME SPAN | Multi-scenario margin lattice | Pre-existing `assess_margin` scenarios |
| Binance | Daily loss limit | Wave 53: `matcher/daily_loss_limit.rs` |
| Binance | Volume rate limit (token bucket) | Wave 54: `matcher/volume_rate_limit.rs` |
| Binance | Per-trader concentration cap | Wave 58: `matcher/concentration.rs` |
| Tarun Chitra, *Autodeleveraging* (arXiv:2512.01112) | Junior-claim haircut motivation | H invariant (Wave 24) |

---

## Net assessment

Of the 23 new pure-math modules + 14 new on-chain ix added across
Waves 24–65:

- **3 are direct ports of Percolator's three invariants** (H, A/K/F/B,
  envelope) — the most novel and load-bearing additions.
- **6 are direct adaptations of GMX V2 ideas** to a CLOB model
  (acceptable_price, OI scaling, borrow fee, funding velocity,
  pending claim, MIT).
- **8 are CEX / institutional patterns** (cancel-on-disconnect,
  tiered LP rewards, daily loss limit, volume rate limit,
  concentration cap, cross-margin weights, stable collateral,
  pro-rata fill).
- **6 are flash-book-original** (ARG implementation details, peg
  pricing helpers, stop-limit composite, conditional cancel,
  min-fill-size policy, position cap, self-trade prevention pure
  module that documents the existing matcher behavior).

Combined with the pre-existing surface (continuous CLOB on hypertree,
stress-lattice margin, isolated buckets, JIT auction, Dutch reward,
per-position cooldown, dual-source liquidation gate, multi-oracle
quorum, ADL, FLP LP pool, sub-accounts), flash-book now ships:

- The **mathematical rigor** of Percolator's research engine.
- The **order-type versatility** of CEX-grade exchanges.
- The **LP economics** of GMX V2.
- The **decentralization** of authority burn.
- The **performance** of Solana + MagicBlock ER.

Within an open-source, formally-documented, internally-audited
codebase.

---

## What's queued (not yet on-chain)

The pure-math modules below are tested but their `apply_fill` /
`settle_funding` / order-intake integration hasn't shipped:

```
Wave 25c  settle_funding rewrite onto A/K/F/B
Wave 29b  ARG wire-in
Wave 31b  position_cap into order intake
Wave 35b  borrow_fee accrual on every fill
Wave 36b  pending_claim on partial pool insolvency
Wave 37b  funding_velocity into settle_funding
Wave 38b  insurance_replenish on fee distribution
Wave 50b  4th policy + matcher walk binding
Wave 56b  duration weights on LP NAV
Wave 57b  JIT defense on FLP withdraw
Wave 58b  concentration cap on order intake
Wave 60b  cross-margin joint math in assess_margin
Wave 61b  CoD heartbeat tracking
Wave 62b  min_fill_size in matcher walk
Wave 63b  reduce-only on limit orders
Wave 64b  conditional cancel state on resting orders
Wave 65b  stable collateral in collateral resolver
```

Each is a focused 1-2 session PR with no remaining design decisions
required.

---

## How this compares to Flash V2's current production scope

Flash V2 ([`@flash_trade/magic-trade-client@1.0.22`](https://www.npmjs.com/package/@flash_trade/magic-trade-client))
is a pool-counterparty perp protocol live on Solana mainnet with real
flow. Flash Book is an open-source CLOB implementation that:

1. Shares Flash V2's runtime stack (Solana + Pyth + MagicBlock ER).
2. Adopts the same dependency versions for SDK alignment (see
   `docs/SDK_ALIGNMENT.md`).
3. Offers a complementary venue: a CLOB for users who want price-time
   priority + tight spreads from competitive market makers, while
   keeping the Flash V2 pool as the always-on backstop for
   instant-fill UX.

Architectural future: a router layer that quotes both venues and
routes per trade to whichever is cheaper for the user. SDK
infrastructure already aligned; the routing UX is the open product
question.
