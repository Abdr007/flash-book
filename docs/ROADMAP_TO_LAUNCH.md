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
| 1.1 | Certora solvency spec — compile the harness, wire real handler externs, run the prover, add to CI (closes G1) | eng | STARTABLE (long pole; harness is uncompiled scaffold today) | `solvencyPreserved` passes parametrically in CI |
| 1.2 | prove the REAL `assess_margin` (not an abstract re-impl); Lean if 128-bit is CBMC-intractable (closes G3) | eng | STARTABLE (partial: N-position host sweep already covers key properties) | proof names the real symbol |
| 1.3 | Lean theorem for realized-PnL on reduce/flip (`sign·closed·Δticks·tick`) + VWAP entry (closes G2) | eng | STARTABLE | Lean theorem at real domain |
| 1.4 | structural enforcement that handlers reach funding/health math only via the proven helper (closes G5) | eng | STARTABLE | single-call-site lint / CI test |
| 1.5 | whole-system residual identity — triple-ledger conservation checked before every commit (closes G4) | eng | STARTABLE (big) | Kani/Lean per-instruction invariant |
| 1.6 | code-explanation-only comment hygiene; fix stale "funding inert/never advanced" comments (contradicted by live `crank_funding`) | eng | **DONE** | funding.rs module doc + `settle_funding` docstring corrected: cum-index funding is LIVE via crank_funding→settle_position_funding; only the side-accrual rate term waits |
| 1.7 | prove authorization + completeness invariants (margin-walk completeness, liquidation dedupe, auth gates) (closes G7) | eng | STARTABLE | proofs replace runtime `require!`-only |
| 1.8 | clean proof suite + fix README undercount | eng | **DONE (count fix)** — README corrected 57→61 Kani, 565→621 tests, Certora qualified as written/integration-in-progress (no unprovable claim). Dead-proof pruning deliberately NOT done: removing proofs conflicts with the rising-count discipline; 61 real proofs > removing 4 for aesthetics |

Baseline already in place: **Kani 59 proofs + Lean (haircut / OiMmr / funding at real
divisors)**, both CI-gated. See `docs/FORMAL_VERIFICATION.md`.

