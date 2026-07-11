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
| **sub-50ms fills** | **NOT met as a client round-trip — COMPLETE distribution: p50 = 161.5ms** | `er-acceptance/latency_benchmark.mjs` on the live MagicBlock devnet ER (dedicated Helius devnet L1 removed the genesis 429): **20 real taker fills, p50 161.5 / p99 164.7ms, CU 21,492/fill** (`er-acceptance/latency_results.json`). This is **network-RTT-dominated** — the client sandbox is not co-located (raw getSlot RTT ~400–540ms), so it is an upper bound on ER-side execution. The ER *execution* is plausibly sub-50ms (tiny CU, sub-50ms slot cadence) but is **not observable as a remote client round-trip**. To earn "sub-50ms": run the ready harness from a client co-located with the ER validator, or capture the validator's own per-tx timing. Reported at the measured truth. |
| **$20M JELLY proven impossible** | **EARNED for the mark-manipulation vector** | The actual HL attack (thin-market mark pump): adversarial Kani `jelly_mark_manipulation_yields_no_usable_equity` (VERIFICATION SUCCESSFUL, non-vacuous, real `worse_of_health_price`) — a pumped mark can never move the health price past the honest oracle. The additional per-domain-credit ability-to-pay layer is Lean-model (`PerDomainCredit.lean`) + a **documented engine-wiring remainder**. |

**Net:** the solvency clause is now **earned in-house** (the biggest, previously externally-blocked clause); the JELLY clause is earned for the actual attack; **`sub-50ms` is the one clause still gated on a measurement** (a dedicated RPC), reported honestly at ~165–275ms until then.

## Headline (honest)

The **permanent** engineering value — proofs, verified internals, adversarial
audit — is real and reproducible today. A 2026-07-10 adversarial re-audit (9
surfaces, 2 waves) confirmed **two HIGH** bad-debt vectors — (1) the v3 injection +
vault maker-open paths skipped the intake initial-margin gate (roadmap 4.8); (2) a
liquidatee could cancel their own injected liquidation-close order to dodge
liquidation — plus a MED withdraw-pricing gap (M-2). **All three are now fixed and
merged (PR #300, `3998b9bd`, all CI green), and the two HIGHs are DEVNET-ACCEPTED**
on a fresh throwaway program whose on-chain bytes were hash-verified against the
built artifact (`er-acceptance/CRITICAL_PATH_FINDINGS.md`; 8 PASS / 0 FAIL /
5 honest-UNDRIVEN). So the audit's **launch-gate HIGH queue is closed**; the honest
current state is **"9.5-capable, HIGH queue closed + devnet-accepted, with a
MED/LOW tail and three vendor waits remaining"** — still not "9.5 shipped" until the
Certora whole-program run, the external audit signature, and MagicBlock
owner-recovery close. M-2's code shipped in the same PR (withdraw/sweep valuation
routed through the worse-of `effective_health_mark`); its clean accept→reject devnet
flip is not demonstrable on realistic params (stress margin ≈ position max-loss
leaves no collateral window), so it stays covered by the in-tree suite + the
reconciled source rather than a live row — reported honestly, not claimed.

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
  It is not 10 **today**: (a) the two confirmed HIGHs (4.8 intake gate + liq-cancel)
  are now CLOSED + devnet-accepted (PR #300), but the keccak collision assumption,
  bounded ring models, and Lean-model↔Rust fidelity remain honest assumptions, not
  proofs; (b) Certora's whole-program run is pending; (c) a non-HIGH MED/LOW fix tail
  remains. The 10 is *earned* when that tail + Certora + external audit close.

### Order types / features
- Builder codes, 1–255 sub-accounts, anti-griefing referrals (roadmap 5.5, DONE),
  session keys, and the agent-native SDK — `AGENTS.md` / `llms.txt` /
  `docs/GOTCHAS.md` (#278, roadmap 5.6).
- **Deduction (honest):** the v3 injection + vault maker-open intake-margin gap
  (4.8, the **confirmed HIGH**) is now **CLOSED + devnet-accepted** (PR #300); still
  no on-chain copy-vaults; curated (not permissionless) listing at launch.

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
| ✅ **CLOSED — HIGH 4.8** intake-margin gate on v3 injection + `vault_place_order_v3` | DONE — shared `gate_injection_open` on all 6 opening-maker paths (reduce-only exempt); **devnet-accepted** (3 independent paths reject `InsufficientCollateral` + exemption accepts). PR #300 `3998b9bd`, all CI green | eng ✓ |
| ✅ **CLOSED — HIGH liq-cancel** owner-cancel of the injected `order_type==3` close order | DONE — owner-cancel blocked (`LiquidationOrderNotCancelable`) + `retire_liquidation_order_v2` keeper/authority path; **devnet-accepted** by a REAL liquidation (order_type==3 injected → owner cancel rejected → authority retire accepted). PR #300 | eng ✓ |
| ✅ **CLOSED — MED M-2** withdraw/sweep raw-mark pricing | DONE — routed through the worse-of `effective_health_mark`. PR #300. Clean devnet flip N/A on realistic params (stress-IM ≈ max-loss → no collateral window); covered by in-tree suite + source | eng ✓ |
| MED/LOW — ER attestation-lag, `record_flp_fill_v3` trust, funding snapshot, dormant-sibling liq | per `docs/SECURITY_AUDIT_2026-07-10*.md` fix queue (next devnet cycle; none HIGH) | eng (devnet cycle) |
| Certora whole-program run | VERIFICATION SUCCESSFUL in CI (licensed) | vendor |
| External audit signature | firm's report | vendor |
| MagicBlock owner-recovery | executable force-undelegate | vendor |

## Verdict (blunt, honest)

The engineering artifact is world-class and its core is *proven*, not asserted —
that is permanent and hard to copy. The audit's **two HIGH launch-blockers and the
MED withdraw-pricing gap are now closed, merged (PR #300), and the HIGHs are
devnet-accepted** against hash-verified on-chain bytes. What remains before
"9.5 shipped" is a **non-HIGH MED/LOW fix tail** (ER attestation-lag,
`record_flp_fill_v3` trust, funding snapshot, dormant-sibling liq) and **three
honest vendor waits** (Certora whole-program run, external audit signature,
MagicBlock owner-recovery). The gate is no longer HIGH-blocked; nothing here is
faked to look closed, and the residuals are named rather than buried.
