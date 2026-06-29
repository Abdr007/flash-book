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

---

## Round 2 — re-audit of the fixes + everything round 1 under-covered

A second 6-reviewer pass: (1) adversarial review of the round-1 fixes, (2) cross-
instruction / race / TOCTOU, (3) matcher + order lifecycle, (4) vault + session +
FLP v3, (5) completeness critic over all 145 dispatch arms. The fix-review agent
confirmed **all 10 round-1 fixes correct — no new bug, off-by-one, aliasing, or
type-confusion introduced.** New port bugs found in files round 1 didn't touch:

| # | Sev | Location | Bug | Fix |
|---|-----|----------|-----|-----|
| 11 | **CRITICAL** | `apply_flp_fill.rs` | Never maintained `open_positions` (anchor does at lib.rs:6943). A trader filling against the FLP pool got a live position while `open_positions` stayed 0 → withdrew **all** collateral while fully exposed → uncovered bad debt. | Maintain `open_positions_after` (parity with `apply_fill`). E2e test `apply_flp_fill_maintains_open_positions`. |
| 12 | HIGH | `place_taker_order.rs` | Rested the residual into a **crossed/locked book** when the walk truncated at `MAX_TAKER_MATCHES` (anchor guards with `!walk_truncated`, comment lib.rs:1127). | Track `walk_truncated`; don't rest the residual when set. |
| 13 | HIGH | `apply_fill.rs` + `apply_flp_fill.rs` | No `fill_seq` / `last_settlement_seq` monotonic guard → a replayed/reordered sequencer settlement re-applies the fill (double fees/PnL/OI). Anchor uses `advance_settlement_seq`. | Carve `Market.last_settlement_seq`; require strictly-increasing `fill_seq` (`Custom(246)`) before mutating, in both handlers. |
| 14 | MED | `apply_flp_fill.rs` | No price band — a compromised sequencer could settle an FLP fill far from the mark to drain pool capital (pool is the maker, no opposing consent). | Band vs mark at `FLP_MAX_FILL_DEVIATION_BPS=300` (`Custom(247)`). E2e test `apply_flp_fill_rejects_out_of_band_price`. |
| 15 | MED | `apply_flp_fill.rs` | No `(trader, market)` position binding (apply_fill has it). | `bind_or_stamp_position` (shared with apply_fill). |
| 16 | MED | `place_taker_order.rs` | Residual rested bypassing the anti-stuffing band that `place`/`modify` enforce. | Band vs mark at `MAX_RESTING_ORDER_DEVIATION_BPS` on the residual. |
| 17 | MED | liquidation (`liquidate_position_v2`/`liquidate_portfolio_v2`/`auto_deleverage`) | Liquidated/ADL'd against an unfreshness-checked mark — a stalled sequencer freezes the mark and a permissionless caller liquidates on it. | Mark-staleness gate (`now - last_mark_update_slot > MARK_STALENESS_MAX_SLOTS` → `Custom(248)`), the mark half of anchor's F4. |
| 18 | LOW | `place_taker_order.rs` | STP cap checked only `n_matches`, not `n_stp` → `stp_cancel[64]` overrun panic on >64 self-matches. | Cap on both buffers. |
| 19 | LOW | `liquidate_position_v2.rs` | `caller_trader_state` not distinct from / bound to the liquidatee → `&mut` aliasing UB. | Reject alias + require `cts.trader == caller`. |
| 20 | LOW | `liquidate_position_v2.rs` | Re-liquidation guard disabled when `cooldown==0` (the default) → same-tx stacked liquidations each skim reward, draining the liquidatee. | Unconditional same-slot guard (`last_liquidated == now`). |
| 21 | MED | `set_market_maintenance_margin.rs` | Allowed MMR up to `BPS_DENOM`; anchor caps `< 5000`. | Tighten to `< 5_000`. |

### Round-2 HIGH left as a documented residual (needs a model change, NOT hastily patched)

**Position ↔ trader_state binding (systemic).** Pin positions are bound only by
`(wallet, market)` — there is no position PDA keyed to the trader_state (anchor
uses `verify_position_pda`). A wallet with sub-accounts can therefore **substitute
siblings** in every cross-portfolio walk (`liquidate_portfolio_v2`,
`set_position_cross`/`set_position_isolated`, `sweep_collateral`,
`partial_withdraw`) to defeat the joint solvency gate → protocol bad debt. The
correct fix is to PDA-key positions to the trader_state and re-derive+assert that
PDA for the target and every sibling — a position-identity **model change**
touching every position-creation and snapshot site plus the `Position` layout. It
is too invasive to land safely mid-audit without risking regressions; a partial
patch would give false assurance. **This is the top follow-up before external
audit sign-off.** (The round-1 `liquidate_portfolio` isolated-leg reject closes the
isolated-substitution sub-case; the cross sub-account substitution needs the model fix.)

### Round-2 inherited / low-priority (documented, not changed)

