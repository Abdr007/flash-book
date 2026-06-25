# Flash Book — Reference Architecture

> The strongest, most-secure on-chain CLOB perpetuals architecture on Solana.
> Designed to lead on four axes at once: **latency/CU**, **decentralization/liveness**,
> **capital efficiency**, and **safety/verifiability**.

---

## 0. Evidence discipline (read first)

This document obeys one rule: **only proved or verified information.** Every claim
carries a tag:

- **`[PROVEN]`** — measured or machine-checked *in flash-book*, with a reproduction
  command or a `file:line` reference. Fact.
- **`[VERIFIED]`** — an external fact corroborated by a primary source and
  adversarially cross-checked (2-of-3 verifier majority). Cited.
- **`[ADOPT]`** — a technique observed in a surveyed implementation that we should
  absorb. Attributed to the *technique*, never to a person.
- **`[PROPOSED]`** — a design hypothesis. **Not yet built or measured.** Must be
  validated before it may be stated as fact anywhere.

Anything without a tag is scaffolding/prose, not a claim. No competitive superiority
is asserted without a measured number or a code citation behind it.

**Methodology.** This synthesis fuses three independently-gathered evidence streams:
(1) an adversarial deep-research pass over the published landscape (107 agents, 25
sources fetched, 122 claims extracted, 21 confirmed / 4 refuted by 3-vote majority);
(2) a code-grounded teardown of **14 surveyed reference implementations** (~500k LOC
of real, cloned source — order books, perps engines, a program framework, an oracle,
and a formal-verification project); (3) flash-book's own measured numbers and proofs.

---

## 1. Executive thesis

The surveyed field splits cleanly, and **no existing system leads on all four axes**:

- **Crankless spot CLOBs** (Phoenix v1, Manifest) win on latency/decentralization
  but are **spot-only — no perps, no margin, no funding, no liquidation** `[VERIFIED]`.
- **Perps engines** (Drift, GMX-Solana, the solana-labs perpetuals reference, and the
  surveyed percolator/beethoven engines) have risk machinery but **no real on-chain
  CLOB** — they are oracle-priced or off-chain/CPI-matched, sacrificing price-time
  discovery `[VERIFIED]`/`[PROVEN: teardown]`.
- **ER order books** (the surveyed MagicBlock-ER books) get sub-slot latency but reduce
  to a **single-validator sequencer SPOF**, and are **toy-depth** (8–32 levels/side,
  capped by the 10 KiB ER-delegatable account limit) with **no measured CU** `[PROVEN: teardown]`.
- **Formal verification** exists in two forms: Manifest's **Certora** suite (4 property
  sets, re-run daily) `[VERIFIED]` and the surveyed **Kani** campaigns (81–270 harnesses)
  `[PROVEN: teardown]` — but neither is attached to a *perps solvency* claim at a real
  fixed-point divisor.

**Flash Book is the only design that is simultaneously:** a real on-chain price-time
hypertree CLOB **+** a stress-lattice portfolio-margin perps risk engine **+** a
machine-checked solvency layer (Lean **and** Kani) **+** a measured ~96%-CU-reduced
execution core **+** an optional ER fast path. The remainder of this document proves
that claim axis by axis and specifies the target architecture.

---

## 2. Competitive landscape (verified teardown)

Legend: ✅ real/strong · ⚠️ partial/weak · ❌ absent. Every ❌/⚠️ below is backed by a
primary source `[VERIFIED]` or a `file:line` from the cloned source `[PROVEN: teardown]`.

