# Flash Book — Security Audit & Remediation Report

**Program:** `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` (Solana devnet)
**Round 1:** PR #150 (`feat/flash-book-devnet-delegation-lazer`), 2026-06-29
**Round 2:** PRs #197–#215, 2026-07-02/03 — see **§8** (deep 13-dimension audit + remediation, incl. two ER-boundary fixes verified on the live MagicBlock devnet rollup, plus a mark-manipulation hardening)
**Status:** all Critical/High/Medium remediated across both rounds, deployed, on-chain-validated; ER trust boundary now exercised live (not just the unit harness).

> This document is a turnkey handoff for an external audit firm and for the team's
> own review. Every figure is from real devnet transactions or the test/proof
> suite — no synthetic data. Each finding lists the fix, the deploy signature, and
> how it was verified. §1–§7 cover Round 1 (2026-06); **§8 covers Round 2
> (2026-07)** and supersedes the Round-1 counts where they differ.

---

## 1. Executive summary

Two full adversarial security audits (six dimensions each: math/accounting,
margin/liquidation, access-control, oracle, ER/matching, arithmetic/DoS) were run
against the deployed program. The first found **2 Critical + 4 High + 3 Medium**;
the second (against the remediated code) confirmed every fix airtight and surfaced
**3 new lower-severity items**, all since closed.

**Current posture: no reachable Critical, High, or Medium on the deployed
program.** The systemic sequencer-trust vector — the architectural ceiling that
capped the access/ER security ratings — is **closed by default** via the §3.2
settlement redesign (fill-commitment authenticity is mandatory for every new
market). Held back from a production rating only by maturity (devnet, no external
audit yet) and the centralization residual (single upgrade/market authority).

| Dimension | 1st audit | After remediation |
|---|---|---|
| Math / accounting | 6.5 | 9.0 |
| Oracle integrity | 3.0 | 9.0 |
| Access control | 2.0 | 8.5 |
| ER / matching | 7.5 | 8.0 |
| Arithmetic / DoS | 9.0 | 9.3 |
| Margin / liquidation | 7.5 | 8.5 |

---

## 2. Findings & remediation

### Critical

**CR-1 — Permissionless market creation + global collateral → shared-vault drain.**
`initialize_market` had no authority gate, and `initialize_market_inner` set the
creator as `market.sequencer`; collateral is a global per-wallet balance withdrawn
from a single shared vault, and `apply_fill` authorized only on `sequencer ==
market.sequencer` with opt-in fill-commitment. An attacker could create a market,
fabricate fills crediting their global collateral, and withdraw real funds.
*Fix:* `constraint = insurance_fund.authority == authority.key()` on
`InitializeMarket` (mirrors `InitErMarginAttestation`). *Verified on-chain:*
non-authority market creation rejected `Unauthorized` (7100).

**CR-2 — `update_oracle_from_lazer` accepted attacker-controlled feed/scale.**
The permissionless Lazer path took `feed_id`/`tick_decimals` as instruction args
with no binding to the market's `oracle_config` and no `source` check; since Lazer
payloads are public, anyone could write an arbitrary price to any market with a
config. *Fix:* require `cfg.source == SOURCE_LAZER` and bind `feed_id` /
`tick_decimals` to the config; new authority-gated `init_lazer_oracle_config`
(`lazer_feed_id` carved from existing padding — Borsh-layout-compatible,
regression-tested). *Verified on-chain:* config binds `source=LAZER`,
`lazer_feed_id` decodes correctly.

### High