- **Per-market FLP v3 NAV-includes-PnL — ✅ DONE (2026-06).** `shares_for_deposit_v3`
  / `amount_for_shares_v3` now price on **NAV = max(0, capital + realized_pnl)** (not
  capital alone), mirroring the singleton `FlpExposure` — so LPs bear the pool's
  realized losses (redeem at a discount) instead of redeeming at par and socializing
  the loss onto the shared vault; a realized gain is capped at the pool's actual token
  capital (`amount > total_capital` guard) so the vault is never over-paid; an
  insolvent pool (NAV ≤ 0 with shares) is unpriceable (`None`). Because this makes
  `realized_pnl` load-bearing, `init_flp_per_market` is now **admin-gated** (requires
  the market authority) and binds the recorder (`record_flp_fill_v3`) to the market
  **sequencer** — closing the former permissionless-recorder NAV-manipulation vector.
  Host-tested (loss-discount / gain-premium / insolvency / round-trip-creates-no-value)
  + e2e (`init_flp_per_market_rejects_non_market_authority`).
- **Vault v3 no flat-gate on deposit/perf-fee** (NAV ignores unrealized PnL) —
  deliberate v3 relaxation, faithfully ported (the non-v3 path gates).
- **2^24 lifetime seq ceiling** — a market reverts resting inserts after ~16.7M
  lifetime placements (24-bit `order_id` seq). Fail-loud guard is correct; widening
  the field / recycling seqs is a follow-up.
- **Admin-input sanity bounds** (`set_market_max_leverage`, `set_market_sequencer`
  zero-brick, `set_position_leverage` zero-escape, `set_market_liquidation_params`
  reward≤penalty, `set_market_risk_params` joint-MMR cap) — admin-trusted; low impact.
- **6 risk gates ported-but-unwired** (`daily_loss_limit`, `volume_rate_limit`,
  `min_fill_size`, `peg_pricing`, `borrow_fee`, `pro_rata`) — live in anchor, dead
  in the port (consistent with WIP plumbing).

---

## Round 3 — "every angle" pass (6 reviewers: regression, arithmetic, panic/DoS, account-model, economic/MEV, position-binding design)

Regression agent re-verified all round-2 fixes (recomputed the `Market` layout
byte-by-byte; place_taker walk + borrow-check; liquidation guards) — **no
regressions**. Panic/DoS agent: **no reachable panic or OOB on any deployed path**
(STP fix confirmed; findings were CU-DoS amplifiers). New fixes:

| # | Sev | Location | Bug | Fix |
|---|-----|----------|-----|-----|
| 22 | **CRITICAL** | `state.rs` + `margin_probe.rs` + `apply_fill`/`apply_flp_fill` + `liquidate_position_v2` + `basket_order` | **Position↔trader_state binding (systemic).** Positions bound only by `(wallet, market)`; every sub-account carries `.trader=wallet`, so a wallet substitutes a 1-lot sub-account position into the MAIN account's solvency walk → **`partial_withdraw` collateral theft**, wrongful liquidation, under-margined cross (3 agents converged; the round-2 documented HIGH, now FIXED). | Carve `Position.sub_index` from `_pad0` (stays 128B), stamp from `ts.sub_index` in `bind_or_stamp_position`, assert `p.sub_index==ts.sub_index` in `build_snapshot` (covers all 5 walks) + single-position handlers. Identity-equivalent to anchor's per-trader_state position PDA. Regression test added. |
| 23 | HIGH | `apply_fill.rs` + `apply_flp_fill.rs` | Taker fee debited with `saturating_sub` → when a taker can't cover the fee, maker/insurance/FLP still get the FULL credit → **quote-lots minted from nothing** (anchor uses `checked_sub`+abort). | `checked_sub` + abort (`Custom(249)`). |
| 24 | CRITICAL→fixed | `liquidate_position_v2.rs` | Reward paid on injection + close settles later; same-slot guard only blocked intra-slot, cooldown defaults to 0 → adjacent-slot **reward-stacking drain**. | Port the portfolio path's type-3 dup-order scan: refuse a second forced-liq order while one rests (`Custom(140)`). |
| 25 | MED | `apply_fill`/`apply_flp_fill` | `u128→u64` fee/rebate cast wraps mod 2^64 at extreme notional → near-zero fee. | Clamp to `u64::MAX` (anchor parity). |
| 26 | MED | `apply_fill.rs` `assert_position` | Disc gate admitted a fresh **zero-disc 8-byte** account → OOB read/write on the `Position` cast. | Require `data_len() >= size_of::<Position>()`. |
| 27 | MED | `place_iceberg_order.rs` | Iceberg skipped the anti-stuffing band every other resting path enforces. | Band vs mark (`MAX_RESTING_ORDER_DEVIATION_BPS`). |
| 28 | MED | `apply_fill.rs` | No settlement price band (a compromised sequencer could move collateral between legs at any price). | Band vs mark (`Custom(247)`). |
| 29 | MED | `vault_deposit_v3.rs` | No flat gate (withdraw side has one) → deposit while the vault holds an ITM position skims original LPs' unrealized gains. | Require `open_positions==0`. |

### Round-3 TOP PRE-PRODUCTION BLOCKERS (documented — too large / architectural to port safely mid-audit)

