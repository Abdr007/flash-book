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

## Full external re-audit on `main` (2026-06) — 6 reviewers, post-everything

After R1/R2 + FLP-v3 + the dead-helper wiring all merged, a fresh 6-reviewer
external audit (settlement, FLP/vault/collateral, liquidation/ADL, account-model/
races, arithmetic, economic/MEV) was run on `main`. The proven pure-math core held
(OI/PnL/funding/shortfall conservation, C-1 margin fix, vault round-trip). **Every
exploitable finding was in handler glue** — chiefly a recurring `sub_index`-binding
gap (round-3 added the binding to most paths but missed several) and the
liquidation-order lifecycle. All fixed:

| Sev | Finding | Fix |
|-----|---------|-----|
| **HIGH** | `settle_funding` bound by wallet not `sub_index` → funding-evasion erases the obligation → bad debt | `p.sub_index == ts.sub_index` |
| **HIGH** | A trader can `cancel_order`/`cancel_all`/`modify_order` their OWN forced-liquidation (type-3) order → liquidation evasion | reject `order_type == 3` in all three |
| **HIGH** | `auto_deleverage` counter leg not `sub_index`-bound → ADL gain to wrong sub-account + `open_positions` desync → withdraw-while-exposed | bind both legs by `sub_index` |
| **HIGH** | Unbounded JIT offer price → injected order rests unfillable → permanent liquidation DoS + reward skim | clamp accepted JIT price to the mark band |
| **HIGH** | FLP v3 no flat-gate → LP escapes the pool's open-position unrealized loss | reject deposit/withdraw when `size_lots != 0` |
| **MED** | `sweep_collateral` gated on MAINTENANCE not INITIAL margin → bypasses the IM withdraw floor | `im_bps()` override (parity with `partial_withdraw`) |
| **MED** | `apply_fill` sampled the iso bucket BEFORE the funding settle → a funding-drained isolated loss mis-socialized to insurance | sample iso AFTER funding (anchor order) |
| **MED** | `apply_fill`/`apply_flp_fill` gated funding on the account being passed, not `market.haircut_enabled` → sequencer omits it to drop funding | require `haircut_state` when `haircut_enabled` |
| **MED** | `vault_deposit_v3` minted 1:1 into a NAV-0-with-shares vault → depositor diluted into dead shares | `vault_math` rejects (`Insolvent`), Kani-updated |
| **MED** | `transfer_collateral` missing `er_active` gate | fail-closed `Custom(241)` |
| **MED** | `record_flp_fill_v3` trusted the snapshotted sequencer (stale after rotation) | check the LIVE `market.sequencer` (market acct added) |
| **MED** | on-chain self-trade not rejected (`apply_fill` only checked distinct accounts) → seq self-settle drains insurance | reject `taker_ts.trader == maker_ts.trader` |
| **LOW** | `convert_position` / `release_gain_to_haircut` `sub_index`; settlement `mark==0` band-open; liq dup-scan `sub_index`; `view_liquidation_preview` priced at mark not synthetic | all fixed |

Regression tests: `settle_funding_rejects_sub_index_mismatch`,
`cancel_order_rejects_forced_liquidation_type3`, `transfer_collateral_rejects_er_active`,
vault_math `Insolvent` (host + Kani). Suite: 481 host + 73 integration + 25 Kani.

### Documented as deferred (features / inherited — NOT exploitable-today bugs)
- **Funding accrual is inert** (economic C-1): `Market.cum_funding_index` is never
  ADVANCED — the velocity/skew engines (`funding_velocity`/`side_accrual`) are
  unwired, so `cum_now ≡ 0` and all funding moves 0. **Fail-safe** (no funding =
  no funding-flow attack), but it means the perp has no funding tether. Wiring the
  rate engine into a sequencer ix is the next feature (the funding-flow attacks
  above were fixed so they're safe WHEN it's wired). **Top remaining feature.**
- **`margin_mode` flag** (economic M-5): iso/cross is inferred from
  `collateral_quote_lots > 0`; a bucket drained to exactly 0 is indistinguishable
  from cross. The `apply_fill` iso-after-funding fix is the anchor-parity remedy;
  an explicit `Position.margin_mode` byte (carved from `_pad0`) would close it
  fully — a hardening BEYOND anchor, deferred.
