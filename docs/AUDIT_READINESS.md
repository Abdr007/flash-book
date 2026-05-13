# Audit Readiness

What an external auditor needs to verify before this protocol holds
real money. Read top-down — each section gates the one below.

## 0. Status

Devnet. Not audited. Not production. Target program ID
`Di8ZzxmMb5Ho2xWHbvcAxKPjcaVXTCM7U5xe5Gm7uLVF` (per `Anchor.toml`).
The on-chain code has passed 211 deterministic tests and is layout-
compatible across the Phase 2 series of account-struct migrations,
but it has not seen adversarial review by an independent firm.

## 1. Documents the auditor reads first

Map of source-of-truth documents:

| Document | Purpose | Length |
|---|---|---|
| `docs/MARGIN_MATH.md` | The complete margin / liquidation / funding model. §9 lists every invariant the on-chain code claims to enforce, cross-referenced to handlers + tests. | 17 KB |
| `docs/SUB_ACCOUNT_TRADING.md` | Sub-account architecture — how positions, orders, and fills are keyed by `(wallet, sub_index)` end-to-end. Documents the Phase 2c–2f migrations. | 20 KB |
| `docs/COMPARISON.md` | File:line-cited comparison with Hyperliquid / Drift v2 / dYdX v4 / GMX v2 / Phoenix. Includes an explicit "honest weaknesses" section so the auditor knows what we know we don't know. | — |
| `docs/ARCHITECTURE.md` | System layout, account lifecycle, instruction families. | 14 KB |
| `docs/INSTRUCTIONS.md` | Per-instruction reference with account layout + invariants per ix. | 9 KB |
| `docs/MATH.md` | FBA clearing math (TS simulator), FLP quoter spread function, mark price blending. | 9 KB |
| `docs/SAFETY.md` | Solvency invariants + threat model. | 11 KB |
| `CHANGELOG.md` | Phase 2 series commit-by-commit. | (whole repo history) |

## 2. Invariants the on-chain code claims

The full list lives in `MARGIN_MATH.md §9`. Reproduced here for the
auditor's quick reference:

| # | Invariant | Enforced by |
|---|-----------|-------------|
| I-1 | Cross health: `assess_margin(𝒫_cross, C_T, 𝒮).is_healthy` | All trade-path call sites via `assess_margin_unified` |
| I-2 | Isolated independence: each isolated position is healthy against its own collateral alone | `assess_margin_split` |
| I-3 | Cross pool insulation: liquidation of an isolated position never debits `C_T` | `liquidate_position_v2`, `auto_deleverage` (Phase 2h), `apply_fill` (Phase 2g) |
| I-4 | Isolated bucket insulation: cross-path operations never debit any per-position bucket | All cross-path handlers |
| I-5 | Funding insulation: funding on an isolated position never touches `C_T` | `settle_funding` |
| I-6 | Phase 2 single-isolated cap (relaxable in Phase 3) | `set_position_isolated` |
| I-7 | Cash conservation on transition | `set_position_isolated` |
| I-8 | Reverse transition + health check | `set_position_cross` |

Each invariant should be tested by independent property-based checks
in `tests/proptest_isolated.rs` (6 properties × 2000 random cases).

## 3. Code surfaces ranked by audit risk

Auditors should weight time by the value-at-risk per surface. Highest
first:

### Tier 1 — collateral movement / state mutation

These are the surfaces where a bug means real money goes to the wrong
address. Read every line.

| Surface | File:line | Why |
|---|---|---|
| `apply_fill` | `lib.rs:2717` | Every fill mutates collateral on both sides plus the insurance fund. Fee routing (Phase 2b), realized-PnL materialisation (Phase 2g), and PDA verification (Phase 2i) all touch this. |
| `apply_flp_fill` | `lib.rs:5134` | Same for the FLP-maker leg. |
| `liquidate_position_v2` | `lib.rs:5267` | Health gate → synthetic close. Dual-source price gate, JIT auction, Dutch reward routing (Phase 2 — isolated). |
| `auto_deleverage` | `lib.rs:5902` | Bankruptcy-price loss + counter-gain settlement. Phase 2h routes per bucket. |
| `settle_funding` | `lib.rs:2607` | Funding accrual; per-position routing (Phase 2). |
| `set_position_isolated` / `set_position_cross` | `lib.rs:2272`, `lib.rs:2386` | Atomic collateral transfer between buckets + post-transfer health check on entire portfolio. |
| `migrate_position_to_trader_state_key` | `lib.rs:1577` | Phase 2c migration; reads legacy PDA, init's new PDA, copies state, closes legacy. |

### Tier 2 — math + risk evaluation

Bugs here change healthy/unhealthy verdicts, which propagate to Tier 1.

| Surface | File:line | Why |
|---|---|---|
| `assess_margin` / `assess_margin_split` / `assess_margin_unified` | `matcher/risk.rs:241`, `:364`, `:432` | Stress lattice + bucket independence. Foundation of every health check. |
| `tiered_mmr_bps` | `matcher/risk.rs:95` | HL-pattern tiered MMR. Audit invariant: monotone non-decreasing in notional. |
| `unrealized_pnl_quote_lots` | `matcher/risk.rs:199` | Per-position PnL math; all integer. |
| `funding_owed` | `matcher/funding.rs` | Per-position funding integral against `cum_funding_index`. |
| `compute_realized_pnl_routing` | `lib.rs` `mod realized_pnl_routing_tests` | Phase 2g pure-math helper. |
| `route_adl_loss` / `route_adl_gain` | `lib.rs` `mod adl_routing_tests` | Phase 2h ADL routing. |

### Tier 3 — placement / cancel / book mutation

