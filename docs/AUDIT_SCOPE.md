# Flash Book — Audit Scope & Readiness

Single entry point for an external auditor. Frames the branch, points at the
evidence, and is **honest about residuals** — nothing here claims more than the
code proves.

## 1. Scope

- **Program:** `programs/flash-book` (the deployed **Anchor 0.31.1** program,
  ~32.9k LOC). The Pinocchio/no_std port (`programs/flash-book-pin`) is WIP and
  **out of scope**.
- **Review surface:** `main` HEAD. All hardening + audit-remediation work is now
  merged (PRs #34–#42); review the whole program at `main` (no review branch).
- **Build/test gate (CI, green):** `cargo build-sbf`; `cargo test` (405 lib + 57
  integration + full proptest/wave suites); `cargo kani` (**41 harnesses**); Lean
  `lake build` (Haircut/Funding/OiMmr, axiom-clean). See `.github/workflows/ci.yml`
  — a regression in any of these fails CI.

## 2. What changed (review focus)

### 0. Audit-2026-06 remediation — latest, MERGED (`docs/AUDIT_REMEDIATION_2026-06.md`, `docs/INTERNAL_AUDIT_2026-06.md`)
A fresh adversarial pass (PRs #40/#41/#42) closing **every reachable Critical/
High/Medium**, each with a discriminating regression test; the two highest-value
fixes are **machine-proven** (Kani):
- **C-1 (CRITICAL, margin):** `assess_margin` double-counted unrealized PnL
  (mark frame in equity + entry frame in scenario loss) — under-margined winners
  (bad debt) and wrongly liquidated solvent losers. Now gates on
  `(collateral − funding) ≥ required`. Proven stress-sound + frame-independent.
- **H1:** `order_id` 24-bit seq could wrap → price-time-priority break + id
  collision. Fail-loud guard at the book-insert chokepoint; proven to admit
  exactly the bound the encoding proofs assume.
- **F1/F2/F3 (ER liveness):** censorship-escape pre-upgrade trap (permissionless
  baseline stamp); and a **sequencer-authenticated ER heartbeat** so a quiet
  healthy market is no longer auto-paused / force-undelegated on a normal lull —
  two-tier `force_undelegate` (fast=ER-dark, slow censorship backstop) proven
  non-griefable while preserving the F1 escape.
- **F4:** oracle-only liquidation fallback now requires provable oracle freshness.
- **M1/M2:** crossed-residual on walk-limit; pro-rata maker overfill.
**Auditor focus:** the margin-frame fix (C-1) backs every solvency decision —
confirm the entry-frame gate is exhaustive across all call sites; and the ER
heartbeat trust model (only the sequencer can attest liveness).

The three bodies below are EARLIER merged work; the **first two touch funds and
settlement and deserve the most scrutiny**.

### a. Security hardening — 2 CRITICAL + 10 HIGH (`docs/FLASHBOOK_SECURITY.md`)
Closed C1/C2 + H1–H9 (deposit-vault binding, margin-walk completeness, duplicate-
liquidation guards, reduce-only rejection, oracle-band mark clamp, bad-debt
waterfall, haircut residual crediting, FLP min-hold, convert-position credit,
settlement replay guard). §11 of that doc is the remediation ledger with fix
commit hashes.

### b. Settlement authenticity — #35 (`docs/FLASHBOOK_SETTLEMENT_COMMITMENT.md`)
Removes the sequencer's ability to fabricate fills.
- **Book fills:** consume-and-clear keccak **commitment ring** — the matcher
  commits each crossed fill on-chain (`place_taker_order_v2`); `apply_fill` may
  only settle a matching, oldest-pending entry. Account rides via
  `remaining_accounts` (truly optional; existing clients unaffected).
- **FLP fills:** **oracle-band bound** (≤20% of the fresh oracle) — a *bound*, not
  exact quote re-derivation (the doc explains why exact is unsound: quoter inputs
  drift between ER quote-time and L1 settle-time).
- **Auditor focus:** the trust boundary is the **ER validator set** on the ER path
  (stated, not "trustless"); review the commitment preimage (producer vs consumer
  must hash identically — covered by an e2e test) and the band width choice.

### c. Book-stuffing DoS — #36 (MEDIUM, 2 of 3 fixes)
Resting-order **price-band** (within 50% of oracle) + **permissionless
expiry-reaper** (`reap_expired_orders`, only touches genuinely-expired GTT orders).
**Residual (open):** does not stop a sybil; the per-trader cap is deliberately NOT
implemented (it would consume the node arena it protects — see issue #36).

## 3. Formal-verification evidence (`certora/PROPERTIES.md`, `docs/FORMAL_VERIFICATION.md`)

**41 Kani harnesses + Lean (Haircut/Funding/OiMmr, axiom-clean), all CI-gated.**
Each binds to deployed code (handlers route through the proven pure function).
2026-06 additions: C-1 stress-soundness + frame-independence, H1 seq-guard ⇔
encoding precondition + collision-freedom, F3 force-undelegate soundness +
anti-grief. Proven: P-SOLV-1…5,
P-FUND-1/2, P-MARGIN-1/2/3, P-SETTLE-1/2, P-LIQ-1, P-MATCH-1/2. Where CBMC can't
reach (128-bit multiply, non-power-of-two division), the proof is in **Lean** over
unbounded `Nat`/`Int` at the real constants.

**Explicitly NOT proven (whole-program — `[CERTORA-TARGET]`, need the Certora
Prover):** P-MARGIN-4 (margin-walk exhaustive vs. on-chain position set),
P-SETTLE-3 (no settlement path bypasses the sequencer gate), P-LIQ-2 (no duplicate
liquidation across instructions), and P-SOLV-4/5 *beyond* their proven pure cores
(the whole-program identity preserved by every instruction). **These are prime
audit targets** — they are enforced by `require!`/constraints today, not proven.

## 4. Performance evidence

CU on real SBF metering (`cu_benchmark_settlement_and_risk_paths`): `apply_fill`
42k, `partial_withdraw` 48k, `place_taker` 14k — all well within the 200k per-ix
budget. The #35 commitment adds **+743 CU/fill**; the #36 band is negligible. The
security gates do not threaten the CU budget.

## 5. Known residuals (honest list)

| Item | Status |
|---|---|
| #35 ER-path trust = ER validator set | By design (trust-minimized, not trustless) |
| #35 FLP band is a *bound*, not exact pinning | ≤20%/fill deviation; exact needs input-snapshot commitment |
| #36 sybil resistance | Open — per-trader cap intentionally deferred (arena cost) |
| Whole-program invariants (§3) | `[CERTORA-TARGET]`; runtime-enforced, not proven |
| #37 mediums M1–M13 / lows L1–L5 | Not yet enumerated from the review record |
| ER commit/undelegate round-trip | ER-gated; not unit-testable without a live MagicBlock ER |
| ER heartbeat (F2/F3) | Off-chain sequencer must call `er_heartbeat` every <150 slots; auditor: confirm the sequencer-only auth + the two-tier timeouts |
| Pinocchio port (`flash-book-pin`) | WIP/out-of-scope; math layer parity-verified, instruction glue (account validation, replay guard, OI) not deployed |
| ~~Hypertree LLRB~~ | RESOLVED — dead/broken `LLRB` deleted (2026-06); live book uses `RedBlackTree` only |

## 6. Deployment plan (post-audit)

This branch **replaces a deployed program**. Gate: audit sign-off → upgrade
authority is a multisig (not a single key) → guarded launch with caps on (low
per-trade/OI limits, small market set, insurance seeded, kill-switch armed) →
widen as it survives real volume. See `docs/FLASHBOOK_PRODUCTION_ROADMAP.md`.

## 7. Reproduce everything
```bash
cargo build-sbf && cargo test                                   # build + 462 tests (405 lib + 57 integ)
cargo kani --package flash-book --features no-entrypoint        # 41 harnesses
cd formal_verification/lean && lake build                       # Haircut/Funding/OiMmr
BPF_OUT_DIR="$PWD/target/deploy" \
  cargo test -p flash-book --test integration cu_benchmark -- --ignored --nocapture
```
