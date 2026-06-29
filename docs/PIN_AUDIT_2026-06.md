# Pinocchio port (`flash-book-pin`) — deep adversarial audit, 2026-06

A line-by-line adversarial pass over the **complete 134/134 Pinocchio port**, run
the same way as the Anchor `INTERNAL_AUDIT_2026-06.md`: 9 parallel reviewers, one
per subsystem (foundations/guards/CPI, settlement/book, collateral/FLP, vault,
risk-engine/basket, liquidation/ADL, oracle/conditional, ER/MagicBlock,
haircut/math). Every finding was then cross-checked against the Anchor original to
classify it as a **PORT BUG** (pin is unsafe where Anchor is safe — a regression I
introduced in the port) vs **INHERITED** (identical to the audited Anchor program;
the faithful port reproduces it) vs **FALSE POSITIVE**.

**Policy:** fix every PORT BUG and every pin-model-specific safety gap. Do **not**
silently diverge the faithful port on INHERITED behavior — that would make pin a
different, unaudited design; those are documented here and in `AUDIT_SCOPE.md §5`
as residuals that also apply to the in-scope Anchor program.

Baseline after this pass: **478 host + 62 integration tests, 24 Kani harnesses,
`build-sbf` clean.**

---

## Fixed — PORT BUGS (pin was less safe than Anchor)

| # | Sev | Location | Bug | Fix |
|---|-----|----------|-----|-----|
| 1 | **CRITICAL** | `cpi.rs:397` | `transfer_inner` set the SPL authority `AccountMeta.is_signer = signer_seeds.is_empty()` → **false on every PDA-signed path**. SPL Token rejects with `MissingRequiredSignature`, so **every vault payout reverts** — all `withdraw_collateral` / `partial_withdraw` / `withdraw_insurance_fund` / `flp_withdraw_v3` / `withdraw_flp_capital` / `vault_withdraw_v3` brick; deposited funds freeze. Latent because the CPI withdrawal paths are `build-sbf`-verified only (harness limit), never positive-e2e-tested on-chain. | `is_signer = true` unconditionally (the transfer authority is always a signer — real wallet or PDA). |
| 2 | HIGH | `attest_er_reserved_margin.rs` | Never wrote `TraderState.er_active`, so the strict-withdraw ER gate (`partial_withdraw.rs:203`) was **dead code**. Anchor sets `s.er_active = if reserved>0 {1} else {0}` here. | Denormalize `er_active` onto the trader_state (matches Anchor). |
| 3 | HIGH | `withdraw_collateral.rs` | Strict full withdraw had **no ER check at all** — an ER-active trader (0 mainnet positions) could pull collateral backing their ER resting orders → ER bad debt. Anchor: `require!(s.er_active == 0, UseXDomainWithdraw)`. | Reject `er_active != 0` (`Custom(241)`). Regression test `withdraw_collateral_rejects_er_active_trader`. |
| 4 | HIGH | `sweep_collateral.rs` | Same ER blind spot: sweep never read `er_active` and the `from_open==0` branch skipped the health gate entirely → ER-reserved margin sweepable to a second account, then withdrawable. | Fail closed on `er_active != 0`. |
| 5 | HIGH (latent) | `book.rs` `insert_bid`/`insert_ask` | No `seq <= MAX_SEQ_ENCODABLE` ceiling at placement, though the doc claimed it and `encode_order_id`'s 24-bit packing depends on it. Anchor enforces `seq < FLP_SEQ_RESERVED_OFFSET` at every injector. Above the ceiling the `& MAX_SEQ` mask wraps → order-id collision + price-time-priority break. | Fail-loud guard at the single insert chokepoint (covers all placers). Host test + **2 new Kani proofs** (collision-freedom + price-dominates-seq). |
| 6 | MED | `basket_order.rs` | Market snapshot hardcoded `cum_funding_index = 0` while the projection used the position's real entry index → phantom, wrong-signed funding credit easing the joint gate. Anchor uses `market.cum_funding_index`. | Read + thread real `m.cum_funding()`; fresh leg opens at it (delta 0). |
| 7 | MED | `basket_order.rs` | Bound the position to `(trader, market)` only when `size!=0`, so an initialized **flat** position for another trader/market could hide a real position from the single joint gate (C-2 omit-a-risky-position). | Bind unconditionally (the disc check already requires an initialized position). |
| 8 | MED | `apply_fill.rs` | Settlement positions were **wholly unbound** — `apply_to_position` never stamps `pos.trader`/`pos.market`, and nothing checked them. A buggy/compromised sequencer could settle onto a mismatched/foreign position and corrupt collateral/OI. Anchor binds `position.trader == trader_state.trader`. Also: the two legs could alias (`&mut` UB). | Stamp-on-first-fill / match-thereafter binding + distinct-account guard for both trader_state and position pairs. |
| 9 | HIGH | `liquidate_portfolio_v2.rs` | The cross walk credited one pooled `ts_collat` but summed every leg's MM, while **not rejecting isolated legs** (own `collateral_quote_lots != 0`). Anchor structurally excludes them because positions are PDA-keyed to the cross trader_state; pin is field-bound, so an isolated leg (same wallet) folds in → **wrongful liquidation**. | Reject any leg with `collateral_quote_lots != 0` (isolated → single-position path). |
| 10 | MED→honest | `init_fill_commitment.rs` | Set the sticky `fill_commitment_required = 1` advertising a settlement-authenticity guarantee that **no path enforces** (the matcher never `buffer_push`es, `apply_fill` never `buffer_settle`s, and pin has no in-SBF keccak). Anchor fully wires the ring. | **De-advertise**: leave the flag `0` and document the gap. Wiring it half-way (preimage mismatch) would be worse than the honest gap; full wiring is a tracked follow-up (needs keccak-in-SBF + producer/consumer + a byte-exact e2e test). The ring primitives stay Kani-proven and ready. |