Doesn't move collateral directly but produces orders that downstream
ixs consume.

| Surface | File:line | Why |
|---|---|---|
| `place_limit_order_v2` | `lib.rs:342` | Hypertree insertion. sub_index written. |
| `place_taker_order_v2` | `lib.rs:465` | Hot-path matcher walk, fills emitted, residual rest. |
| `modify_order_v2` | `lib.rs:863` | Atomic cancel+place; preserves sub_index. |
| `cancel_order_v2` / `cancel_all_v2` | various | Removes from hypertree. |

### Tier 4 — oracle, market initialization, governance

| Surface | File:line | Why |
|---|---|---|
| `update_oracle_quorum` | `lib.rs:3462` | Median-of-3, dispersion gate, staleness gate. |
| `update_oracle_from_pyth` | `lib.rs` | Pyth CPI; cross-check fields. |
| `initialize_market` / `permissionless_initialize_market` | `lib.rs` | Param bounds, safe envelope. |
| `verify_market_invariants` | `lib.rs` | Kill switch; permissionless. |

## 4. Pure-math test coverage (deterministic guarantees)

The pure-math layers carry the bulk of risk and are exhaustively
unit-tested. An auditor should re-run these in a clean tree before
reading any handler:

```
cargo test -p flash-book --lib realized_pnl_routing_tests
cargo test -p flash-book --lib adl_routing_tests
cargo test -p flash-book --lib isolated_margin_tests
cargo test -p flash-book --lib fee_tier_tests
cargo test -p flash-book --lib tier_tests
```

Total per-module test counts at this release:

```
realized_pnl_routing_tests  11
adl_routing_tests           11
isolated_margin_tests        5  (the unit-test mirror; 6 more as proptests)
fee_tier_tests               5
tier_tests                   3
matcher::risk + matcher::*  100  (covers everything else in the matcher)
```

## 5. Property-based test coverage

Proptests run 2000 random cases per property. The properties exercise
exactly the §9 invariants:

```
tests/proptest_risk.rs           6 properties — cross-margin invariants
tests/proptest_isolated.rs       6 properties — bucket independence (I-1, I-2, I-3, I-4)
tests/proptest_liquidation.rs    7 properties — liquidation health checks
tests/proptest_new_features.rs  19 properties — JIT auction, tiered MMR, etc.
tests/proptest_modules.rs       14 properties — funding integral, mark blend
```

If the auditor identifies a new invariant they want covered, the
proptest patterns are mechanical to extend.

## 6. On-chain integration test coverage

37 integration tests in `tests/integration.rs`. These exercise the
program ix surface against the live Solana test validator. Highlights:

- 3 ApplyFill scenarios (Phase 2j): open positions + OI, realized-PnL
  materialisation on close, wrong-sub_index rejection.
- 3 sub-account scenarios (Phase 2d): deposit credits sub-account
  not main, wrong-trader-state rejection, Position PDA migration.
- 31 other handler scenarios (deposit, withdraw, oracle, market
  init, LP units, insurance fund, etc.).

## 7. Known limitations (the auditor should NOT spend time confirming)

These are documented gaps. Reporting them is fine; they don't need
new analysis. See `SECURITY.md` for the full list.

- **No FBA / Walrasian clearing on-chain.** TS simulator only.
  See `docs/FBA_ON_CHAIN.md` for the migration plan.
- **No commit-reveal on-chain.** Same. See `docs/COMMIT_REVEAL_ON_CHAIN.md`.
- **No HLP-style dedicated backstop vault.** JIT-liq auction is the
  closest analogue but opportunistic.

## 8. Recommended audit scope

A focused audit can be sized around:

| Slice | Time | What it covers |
|---|---|---|
| Tier 1 collateral math | ~3 days | apply_fill, apply_flp_fill, liquidate_position_v2, auto_deleverage, settle_funding, set_position_*. The Phase 2 series. |
| Tier 2 risk math | ~2 days | risk.rs, funding.rs, routing helpers. Most of the work is verifying the proptests cover the invariant space. |
| Tier 3 + 4 | ~2 days | placement, cancel, oracle, init. |
| Property test review | ~1 day | Does the proptest space cover the invariant space? |
| Documentation cross-check | ~1 day | Are the docs accurate? |
| **Total** | **~9 days** | Single-firm engagement; multi-firm bake-off can parallelise Tiers 1-3. |

## 9. Hand-off artifacts the auditor should request

- This repo at the audited commit hash (the audit is pinned to a
  specific commit, NOT `main`).
- The IDL file at `idl/flash_book.json`.
- A reproducible build environment (Rust toolchain, Solana
  toolchain, Anchor version pinned).
- `cargo test -p flash-book` should pass clean in their sandbox.
- `cargo build-sbf` should produce the same .so bytes as our CI.

## 10. After-audit deployment gate

Before any mainnet deployment, all of these must be true:

- All audit findings rated High or Critical: resolved.
- All Medium findings: resolved OR explicit deferred-acknowledgment
  with the audit firm.
- A second audit pass on the fixes (delta-audit) — typically 2-3 days.
- Bug bounty program live (target: Immunefi at $250k-$1M cap).
- Operator runbooks signed off (`docs/KEEPER_RUNBOOK.md` covers the
  keeper side; a separate ops doc covers oracle / authority / pause).
- Insurance fund seeded with at least N notional capital (operator
  decision; HL precedent is ~10× peak daily liquidations).

## 11. Versioning

This document tracks the audit-readiness surface as of `v0.2.0` (commit
`66bde61`). Future Phase 3 work — FBA on-chain, commit-reveal,
HLP-style backstop vault — will substantially expand the audit
surface; this document should be updated alongside the implementing
PRs so the auditor's read-through stays accurate.