| ID | Issue | Fix |
|---|---|---|
| **H-1** | Realized PnL dropped the `tick_size` factor (mis-scaled settled collateral / FLP NAV on `tick_size>1` markets) | `× tick_size` in both settlement helpers + regression test at `tick_size ∈ {1,10,50}` |
| **H-2** | Producer-side fill-commitment asymmetry (armed market's book cleared with no commitment → settlement DoS / griefing) | mirror the `apply_fill` consumer guard in `place_taker_order_v2` |
| **H-3** | ADL credited the counterparty more than the bankrupt forfeits (unbacked mint) | cap `counter_gain` at `loss_quote_lots` (strictly value-conserving) |
| **H-4** | Per-slot envelope move-cap optional on permissionless oracle paths | mandatory `envelope_config` on `update_oracle_from_{pyth,lazer}` |

### Medium

| ID | Issue | Fix |
|---|---|---|
| **M-1** | Oracle `published_at` caller-supplied (vacuous staleness gate) + future-dateable | reject future timestamps; store the real `Clock` observation time |
| **M-2** | Fill-ring cap 64 < matcher batch cap 256 (large sweeps reverted) | size the ring to 256; add `grow_fill_commitment` (see §3) |
| **re-audit F-1/F-2** | Lazer path lacked the confidence-interval gate the Pyth/trusted paths have | enforce `conf_bps ≤ cfg.max_confidence_bps`; range-bound it at config init |
| **re-audit (margin)** | `auto_deleverage` priced health off the raw mark (no staleness gate) | route through `effective_health_mark` (degrade-to-oracle / revert), as both liquidate paths do |

### Low / defense-in-depth

- **Arith L-1** — `risk.rs::shocked_price` rejected a `u64`-overflowing stressed price instead of silently truncating.
- **ER L-2** — `MarketBookHandle::from_account_data` now bounds-checks all six header node indices (NIL or node-aligned, in-bounds byte offset), so a malicious-sequencer-committed or tampered book fails closed (`OutOfRange`) instead of panicking a raw slab accessor. Rejection test added.
- **Arith INFO** — `envelope::assess` uses `checked_add` for the one remaining raw `u128 +`.

---

## 3. §3.2 settlement redesign — fill-authenticity (the systemic fix)

The fill-commitment ring is the anti-fabrication mechanism: the on-chain matcher
commits each fill's economic content (`keccak(fill_preimage)`) as it crosses the
book; settlement recomputes and pops it FIFO, so a compromised sequencer cannot
fabricate, reorder, or alter a fill. The redesign makes it complete and the
default.

| Phase | Change | Deploy |
|---|---|---|
| **P1** | Bind `taker_was_jit` into the preimage (was an unbound arg → a sequencer could skim the JIT rebate) | `2GFcNZJq…` |
| **P2** | Fill-commitment **mandatory by default** (`fill_commitment_required = true` at market creation) — no market can settle a fabricated fill | `W5KsHHZ5…` |
| **P3** | `grow_fill_commitment` — raise the ER-session ring ceiling in place (drained + base-layer gated) | `ZtPGZKcm…` |
| **P4** | Settle funding on fill-close (closed-portion funding was dropped) — shared `settle_position_funding`, idempotent vs the crank | `4zoeS6ax…` |

P2 is deploy-safe: existing markets keep their stored flag (grandfathered); only
new-market semantics change (must be armed to settle). Verified on-chain.

---

## 4. Test & formal-verification posture

- **449 host unit tests + 69 integration tests** pass (`cargo test --lib`;
  `BPF_OUT_DIR=$PWD/target/deploy cargo test --test integration`).
- **49 Kani proofs** over the matcher pure-math (settlement nonce, price-time
  priority, margin frame C-1, haircut conservation/solvency, fill-ring,
  fill-outbox no-overwrite, liveness). `proof_solvency_single_convert` re-verified
  SUCCESSFUL after the P4 funding change (the proven `matcher/` math is untouched —
  P4 is handler glue).
- **Lean** machine-proves the haircut bound at the real `1e9` divisor.
- New regression tests this session: realized-PnL `tick_size`, preimage
  `taker_was_jit` sensitivity, `grow_fill_commitment`, `settle_position_funding`
  (charge + RISK-1 conservation + idempotency), corrupt-book-index rejection,
  oracle-config Borsh-layout compatibility, the ADL true-bankruptcy gate (R-1)
  + isolated-bankruptcy socialization (R-2), and the O-1 corrupt-internal-link
  rejection (`validate_node_links_rejects_corrupt_internal_link`).

**Build:** `cargo build-sbf --tools-version v1.52` (Rust 1.89; default
platform-tools fail on edition2024 deps).

---

## 5. Real-CU measurements (devnet)

| Operation | CU |
|---|---|
| `place_limit_order_v2` (any depth) | ~13.3k–13.9k (flat across 24 levels = O(log n) hypertree) |
| Taker sweep, 24 levels, **armed** (+24 keccak commitments) | **16,687** (~695/level) |
| Anti-fabrication overhead | ~14 CU per fill |

For reference (real mainnet competitor txns): Phoenix place/cancel batch 93k–182k;
Drift place-and-make 400k–800k.

---

## 6. Residual risk — what an auditor should know

1. **Centralization.** A single key is the upgrade authority *and* (per-market)
   the authority + sequencer. A compromised sequencer key still cannot fabricate
   fills on an armed market (§3.2), but the authority can set the trusted-bootstrap
   oracle (`update_oracle`) and create markets. *Recommendation:* move upgrade and
   market authority to a multisig/timelock before mainnet.
2. **Operational arming.** §3.2 enforces authenticity for new markets; existing
   markets are grandfathered and should be armed (`init_fill_commitment`) in
   coordination with the off-chain sequencer (which already pushes commitments via
   the on-chain matcher when a ring exists).
3. **ER trust boundary.** The MagicBlock sequencer is semi-trusted; ER L-2 hardens
   the book against a corrupt committed state, but the rollup-only commit/process
   instructions are validator-driven and exercised on the live ER, not the unit
   harness.
4. **Acknowledged low items (non-blocking):** `ExecuteTriggerOrderV3.position`
   isn't sub-account-scoped (same-trader edge, pending a `sub_index` schema field);
   ADL force-closes an eligible counter for zero credit when the bankrupt has no
   collateral (fairness edge, value-conserving by design).

---

## 7. Deploy history (devnet, this remediation)

`3edxZN1R` (CR+H) → `YX6yMWWp` (M-1/M-2) → `2wWbL5Jg` (re-audit) → `2GFcNZJq` (§3.2 P1)
→ `W5KsHHZ5` (P2) → `ZtPGZKcm` (P3) → `4zoeS6ax` (P4) → `71oH5VDC` (Lows) →
`3YbGwWBA` (ER L-2). Upgrade authority `GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD`.

---

## 8. Round 2 — deep 13-dimension audit + remediation (2026-07-02)

A second, deeper adversarial audit (13 parallel domain auditors + a line-by-line
self-audit of the solvency core), run against the deployed program and **treating
the Kani proofs and the test suite as untrusted**. It found **1 Critical, 9 High,
and a full Medium/Low set**, plus a systemic "inert safety controls" theme. **Every
code-level finding is remediated and merged** across **PRs #197–#215**, each
CI-green on all four required checks (`Rust on-chain program`, `cargo build-sbf`,
`Formal verification (Kani)`, `Formal verification (Lean)`). Two ER-boundary fixes
were additionally **verified on the live MagicBlock devnet rollup** — the one
surface a `solana-program-test`/BanksClient CI structurally cannot exercise.

### 8.1 Critical
- **FLP unbacked mint** — `initialize_flp_exposure` minted LP shares with **no token
  transfer**, no admin gate, first-caller-wins singleton over the *shared* insurance
  vault → phantom shares redeem real trader collateral. *Fix:* require
  `initial_capital == 0` + admin-gate; capital is seeded only via `deposit_flp_capital`
  (real transfer). (#197, `939ac57`)

### 8.2 High — 8 fixed, 1 downgraded
| ID | Issue | Fix | PR |
|---|---|---|---|
| H-1 | Stress-lattice netted opposing legs across markets (~2× under-margin) | per-market decomposition in `assess_margin` | #197 |
| H-2 | ER reserved-margin bypass (`transfer_main_to_sub`/`sweep`) | `er_active` gate | #197 |
| H-3 | Delegate-custody escalation via `sweep_collateral` | require source **owner** signs | #197 |
| H-4 | `process_undelegation` forged-buffer → fabricate `TraderState` → drain | bind buffer to the DLP's canonical undelegation PDA `["undelegate-buffer", delegated]` — **seed recovered empirically from the live ER**, then enforced | #210 |
| H-5 | Cyclic committed book → infinite loop (market brick) | bounded-reachability DFS in `validate_node_links` | #197 |
| H-6 | Unmargined taker wedges the FIFO settlement ring | cap taker fee at available collateral (never revert) | #203 |
| H-7 | Bracket OCO double-fill flips position | sibling-trigger mutual-backlink deactivation | #199 |
| H-8 | Pause asymmetry (liquidations run while frozen) | `MarketPaused` gate on liquidate/ADL | #202 |
| H-9 | Haircut warmup backdating | downgraded → Medium; fixed with M-7 | #204 |

### 8.3 Medium / Low
Param caps + init-gates (M-2/M-4/M-5/M-13/M-17/M-18), stress-scenario cap (M-9),
zero-NAV reject (M-3), haircut residual reconcile via an optional solvency-monitor
account (M-6, #208), cached-`h` refresh (M-7, #204), order-seq reseat ix (M-8, #200),
**M-15 sequencer-gated ER `commit_and_undelegate_*`** (#211, verified live), liquidation
price off the freshness-gated health price + per-fill fee dust-floor (F3/L-1, #209),
and the **Lazer replay-nonce** — a strictly-increasing publisher timestamp per
`oracle_config` rejects re-posting a public signed payload within the staleness
window (#212).

**Mark-manipulation hardening (M-14 mark surface).** The mark is an EMA of fills
the semi-trusted sequencer produces and it feeds **worse-of(mark, oracle)**
liquidation health, so it must stay pinned to the trustless oracle. Two fixes:
- **Always-on, tight oracle band (#215, `bf46bea`).** `apply_fill`'s oracle-band
  clamp previously ran only when `oracle_band_bps > 0`, so a market that left the
  band unset (0) had an **unclamped mark** — a sequencer could walk it off the
  oracle via wash fills and drive wrongful liquidations. The *effective* band is
  now enforced at runtime regardless of config: unset → 2% default, any stored
  band → capped to 5% (`constants::effective_oracle_band_bps`). A fresh oracle
  always pins the mark within 5% of the oracle. Pure clamp refinement
  (clamp-not-reject, no account/field/API change) → **settlement/FIFO structurally
  identical**; it only ever tightens the mark toward the oracle, strictly reducing
  wrongful-liq risk. Config-time cap also tightened 100% → 5%.
- **Envelope backstop 2000 → 1000 bps/slot (#214, `712152b`).** `ABS_MAX_PRICE_MOVE_BPS_PER_SLOT`
  (the per-slot price-move ceiling) pulled toward the realistic ~500 bps/slot
  worst case while keeping 2× headroom.

### 8.4 Inert-controls cleanup (false assurance removed)
Deleted 6 dead risk modules (concentration / position-cap / daily-loss / volume-rate
/ stable-collateral / pending-claim), the ARG anti-sandwich stub, the never-read
leverage-tier state, and dead/broken LLRB (#205/#206/#43), plus the dead
`peg_pricing` module (`align_to_tick` et al. — no production caller; #213). VPIN +
the toxicity-tax branches are **documented inert** (#207) — a full removal needs a
state migration that would break live accounts, so they are marked, not ripped out.

### 8.5 Live-ER verification (the CI-unreachable surface)
The `er-acceptance/` harness runs the full **delegate → match-on-rollup → commit →
commit_and_undelegate → process_undelegation → L1** round-trip on the **real
MagicBlock devnet ER** (`magicblock-core 0.13.2`). Both ER fixes were confirmed
**7/7 green with the fix enforced**:
- **H-4** — the guessed buffer derivations *broke undelegation on the live rollup*;
  the true seed (`["undelegate-buffer", delegated]` under the delegation program)
  was recovered via a non-enforcing diagnostic build + offline brute-force against 3
  real captured `(buffer, delegated)` pairs, then locked in by a regression test.
- **M-15** — the market is delegated alongside the book/ring/outbox during a session,
  so the sequencer is reachable on the ER; the gate re-derives each account's
  per-market PDA to bind the passed `market`, then requires `payer == market.sequencer`.

### 8.6 Test & formal-verification posture (Round 2, current)
- **411 host unit tests** (`cargo test -p flash-book --lib`) + the integration suite
  (16 files, loaded as a real compiled SBF `.so` in the BPF VM) — all green. Lower
  than Round 1's 449 because the dead modules (and their tests) were deleted.
- **50 Kani proofs** over the matcher/solvency pure-math (incl. the C-1 margin frame,
  haircut conservation/solvency, fill-ring/outbox no-overwrite, the `grow_fill_outbox`
  drained-gate remap safety, order-id price-time priority, ER liveness) + the **Lean
  theorems** (Haircut / OI-MMR / Funding) at the real `1e9` divisors. All green.
- New Round-2 regression tests: cross-market margin (H-1), cyclic-book rejection (H-5),
  OCO sibling backlink (H-7), scenario cap (M-9), residual over-backing (M-6),
  the live-ER buffer-seed derivation (H-4, real captured pairs), the M-15 gate's
  both reject branches, the Lazer replay-nonce Borsh-compat + round-trip, and the
  effective-oracle-band default/cap logic (M-14 mark clamp).

### 8.7 Residual risk — architectural items an auditor should weigh
These are **trust-model / protocol-design properties, not code defects**; each is
bounded and documented rather than "fixed", because a code fix would require
decentralizing the sequencer, an upstream MagicBlock change, or a matcher redesign
that would regress the proven hot path.

1. **Sequencer within-band discretion (M-14).** On an armed market the sequencer
   cannot fabricate/reorder/alter fills (§3.2 authenticity is mandatory), and
   fill ordering is reorder/replay-proof at settlement (monotonic `fill_seq`,
   Kani-proven). The **mark-manipulation surface is now hardened** (§8.3): the mark
   is always pinned within 5% of the trustless oracle, and the per-slot move
   backstop was tightened. What remains is irreducible without decentralization —
   a single sequencer still chooses *which* crossable order to service first and
   sets the mark *within* the (now-tight) band. *Recommendation:* a decentralized /
   BFT-run continuous CLOB (Hyperliquid-style) is the endgame; it keeps continuous
   price-time execution (no FBA) while removing the single-sequencer trust point.
2. **`force_undelegate` DLP limitation (M-16).** The L1-initiated force-undelegate
   path fails closed (`Custom(221)`); undelegation flows through
   `commit_and_undelegate_* → process_undelegation`, which is what the live ER
   harness exercises. Full unilateral L1 reclaim depends on an upstream delegation-
   program capability that does not yet exist.
3. **Reduce-only for *resting* CLOB makers (H-7 residual).** Plain-limit reduce-only
   is **rejected fail-closed**; *triggers* enforce the reduce-only cap at fire-time
   (position > 0, opposes, `size ≤ position`). A resting maker's reduce-only cannot
   be enforced at bilateral settlement — the fill is already committed, and capping
   the maker unilaterally would unbalance the taker — so a position that shrinks
   between fire and fill is a known, narrow, self-inflicted edge inherent to resting
   reduce-only orders in any CLOB (not a protocol-solvency risk).
4. **Centralization** (carried from §6): single upgrade + per-market authority/
   sequencer key → multisig/timelock recommended before mainnet.

*Verified-sound on re-inspection:* `grow_fill_outbox`'s drained gate reads the
outbox's **own** cursors and requires the account be program-owned (L1, not
delegated), so its remap invariant is correct — now **Kani-proven**
(`drained_grow_has_no_remappable_pending_slot`, #213); the `align_to_tick`
bid-floor was unreachable dead code and the whole `peg_pricing` module has been
**deleted** (#213); the entry-price weighted-average sub-tick rounding is
value-conserving (INFO).

### 8.8 Round-2 deploy history (devnet)
The Round-2 fixes deploy to the same program via the CI-built `.so`
(`cargo build-sbf` runs in CI and uploads the artifact; local `build-sbf` is blocked
by an `edition2024` toolchain issue). The two ER fixes landed and were re-verified on
the live rollup: **H-4** enforcing build `5iPgnWpg…`, **M-15** `4wEyrJMM…`. The
deployed program is kept current with `main` (latest upgrade `2RQV37R…`, covering
through #215) and the `er-acceptance` round-trip re-verifies **7/7 green** after
each upgrade — so H-4/M-15 stay enforced and the mark-clamp / cleanup changes
introduce no ER regression. Upgrade authority + market authority:
`GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD`.

**Round-2 posture:** no reachable Critical/High/Medium on the deployed program; the
ER trust boundary is now exercised on the live rollup, not just the unit harness.
Held from a production rating only by the centralization residual and the absence of
an external audit — for which this document is the turnkey input.