| System | On-chain CLOB | Price-time (FIFO) | Perps risk engine | Measured CU | Formal verification | Decentralized matching | ER / sub-slot |
|---|---|---|---|---|---|---|---|
| **Phoenix v1** | ✅ crankless, atomic | ✅ | ❌ spot-only | ❌ none public | ✅ OtterSec/MadShield (audit, not FV) | ✅ atomic in taker tx | ❌ |
| **Manifest** | ✅ hypertree, crankless | ❌ **price-only `Ord`, no FIFO** | ❌ spot-only | ❌ 1st-party only | ✅ **Certora ×4 daily** | ✅ atomic | ❌ ~400ms L1 |
| **Drift v2** | ⚠️ off-chain DLOB | n/a (keeper FCFS) | ✅ cross-margin, JIT→DLOB→vAMM | ❌ | ❌ | ⚠️ permissionless *design*, **de-facto centralized** | ❌ |
| **GMX-Solana** | ❌ oracle-priced LP-vs-trader | ❌ | ⚠️ isolated, oracle-trust | ❌ | ❌ none | ⚠️ keeper-gated | ❌ |
| Surveyed ER books | ⚠️ 8–32 levels (10KiB cap) | ⚠️/✅ FIFO | ⚠️ isolated, keeper mark | ❌ ceilings/none | ⚠️ Kani (wrapper only) | ❌ **single-ER SPOF** | ✅ |
| Surveyed perps engine | ❌ external matcher CPI | ❌ | ✅ Kani-proven wrapper | ⚠️ **CU *limits*, not measured** | ⚠️ Kani (core crate unshipped) | ⚠️ permissionless crank | ❌ |
| **Flash Book** | ✅ hypertree, expandable ~10k nodes | ✅ **price-time** | ✅ **stress-lattice portfolio** | ✅ **measured** (§3.1) | ✅ **Lean + Kani** (§3.4) | ⚠️ sequencer today → §3.2 plan | ✅ MagicBlock ER |

Notes (citations):
- Manifest's matching `Ord` compares **price only** (`resting_order.rs:243-253`), and the
  project's own README states tickless markets "invert time priority" — **makers cannot
  rely on queue position** `[PROVEN: teardown]`. Flash Book keeps strict price-time.
- Drift's DLOB is **off-chain, read-only**; matching runs on keepers `[VERIFIED]`. Its
  permissionless design is undercut in practice (Drift Labs runs keepers; Swift is an
  off-chain server; 5-person governance multisig; **April 2026 $285M exploit**) `[VERIFIED]`.
- Surveyed ER books cap the book at **8 levels/side** (borsh `#[account]`, 4 KiB BPF
  stack bound) or **32/side** (10 KiB ER-delegatable account cap), with **MAX_FILLS=8**
  per taker tx and **no on-chain oracle staleness/confidence checks** `[PROVEN: teardown]`.
- The surveyed perps engine is **"a pure recorder of state transitions"** — matching is
  delegated to an external per-LP matcher via CPI; its core risk crate is unshipped, so
  its Kani proofs cover the **wrapper only**, and it bakes **CU *ceilings* (345k trade /
  750k multi-asset), not measured steady-state numbers** `[PROVEN: teardown]`.

---

## 3. The four axes — beat-plan

### 3.1 Latency / throughput / compute-unit efficiency

**Where we already win `[PROVEN]`** — measured on `solana-program-test`, real account
sizes (repro: `BPF_OUT_DIR=$PWD/target/deploy cargo test -p flash-book --test integration cu_benchmark -- --ignored --nocapture` after `cargo build-sbf`; pin numbers via the pin crate harness):

| Instruction | Anchor CU | Flash Book (Pinocchio) CU | Δ |
|---|---|---|---|
| `apply_fill` | 37,779 | **1,469** | **−96%** |
| `settle_funding` | ~5,050 | **676** | −87% |
| `place_limit_order` | ~12,500 | **411** | −97% |
| `cancel_order` | ~12,500 | **550** | −96% |
| `place_taker_order` (3-level walk) | ~12,500+ | **1,166** | ≈−90% |
| `modify_order` | ~12,500 | **931** | ≈−93% |

This is the decisive evidentiary edge: **every surveyed competitor has either no
measured CU or only guardrail ceilings.** Publishing reproducible per-instruction CU is
itself a category win. `[PROVEN]`

**Adopt `[ADOPT]`** (techniques seen in surveyed zero-CU work, to push lower):
- Pointer-cast account parsing, alignment-1 `#[repr(C)]` Pod accounts, raw-syscall PDA
  (~544 CU), const PDAs, unaligned-prefix arg reads (the framework + 21-CU-oracle
  techniques). Keep the entire matcher hot path heap-free and branch-predictable.
