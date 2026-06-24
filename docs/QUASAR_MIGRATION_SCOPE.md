# Scope: Quasar / Pinocchio Migration (the entrypoint + framework lever)

Status: **scoping only — not started, not approved.** This document sizes the
last big CU lever so the decision can be made with eyes open.

## Why

Profiling (`docs/CU_OPTIMIZATION.md`) shows that after the find_program_address
fix and the PositionAccount zero-copy, `apply_fill` is ~37.8k CU and the
**handler logic is only ~3.8k** — the remaining ~34k is the **Anchor framework**:
the entrypoint, Borsh on the heavily-accessed accounts (`MarketAccount`,
`InsuranceFund`), and the event emit + Borsh reserialize. Per-account Anchor
`AccountLoader` is exhausted — its per-call `load()` overhead makes heavy-access
accounts net-zero/negative. Capturing the rest needs a different entrypoint +
true pointer-cast accounts (no `load()` overhead) — i.e. Pinocchio/Quasar.
Research (SPL-token): Pinocchio cuts 88–95% of CU, ~70% from the entrypoint +
zero-copy.

## Migration surface (measured)

| Item | Count | Migration cost |
|---|---|---|
| `pub fn` instructions in `#[program]` | **112** | each handler re-expressed |
| `#[derive(Accounts)]` structs | **100** | each re-declared in the new model |
| Borsh `#[account]` structs (state.rs/state_v3.rs) | **24** | → pointer-cast layouts (Pod, `[u8;16]` for i128) |
| `emit!` sites | **121** | re-expressed as the framework's log/event |
| `anchor_spl` / token CPI sites | **32** | rewritten as raw/framework CPIs |
| Hand-rolled ER CPI (`er.rs`) | 1 module | re-check against the new entrypoint |
| Matcher / risk modules (`matcher/*.rs`) | **36** | **port as-is** (pure logic, framework-agnostic) |
| Kani proofs (`haircut.rs`) | 5 | **survive as-is** (prove pure functions) |
| Hypertree (`state_v2.rs`) | — | **already zero-copy** (raw byte slicing) — no change |

The good news: the *hard, novel* parts (hypertree, risk math, the proofs) are
framework-agnostic and carry over. The cost is the **breadth** — 112 ix + 100
Accounts structs + 121 emits + 32 CPIs is a large mechanical+careful rewrite.

## Candidate frameworks

| | Quasar | Pinocchio | Stay on Anchor |
|---|---|---|---|
| Model | Anchor-style macros (`#[program]`/`#[account]`/`#[derive(Accounts)]`) | low-level, unopinionated library | current |
| Porting friction from Anchor | **lowest** (has a "Migrating from Anchor" guide) | high (manual structure) | n/a |
| Maturity | **beta**, young | more battle-tested (1,200+ devs, Backpack prod) | mature |
| **Audit status** | **unaudited** ("APIs may change. Use at your own risk.") | **unaudited** | audited ecosystem |
| Tooling | CU profiler + flamegraphs, QuasarSVM, typed clients | minimal | rich |
| CU ceiling | Pinocchio-tier | Pinocchio-tier | current |

## Risks (read before approving)

1. **Both frameworks are unaudited.** flash-book is a perps DEX targeting
   mainnet. Building the settlement layer on an unaudited, fast-moving beta
   framework adds supply-chain + maintenance risk on top of the protocol's own
   (still-unaudited) risk. This is the dominant consideration.
2. **No incremental path within one program.** You cannot run Anchor and
   Quasar/Pinocchio entrypoints in the same program — it is effectively a
   **new program** (same program ID, rewritten). All 112 ix must land before it
   can replace the deployed program. (You *can* develop it as a parallel crate.)
3. **API churn (Quasar):** "APIs may change" — a migration mid-beta may need
   rework as the framework evolves.
4. **Re-validation:** all **569 tests + 5 Kani proofs + build-sbf** must pass on
   the new program; the integration tests (Anchor `program-test`) may need
   reworking for the new test harness (QuasarSVM / Mollusk).
5. **IDL / client compatibility:** the IDL shape may change; any external client
   built against the current IDL would need updating.

## Phasing options

- **A — Full Quasar rewrite.** Lowest per-handler friction (Anchor-style), but
  adopts an unaudited beta framework for the whole program. Biggest blast radius.
- **B — Full Pinocchio rewrite.** More mature/battle-tested, but more manual
  (no opinionated Accounts model) — more hand-written code across 112 ix.
- **C — Measured pilot first (recommended).** Build a *throwaway* Pinocchio (or
  Quasar) program implementing **only the `apply_fill` hot path** (the matcher
  entrypoint + the handful of accounts it touches), and measure its real CU.
  This gets the *actual* ceiling (not the SPL-token extrapolation) for a few
  days of work, before committing weeks to the full rewrite. Decide A vs B vs
  "not worth it" based on the measured delta.

## Effort (honest ranges)

- Pilot (option C): **~2–4 days** — one instruction path, real CU number.
- Full migration (A or B): **multi-week** (112 ix, 100 Accounts structs, 121
  emits, 32 CPIs, re-validate 569 tests + 5 proofs + integration harness).

## Recommendation

1. **Do the pilot (C) first.** It's cheap and turns the "~70%" hypothesis into a
   measured number for *this* program. Everything downstream depends on it.
2. **Sequence the framework decision after an external audit of the current
   Anchor program.** Rewriting the settlement layer onto unaudited tooling
   *before* auditing the protocol compounds risk; the current Anchor CU is
   already competitive (apply_fill ~37.8k; place ~12.5k — see
   `benchmark-results` history). Audit first, then decide if the framework win
   justifies a second audit of the rewritten program.
3. If approved after the pilot: prefer **Quasar** for the Anchor-style porting
   path *iff* it has stabilized/been audited by then; otherwise **Pinocchio**
   for maturity.

## What does NOT need migrating regardless

The hypertree CLOB, the 36 matcher/risk modules, and the 5 Kani proofs are
framework-agnostic and carry over unchanged — the protocol's differentiated core
is insulated from this decision.