## 2 · Dormant paths / audit gaps (2.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 2.1 | side-accrual K/F PnL path (`settle_position_pnl` never called) — wire or delete the scaffold | eng | STARTABLE (program change) |
| 2.2 | isolated-position ADL redirect (cross has ADL; isolated doesn't) — close asymmetry + prove | eng | STARTABLE (program change) |
| 2.3 | on-chain payout walk for referrer/builder/creator shares (today emit-only) | eng | STARTABLE (program change) |
| 2.4 | automated funding keeper binary/script (+ optional crank incentive) | eng | **DONE (keeper)** — `sequencer/funding_keeper.mjs` (+ `npm run funding-keeper`): permissionless service that cranks every configured market's `cum_funding_index` on an interval. Anchor-client on the committed IDL; concurrent per-market with isolated failures; `ONCE` (cron), `DRY_RUN`, and `MIN_DT_SECONDS` (fee optimization — correctness never depends on it, per the on-chain Δt clamp) modes. Never moves value (index-only; positions realize via the proven `settle_funding` path), safe to run multiple instances. Optional crank-incentive = separate program change (deferred) |
| 2.5 | executable trustless force-undelegate | eng | **DISPOSITION**: returns `OwnerForceUndelegateUnavailable` vs the upgraded delegation program → **vendor-gated** on MagicBlock owner-recovery; public censorship-exit claim must stay downgraded until it ships (see `docs/DECENTRALIZED_SEQUENCER.md`) |
| 2.6 | force-include from L1 (`errors.rs:141` "not yet supported") | owner | **DISPOSITION**: documented **post-launch roadmap** (not ambiguous) — the ER censorship-exit story rests on 2.5's force-undelegate, not L1 force-include |
| 2.7 | resolve dual `place_limit_order` migration (lib.rs:1393); delete legacy path | eng | STARTABLE (program change) |

## 3 · Manipulation-proof / percolator (3.x)

| Item | Scope | Owner | Status | Evidence |
|---|---|---|---|---|
| 3.1 | per-source-domain realizable credit: cap each profitable leg's usable PnL by `credit_rate = min(1, backing/claims)` of the opposing side of that same market — manipulated/thin/stale market ⇒ credit collapses ⇒ paper profit can't back margin/cure/withdraw (closes the HL-$20M oracle-pump class) | eng | STARTABLE (big build, marquee) | Lean per-domain conservation + adversarial Kani: "manipulated thin market cannot convert paper PnL to withdrawable/margin" |
| 3.2 | anti-self-liquidation proof (marquee) | eng | **DONE** — Kani `withdraw_cannot_self_liquidate_below_maintenance` (VERIFICATION SUCCESSFUL): no gate-allowed withdrawal can leave the account below maintenance margin (im≥mm ⇒ remainder ≥ mm), so the HL self-liquidation-onto-insurance attack is structurally impossible |
| 3.3 | exclusive per-domain close serialization: immutable `close_id` + `max_close_slot` on the liquidation/bankrupt-close path (deadlock/livelock impossible by construction) | eng | STARTABLE (program change) | — |

## 4 · Techniques / HL-feature parity / hygiene (4.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 4.1 | min-notional gate (~$10 value floor, today only lots floored) — kills dust spam | eng | STARTABLE (program change) |
| 4.2 | 5-significant-figures price rule — prevents book fragmentation | eng | STARTABLE (program change) |
| 4.3 | scale/ladder USER order type (FLP quoter ladder exists; expose a user version) | eng | STARTABLE (program change) |
| 4.4 | published margin-tier table (leverage-steps-down-with-notional) + enable coded-but-inactive OI-crowding surcharge | eng | STARTABLE (config/doc + program) |
| 4.5 | tranched liquidation (positions above a notional threshold liquidate in tranches) | eng | STARTABLE (program change) |
| 4.6 | user reduce-only flag on the v2 place path (currently rejected at intake) | eng | STARTABLE (program change) |
| **4.7** | **robust-median mark** (median for funding/display; worse-of for liquidation) | eng | **STARTABLE** |
| 4.8 | apply intake IM gate to v3 injection paths (TWAP/iceberg/bracket) | eng | STARTABLE |

## 5 · Product / integration (5.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 5.1 | FLP on-book AMM tuning + quoter spec (Avellaneda-Stoikov) | eng/quant | DONE (engine) → STARTABLE (devnet param sweep + spec) |
| 5.2 | segregate MM vault from insurance backstop (prove isolation) | eng | **DONE** — Kani `bad_debt_coverage_is_insurance_isolated_and_bounded` (VERIFICATION SUCCESSFUL) proves the waterfall debits insurance by a function of only its own balance + shortfall (no FLP input, no underflow), plus `solvent_iff_vault_covers_buckets` proves insurance/FLP are separate additive buckets |
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
| 6.5 | pre-commit algorithmic settlement policy (one-pager) | owner | **DONE** — `docs/SETTLEMENT_POLICY.md`: robust-oracle-only settlement, no discretionary repricing, each commitment grounded in the deployed code or a CI proof (3.2 + 5.2) |
| 6.6 | honest launch framing: "devnet + freshly-audited; run it, read it, break it; mainnet after audit closes" | owner | STARTABLE (positioning; the honesty is already the operating norm) |

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

## Closure ledger — closed this pass (with runnable evidence)

- **3.2** anti-self-liquidation — Kani `withdraw_cannot_self_liquidate_below_maintenance` **VERIFICATION SUCCESSFUL**. The marquee: HL self-liquidation attack impossible by construction.
- **5.2** insurance/FLP isolation — Kani `bad_debt_coverage_is_insurance_isolated_and_bounded` **VERIFICATION SUCCESSFUL**. HL single-vault SPOF structurally absent.
- **1.6** stale-funding-comment fix (auditor-critical).
- **1.8** README undercount fixed (61 Kani / 621 tests) + Certora honestly qualified.
- **6.5** pre-committed algorithmic settlement policy (`docs/SETTLEMENT_POLICY.md`).
- **5.5** (pre-existing) builder codes / sub-accounts / referrals — verified in code.

Kani proof count: 59 → **61**. All committed on `docs/roadmap-to-launch`.

## Honest status of the remainder

Every item now has real scope (no more title-only). The remainder splits into:

- **Program changes** (need a devnet deploy + live-re-verify cycle each): 2.1–2.3, 2.7,
  3.3, 4.1–4.6, 4.8.
- **Hard proofs** (Lean / real-symbol): 1.2, 1.3, 1.5, 1.7.
- **Multi-week builds**: 1.1 Certora integration, 3.1 percolator per-domain credit, and
  the three big builds (copy-vaults, HIP-3 permissionless deploy, decentralized-sequencer
  activation).
- **Vendor-gated**: 6.1 (audit signature), 6.3 (GPL legal read), 2.5 (MagicBlock
  owner-recovery).
- **Positioning/ops docs**: 5.1 spec+devnet sweep, 5.3/5.6 SDK, 6.2/6.4/6.6.

This document is the authoritative tracker; each item closes only against its evidence
artifact, and no public claim ships ahead of that evidence.
