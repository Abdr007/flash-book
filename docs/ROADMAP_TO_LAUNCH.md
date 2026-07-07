# Flash Book — Roadmap to Launch

The complete pre-launch item register. Every item has an **owner**, a **track**, a
**status**, and the **evidence artifact** that closes it. Status values:

- **DONE** — closed with a committed, verifiable artifact (proof / test / decoded tx).
- **CODE-COMPLETE, VENDOR-WAIT** — nothing left for us; blocked only on an external signature.
- **STARTABLE** — scoped and code-closable; not yet started.
- **SCOPE-PENDING** — only a title exists in the source plan; the full scope text is
  required before execution (flagged so nothing is silently invented).

The North-Star claim every item serves — and which must remain literally provable:

> *Every money-moving instruction is machine-proven to preserve solvency, on a fully
> on-chain order book with sub-50ms fills, and the Hyperliquid-$20M oracle
> manipulation is proven impossible.*

No public word ships that we cannot prove on demand.

---

## Gap resolution (4.7 / 5.3 / 5.5 — flagged absent from the source table)

Resolved from the source-plan prose (they exist; they were missing only from the table):

- **4.7 — Robust-median mark for funding/display.** Mark used for funding + display =
  median of {oracle+EMA blend, on-book mid, external reference}; **liquidation keeps the
  strict worse-of** (never the median). STARTABLE.
- **5.3 — Copy-trading.** Off-chain snapshot-diff mirroring ported from the Flash V2
  `examples/copy-trade` pattern to Flash Book endpoints; on-chain copy-vaults are the
  separate big-build track. STARTABLE (off-chain).
- **5.5 — Builder codes + sub-accounts + referrals.** DONE — consent flow, 1–255
  sub-accounts, anti-griefing referrals already shipped; remaining work is docs + SDK
  exposure (folded into 5.6).

---

## 1 · Formal verification (1.x)

| Item | Scope | Owner | Status | Evidence |
|---|---|---|---|---|
| 1.1 | Certora integration (the long pole) | eng | STARTABLE | Certora specs green in CI — *not yet integrated; current FV is Kani (59) + Lean* |
| 1.2 | real-engine money-path proof | eng | SCOPE-PENDING | need source text |
| 1.3 | money-path proof | eng | SCOPE-PENDING | need source text |
| 1.4 | real-engine money-path proof | eng | SCOPE-PENDING | need source text |
| 1.5 | money-path proof | eng | SCOPE-PENDING | need source text |
| 1.6 | code-explanation-only comment hygiene (no audit tags/provenance) | eng | STARTABLE | grep audit shows 0 provenance comments |
| 1.7 | money-path proof | eng | SCOPE-PENDING | need source text |
| 1.8 | same-day win | eng | SCOPE-PENDING | need source text |

Baseline already in place: **Kani 59 proofs + Lean (haircut / OiMmr / funding at real
divisors)**, both CI-gated. See `docs/FORMAL_VERIFICATION.md`.

## 2 · Dormant paths / audit gaps (2.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 2.1–2.3 | dormant-path / gap closures | eng | SCOPE-PENDING |
| 2.4 | gap closure | eng | SCOPE-PENDING |
| 2.5 | final gate | eng | SCOPE-PENDING |
| 2.6 | built pre-launch (see big-builds) | eng | SCOPE-PENDING |
| 2.7 | same-day win | eng | SCOPE-PENDING |

## 3 · Manipulation-proof / percolator (3.x)

| Item | Scope | Owner | Status | Evidence |
|---|---|---|---|---|
| 3.1 | per-domain credit + proofs (percolator upgrade) | eng | STARTABLE (big build) | proof of per-domain isolation |
| 3.2 | anti-self-liquidation proof (marquee) | eng | STARTABLE | proof: oracle manipulation cannot self-liquidate |
| 3.3 | final gate | eng | SCOPE-PENDING | — |

## 4 · Techniques / HL-feature parity / hygiene (4.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 4.1 | same-day win | eng | SCOPE-PENDING |
| 4.2 | same-day win | eng | SCOPE-PENDING |
| 4.3 | technique/feature | eng | SCOPE-PENDING |
| 4.4 | technique/feature | eng | SCOPE-PENDING |
| 4.5 | technique/feature | eng | SCOPE-PENDING |
| 4.6 | same-day win | eng | SCOPE-PENDING |
| **4.7** | **robust-median mark** (median for funding/display; worse-of for liquidation) | eng | **STARTABLE** |
| 4.8 | apply intake IM gate to v3 injection paths (TWAP/iceberg/bracket) | eng | STARTABLE |