- **ADL victim selection** (M-3, off-chain (pnl×lev) rank) and **ADL global
  conservation leg** (M-4) — inherited; need on-chain ranking + an insurance/FLP
  settlement leg.
- **Envelope mark-move cap optional** (S-1) + **trader-vs-trader 50% band** +
  **`fill_commitment` unwired** (S-2) — sequencer-trust-boundary hardening,
  inherited/documented; the self-trade half is now fixed on-chain.
- **FLP `realized_pnl` not crystallized into `capital`** (H-5) — the flat-gate
  (above) closes the exploit; folding realized PnL into capital is bookkeeping.
- **Double insurance-draw** (`apply_fill::materialize_realized` + `cover_bad_debt`
  for the same shortfall) — sequencer-coordination; reconcile the two paths.

---

## Second full external re-audit on `main` (2026-06) — 6 reviewers, post-#187

After #187 merged, a SECOND independent 6-reviewer audit (settlement/funding,
FLP/vault/collateral, liquidation/ADL, account-model/races, arithmetic,
economic/MEV/sequencer-trust) was run on `main`. It went DEEPER than the surface
sub_index sweep and found a **CRITICAL** the earlier rounds missed — an
un-ported ADL conservation cap — plus several genuine HIGH/MED port bugs in
handler glue. The proven pure-math core held again (OI/PnL/funding/shortfall
conservation, C-1 margin, vault round-trip, all Kani proofs). All fixed; pin-only.