- **R1 — realized-PnL materialization — ✅ DONE (2026-06, `feat/pin-settlement-materialization`).**
  `apply_fill`/`apply_flp_fill` now fold each fill's realized-PnL delta into the
  correct collateral bucket (isolated = position, cross = trader_state, sampled
  pre-resize): a gain credits, a loss debits and — when it exceeds the bucket —
  drains it to 0 with the shortfall drawn from the insurance fund (then ADL), the
  faithful Anchor bad-debt waterfall. Fees were reordered BEFORE the resize (anchor
  order) so a close-at-a-loss still pays its fee. Also fixed the latent AUDIT-H-1
  scaling bug: realized PnL now carries the `× tick_size` factor that unrealized
  PnL + funding use (previously absent — harmless only because the field was never
  materialized). Pure math in `fill_math` (`apply_to_position` returns the delta,
  `route_realized_pnl` does the bucket+shortfall split); e2e-tested (gain→collateral,
  bankrupt-loss→insurance) + Kani-proven conservation (`route_realized_pnl_conserves`,
  the 25th pin harness).
- **R2 — funding settle-before-resize — ✅ DONE (2026-06, `feat/pin-settlement-materialization`).**
  `apply_fill`/`apply_flp_fill` now settle each leg's funding on its PRE-trade size
  before the resize, gated on an OPTIONAL trailing `haircut_state` account (the
  market's `[b"haircut", market]` PDA) — mirroring anchor's `market_haircut.is_some()`
  gate; omitting it preserves the legacy account count + lazy `settle_funding` crank.
  The funding math was extracted from the `settle_funding` ix into a single shared
  `funding::settle_position_funding` helper (mark-priced notional, isolated/cross
  bucket routing, residual move with `Δcollateral == −Δresidual`, re-stamp the entry
  index, pay clamped to availability) — so the crank and the inline settle can never
  diverge. Settling + re-stamping before a same-side add stops the post-add size
  being charged funding for the prior interval (the phantom-funding bug).
  Host-tested (`settle_position_funding_routes_and_restamps`: pay/receive/isolated/
  cross/clamp/underflow/flat) + e2e (`apply_flp_fill_settles_funding_before_resize`:
  funding settled on the pre-add size 10 ⇒ 100 paid, not the post-add 20 ⇒ 200;
  residual moved by RISK-1).
- **Dead `compute_shortfall` / `health_price_with_staleness` — ✅ WIRED (2026-06).**
  `health_price_with_staleness` now drives the staleness-checked health/penalty
  price in `liquidate_position_v2` + `liquidate_portfolio_v2` (replacing the inline
  mark + round-2 staleness gate; behavior-identical in the mark-only model — fresh
  ⇒ `(mark,_)`, stale-with-no-oracle ⇒ refuse — and oracle-ready). `compute_shortfall`
  is wired into a new read-only `view_liquidation_preview` (Ix 145) that emits a
  position's liquidation bankruptcy resolution (penalty / insurance shortfall /
  recovered) — exactly the resolution the injection→`apply_fill` (R1) settlement
  produces, surfaced for keepers/UIs. Both helpers were already host-tested + (for
  the liquidation core) Kani-proven; this makes them live. Per-market FLP v3
  par-redemption is itself fixed (above). The pin port remains devnet/WIP/UNAUDITED.
- **Liquidation reward-from-liquidatee model** (reward skimmed from the liquidatee's
  backing, sized on leveraged notional, paid before the close settles): the stacking
  drain is closed (#24), but the single-call "reward can be ~100% of backing when
  `reward_bps>maintenance_bps`" and the second-wallet self-liquidation remain economic
  concerns of the model. Anchor pays from the same bucket; needs an escrow-to-settlement
  redesign for full safety.
- **JIT liquidation offers** are unescrowed + price-unbounded (the maker-commitment
  follow-up is unimplemented in both pin and anchor) → can stall a liquidation. Safe
  subset: ignore offers until escrow lands.
- **Envelope mark-move cap is OPTIONAL** (`update_oracle` branches on the account being
  passed) — inherited from anchor; make it mandatory on envelope-configured markets.
- **CU-DoS amplifiers (inherited):** `vpin::record_fill` unbounded bucket loop and the
  book-walk visit count (bounded by depth, not the match cap) can abort shared/keeper
  paths. Cap both.
- **`side_accrual` latent** (unwired Wave-25): funding sign double-applied for shorts;
  fix before wiring.
- **2^24 lifetime seq ceiling**, **per-market FLP v3 realized-PnL** (inert), **vault
  `nav==0` bootstrap** (tail) — inherited, documented above/round-2.

---

## Verified SOUND (high-signal negatives)

Pure-math core (haircut, vault_math, fill_math, funding, peg, concentration,
reduce-only, vpin, …), the fill-commit ring state machine, migrations, views,
carved-field offsets + size asserts + alignment strategy, admin/authority gating,
dispatch (no discriminator collision), CPI program pinning, force-undelegate
liveness gate, ER heartbeat auth, and the Pyth byte-parser were each reviewed and
found correct. Caller-supplied stress shocks cannot trigger a liquidation (all
liquidation paths hard-code the no-shock scenario).
