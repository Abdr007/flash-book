# Compute-Unit (CU) Optimization

Goal: make Flash Book the most CU-efficient on-chain orderbook. This doc
tracks the measured baseline, what's shipped, and the plan for the big lever.

All numbers are **real on-chain CU** from the program-test benchmark:

```bash
cargo build-sbf --manifest-path programs/flash-book/Cargo.toml
BPF_OUT_DIR=$PWD/target/deploy \
  cargo test -p flash-book --test integration cu_benchmark -- --ignored --nocapture
```

## Baseline → current

| Hot path | Baseline | After Phase 0 | Δ |
|---|---|---|---|
| `apply_fill` (open, both sides) | 55,502 | **42,093** | **−24%** |
| `apply_fill` (close, realize PnL) | 47,581 | **34,172** | **−28%** |
| `apply_flp_fill` | — | −~6.7k | one PDA call removed |
| `partial_withdraw` (lattice) | 49,445 | 47,945 | −3% |

## Phase 0 — in-place wins (no framework change)

**Shipped (PR #4):** `verify_trader_state_pda` used `find_program_address` —
a *descending bump search* costing **~6.7k CU each on-chain** — for the
Phase-2i routing check, twice per `apply_fill`. Switched to
`create_program_address` + the account's stored canonical bump (the pattern
`verify_position_pda` already uses). Security unchanged; **−13.4k CU/fill**.

**Assessed, not pursued (diminishing returns / wrong phase):**
- *Hot-path `emit!`* — already optimal. `TraderTierUpgradedEvent` is already
  gated on an actual tier change; the optional-fee events are already
  conditional. Merging event schemas would save ~600 CU but breaks the SDK
  decoder + indexers (outward-facing) for little gain. Skip.
- *`Clock::get()` hoist* in `apply_fill` (2 calls → 1) — ~150 CU, fiddly
  cross-scope change. Not worth the risk.
- *Zero-copy conversion of individual accounts* — this is the real remaining
  lever, but it is **Phase 1** work (see below), not a low-risk one-off:
  `InsuranceFundAccount` alone has 58 access sites, `MarketHaircutState` 53.

**Conclusion:** Phase 0's clean wins are harvested. The remaining CU lives in
**Borsh deserialization**, which needs the systematic migration below.

## Where the remaining CU goes (anatomy of `apply_fill`)

After the PDA fix, the dominant cost is **Borsh serialize/deserialize**. Every
big risk account is plain Borsh `#[account]` (NOT `zero_copy`):

| Account | Borsh deser ~CU | Notes |
|---|---|---|
| `MarketAccount` | ~950 | largest struct; read-heavy |
| `PositionAccount` × 2 | ~700 ea | taker + maker; read + write |
| `TraderStateAccount` × 2 | ~700 ea | taker + maker |
| `InsuranceFundAccount` | ~700 | |
| `FeeTiersAccount` (opt) | ~700 | |
| `MarketHaircutStateAccount` (opt) | ~700 | |
| `PositionHaircutStateAccount` × 2 (opt) | ~700 ea | |

`apply_fill` deserializes **up to 10** of these, then Anchor **re-serializes
every `mut` one on exit**. That is the bulk of the remaining ~42k CU.

## Profiling — where `apply_fill`'s CU actually goes (measured)

Instrumented `apply_fill` with `sol_log_compute_units()` checkpoints and read
the per-section deltas from the program-test log. For the **open** fill
(~51.7k CU with checkpoints in place):

| Section | CU | Note |
|---|---|---|
| **Before the handler body runs** | **~32,500** | Anchor **Borsh deserialize** of 6 accounts + auth + sub-account PDA verify + (open only) position **init** |
| Fee computation | 4,868 | tier resolution + fee/rebate math |
| Fee attribution + toxicity + **the actual fill application** | ~1,400 | the matcher settlement itself is *tiny* |
| PnL routing | 383 | |
| **After the handler body** | **~12,500** | `FillAppliedEvent` emit + Anchor **Borsh re-serialize** of every `mut` account on exit |

Steady-state (**close**, no init): ~26.5k CU is spent **before the handler even
starts** — pure Anchor Borsh deserialization of the 6 accounts.

**Verdict: ~45k of ~51k is the Anchor Borsh ser/deser framework, not our
logic.** The handler is ~6.6k. This empirically confirms the thesis and kills
the alternatives: optimizing handler logic, emits, or math is pointless — the
*entire* remaining win is in **account serialization**. The biggest single
contributors are `MarketAccount` (largest struct, deserialized in every ix) and
the 2× `Position`/`TraderState` (deserialized twice per fill).

## Phase 1 — the 70% lever: zero-copy / Quasar migration

Research (SPL-token): Pinocchio cuts **88–95%** of CU, **~70% from just the
entrypoint + zero-copy** account access (killing Borsh). Two routes:

1. **Anchor `#[account(zero_copy)]` + `AccountLoader`** — incremental, stays
   in Anchor. Convert one account at a time. Each conversion: make the struct
   `repr(C)` + `Pod` (no `Option`/`Vec`/`String` — use sentinels/fixed
   layout), swap `Account<T>`→`AccountLoader<T>`, change `acct.field`→
   `acct.load()?`/`load_mut()?`. Lowest risk; ~700 CU per account per ix.
2. **[Quasar](https://github.com/blueshift-gg/quasar)** — `no_std`, accounts
   pointer-cast directly from the SVM input buffer (no deser/heap/copy), but
   keeps Anchor-like `#[program]`/`#[derive(Accounts)]` ergonomics + ships CU
   flamegraphs. This is the path to **Pinocchio-tier** CU while preserving the
   programming model flash-book already uses. Bigger blast radius.

The matching engine (`state_v2.rs` hypertree) is **already zero-copy** — this
work is about the *risk/settlement* accounts. Reference: Manifest
(hypertree CLOB); Flash Book is already hypertree-lineage, so the data
structure is right.

### Proposed order (each step measured + must keep 569 tests + Kani green)

1. **Pilot:** convert `MarketHaircutStateAccount` + `PositionHaircutStateAccount`
   to `zero_copy` (already near-Pod; isolated to the haircut path the Kani
   proofs cover). Proves the pattern end-to-end and de-risks the rest.
2. `InsuranceFundAccount`, `FeeTiersAccount` → `zero_copy`.
3. `PositionAccount`, `TraderStateAccount` → `zero_copy` (2 deser each in
   `apply_fill`; biggest single win).
4. `MarketAccount` → `zero_copy` (largest; touched program-wide — highest churn,
   do last).
5. **Evaluate Quasar** for the entrypoint + remaining copies once accounts are
   zero-copy — the last ~20–30% toward Pinocchio-tier.

### Targets (rough, to be measured)

`apply_fill` open from **42k → ~15–20k CU** after steps 1–4; lower with Quasar.

### Guardrails

- One account per PR; re-run `cu_benchmark` and post the before/after delta.
- 569 cargo tests + 5 Kani proofs + `build-sbf` must stay green every step.
- No instruction/IDL signature changes where avoidable; SDK updated in lockstep
  when an account layout changes.
- **Needs sign-off before starting** — this is multi-session and touches the
  whole settlement layer.