| Sev | Finding | Fix |
|-----|---------|-----|
| **CRITICAL** | `auto_deleverage` credited the counter its FULL PnL at the bankruptcy price with **no `.min(loss)` cap** (anchor's AUDIT-H-3) → counter credited more than the bankrupt forfeits → **unbacked mint**, eroding `vault ≥ Σcollateral+flp+insurance` on *every* ADL (and insurance is, by the ADL trigger, already below pause — no backstop) | cap `gain = adl_counter_gain(..).min(loss)` |
| **HIGH** | `auto_deleverage` missing anchor's R-1 `adl_bankruptcy_reached` gate → a merely stress-unhealthy but **mark-SOLVENT** position (0<equity<maintenance) could be ADL'd at `bp`, seizing its residual equity | port `adl_bankruptcy_reached`; gate on the fresh health mark (Kani-proven helper) |
| **HIGH** | `basket_order` injected resting legs with `sub_index: 0` (the lone injection path not propagating the real sub) → a sub-account user passes the joint-margin gate on a funded sub while legs route fills/PnL to an empty sub 0 → unbacked, instantly-liquidatable position → bad debt | `sub_index: trader_sub` |
| **HIGH** | a liquidatee could remove their own forced-liq (type-3) order via the **taker STP self-cancel** path (`place_taker_order`) — the #187 type-3 guard missed STP | skip type-3 in the STP self-match branch |
| **HIGH** | FLP v3 deposit/withdraw **NAV asymmetry**: deposits priced on `nav = capital+realized_pnl` (incl. gains) but withdraws capped at `capital` → realized gains charged to depositors yet locked from ALL redeemers (full redemption *rejected*), and extractable by an early LP from a later depositor | price BOTH sides on `nav_for_pricing = min(nav, capital)` (bear losses, ignore un-crystallized gains) |
| **MED** | a vault strategist could cancel the vault's own forced-liq (type-3) order via `vault_cancel_order_v3` (inherited gap) | mirror the #187 type-3 guard |
| **MED** | `settle_vault_perf_fee_v3` had no flat-gate → perf fee minted against an open winning position's high, diluting depositors when the loss later realizes | gate on `open_positions == 0` (parity with deposit/withdraw) |
| **LOW** | `risk::shocked_price` used silent `as u64` truncation (a cast `overflow-checks` does NOT catch) → an extreme positive governance shock wraps the stressed price small, under-margining a short | checked: reject when `r > u64::MAX` |
| **LOW** | `apply_flp_fill` sampled the iso bucket BEFORE the funding settle (the #187 `apply_fill` M-1 fix, missed in the FLP path) | sample iso AFTER funding |

Regression tests: `auto_deleverage_settles_underwater_vs_counter` updated to the
**conserving** credit (counter +50 == loss, was the buggy +300 unbacked mint);
new `auto_deleverage_rejects_mark_solvent_position`, `vault_cancel_rejects_forced_
liquidation_type3`, `flp_v3_nav_for_pricing_is_min_nav_capital`, and a Kani proof
`bankruptcy_gate_brackets_bp`. Suite: **483 host + 75 integration + 26 Kani**,
build-sbf clean.

### Documented as deferred (features / inherited / within-trust — NOT fixed)
- **Commit-reveal fill authenticity is fully dead** (HIGH, deliberate): pin neither
  pushes (matcher) nor settles (`buffer_settle`) the `fill_commitment` ring, and
  `fill_commitment_required` is never read. A *compromised* sequencer can fabricate
  a fill between a victim's real position and its own wallet within the ±50% band.
  Wiring matcher `buffer_push` + handler `buffer_settle` + keccak is a multi-week
  feature; until then the trader-vs-trader band is the only bound.
- **Sequencer sets the mark directly** (HIGH, within pin's stated trust model): pin
  folds oracle→mark into one sequencer-gated write where anchor stages an
  authority-gated oracle bridged by a rate-limited `settle_mark`. The misleading
  "even a compromised sequencer cannot jump the mark" doc claim in `update_oracle`
  was corrected to an honest limitation.
- **Envelope mark-move cap is optional / sequencer-bypassable** (MED, inherited):
  the sequencer omits the account to skip the cap. A mandatory-envelope arming flag
  (carved from `Market._reserved`, set at envelope init) is a deferred hardening.
- **Quorum oracle is sequencer-supplied/unattested**, **±50% trader band is loose**
  (LOW, inherited) — both within the sequencer trust boundary.
- **Haircut junior-claim engine unwired** (MED): realized gains credit collateral in
  full and losses don't accrue to the market residual (anchor routes via
  `apply_realized_pnl_delta_v2` / `accrue_haircut_loss`). Fails CLOSED (no theft,
  no mint; `convert_position` can't over-pay) — a missing protection layer, not a
  hole. Requires passing the per-position haircut accounts through the hot path.
- **Funding accrual still inert** (INFO, inherited): `cum_funding_index` never
  advanced in EITHER program → all funding moves 0, **symmetric** (no one-sided
  free-carry). Benign; wiring the rate engine is the top remaining feature.
- **`set_position_leverage` / `verify_leverage_cap` omit the sub_index bind** (LOW):
  self-scoped / read-only and `leverage_cap` enforcement is currently inert; the
  fix needs an ABI change (add `trader_state`) — deferred over churning the
  interface for an inert, no-fund-impact gap.
- **ADL `position_liq` timestamps not reset after an ADL close** (LOW, benign);
  **fee/rebate not routed to the isolated bucket** (LOW parity, no leak);
  **withdraw destination ATA not authority-constrained** (parity, not theft);
  **`execute_trigger_order` reduce-only soft-gate omits sub_index** (inherited
  schema limit; the injected order still carries the right sub).

---

## Third full external re-audit — EXHAUSTIVE 12-agent line-by-line (2026-06-30)

After two thematic rounds (#187 sub_index, #188 ADL/FLP) kept surfacing a different
LAYER each time, this pass switched method: **12 agents, every instruction diffed
line-by-line against its Anchor original**, plus dedicated arithmetic/cast and
global-invariant/race sweeps, plus **adversarial verification** of each finding
against both sources before any fix. It found a **CRITICAL the thematic rounds
missed** (a cross-cutting init-gate × settlement-drain chain) and a spread of
HIGH/MED/LOW port bugs. All L1-validatable findings fixed + regression-tested.

| Sev | Finding | Fix |
|-----|---------|-----|
| **CRITICAL** | **CR-1**: `initialize_market` is PERMISSIONLESS (no insurance-authority gate; doesn't even take the insurance account) and sets the creator as `sequencer`. Anyone creates a market → as its sequencer fabricates a fill (within the ±50% band, at a mark they also set) forcing wallet B a loss far exceeding B's collateral → the shortfall is drawn from the SHARED insurance fund and credited to wallet A → A withdraws → permissionless drain of the shared vault. Conserves `collateral+insurance` (why the invariant sweep waved it through) but drains the fund. Anchor has the CR-1 gate (`insurance_fund.authority == authority`); never ported to pin. | bind the canonical insurance PDA + require `insurance.authority == authority` (Custom 7100). Regression test `initialize_market_rejects_non_authority`. |
| **HIGH** | `book.rs::from_account_data` had only length+disc, **no header node-index bounds loop**, and `process_undelegation` ran **no `validate_node_links`** (Anchor AUDIT O-1 / ER L-2). A malicious/buggy ER validator commits a book with out-of-range RBT indices → persisted unvalidated on undelegate → next op panic-DoS (permanent brick) or in-bounds-misaligned tree corruption. | port the 6-index header bounds loop into `from_account_data` + `validate_node_links` called once from `process_undelegation` (fails closed). Test `book_validation_accepts_clean_rejects_corruption`. |
| **HIGH** | `update_oracle_from_pyth` envelope cap OPTIONAL on the **permissionless** path (Anchor H-4 makes it MANDATORY) → any keeper omits it → unbounded one-slot mark jump to a large Pyth move → mass-liquidate past the lattice budget. | make `envelope_config` a required account (revert to H-4). |
| **HIGH** | `liquidate_portfolio_v2` staleness-gated only the EXEC market; **sibling legs' marks unchecked** → a frozen sibling mark (illiquid market in an ER stall) inflates the portfolio loss → wrongful liquidation of a portfolio-healthy trader. Anchor passes every leg through `effective_health_mark`. | per-sibling `last_mark_update_slot` staleness gate. |
| **HIGH** | ER delegate handlers pass `args.seeds` **with the bump**; the undelegate callback re-derives via `find_program_address` (no bump) and matches `seeds.len()` as book=2/market=3 → a delegated account can NEVER be undelegated (funds trapped on ER). Self-inconsistent with pin's own `process_external_undelegate`. Matches the anchor 2026-06-28 fix. | drop the bump from `args.seeds` in all 3 delegate handlers (kept in the Signer seeds). |
| **MED** | fee/rebate ALWAYS debited from / credited to the CROSS pool, never the isolated bucket (anchor routes to the iso bucket when the leg is isolated) → an isolated trader with an empty cross pool could NOT settle a fill (H-1 checked_sub aborted) — a liveness bug. | route the taker fee / maker rebate to the iso bucket when isolated (sampled pre-funding); conservation unchanged. (`apply_fill` + `apply_flp_fill`.) |
| **MED** | trigger/TWAP slippage cap (`acceptable_price_ticks`) validated at PLACEMENT but **never at execution** → a keeper fires the order into a gap past the trader's cap. | port `slippage_cap_breached`; refuse to fire on breach (`execute_trigger_order` + `execute_twap_slice`). Test `slippage_cap_breached_semantics`. |
| **MED** | JIT liquidation offer admitted any price within the ±50% band → an "improving" offer PAST mark (ask above / bid below) rests un-fillable → dup-scan + `position_liq` stamp then block re-liquidation → position frozen open while the caller already skimmed the reward. | clamp the close `limit` to the marketable side of mark (close-ask ≤ mark / close-bid ≥ mark). |
| **MED** | singleton FLP (`deposit_flp_capital`/`withdraw_flp_capital`) dropped Anchor's H8 JIT-LP min-hold lock (`LpPosition` had no `deposited_at_slot`; `jit_lp_defense` wired to nothing) → flash deposit-before-a-rebate-event / withdraw-after skims honest LPs' revenue. | carve `deposited_at_slot` into `LpPosition` (104-byte layout preserved), stamp on deposit, gate withdraw via `can_withdraw(FLP_MIN_HOLD_SLOTS=150)`. |
| **LOW** | `execute_trigger_order` reduce-only gate omitted `sub_index` → a "reduce-only" trigger could increase exposure on a DIFFERENT sub. `verify_leverage_cap` omitted `sub_index` (monitor false-negative). `verify_portfolio_solvency`/`_stress` assessed isolated legs as cross (false-negative). `update_oracle_quorum`/`_from_pyth` oracle_config not PDA-bound. quorum didn't future-reject `published_at`. singleton-FLP `nav_for_pricing` (latent v3 asymmetry). trigger flag-mask / expiry-at-placement / mark-staleness-at-fire / TWAP flag-mask. | all fixed for parity. |

Suite after: **485 host + 76 integration + 26 Kani**, build-sbf clean. New tests:
`initialize_market_rejects_non_authority`, `book_validation_accepts_clean_rejects_corruption`,
`slippage_cap_breached_semantics`, `flp_v3_nav_for_pricing_*` (carried).

### Verified CLEAN by this pass (high-signal negatives)
The money-path arithmetic (every truncating `as` cast guarded; the #188 `shocked_price`
fix confirmed), the proven core (OI/PnL/funding/shortfall conservation, C-1 margin,
vault round-trip, ADL #188 cap+gate, FLP/vault #188 fixes), funding/haircut (faithful,
inert-but-symmetric), the dispatch (collision-free), the guard layer, and account
aliasing/binding were all independently re-verified sound.

### ER delegation port — DONE (2026-06-30, pending live-ER acceptance)
Finding #1 (ABI) and #3 (`cpi_undelegate`) are now **ported faithfully against the
Anchor `er.rs`** (a follow-up to this audit):
- **`cpi_delegate`** now does the full WAVE-24i fast-path staging: 8-byte
  discriminator (`write_delegate_data`); create the owner-program buffer PDA → copy
  the account in → zero it → `assign` it to System then `System::assign` → the DLP
  under the PDA seeds → CPI Delegate → close the buffer (refund rent). Byte-for-byte
  the Anchor sequence, in pinocchio (`create_pda_account`, `unsafe assign`,
  `borrow_mut_lamports_unchecked`). Compiles under **build-sbf** (real CPI codegen).
- **`cpi_undelegate` REMOVED** (Anchor deleted it). `force_undelegate_market_book` now
  returns `OwnerForceUndelegateUnavailable` (Custom 221) after its liveness gate, and
  the L1 `undelegate_market_book`/`_market`/`_fill_commitment` handlers fail closed with
  the same code, directing callers to the ER path (`commit_and_undelegate_*` →
  `process_undelegation`). Regression test `undelegate_market_book_directs_to_er_path`.
- **CAVEAT (honest):** the delegation round-trip is **ER-only-validatable** — building +
  the L1 reject/redirect tests pass, but the actual stage→delegate→commit→undelegate
  cycle MUST be confirmed on the live MagicBlock devnet ER (an `er-acceptance` run)
  before relying on it. The pin port is not deployed, so nothing live depends on it yet.

### Funding-rate engine — WIRED (2026-06-30)
`cum_funding_index` was never advanced (funding inert in both crates). Now wired with
pin's mark-only-friendly **skew-velocity** model (GMX-V2), separate from Anchor's
premium model: a new permissionless `advance_funding` crank reads the on-chain OI
skew (`long_oi − short_oi`), ramps `Market.funding_rate_e9` toward the skew target at a
bounded velocity (`funding_velocity`), and accrues the trapezoidal-average rate into
`cum_funding_index` (Q64.64) — which `settle_position_funding` already applies to every
position on fill / `settle_funding`. `set_funding_params` (market-authority) turns it on
per market (skew K / velocity / rate cap); all-`0` (the carved default) keeps it INERT,
so existing markets are unchanged. Fail-safe: no-op on a paused or stale-mark market;
per-crank `dt` clamped to `MAX_FUNDING_DT_SLOTS`; config bounded by `MAX_FUNDING_RATE_E9`.
Carved 28 bytes from `Market._reserved` (1152-byte layout unchanged). Proven e2e
(`advance_funding_accrues_index_on_positive_skew`: positive skew → index rises → longs
pay) + `inert_when_unconfigured`. Sign: positive skew ⇒ `funding_owed(long) > 0`.

### Commit-reveal fill authenticity — WIRED (2026-06-30)
The ring primitives existed but nothing pushed/settled it and `fill_commitment_required`
was never read (the HIGH "compromised sequencer can fabricate fills within the ±50%
band"). Now fully wired with on-chain keccak (the `sol_keccak256` syscall, zero-dep):
- **Producer** — `place_taker_order` pushes `keccak(fill_preimage(market, taker, maker,
  side, size, price, subs, produced_index))` for every fill it crosses (when the market
  carries the ring); ARMED-market-with-fills MUST carry the ring (H-2, else fills are
  unsettleable).
- **Consumer** — `apply_fill` recomputes the same keccak from the settled fill and
  `buffer_settle`s it FIFO; a sequencer-fabricated/reordered fill → `FillNotCommitted`
  (Custom 1102) and reverts atomically. ARMED ⇒ the ring is MANDATORY (Custom 1103).
- **`init_fill_commitment`** now ARMS the market (`fill_commitment_required = 1`),
  matching Anchor; opt-in per market, OFF for all existing markets (no behaviour change).
- The haircut (also a trailing optional account) is now identified by DISC, not
  position, so it and the ring can't be confused.
Proven e2e: `place_taker_order_pushes_commitment_when_armed` (producer pushes the
byte-exact commit), `apply_fill_settles_committed_fill` (round-trip: on-chain
`sol_keccak256` == off-chain `solana_sdk::keccak`), `apply_fill_rejects_fabricated_fill_
when_armed` (Custom 1102), `apply_fill_requires_ring_when_armed` (Custom 1103).

### Haircut junior-claim engine — WIRED (2026-06-30)
Anchor routes realized PnL through the junior-claim engine (gains DEFER to a warmup
reserve; losses credit the market Residual) to gate profit behind capital and keep
`V − C − I == Σresidual`. pin credited gains to collateral in full and didn't accrue
losses to the Residual (the audit's "fails-closed" MED). Now wired — surgically, atop
the proven primitives, gated behind `haircut_enabled` (OFF for all existing markets):
- A new `materialize_leg` (apply_fill + apply_flp_fill) routes per leg: an OPTED gain
  (the per-position haircut account is present) defers to the warmup reserve via the
  proven `haircut::apply_release` (no collateral); an opted loss debits collateral via
  the UNCHANGED `materialize_realized` AND credits the market Residual by the removed
  amount; a non-opted leg uses the proven path verbatim.
- The per-position haircut accounts are MANDATORY on a haircut-enabled market (mirror
  anchor's security gate — `Custom(244)` — so a sequencer can't omit one to route a
  gain to senior collateral and bypass the gating); found by canonical PDA + disc. The
  residual is loaded once / written once, shared by the funding settle + loss accrual.
- Extraction stays gated by `convert_position`'s `compute_h(residual, matured)`, so an
  under-fed residual just means deferred gains don't convert (fails closed; no over-pay).
Proven e2e: `apply_fill_haircut_defers_gain_accrues_loss` (gain→reserve, loss→collateral
+Residual, conservation holds), `apply_fill_haircut_requires_position_haircuts` (Custom 244).

### HONESTLY DEFERRED — features, not bugs
- **leverage/position/concentration caps unwired**, **VPIN tax / FLP-exposure tracking**
  — feature-wiring documented in prior rounds; not exploitable-today bugs.
- **`position_liq` timestamps not reset on close** (benign: over-reward capped at backing,
  self-affecting, and the cooldown defaults to 0); **`apply_flp_fill` rebate/insurance
  `saturating_add`** (deliberate liveness choice at an unreachable u64 balance — `checked_add`
  there would DoS settlement).

---

## Verified SOUND (high-signal negatives)

Pure-math core (haircut, vault_math, fill_math, funding, peg, concentration,
reduce-only, vpin, …), the fill-commit ring state machine, migrations, views,
carved-field offsets + size asserts + alignment strategy, admin/authority gating,
dispatch (no discriminator collision), CPI program pinning, force-undelegate
liveness gate, ER heartbeat auth, and the Pyth byte-parser were each reviewed and
found correct. Caller-supplied stress shocks cannot trigger a liquidation (all
liquidation paths hard-code the no-shock scenario).