## 5 · Product / integration (5.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 5.1 | FLP on-book AMM tuning + quoter spec (Avellaneda-Stoikov) | eng/quant | DONE (engine) → STARTABLE (devnet param sweep + spec) |
| 5.2 | segregate MM vault from insurance backstop (prove isolation) | eng | STARTABLE (verify + prove) |
| **5.3** | **off-chain copy-trading** (snapshot-diff mirror) | eng | **STARTABLE** |
| 5.4 | activate a real paying maker-rebate schedule (negative-fee tier) | eng | STARTABLE (code exists, disabled) |
| **5.5** | **builder codes + sub-accounts + referrals** | eng | **DONE** (docs/SDK exposure → 5.6) |
| 5.6 | agent-native SDK (typed REST, AGENTS.md, llms.txt, OpenAPI, GOTCHAS.md) | eng | STARTABLE |

## 6 · Ship gates (6.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 6.1 | external audit | owner | CODE-COMPLETE, VENDOR-WAIT (package ready; engage) |
| 6.2 | multisig authority migration + fill-commitment-v1 upgrade | owner | STARTABLE (ops) |
| 6.3 | GPL-vs-MIT license decision (vendored hypertree GPL in MIT repo) | owner/legal | STARTABLE (legal read) |
| 6.4 | latency benchmark, disclosed methodology (tx sigs + CU + timing) | eng | STARTABLE |
| 6.5 | pre-commit algorithmic settlement policy (one-pager) | owner | STARTABLE |
| 6.6 | honest launch framing (devnet + freshly-audited) | owner | STARTABLE |

## Big builds (first-class pre-launch tracks)

| Build | Status | Evidence to close |
|---|---|---|
| Copy-vaults (on-chain) | STARTABLE (big build) | vault program, conservation-proven, live-verified |
| Permissionless market deploy (HIP-3) | STARTABLE (big build) | front-run-safe inits, rent/lifecycle correctness, proven |
| Decentralized sequencer | CODE present, activation STARTABLE | BFT committee gates settlement + trustless censorship-exit; MagicBlock owner-recovery = vendor-wait. See `docs/DECENTRALIZED_SEQUENCER.md` |

---

## Already shipped (foundation the register builds on)

- **D19 event-replay reconciler — all 8 state dimensions self-reconstruct from events
  byte-for-byte** (collateral incl. fees, funding, positions, OI, book via hypertree
  slab decode, insurance, FLP, haircut). Merged (#267).
- **Conservation sequence-fuzzer** (solvency + two-sided OI after every step),
  **differential-vs-V2 grid** (exact i128 over 1200 combos), **N-position assess_margin
  sweep** (3200 portfolios), **oracle-parser fuzzers** (pyth+lazer). Merged (#268).
- **WITHDRAW-ANYTIME** reserve-margin gate on all release paths (#254).
- Funding keeper, reduce-only/close-only, MAX_POSITIONS intake gate, fee + insurance
  events (deployed + live-verified on devnet).
- Kani 59 + Lean proofs; audit-2026-06 remediation; TEE dark-pool privacy; session keys.

## Launch gate

Every item above closed with its evidence artifact, and full green: fmt 0 · clippy -D 0
· cargo test ≥ baseline · Kani ≥ prior · Certora/Lean green · build-sbf --tools-version
v1.52 clean · IDL-drift 0 · cargo-audit 0 · SDK green · devnet + chaos green. The only
remaining waits are the two honest vendor dependencies — the audit firm's signature and
MagicBlock owner-recovery — both code-complete on our side.

## Honest status of this register

The SCOPE-PENDING items (1.2–1.5, 1.7–1.8, 2.1–2.7, 3.3, 4.1–4.6) carry only titles in
the source plan; their full scope text is required before execution so nothing is
invented. Certora (1.1), the percolator upgrade (3.1), and the three big builds are
multi-week efforts. 6.1 and 6.3 are external/vendor gates. This document is the
authoritative tracker; each item closes only against its evidence artifact.
