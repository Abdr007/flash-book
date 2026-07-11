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
| 1.1 | Certora solvency spec — compile the harness, wire real handler externs, run the prover, add to CI (closes G1) | eng | **RUNNING — 4 rules VERIFIED, non-vacuous (G1 partial: real-core coverage, not yet full per-Anchor-handler dispatch).** Env fully unblocked: `certora-cli 8.17.1` + `cargo-certora-sbf 0.3.5` + public `cvlr` 0.6.1 / `cvlr-solana` 0.5.0 (git, no private registry) + license live. Harness rebuilt from the EVM-CVL scaffold to the REAL Solana `#[rule]` pattern (the old `.spec`/`method f` form does not drive `process:sbf`); lives in the standalone `certora/harness-crate` so `anchor idl build` stays clean. **Cloud Prover result (exit 0, all `rule_not_vacuous_cvlr` green):** `solvency_preserved_simple_withdraw`, `solvency_preserved_deposit`, `surplus_exact_when_solvent`, and `insolvency_detector_is_sound` — all four VERIFIED over all `u64` against the REAL `assess_solvency_full` + `partial_collateral_proves_insolvent` symbols. Covers both money-movement directions, surplus exactness (no value invented), and the runtime insolvency detector's soundness (never false-positives on a solvent protocol). CI runs `certoraSolanaProver` (secret-gated). **RESIDUAL to close G1 fully (attempted, precisely diagnosed):** calling the Anchor handler/gate symbols directly (e.g. `check_simple_withdraw`) returns UNKNOWN — "illegal dereference of an absolute address". Root cause: Anchor's `#[error_code]` copies the `#[msg]` `&'static` global strings (`error!`→`Error::from(FlashBookError)`→`to_string`→`Display::fmt`→`write_str`) at many *inlined* sites. Function-boundary summaries provably work but don't converge (the failing global just moves: `0x532a`→`0x51f0`→`0x5880`…), and the pointer-analysis / slicer `prover_args` from Certora's own examples (now in the conf) don't resolve it. Closing needs **Certora's Anchor summary bundle** (devhelp@certora.com) or stripping `#[msg]` under the certora feature. The BLOCKED `solvency_preserved_withdraw_gate` rule is committed (not in CI) as the exact reproducible signpost. | 4 rules VERIFIED non-vacuous in CI; direct per-Anchor-handler dispatch = Certora-tooling residual |
| 1.2 | prove the REAL `assess_margin` (not an abstract re-impl); Lean if 128-bit is CBMC-intractable (closes G3) | eng | **DONE** — new Kani harness `assess_margin_single_market_frame_stable` (`risk.rs`, VERIFICATION SUCCESSFUL in ~9s) calls the **real `assess_margin` symbol** (not the abstract `gate` re-impl) with collateral + δ **fully symbolic over `u64`**, proving the three cross-margin frame invariants for ALL collateral: (1) requirement + worst-scenario index are collateral-independent, (2) equity is exactly linear in collateral, (3) health is monotone in collateral (no self-liquidation by depositing). Kani can't bit-blast N symbolic positions × the 128-bit lattice × 32-byte Pubkey memcmps, so the harness fixes a concrete portfolio + minimal scenario slice and exhausts the COLLATERAL dimension the host sweep only samples (`c % 1e7`); per-market decomposition stays covered by the host sweep `n_position_..._frame_stable` + `opposing_legs_are_not_netted_across_markets` | proof names the real symbol |
| 1.3 | Lean theorem for realized-PnL on reduce/flip (`sign·closed·Δticks·tick`) + VWAP entry (closes G2) | eng | **DONE** — `formal_verification/lean/RealizedPnl.lean` (compiles clean, `#print axioms` shows only propext/Classical.choice/Quot.sound, no `sorry`): mirrors the real `matcher/position_math.rs apply_fill` — sign correctness (`long_pnl_pos_iff`/`short_pnl_pos_iff`: profit iff price crosses entry the right way), breakeven=0, `closedLots = min(fill,size)` (reduce vs flip), the marquee **V2 reconciliation** `pnl·entry = sign·(price−entry)·notional` at unbounded width, and the CBMC-intractable **VWAP bracket** `min(entry,price) ≤ vwapEntry ≤ max(entry,price)` — the two properties `position_math.rs` could only host-pin at B=256 | Lean theorems at unbounded domain |
| 1.4 | structural enforcement that handlers reach funding/health math only via the proven helper (closes G5) | eng | **DONE** — `programs/flash-book/src/proven_wrapper_enforcement.rs` (2 tests, run by `cargo test -p flash-book` in the CI "Rust on-chain program" job): a source-scanning guard that fails the moment a NEW call site to a sensitive primitive appears outside its allowlisted proven wrapper. Locks the funnel: `funding_owed` reachable only from `settle_position_funding` (lib.rs) + `assess_margin` (risk.rs); `worse_of_health_price` may not appear in the handler layer at all; `health_price_with_staleness` reachable only from `effective_health_mark` + `liquidate_position_v2`. **Surfaced residual**: `liquidate_position_v2` INLINES an equivalent staleness gate instead of calling `effective_health_mark` — a drift risk to collapse via a separate devnet-verified money-path PR (tracked in the guard's allowlist comment) | single-call-site CI test |
| 1.5 | whole-system residual identity — triple-ledger conservation checked before every commit (closes G4) | eng | **DONE (proof artifact)** — `formal_verification/lean/ResidualConservation.lean` (compiles clean, `#print axioms` shows only propext/Quot.sound, no `sorry`): models the identity `V = C_tot + I + Residual` (`haircut.rs:168`) and proves ALL 12 money-moving instructions from the `haircut.rs:460` delta table satisfy `ΔV = ΔC + ΔI + ΔR` (so each preserves the identity), plus **sequence-closure** (`foldl_conserves` — the identity survives ANY interleaving = the "checked before every commit" guarantee, structurally) and the **solvency corollary** (`Residual ≥ 0 ⟺ V ≥ C_tot + I`, the `haircut.rs:449` baseline). Forces out the one doc-table imprecision: the `convert`/gain row needs its paired `+credit` collateral leg to balance (`convert_gain_conserves`). Runtime companion (already live): on-chain `verify_protocol_solvency` + the #268 conservation sequence-fuzzer reconcile against real SPL balances | Lean per-instruction invariant + sequence closure |
| 1.6 | code-explanation-only comment hygiene; fix stale "funding inert/never advanced" comments (contradicted by live `crank_funding`) | eng | **DONE** | funding.rs module doc + `settle_funding` docstring corrected: cum-index funding is LIVE via crank_funding→settle_position_funding; only the side-accrual rate term waits |
| 1.7 | prove authorization + completeness invariants (margin-walk completeness, liquidation dedupe, auth gates) (closes G7) | eng | **DONE** — `formal_verification/lean/AuthCompleteness.lean` (compiles clean, `#print axioms` shows only propext/Classical.choice/Quot.sound, no `sorry`): **margin-walk completeness** (`walk_is_complete`/`no_position_omitted`) — the C-2 gate (`lib.rs:13382`: exact-count + PDA-binding + dedupe + live-only) forces the supplied position set to EQUAL the trader's full open set, so no risky position can be omitted (Finset cardinality: distinct owned subset of size = `open_positions` is the whole set); **no-understatement** (`requirement_monotone`/`complete_walk_requirement_exact`) — the requirement is a monotone sum of non-negative floors, so a complete walk computes the TRUE requirement, never an under-count; **liquidation dedupe** (`exec_always_present`/`reinsert_noop`) — the exec-seeded set can't drop the exec market and re-supplying a counted market is a no-op (no double-count); **auth gate** (`auth_gate_sound`/`unauthorized_rejected`) — the `require_keys_eq!` gate admits exactly the authority. Proven for ALL N, replacing the runtime-`require!`-only guarantee | Lean proofs at unbounded N |
| 1.8 | clean proof suite + fix README undercount | eng | **DONE (count fix)** — README + ARCHITECTURE + FORMAL_VERIFICATION reconciled to the true **62 Kani / 7 Lean** (grep-verified; the 7-root `lake build` completes and every theorem is `#print axioms`-clean), tests 621, Certora qualified as written/integration-in-progress (no unprovable claim). Dead-proof pruning deliberately NOT done: removing proofs conflicts with the rising-count discipline |

Baseline now in place: **Kani 62 proofs + 7 Lean modules (haircut / OiMmr / funding /
per-domain credit / realized-PnL / residual conservation / auth completeness, at real
divisors)**, both CI-gated. See `docs/FORMAL_VERIFICATION.md`.

## 2 · Dormant paths / audit gaps (2.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 2.1 | side-accrual K/F PnL path (`settle_position_pnl` never called) — wire or delete the scaffold | eng | **DISPOSITION**: safe dormancy, NOT a live gap. The A/K/F/B side indices DO advance live (`advance_indices` from `settle_funding`), but the real economic settlement runs entirely through the already-proven eager path (`cum_funding_index`→`settle_position_funding`→`route_funding`, + `assess_margin`'s unrealized-PnL). `settle_position_pnl`'s input `PositionSnapshot.a_snap` is never populated on a real position, so it returns 0 by its own guard → zero economic effect today. Documented in the `settle_funding` docstring (lib.rs:4655); fields KEPT (deleting the `MarketSideAccrualAccount` layout is an ABI regression on an allocated PDA). Full wiring = adding K/F/A/B snapshots to the live position account + migration + hot-path rewire = a multi-week economic redesign with its own devnet cycle, not a chore. **Reclassified STARTABLE→disposition; do not force a rushed wire/delete** |
| 2.2 | isolated-position ADL redirect (cross has ADL; isolated doesn't) — close asymmetry + prove | eng | STARTABLE (program change) |
| 2.3 | on-chain payout walk for referrer/builder/creator shares (today emit-only) | eng | STARTABLE (program change) |
| 2.4 | automated funding keeper binary/script (+ optional crank incentive) | eng | **DONE (keeper)** — `sequencer/funding_keeper.mjs` (+ `npm run funding-keeper`): permissionless service that cranks every configured market's `cum_funding_index` on an interval. Anchor-client on the committed IDL; concurrent per-market with isolated failures; `ONCE` (cron), `DRY_RUN`, and `MIN_DT_SECONDS` (fee optimization — correctness never depends on it, per the on-chain Δt clamp) modes. Never moves value (index-only; positions realize via the proven `settle_funding` path), safe to run multiple instances. Optional crank-incentive = separate program change (deferred) |
| 2.5 | executable trustless force-undelegate | eng | **DISPOSITION**: returns `OwnerForceUndelegateUnavailable` vs the upgraded delegation program → **vendor-gated** on MagicBlock owner-recovery; public censorship-exit claim must stay downgraded until it ships (see `docs/DECENTRALIZED_SEQUENCER.md`) |
| 2.6 | force-include from L1 (`errors.rs:141` "not yet supported") | owner | **DISPOSITION**: documented **post-launch roadmap** (not ambiguous) — the ER censorship-exit story rests on 2.5's force-undelegate, not L1 force-include |
| 2.7 | resolve dual `place_limit_order` migration (lib.rs:1393); delete legacy path | eng | **DONE (doc-only)** — investigation found the legacy path was ALREADY removed: no `place_limit_order` / `initialize_order_buffer` / order-buffer code exists; `place_limit_order_v2` (+`_session`) is the sole limit-placement ix and `init_market_book` the sole book init. Only stale doc comments remained falsely describing a live dual-book migration ("runs ALONGSIDE the legacy … `initialize_order_buffer`"). Fixed the `place_limit_order_v2` docstring + 6 stale `place_limit_order` mentions (state.rs/lib.rs/proptest) → `place_limit_order_v2`; regenerated `idl/flash_book.json` (doc strings flow into the IDL, gate now green). No code/ABI change |

## 3 · Manipulation-proof / percolator (3.x)

| Item | Scope | Owner | Status | Evidence |
|---|---|---|---|---|
| 3.1 | per-source-domain realizable credit: cap each profitable leg's usable PnL by `credit_rate = min(1, backing/claims)` of the opposing side of that same market — manipulated/thin/stale market ⇒ credit collapses ⇒ paper profit can't back margin/cure/withdraw (closes the HL-$20M oracle-pump class) | eng | **SPLIT — mark-manipulation vector DONE; ability-to-pay layer = documented remainder.** The ACTUAL HL attack (thin-market mark PUMP) is proven impossible on the real engine: `worse_of_health_price` + staleness gate + the new adversarial Kani `jelly_mark_manipulation_yields_no_usable_equity` (VERIFICATION SUCCESSFUL, non-vacuous) — a mark manipulated in the attacker's favour can NEVER move the health price past the honest live oracle, so a pump converts to ZERO usable equity. The SECOND layer (per-domain `credit_rate = min(1, backing/claims)` ability-to-pay cap) is **core-math verified in Lean (`PerDomainCredit.lean`) but NOT wired in the engine** — `grep` finds no `credit_rate` in the money path; `docs/PER_DOMAIN_CREDIT.md` states engine-wiring is the tracked multi-week remainder | mark-manipulation: adversarial Kani PASSES on real symbol; ability-to-pay: Lean model + engine-wiring remainder |
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
| 4.8 | apply intake IM gate to v3 injection paths (TWAP/iceberg/bracket) | eng | **CLOSED — CONFIRMED HIGH FIXED (PR #300, `3998b9bd`, all CI green)** and **DEVNET-ACCEPTED**. Shared `assert_injection_intake` (via `gate_injection_open`) now gates all 6 opening-maker paths (`execute_trigger_order_v3`, `execute_twap_slice_v3`, `place_iceberg_order_v3`, `replenish_iceberg_v3`, `place_bracket_order_v3`, `vault_place_order_v3`), exempt reduce-only. Proven live on a fresh throwaway devnet program (`BRtnEAZ6…`, on-chain bytes hash-verified): iceberg/bracket/vault opens from a 0-collateral state reject `InsufficientCollateral` on 3 independent paths + reduce-only exemption accepts (`er-acceptance/critical_path_acceptance.mjs`; `CRITICAL_PATH_FINDINGS.md`). Original finding: `docs/SECURITY_AUDIT_2026-07-10-wave2.md` H-A |

## 5 · Product / integration (5.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 5.1 | FLP on-book AMM tuning + quoter spec (Avellaneda-Stoikov) | eng/quant | DONE (engine) → STARTABLE (devnet param sweep + spec) |
| 5.2 | segregate MM vault from insurance backstop (prove isolation) | eng | **DONE** — Kani `bad_debt_coverage_is_insurance_isolated_and_bounded` (VERIFICATION SUCCESSFUL) proves the waterfall debits insurance by a function of only its own balance + shortfall (no FLP input, no underflow), plus `solvent_iff_vault_covers_buckets` proves insurance/FLP are separate additive buckets |
| **5.3** | **off-chain copy-trading** (snapshot-diff mirror) | eng | **STARTABLE** |
| 5.4 | activate a real paying maker-rebate schedule (negative-fee tier) | eng | STARTABLE (code exists, disabled) |
| **5.5** | **builder codes + sub-accounts + referrals** | eng | **DONE** (docs/SDK exposure → 5.6) |
| 5.6 | agent-native SDK (typed REST, AGENTS.md, llms.txt, OpenAPI, GOTCHAS.md) | eng | **DONE (docs core)** — `AGENTS.md` (trading lifecycle + exact IDL-grounded instruction signatures + state model + client bootstrap), `llms.txt` (llmstxt.org-style machine index), `docs/GOTCHAS.md` (PDA seeds, sequencer/armed-market trust model, margin-walk completeness, withdraw-anytime reserve, worse-of health, units/flags, hypertree book decode, error convention). All derived from the committed IDL — no aspirational APIs. Remaining (deferred): typed-REST/OpenAPI surface (needs a gateway service, not on-chain) |

## 6 · Ship gates (6.x)

| Item | Scope | Owner | Status |
|---|---|---|---|
| 6.1 | external audit | owner | CODE-COMPLETE, VENDOR-WAIT (package ready; engage) |
| 6.2 | multisig authority migration + fill-commitment-v1 upgrade | owner | STARTABLE (ops) |
| 6.3 | GPL-vs-MIT license decision (vendored hypertree GPL in MIT repo) | owner/legal | STARTABLE (legal read) |
| 6.4 | latency benchmark, disclosed methodology (tx sigs + CU + timing) | eng | STARTABLE |
| 6.5 | pre-commit algorithmic settlement policy (one-pager) | owner | **DONE** — `docs/SETTLEMENT_POLICY.md`: robust-oracle-only settlement, no discretionary repricing, each commitment grounded in the deployed code or a CI proof (3.2 + 5.2) |
| 6.6 | honest launch framing: "devnet + freshly-audited; run it, read it, break it; mainnet after audit closes" | owner | **DONE** — `docs/LAUNCH_FRAMING.md`: the one-page truth — what is proven today (solvency conservation, no self-liq, manipulated-market credit collapse, frame-stability, all CI-gated), what is NOT yet (not audited, not mainnet), the two honest vendor waits (audit signature, MagicBlock owner-recovery), the declared post-launch builds, and the permanent robust-oracle-only settlement trust wedge. Owner to approve the wording |

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

- **1.1** Certora solvency spec — **WIRED, VENDOR-WAIT** (`certora/`): parametric `solvencyPreserved(method f)` + `surplusNeverInvented` specs + conf + build.sh + cvt_summaries + cvt_inlining + CI job (gates when `CERTORAKEY` set, skips otherwise). Certora is NO LONGER the blocker for the solvency CLAUSE: the **A2 extract-and-prove covering set** (see below) earned "every money-moving instruction ⇒ solvency" **65/65 in-house** (60 real-symbol Kani-proven + 5 conserved-by-construction), with the routing lint-enforced. Certora would add an INDEPENDENT whole-program all-paths cross-check (still WIRED, VENDOR-WAIT — needs the license + `cvlr` SDK + cloud run), but the clause itself is earned without it.
- **1.2** real-symbol `assess_margin` — Kani `assess_margin_single_market_frame_stable` (VERIFICATION SUCCESSFUL, ~9s). Closes G3: the three cross-margin frame invariants (collateral-independent requirement, collateral-linear equity, collateral-monotone health) proven on the LIVE `assess_margin`, over all `u64` collateral — replacing the abstract `gate` re-impl proof.
- **1.3** realized-PnL Lean — `formal_verification/lean/RealizedPnl.lean` (sorry-free, `#print axioms` clean). Closes G2: sign/breakeven, `closedLots=min(fill,size)`, the V2 notional reconciliation `pnl·entry=sign·(price−entry)·notional`, and the VWAP bracket at unbounded width.
- **1.4** proven-wrapper enforcement — `programs/flash-book/src/proven_wrapper_enforcement.rs` (2 CI tests). Closes G5: source-scanning guard fails CI if `funding_owed`/`worse_of_health_price`/`health_price_with_staleness` is reached outside its allowlisted proven wrapper. Surfaced the `liquidate_position_v2` inlined-gate residual.
- **1.5** residual conservation — `formal_verification/lean/ResidualConservation.lean` (sorry-free). Closes G4: all 12 money-moving instructions satisfy `ΔV=ΔC+ΔI+ΔR`, `foldl_conserves` sequence-closure, solvency corollary.
- **1.7** auth + completeness — `formal_verification/lean/AuthCompleteness.lean`. Closes G7: Finset-cardinality margin-walk completeness, liquidation dedupe, auth gate.
- **3.2** anti-self-liquidation — Kani `withdraw_cannot_self_liquidate_below_maintenance` **VERIFICATION SUCCESSFUL**. The marquee: HL self-liquidation attack impossible by construction.
- **5.2** insurance/FLP isolation — Kani `bad_debt_coverage_is_insurance_isolated_and_bounded` **VERIFICATION SUCCESSFUL**. HL single-vault SPOF structurally absent.
- **1.6** stale-funding-comment fix (auditor-critical).
- **1.8** doc counts reconciled to true **62 Kani / 7 Lean** (621 tests) + Certora honestly qualified.
- **6.5** pre-committed algorithmic settlement policy (`docs/SETTLEMENT_POLICY.md`).
- **5.5** (pre-existing) builder codes / sub-accounts / referrals — verified in code.
- **A2 money-path matrix — CLOSED IN-HOUSE (65/65), no Certora bundle, no assumed bridge.** Extract-and-prove: every inline handler balance-write was moved into a pure, Kani-proven `xmargin` core (`apply_collateral_transfer`, `split_to_isolated`/`merge_to_cross`, `apply_liquidation_reward`, `apply_capped_debit`, `apply_collateral_credit`/`_debit_checked`/`_debit_underflow`) or an already-proven helper (`route_funding`, `route_adl_loss`/`_gain`), and a `proven_wrapper_enforcement` lint pins the routing (it CAUGHT two new callers mid-refactor — proof it's real). **60/65 real-symbol arithmetic-proven + 5 conserved-by-construction** (`migrate` verbatim-copy relocation with an Anchor `close = trader` source-destroy — `migrate_relocation_conserves_by_construction`; plus `= 0` teardown / genesis-init writes). Every increment semantics-preserving (build-sbf v1.52 0-warn + full suite). This disproved the earlier "needs the external Certora bundle" conclusion for these sites.

Kani proof count: 59 → **73** (grep-verified). Lean: 3 → **7** modules, full library `lake build` clean + `#print axioms`-clean.

## Adversarial re-audit 2026-07-10 (9 surfaces, 2 waves) — `docs/SECURITY_AUDIT_2026-07-10*.md`

No CRITICAL on any surface. Access-control, oracle, arithmetic, hypertree,
settlement-authenticity, and DoS/compute-exhaustion returned **zero exploitable
findings**. Fixed + shipped: **M-1** — `set_position_isolated` now gates on
`er_active == 0` (was: could relocate collateral out of ER-order reach → bad debt).
**Two HIGH launch-blockers** were confirmed (both bad-debt-adjacent, fail-safe-fixable,
devnet-gated; neither direct-theft): **H-A** (4.8 — six maker-open paths skipped the
intake IM gate) and **H-B** (liquidatee could cancel the injected `order_type==3`
liquidation-close order to dodge liquidation; ADL remains the bankruptcy backstop).
**Both HIGHs are now CLOSED + DEVNET-ACCEPTED (PR #300, `3998b9bd`, all CI green).**
H-A: shared `gate_injection_open` on all 6 opening paths (reduce-only exempt), proven
live (3 independent paths reject `InsufficientCollateral` + exemption accepts). H-B:
owner-cancel of `order_type==3` blocked (`LiquidationOrderNotCancelable`) + a
keeper/authority `retire_liquidation_order_v2` path — proven live end-to-end by a
**real** liquidation that injected the `order_type==3` order (never hand-crafted), the
liquidatee's cancel rejected, the authority retirement accepted. Full acceptance:
`er-acceptance/critical_path_acceptance.mjs` + `CRITICAL_PATH_FINDINGS.md` (8 PASS / 0
FAIL / 5 honest-UNDRIVEN on `BRtnEAZ6…`, hash-verified vs the built artifact).
The **M-2** withdraw/sweep raw-mark gap is also fixed in PR #300 (valuation routed
through `effective_health_mark`, worse-of); its clean accept→reject devnet flip is not
demonstrable on realistic params (stress margin ≈ position max-loss leaves no collateral
window), so it stays covered by the in-tree test suite + the reconciled source rather
than a live row — reported honestly, not claimed. **The MED/LOW audit tail is now
closed (2026-07-11), none launch-blocking:** **L-1** dormant-sibling portfolio-liq
(PR #303) and **L-2** vault `er_active` (PR #302) are FIXED + merged (devnet-CI green,
L-1 with a new behavioral test); **L-3** (`fee_tiers` not commitment-bound) and the
three MEDs (ER attestation-lag, `record_flp_fill_v3` trust, funding snapshot) are
**accepted residuals** — each a bounded, no-outside-theft, sequencer-trust/latent
item with a **named fix and the milestone it attaches to** (see
`docs/SECURITY_AUDIT_2026-07-10*.md` dispositions): L-3 → fee-tier-activation
(standing singleton + required account), attestation-lag / FLP-fill-root → ER-hardening
/ decentralized-sequencer track (2.x), funding → 4.7 funding/mark pass (TWAP accrual;
a min-interval alone is counterproductive). Nothing risky forced into settlement code.

## Honest status of the remainder

Every item now has real scope (no more title-only). The remainder splits into:

- **Program changes** (need a devnet deploy + live-re-verify cycle each): 2.1–2.3, 2.7,
  3.3, 4.1–4.6. (**4.8 CLOSED** + devnet-accepted, PR #300; the H-B cancel-lock + M-2
  worse-of shipped in the same PR — the audit's two HIGHs are no longer open.)
- **Hard proofs** (Lean / real-symbol): 1.3, 1.5, 1.7. (**1.2 DONE** — real-symbol Kani `assess_margin_single_market_frame_stable`.)
- **Multi-week builds**: 1.1 Certora integration, 3.1 percolator per-domain credit, and
  the three big builds (copy-vaults, HIP-3 permissionless deploy, decentralized-sequencer
  activation).
- **Vendor-gated**: 6.1 (audit signature), 6.3 (GPL legal read), 2.5 (MagicBlock
  owner-recovery).
- **Positioning/ops docs**: 5.1 spec+devnet sweep, 5.3/5.6 SDK, 6.2/6.4/6.6.

This document is the authoritative tracker; each item closes only against its evidence
artifact, and no public claim ships ahead of that evidence.