---

## Documented — INHERITED (identical to the audited Anchor program; not changed, to preserve the faithful port)

These also apply to the in-scope Anchor program; they are residuals, not pin
regressions. Changing them in pin alone would diverge an unaudited port from the
audited original.

- **Basket flip/add worst-case entry** keeps the old entry price (Anchor
  `project_post_leg` line 15488 is identical, documented "Conservative"). Can
  inject phantom PnL on a flip; same in both programs.
- **Conditional orders inject with `flags: 0`** (trigger/bracket/TWAP/iceberg),
  dropping the reduce-only bit at fill — Anchor does the same (line ~5800).
- **Haircut residual is not delta-tracked across FLP capital flows** — Anchor's
  `deposit_flp_capital`/`withdraw_flp_capital`/`flp_withdraw_v3` also don't touch
  residual; the `haircut.rs` ΔResidual table is aspirational. FLP capital is a
  *global* pool while residual is *per-market*, so naive tracking is incorrect.
- **Envelope rate-limit gate is optional** (skipped if the account is omitted) —
  Anchor is also `Option` ("Wave 26b — optional envelope gate").
- **Permissionless undelegation-callback squat/DoS** (`process_undelegation`) —
  inherited from the Anchor/MagicBlock design (same `is_signer`+owner-only gate);
  zero-data only ⇒ DoS, not state forgery. Hardening (canonical buffer-PDA bind)
  recommended for both.
- **Vault `nav==0` bootstrap & perf-fee no-flat-gate** — inherited (Anchor
  `vault_deposit_v3` / `settle_vault_perf_fee_v3` identical).
- **JIT liquidation offers unescrowed / price-unbounded** — the maker-commitment
  follow-up is unimplemented in both; the safe subset is to ignore offers.
- **`gate_price_move` doesn't clamp `dt_slots` to `max_accrual_dt_slots`** —
  oracle envelope hardening, applies to both.

## Verified SOUND (high-signal negatives)

Pure-math core (haircut, vault_math, fill_math, funding, peg, concentration,
reduce-only, vpin, …), the fill-commit ring state machine, migrations, views,
carved-field offsets + size asserts + alignment strategy, admin/authority gating,
dispatch (no discriminator collision), CPI program pinning, force-undelegate
liveness gate, ER heartbeat auth, and the Pyth byte-parser were each reviewed and
found correct. Caller-supplied stress shocks cannot trigger a liquidation (all
liquidation paths hard-code the no-shock scenario).