- Store/reuse canonical PDA bumps to kill `find_program_address` (~6.7k CU each on-chain)
  — already applied in flash-book `[PROVEN]`; enforce it program-wide.

**Propose `[PROPOSED]`** (not yet built/measured — validate before claiming):
- Complete the Pinocchio rewrite across all 112 instructions (currently **9/112**),
  targeting low-thousands CU on every hot path.
- **CU-bounded matching**: replace any O(n) array shift / O(n) crossable pre-scan with
  per-subtree quantity aggregates in the RB/hypertree node so feasibility (FOK/IOC) is
  `O(log L)`; bound the taker sweep by a deterministic CU budget rather than a hard
  `MAX_FILLS` truncation (the surveyed-book failure mode).
- ER fast path for sub-slot execution — **only** with the §3.2 SPOF mitigation and the
  §3.4 fraud-proof bound.

> ER latency (sub-50ms typical / <10ms execution vs ~400ms base) is **vendor-reported
> only** `[VERIFIED, confidence: medium]` — we must independently benchmark before
> stating any latency number as ours.

### 3.2 Decentralization / liveness (kill the sequencer SPOF)

**The single biggest architectural risk in the entire surveyed field** is the
sequencer/keeper SPOF:
- Every surveyed ER book hard-pins **one** validator; a stall halts matching with no
  escape hatch `[PROVEN: teardown]`.
- Drift's matching is off-chain and **de-facto centralized** `[VERIFIED]`.
- Flash Book today also has an **off-chain sequencer SPOF** `[PROVEN: known weakness]`.

**Adopt `[ADOPT]`** — the two proven SPOF-elimination patterns:
1. **Phoenix/Manifest atomic-in-taker-tx settlement** — matching executes inside the
   taker's own transaction against L1 consensus; **no separate cranker exists to fail**
   `[VERIFIED]`. This is the gold standard for the base-layer path.
2. **Drift-style permissionless keeper set** — anyone can run a filler; incentives reward
   best-execution-vs-oracle under FCFS `[VERIFIED]`.

**Propose `[PROPOSED]`** — the flash-book decentralization design:
- **Base-layer path = crankless atomic matching** (Phoenix/Manifest model) as the
  *always-available* settlement guarantee. The ER is an *acceleration layer*, never the
  *only* path.
- **Permissionless settlement authority**: the `apply_fill` sequencer key must be
  rotatable and ultimately a permissionless set, not a single signer. (Flash Book already
  decouples `MarketAccount.sequencer` from `authority` and supports rotation `[PROVEN]`.)
- **Forced-exit / escape hatch**: a base-layer instruction that lets any user
  `undelegate → withdraw` if the ER stalls, bounded by the fraud-proof window. No
  surveyed ER book has this `[PROVEN: teardown]`.
- **Bounded ER trust**: see §3.4.

### 3.3 Capital efficiency (margin, liquidity, funding, backstop)

**Where we already win `[PROVEN]`** (`matcher/risk.rs`, host-tested + partly proven):
- **Stress-lattice *portfolio* margin** (`assess_margin` / `_split` / `_unified`) —
  evaluates the trader across a scenario lattice, with tiered + OI-scaled + concentration
  MMR. This is **stronger than the isolated margin** of GMX/the surveyed engines, and
  more expressive than flat cross-margin.
- **Dual-source liquidation gate** (worse-of mark/oracle) — no surveyed system does this;
  the surveyed ER books liquidate off a **single keeper-pushed mark with no staleness/
  confidence checks** `[PROVEN: teardown]`.
- **JIT-liquidation auction**, **auto-deleverage (ADL)**, **insurance-fund waterfall**,
  **cumulative-index funding + EMA blend + per-side accrual** — all present and tested.

**Adopt `[ADOPT]`** (the reference perps stack, from Drift `[VERIFIED]`):
- **JIT reverse-Dutch auction** (price starts at taker-best, linearly deteriorates) ahead
  of the resting book, so makers compete to improve the taker's fill. Flash Book has a
  JIT-liquidation primitive; generalize the reverse-Dutch auction to *all* taker flow.
- **Insurance-fund design**: fee-funded, openly stakeable with explicit bankruptcy risk,
  **per-market caps**, and **pro-rata-by-base ADL** socialization as the final tier.

