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

### Results (measured, fresh builds, apples-to-apples)

| Change | `apply_fill` open | Δ | Verdict |
|---|---|---|---|
| `find_program_address` fix (PR #4) | 55,502 → 42,093 | **−24%** | real, shipped |
| `TraderStateAccount` → zero-copy (PR #8) | 42,093 → 42,102 | **~0%** | merged, **no CU benefit** |
| `PositionAccount` → zero-copy (PR #9) | 42,102 → 37,779 | **−10.3%** | real |

### The key insight: Anchor `AccountLoader` is not free

Anchor's `AccountLoader::load()`/`load_mut()` does a `RefCell` borrow + discriminator
check + pointer cast **per call**. So per-account zero-copy only nets a win when
the **Borsh deser/serialize cost it removes exceeds the `load()` overhead it adds**:

- **PositionAccount won (−10%)** — `init_if_needed` on open avoids a full Borsh
  *serialize*, and close avoids 2× deser/reserialize, both larger than the load
  overhead.
- **TraderStateAccount netted ~0** — it's read/written so many times in
  `apply_fill` that the load overhead canceled the Borsh savings. (Its PR-#8 CU
  claim was a stale-`.so` artifact; corrected here.)
- **`MarketAccount` is therefore a likely *loss*** — it's the most heavily
  accessed account (`market.params.*` read dozens of times per ix), so an
  `AccountLoader` conversion would add the most load overhead. **Not pursued.**

**Conclusion: the per-account Anchor-zero-copy lever is largely exhausted.** The
remaining ~45k of `apply_fill` CU is the Anchor framework (entrypoint + the Borsh
on the accounts that don't benefit from `AccountLoader`). Capturing it *without*
per-access `load()` overhead requires the entrypoint + true pointer-cast accounts —
i.e. **Quasar / Pinocchio**.

### Two zero-copy gotchas (proven in PR #9 — reuse these)

- **No native `u128`/`i128` in a zero-copy account.** It forces 16-byte struct
  alignment, but Anchor zero-copy data begins at disc offset **+8** (8-aligned
  only) → `bytemuck::from_bytes` panics at runtime. Store 128-bit fields as
  `[u8; 16]` with `i128` accessors. (`MarketAccount`'s funding index needs this.)
- **`init_if_needed` discriminator.** `AccountLoader` writes the discriminator
  only in `exit()`, so a freshly-created account has a zero disc *during* the
  handler → `load()` fails `AccountDiscriminatorMismatch`. Stamp it immediately
  (`stamp_zc_discriminator` in `lib.rs`).

### Recommended next step

**Pivot to the Quasar/Pinocchio track** (custom entrypoint + pointer-cast
accounts) rather than converting more individual accounts — that is where the
remaining ~70% lives, and it avoids the `load()`-overhead ceiling. This is a
multi-session, sign-off-gated effort (new program structure; must re-pass 569
tests + 5 Kani proofs + build-sbf).

### Guardrails

- One account per PR; re-run `cu_benchmark` and post the before/after delta.
- 569 cargo tests + 5 Kani proofs + `build-sbf` must stay green every step.
- No instruction/IDL signature changes where avoidable; SDK updated in lockstep
  when an account layout changes.
- **Needs sign-off before starting** — this is multi-session and touches the
  whole settlement layer.
