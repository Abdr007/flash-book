# Flash Book — Security Audit & Remediation Report

**Program:** `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` (Solana devnet)
**Branch:** `feat/flash-book-devnet-delegation-lazer` · **PR:** #150
**Date:** 2026-06-29 · **Status:** all Critical/High/Medium remediated, deployed, on-chain-validated

> This document is a turnkey handoff for an external audit firm and for the team's
> own review. Every figure is from real devnet transactions or the test/proof
> suite — no synthetic data. Each finding lists the fix, the deploy signature, and
> how it was verified.

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

- **435 host unit tests + 66 integration tests** pass (`cargo test --lib`;
  `BPF_OUT_DIR=$PWD/target/deploy cargo test --test integration`).
- **41 Kani proofs** over the matcher pure-math (settlement nonce, price-time
  priority, margin frame C-1, haircut conservation/solvency, fill-ring,
  liveness). `proof_solvency_single_convert` re-verified SUCCESSFUL after the P4
  funding change (the proven `matcher/` math is untouched — P4 is handler glue).
- **Lean** machine-proves the haircut haircut bound at the real `1e9` divisor.
- New regression tests this session: realized-PnL `tick_size`, preimage
  `taker_was_jit` sensitivity, `grow_fill_commitment`, `settle_position_funding`
  (charge + RISK-1 conservation + idempotency), corrupt-book-index rejection,
  oracle-config Borsh-layout compatibility.

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