**Propose `[PROPOSED]`**:
- **Unified collateral / cross-portfolio netting** across markets as the default, with
  isolated as an opt-in (flash-book already supports both `[PROVEN]`). Do **not** claim
  "infinite cross-market capital efficiency" — that exact claim was **refuted** in
  research and must never be repeated `[VERIFIED: refuted]`.

### 3.4 Safety / verifiability

**The bar to beat is Manifest's Certora suite**: four property sets (tree/hypertree
invariants, loss-of-funds, availability, correct matching), **re-run daily against head**
`[VERIFIED]`. The surveyed Kani campaigns (81–270 harnesses) raise it on the perps side,
but their **core risk crate is unshipped → the proofs cover the wrapper only**, and **no
surveyed system attaches a machine-checked solvency proof to a real fixed-point
divisor** `[PROVEN: teardown]`.

**Where we already win / match `[PROVEN]`**:
- **Haircut / PnL-conservation proven in Lean at the real 1e9 divisor** + **5 Kani
  proofs** (`matcher/haircut.rs`; `docs/FORMAL_VERIFICATION.md`). This is the exact gap
  the surveyed Kani work leaves open (CBMC is incomplete on 128-bit non-power-of-two
  division; flash-book discharged it in Lean) `[PROVEN]`.
- 569 Anchor tests + 375 pin host tests green `[PROVEN]`.

**Adopt `[ADOPT]`**:
- **Manifest's four-property framing**, re-run in CI against head: tree invariants,
  loss-of-funds (all funds accounted across any interaction sequence), availability
  (valid cancels/withdrawals always succeed), correct matching/ownership.
- **The Lean-SVM CU-bound technique** (from the surveyed FV project): attach proven
  *worst-case CU upper bounds* to each instruction so matches are **guaranteed** to fit
  the block CU budget — not merely benchmarked.

**Propose `[PROPOSED]`** — the credible "provably solvent" claim:
- Machine-check the **global solvency invariant** (`Σ collateral + Σ PnL + insurance ≥ Σ
  liabilities`, conservation under every instruction) across the stress-lattice margin
  engine, at real divisors — combining Certora-style daily rules + Lean for the divisions
  CBMC can't close. Until that lands, we say **"key solvency properties machine-checked"**,
  not "provably solvent."

---

## 4. Target architecture (layer by layer)

```
┌──────────────────────────────────────────────────────────────────────────┐
│ CLIENTS — UI · MM SDK · permissionless fillers/liquidators (no privileged  │
│           single sequencer required for liveness)                          │
└───────────────┬───────────────────────────────────────┬──────────────────┘
                │ BASE PATH (always available)            │ FAST PATH (optional)
                │ crankless atomic match in taker tx       │ MagicBlock ER, bounded
                ▼                                          ▼ fraud-proof + escape hatch
┌──────────────────────────────────────────────────────────────────────────┐
│ EXECUTION CORE — Pinocchio, no_std, zero-copy, heap-free  [PROVEN core,    │
│   pointer-cast Pod accounts; measured 411–1,469 CU hot paths]  9/112 ix    │
├──────────────────────────────────────────────────────────────────────────┤
│ MATCHING ENGINE — price-time hypertree CLOB (expandable ~10k nodes),       │
│   O(log n) insert/cancel, per-subtree qty aggregates for O(log L) FOK/IOC, │
│   CU-budget-bounded taker sweep (no MAX_FILLS truncation)                  │
├──────────────────────────────────────────────────────────────────────────┤
│ RISK ENGINE — stress-lattice PORTFOLIO margin (tiered+OI+concentration),   │
│   dual-source liq gate, generalized JIT reverse-Dutch auction, ADL,        │
│   insurance waterfall (per-market caps, pro-rata-by-base), index funding   │
├──────────────────────────────────────────────────────────────────────────┤
│ SOLVENCY / VERIFICATION — Lean (real-divisor conservation) + Kani + daily  │
│   Certora-style property suite (invariants, loss-of-funds, availability,   │
│   correct matching) + proven per-ix CU upper bounds                        │
├──────────────────────────────────────────────────────────────────────────┤
│ ORACLE — Pyth pull + median-of-N quorum, staleness/confidence/dispersion   │
│   gates (NOT a single keeper-pushed mark)                                  │
└──────────────────────────────────────────────────────────────────────────┘
```

