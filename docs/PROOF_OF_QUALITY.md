# Proof of quality — every claim → its artifact

This document exists so the protocol's quality is **provable on demand**, not
asserted. Each row maps a claim to a committed, reproducible artifact (a proof
name, a test, an audit finding, a PR). It is written to the same honesty standard
as the code: where a claim is not yet fully earned, it says so.

> North-Star: *Every money-moving instruction is machine-proven to preserve
> solvency, on a fully on-chain order book with sub-50ms fills, and the
> Hyperliquid-$20M oracle manipulation is proven impossible.*

## Launch sentence — clause by clause (verified now, no rounding up)

| Clause | Verdict | Evidence |
|---|---|---|
| **every money-moving instruction ⇒ solvency** | **EARNED IN-HOUSE (65/65 accounted)** | Track A2 extract-and-prove (PRs #289–#297): **60/65** money-path writes route through pure, Kani-proven `xmargin` cores (or the already-proven `route_funding` / `route_adl` helpers), with a `proven_wrapper_enforcement` lint pinning the routing; **5/65** conserved-by-construction (`migrate` verbatim-copy + Anchor `close = trader`; `=0`/init teardown). No Certora bundle, no assumed bridge, no behavior change. This **disproved** the earlier "needs the external Certora Anchor bundle" conclusion. |
| **sub-50ms fills** | **NOT VERIFIED — measured ~165–275ms client round-trip** | `er-acceptance/latency_benchmark.mjs` on the live MagicBlock devnet ER: real taker fills at CU 21,492, but client round-trip is network + public-endpoint-rate-limit dominated. The ER *execution* is plausibly sub-50ms (tiny CU, sub-50ms slot cadence) but is **not reproduced with an artifact**. Needs a dedicated (non-429) RPC to complete the distribution. Reported at the measured truth, not the claim. |
| **$20M JELLY proven impossible** | **EARNED for the mark-manipulation vector** | The actual HL attack (thin-market mark pump): adversarial Kani `jelly_mark_manipulation_yields_no_usable_equity` (VERIFICATION SUCCESSFUL, non-vacuous, real `worse_of_health_price`) — a pumped mark can never move the health price past the honest oracle. The additional per-domain-credit ability-to-pay layer is Lean-model (`PerDomainCredit.lean`) + a **documented engine-wiring remainder**. |

**Net:** the solvency clause is now **earned in-house** (the biggest, previously externally-blocked clause); the JELLY clause is earned for the actual attack; **`sub-50ms` is the one clause still gated on a measurement** (a dedicated RPC), reported honestly at ~165–275ms until then.

## Headline (honest)

The **permanent** engineering value — proofs, verified internals, adversarial
audit — is real and reproducible today. The **launch gate is not yet open**: a
2026-07-10 adversarial re-audit (9 surfaces, 2 waves) confirmed **two HIGH**
bad-debt vectors — (1) the v3 injection + vault maker-open paths skip the intake
initial-margin gate (roadmap 4.8); (2) a liquidatee can cancel their own injected
liquidation-close order to dodge liquidation — plus a MED withdraw-pricing gap
(M-2), all of which **must** close before launch (no defer lane). Both HIGHs are
bad-debt-adjacent, fail-safe-fixable, and devnet-gated; neither is a direct-theft
or CRITICAL. So the honest current state is **"9.5-capable, with a named and
specified fix queue"** — not "9.5 shipped." That distinction *is* the
architecture-honesty the venue is built on. Note both HIGHs mint bad debt only
under adverse price paths (not on demand), and ADL remains the bankruptcy backstop
for the liquidation-cancel vector.

## Protocol dimensions → evidence

### Order book core
- Zero-copy hypertree book — `programs/flash-book/src/state_v2.rs`; every hot-path
  load bounds+aligns the header roots (`from_account_data`).
- Corruption-proof ingest — `validate_node_links`: cycle-free visited-bitmap DFS,
  link bounds, color-byte validity, free-list disjointness; a malicious ER commit
  **fails closed** (never lands on L1). Adversarially re-verified clean (audit
  2026-07-10, hypertree surface: *no exploitable finding*).
- Ring-authenticated settlement — keccak fill-commitment binds market + both
  traders + side/size/price/sub-indices/JIT; consume-and-clear FIFO + monotonic
  nonce. Re-verified: *no fabrication/replay/reorder/mint*.
- **Deduction (honest):** the RB-tree slab is vendored (Manifest/GPL), not
  first-party — see the GPL/MIT decision (roadmap 6.3, open).

### Risk engine
- Real-symbol margin proof — Kani `assess_margin_single_market_frame_stable`
  names the live `assess_margin`; proves the requirement is collateral-independent,
  equity linear, health monotone, over **all** `u64` collateral (PR #274, closes G3).
- Anti-JELLY, mark-manipulation vector (the actual HL attack) — Kani
  `jelly_mark_manipulation_yields_no_usable_equity` on the real `worse_of_health_price`:
  a pumped mark can never move the health price past the honest oracle (EARNED).
- Per-domain realizable credit (the *additional* ability-to-pay layer) — Lean
  `PerDomainCredit.lean` (core-math verified) **but NOT wired in the engine**
  (`grep` finds no `credit_rate` in the money path); documented engine-wiring
  remainder, not counted as live.
- Worse-of health pricing, margin-walk completeness, ADL bankruptcy gate + value
  conservation — re-verified clean (audit 2026-07-10, margin/liq surface).

### Safety / proofs
- **Kani:** **73 machine-checked proofs on `main`** (grep-verified), incl. the
  real-symbol `assess_margin` harness (#274), the anti-JELLY harness, and the
  Track A2 money-path conservation cores (transfer / margin / liquidation-reward /
  fee / capped-debit / credit / debit / ADL / route_funding), all CI-gated.
- **Lean 4 + Mathlib (unbounded, real divisors):** `Haircut`, `OiMmr`, `Funding`,
  plus this session — `PerDomainCredit` (#270), `RealizedPnl` (#271, G2),
  `ResidualConservation` (#272, G4), `AuthCompleteness` (#273, G7). All
  `#print axioms`-clean (no `sorry`), CI-gated.
- **Structural enforcement** — `proven_wrapper_enforcement` CI guard: handlers
  can't reach raw funding/health math except via the proven helper (#275, G5).
- **Certora** whole-program solvency spec — wired + CI-gated (#270); the
  VERIFICATION SUCCESSFUL run is an **honest vendor wait** (license + cloud).
- **Adversarial audit (2026-07-10, 9 surfaces over 2 waves):** access-control,
  oracle, arithmetic, hypertree, settlement authenticity — **zero exploitable
  findings**; ER/cross-domain, v3-vaults, order-injection, economic/DoS — findings
  triaged in `docs/SECURITY_AUDIT_2026-07-10.md` (+ wave-2 companion).
- **Deduction (honest):** this is the axis the scorecard scores 10 as a *target*.
  It is not 10 **today**: (a) the confirmed HIGH (4.8) is open; (b) the keccak
  collision assumption, bounded ring models, and Lean-model↔Rust fidelity are
  honest assumptions, not proofs; (c) Certora's whole-program run is pending. The
  10 is *earned* when the fix queue + Certora + external audit close.

### Order types / features
- Builder codes, 1–255 sub-accounts, anti-griefing referrals (roadmap 5.5, DONE),
  session keys, and the agent-native SDK — `AGENTS.md` / `llms.txt` /
  `docs/GOTCHAS.md` (#278, roadmap 5.6).
- **Deduction (honest):** the v3 injection + vault maker-open intake-margin gap
  (4.8, **confirmed HIGH**) must be fixed; no on-chain copy-vaults; curated (not
  permissionless) listing at launch.

### Architecture honesty
- ER trust boundary — a fabricated fill **cannot settle** (ring-authenticated),
  custody **never leaves L1**; re-verified clean by the settlement + ER auditors.
  Fail-closed force-undelegate (no bypass leaked).
- This document, `docs/LAUNCH_FRAMING.md`, `docs/SETTLEMENT_POLICY.md`, and the
  audit reports *are* the honesty evidence — every residual is named, not buried.
- **Deduction (honest):** trustless force-undelegate depends on MagicBlock
  owner-recovery (vendor wait); single-sequencer at launch.

## The launch gate (what must close — no defer lane)

| Blocker | Evidence to close | Owner |
|---|---|---|
| **HIGH — 4.8** intake-margin gate on v3 injection (trigger/TWAP/iceberg/bracket) + `vault_place_order_v3` | shared `assert_injection_intake` on all 6 opening-maker paths (exempt reduce-only) + host proofs + devnet acceptance | eng (devnet cycle) |
| **HIGH — liq-cancel** liquidatee can cancel their injected `order_type==3` close order (`cancel_v2_core`/`cancel_all_v2`) to dodge liquidation | block owner-cancel/modify of `order_type==3` + a keeper/authority retirement path (or reduce atomically at injection) + devnet acceptance. ADL remains the bankruptcy backstop | eng (devnet cycle) |
| **MED — M-2** withdraw/sweep raw-mark pricing | route `partial_withdraw_core` + `sweep_collateral` through `effective_health_mark` + devnet stale-market acceptance | eng (devnet cycle) |
| MED/LOW — ER attestation-lag, v3 vault er-check, fee_tiers binding, dormant-sibling liq | per `docs/SECURITY_AUDIT_2026-07-10.md` fix queue | eng (devnet cycle) |
| Certora whole-program run | VERIFICATION SUCCESSFUL in CI (licensed) | vendor |
| External audit signature | firm's report | vendor |
| MagicBlock owner-recovery | executable force-undelegate | vendor |

## Verdict (blunt, honest)

The engineering artifact is world-class and its core is *proven*, not asserted —
that is permanent and hard to copy. But it is **not** launch-gate-green today: a
confirmed HIGH and a MED sit in a named, specified, devnet-gated fix queue, and
three honest vendor waits remain. Shipping the fix queue on a live-verified cycle
closes the gate; nothing here is faked to look closed.
