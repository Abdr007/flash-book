# Flash Book — Audit Scope & Readiness

Single entry point for an external auditor. Frames the branch, points at the
evidence, and is **honest about residuals** — nothing here claims more than the
code proves.

## 1. Scope

- **Program:** `programs/flash-book` (the deployed **Anchor 0.31.1** program).
  The Pinocchio/no_std port (`programs/flash-book-pin`) is WIP and **out of scope**.
- **Branch / PR:** `fix-security-c1-c2` → PR #34. Squash-diff against `main` is the
  review surface.
- **Build/test gate (CI, green):** `cargo build-sbf`; `cargo test` (388 lib + 44
  integration); `cargo kani` (31 harnesses); Lean `lake build` (7 theorems). See
  `.github/workflows/ci.yml` — a regression in any of these fails CI.

## 2. What changed on this branch (review focus)

Three bodies of work; the **first two touch funds and settlement and deserve the
most scrutiny**.

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

**31 Kani proofs + 7 Lean theorems, all CI-gated.** Each binds to deployed code
(handlers route through the proven pure function). Proven: P-SOLV-1…5,
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

## 6. Deployment plan (post-audit)

This branch **replaces a deployed program**. Gate: audit sign-off → upgrade
authority is a multisig (not a single key) → guarded launch with caps on (low
per-trade/OI limits, small market set, insurance seeded, kill-switch armed) →
widen as it survives real volume. See `docs/FLASHBOOK_PRODUCTION_ROADMAP.md`.

## 7. Reproduce everything
```bash
cargo build-sbf && cargo test                                   # build + 432 tests
cargo kani --package flash-book --features no-entrypoint        # 31/31 proofs
cd formal_verification/lean && lake build                       # 7 theorems
BPF_OUT_DIR="$PWD/target/deploy" \
  cargo test -p flash-book --test integration cu_benchmark -- --ignored --nocapture
```