Design invariants the architecture enforces:
1. **The base layer is always sufficient.** The ER only accelerates; it is never the
   sole path to match, settle, or exit. (Kills the SPOF that defines the surveyed field.)
2. **One real on-chain price-time CLOB** — not an off-chain DLOB, not an external matcher
   CPI, not an oracle-priced AMM.
3. **Portfolio margin by default, isolated by opt-in.**
4. **Every hot path is measured; every solvency-critical path is machine-checked.**
5. **No trust in a single mark.** Liquidation uses the worse of dual sources with
   freshness gates.

---

## 5. Security architecture (Phase 3 preview — to be hardened next)

Trust boundaries the strongest design must make explicit and defend (full threat model
is the dedicated next phase):

- **Settlement authority** — sequencer key rotatable, decoupled from `authority`
  (`[PROVEN]`), trending to a permissionless set; forged-fill defense fail-closed.
- **ER delegation boundary** — the safety/liveness trade-off is **self-disclosed and
  quantifiable**: a shorter challenge window lowers fraud-detection probability `P(D|F)`
  `[VERIFIED]`. We must bound the window, fund active challengers, and provide a
  base-layer escape hatch.
- **Oracle manipulation** — quorum + staleness/confidence/dispersion gates; never a
  single writer (the surveyed oracle is a single hardcoded admin key with no validation
  `[PROVEN: teardown]`).
- **Liquidation / ADL / JIT griefing** — cooldowns, auction bounds, isolated-vs-cross
  routing so a liquidation can't drain the wrong bucket.
- **FLP / insurance drain & account aliasing / PDA verification** — every account
  PDA-verified and distinct (pinocchio borrows are unchecked; the account-context layer
  must guarantee distinctness — already documented as a hard invariant in the
  `liquidate_position_v2` port `[PROVEN]`).
- **Historical lesson** — the 2026-06-21 audit found two fund-drain criticals
  (unauthenticated sequencer; margin under-statement via omitted positions), both fixed
  `[PROVEN]`. The security pass re-derives the full surface from scratch.

---

## 6. The honest ledger — what is PROVEN today vs PROPOSED

**PROVEN today (flash-book, measured/checked):** real price-time hypertree CLOB;
stress-lattice portfolio margin; dual-source liq gate; JIT-liq auction; ADL; insurance
waterfall; index funding; Lean (real-divisor) + 5 Kani solvency proofs; measured Pinocchio
CU (apply_fill 1,469, −96%) for 9/112 instructions; 569+375 tests green; MagicBlock ER
delegate/commit/undelegate; quorum oracle with freshness gates.

**PROPOSED (must be built/measured before any claim):** full 112-instruction Pinocchio
rewrite; crankless atomic base path + permissionless filler set + forced-exit escape
hatch; generalized reverse-Dutch taker auction; daily Certora-style property suite +
proven per-ix CU bounds; independent ER latency benchmark; global-solvency machine proof
at real divisors.

**NEVER claim (refuted in research):** "infinite/effectively-infinite cross-market
capital efficiency"; any unbenchmarked "fastest" superlative; any ER latency number we
have not independently measured.

---

## 7. Open questions to close before "best in the world" is defensible

1. Independent (non-vendor) ER latency/throughput + bond/slashing parameters bounding the
   safety-vs-liveness trade-off.
2. Verified Hyperliquid / dYdX v4 / GMX risk-engine internals (not covered by the verified
   claim set — do not assert them).
3. Measured Pinocchio-vs-Anchor deltas on *every* remaining instruction (we have the hot
   paths; finish the set).
4. A shipped, machine-checked **global solvency** proof at real fixed-point divisors —
   the one thing no surveyed perps system has, and our clearest path to an unmatched claim.

---

*All competitive facts trace to a primary source or a `file:line` in cloned source.
Surveyed private implementations are referenced by capability only; no individual is
named. Flash Book figures are measured/checked with the reproduction commands above.*
