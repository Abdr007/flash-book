# Flash Book — Internal Security Audit (2026-06)

> **Rigorous internal pre-audit** by a 6-auditor adversarial team across the full
> attack surface of `programs/flash-book/src` (~32k LOC). Every finding was
> **independently re-verified against source** (file:line) by the lead before
> inclusion; refuted hypotheses are listed as **DEFENDED**. This is **not** a
> substitute for an external firm — it is the highest-fidelity internal pass and
> the input to the external audit (#39).

## Verdict

**Rating (as-is): 4 / 10 — material findings; NOT deployable until remediated.**
Down from the pre-audit estimate of 6 because the audit found **1 Critical + 8 High**,
including a critical that nullifies the flagship anti-fabrication feature (#35).
The formal-verification rigor (9/10) and the **large defended surface** (most attack
hypotheses were refuted by a real guard) keep the trajectory strong: after
remediating the Critical + Highs and a clean external audit, the realistic target is
**7–8**. Most Criticals/Highs require a **compromised sequencer** or specific setups
(not anonymous one-tx drains) — but they are real and several allow fund loss.

**Counts:** 1 Critical · 8 High · 8 Medium · ~8 Low/Info.

## Two systemic themes (fix these patterns, not just instances)

1. **Opt-in security via optional accounts with NO on-chain enforcement.** A
   solvency/authenticity control implemented as an *optional* `remaining_accounts` /
   `Option<Account>` is disable-able by the very party that builds the transaction.
   Affects **C-1 (fill-commitment ring)** and **H-2 (haircut junior-claim gate)**.
   *Fix pattern:* a sticky `*_required` bit on `MarketAccount`, set at init, that turns
   "account absent" into a hard error once armed.
2. **Incomplete propagation of the post-2026-06-21 margin-walk guard set**
   (`verify_position_pda` + exact-count + dedupe + `size>0`). Applied to
   `sweep_collateral` / `partial_withdraw_collateral` / `liquidate_portfolio_v2`, but
   **missing** on `liquidate_position_v2` (H-4), `auto_deleverage` (H-5), and
   `place_basket_order_n_v2` (H-7). Same root cause, three more sites.

---

## CRITICAL

### C-1 · Fill-commitment ring (#35) is bypassable — optional account, no arming flag
- **Location:** `lib.rs:3519` (`if let Some(fc_acct) = find_fill_commitment(...)`), `state.rs` MarketAccount (no `fill_commitment_required` field).
- **Verified:** ✅ the gate is `if Some`; no on-chain flag forces the account present.
- **Vulnerability:** The ring's stated guarantee ("a compromised sequencer cannot fabricate fills") depends on `apply_fill` consuming a ring entry — but the ring account is supplied by the sequencer via `remaining_accounts`. A compromised sequencer simply **omits it**; `find_fill_commitment` returns `None`, the verify-and-pop is skipped, and settlement proceeds unguarded (C-1 sig gate and `fill_seq` both pass for the real sequencer). Fabricate a fill between any two real traders at any price → drain a victim's collateral. The Kani-proven ring is never invoked.
- **Fix:** Add `fill_commitment_required: bool` to `MarketAccount`, set in `init_fill_commitment`; in `apply_fill`/`apply_flp_fill`, if required → `find_fill_commitment(...).ok_or(FillCommitmentMissing)?` (hard error on absence). Once armed, a market can never settle unguarded again.

---

## HIGH

### H-1 · `apply_flp_fill` price band has no oracle-staleness check
- **Location:** `lib.rs:6280` (band gate); contrast `4253`/`4428`/`6671` which DO check staleness. **Verified:** ✅ no `oracle_published_at`/`oracle_staleness` reference in `apply_flp_fill`.
- **Vulnerability:** The FLP band anchors to `market.oracle_price_ticks` without checking freshness. During an oracle outage the anchor is stale; a compromised sequencer settles FLP fills priced at the stale anchor (passes the band) but far from the real market, extracting from the pool, repeatable per `fill_seq`.
- **Fix:** Enforce `now - oracle_published_at_unix_seconds <= oracle_staleness_max_seconds` (and `published_at != 0`) before the band; fail closed otherwise.

### H-2 · Haircut junior-claim gate is bypassable (same class as C-1)
- **Location:** `lib.rs:12319/12331/12349` (`ApplyFill` haircut accounts `Option<>`); no `market.haircut_enabled` flag. **Verified:** ✅ optional, no required flag.
- **Vulnerability:** Comment says "must be present iff `market_haircut` is present" but nothing enforces it. Omitting the optional haircut accounts routes positive realized PnL **directly to collateral with zero Residual gating**, defeating the solvency engine. Since `withdraw_collateral` has no protocol-wide solvency check, the ungated profit is immediately withdrawable before the counterparty's loss is collected.
- **Fix:** `haircut_enabled` bit on `MarketAccount` (set at `initialize_haircut_state`); `require!` all three accounts present when enabled.

### H-3 · `flush_haircut_dust` breaks Residual conservation (dust double-counted)
- **Location:** `lib.rs` `flush_haircut_dust` (credits `insurance.balance_quote_lots`, never debits `residual_quote_lots`); contract in `haircut.rs:443`. **Verified:** ✅ no residual debit in the flush.
- **Vulnerability:** Per the module's own ΔResidual table, flush must apply `−dust` to Residual. It doesn't. Each flush overstates Residual by the flushed dust → inflates `h = min(Residual,Matured)/Matured` → every trader can `convert_position` more matured PnL than the real backing supports. Systematic over-extraction in a stressed market.
- **Fix:** `residual = apply_residual_delta(residual, -(dust as i128))?` after crediting insurance.

### H-4 · `liquidate_position_v2` assesses one cross leg against the full pool → wrongful liquidation
- **Location:** `lib.rs:6747` (`assess_margin_unified_fn` on a single position); no `open_positions` guard, no other-legs walk. **Verified:** ✅ no walk/guard.
- **Vulnerability:** For a cross-margined trader with several legs sharing one pool, the health gate excludes the other legs' equity. A hedged, portfolio-healthy trader (losing leg A + winning leg B) is liquidatable on A alone (B's offset excluded) → loses the hedge + pays penalty + liquidator reward. (Ignoring losing legs also lets a trader dodge — both directions wrong.)
- **Fix:** Add `open_positions <= 1` guard (route multi-position cross traders to `liquidate_portfolio_v2`), or thread the full complete-walk guard set and assess the whole portfolio.

### H-5 · `auto_deleverage` — same single-leg defect + targeted ADL
- **Location:** `lib.rs:7238` (underwater health on inline position only). Same root cause as H-4. Plus: no ranking/necessity proof — any caller can ADL any positive-PnL counterparty once `insurance < pause_threshold`, force-closing a chosen winner at bankruptcy price.
- **Fix:** Full-portfolio walk for the underwater check; restrict ADL caller or require a "no more-profitable counter" constraint.

### H-6 · `vault_withdraw_v3` has no margin / open-position gate
- **Location:** `lib.rs:8757` handler. **Verified:** ✅ burns shares + debits `collateral_quote_lots`, no `open_positions==0`, no margin re-assessment (contrast `withdraw_collateral:2650`, `partial_withdraw_collateral`).
- **Vulnerability:** A v3 vault holds open positions (`vault_place_order_v3`). `live_nav = collateral_quote_lots` ignores unrealized loss; while the vault holds a losing position, early depositors burn shares and exit at the inflated mark, removing the collateral backing the position and socializing the loss onto remaining depositors — a run on an insolvent vault. Also pulls collateral below maintenance with no IM gate.
- **Fix:** Require `open_positions == 0` on redeem, or run `assess_margin_unified_fn` over the post-withdraw collateral (as `partial_withdraw_collateral` does).

### H-7 · N-leg basket (`place_basket_order_n_v2`) missing `verify_position_pda`
- **Location:** `lib.rs:5250-5268` (owner check + *conditional* trader/market, no PDA bind); the 2-leg `PlaceBasketOrderV2` IS PDA-bound. **Verified:** ✅ no `verify_position_pda`; `size_lots==0` skips the trader/market checks.
- **Vulnerability:** Pass a small/empty PositionAccount for the leg's market so the margin lattice sees ~no exposure (healthy), but the leg injects tagged to the `trader_state`'s `sub_index` → at fill the size accrues to the trader's real, large, **unassessed** position → undercollateralized below initial margin, loss shifted to counterparties/insurance.
- **Fix:** Call `verify_position_pda(...)` unconditionally per leg and apply trader/market/liveness checks unconditionally.

### H-8 · `TwapOrderAccountV3::space()` is 4 bytes too small (correctness/availability)
- **Location:** `state_v3.rs:124-128`. **Verified:** ✅ body = 148 bytes (32+32+4+64+1+8+7); `space()` returns `8 + 144 = 152`; need `8 + 148 = 156`. The inline comment's arithmetic miscounts.
- **Impact:** A fully-populated `TwapOrderAccountV3` overruns → `AccountDidNotSerialize`, reverting V3 TWAP create/update. Not an exploit; a latent functional break.
- **Fix:** `8 + 148` (or shrink `_reserved` to `[u8;3]`).

---

## MEDIUM

- **M-1 · FLP band 20% wide** (`FLP_MAX_FILL_DEVIATION_BPS=2000`): a compromised sequencer can extract ≤20% of notional per FLP fill, repeatable. *Fix:* tighten to ~100–300 bps + per-batch deviation budget. (Depends on H-1 staleness too.)
- **M-2 · Liquidator reward pre-skimmed from liquidatee collateral** before the close fills → enlarges the insurance `cover_bad_debt` draw on a bankrupt close; no `caller != liquidatee` check (self-liquidation routes residual to main account ahead of insurance). *Fix:* pay reward from recovered collateral post-fill; block self-liquidation.
- **M-3 · v3 FLP (`flp_deposit_v3`/`flp_withdraw_v3`) missing H8 min-hold + undercollateralization check** — latent (v3 FLP exposure not yet wired into matching), becomes High the moment it is. *Fix:* port `deposited_at_slot`/`can_withdraw` + `FlpWithdrawUndercollateralized` before wiring.
- **M-4 · No global cross-domain solvency invariant** on the shared `quote_vault` (backs trader collateral + FLP + vaults + insurance). SPL is the only physical backstop; a single ledger over-credit bug → cross-domain drain. *Fix:* documented `Σledgers == vault.amount` invariant (proof/test) and/or segregate pools.
- **M-5 · Negative-fee tier mints unbacked collateral** (`MAX_FEE_DISCOUNT_BPS=12000`): the >100%-rebate credit isn't sourced from a real debit (`saturating_sub` no-op). Authority-gated (`set_trader_fee_tier`), so misconfig footgun, but the mint is real once granted. *Fix:* debit insurance/Residual for the rebate; revert if uncovered.
- **M-6 · Arena-exhaustion DoS / no per-trader order cap** (the known #36 sybil): GTC orders aren't reaped; one trader fills the shared node arena (~$0.05 for a 10k-node book), denying placement market-wide. *Fix:* per-trader cap (wire `ClaimedSeatV2.open_orders_count`) or per-order rent.
- **M-7 · N-leg basket assesses only touched markets**, not the full position set → cross-margin risk understatement. *Fix:* assess against all `open_positions`.
- **M-8 · `MAX_POSITIONS_PER_TRADER` / `MAX_STRESS_SCENARIOS` defined but UNENFORCED** → `assess_margin` is O(N²) unbounded; a trader spread across many markets can exceed the CU budget and become un-liquidatable via the portfolio path. *Fix:* enforce the caps at position-open / in the assess wrappers.

---

## LOW / INFORMATIONAL

- **L-1 · `order_id` 2²⁴ seq ceiling unenforced** — silent mask; after 16.7M lifetime placements, same-price orders collide → cancel/modify/reap mis-targets the owner's own order (no cross-trader theft; owner check holds). *Fix:* `require!(seq <= MAX_SEQ_ENCODABLE)`.
- **L-2 · No price ceiling (2⁴⁰)** — mis-ordering only in the `oracle==0` bootstrap window (band skipped). *Fix:* `require!(limit_ticks <= MAX_PRICE_TICKS_ENCODABLE)` / block placement while oracle 0.
- **L-3 · `liquidate_portfolio_v2` uses `effective_health_mark` (mark-if-fresh) not worse-of(mark,oracle)** — less conservative than the single-position path (trader can dodge briefly; never wrongful). *Fix:* parity with `health_price_with_staleness`.
- **L-4 · Truncated isolated funding advances entry index** → later margin reads less funding owed than unpaid (bounded to isolated bucket). *Fix:* track the unpaid remainder.
- **L-5 · Oracle staleness/confidence gates disable-able by config** (`== 0` skips) — authority-trusted, footgun. *Fix:* nonzero floor in param validation; reject non-distinct quorum sources.
- **L-6 · `process_undelegation` doesn't verify the target is currently delegated** before overwrite (defended by `buffer.owner == DELEGATION_PROGRAM_ID`, but defense-in-depth gap). *Fix:* require `delegated_account.owner == DELEGATION_PROGRAM_ID` at entry.
- **L-7 · Permissionless first-init of insurance/FLP singletons** → deploy-time authority front-run if deploy+init not atomic. *Fix:* pin initial authority / atomic init.
- **L-8 · `ClaimedSeatV2` seat infra is defined but entirely unwired** (dead code; the missing per-trader DoS guard).
- **Info:** funding is **dormant** (`cum_funding_index` never advanced — crank unwired); OI-scaled MMR term is dead (always 0); `view_portfolio_risk` has no PDA binding (read-only, don't trust as oracle); v3 withdraw destination ATAs unbound (defended in depth).

---

## Notable DEFENDED (refuted — real guards confirmed)

- **Fund-destination binding (the original C1 critical class):** every deposit/withdraw binds `quote_vault` via `address = insurance_fund.quote_vault` + source ATA `mint`/`authority`. No drain.
- **#35 producer↔consumer preimage equality:** byte-identical; maker/taker identity + sub_index re-bound via `verify_trader_state_pda`. (Sound *when the ring is consulted* — see C-1.)
- **`fill_seq` replay/reorder/wraparound:** Kani-proven strictly monotone; apply_fill/apply_flp_fill share one nonce.
- **C-1 sequencer gate fail-closed on the zero pubkey;** `set_market_sequencer`/`burn_market_authority` reject zero/burned.
- **The three complete margin walks** (`sweep_collateral`, `partial_withdraw_collateral`, `liquidate_portfolio_v2`): exact-count + `verify_position_pda` + dedupe + `size>0`. Gold standard.
- **Worse-of(mark,oracle) liquidation health + staleness fallback** (single-position path): Kani-proven, no wrongful liquidation via stale mark.
- **Insurance waterfall / `cover_bad_debt` / `assess_solvency*`:** Kani-proven, never mints, never reverts settlement.
- **New-code buffer layer / `find_fill_commitment` / ER ix / `reap_expired_orders` / `verify_*_pda` / `stamp_zc_discriminator`:** sound (bytemuck length-exact, owner-bound, no reinit/double-free, griefing-proof reaper).
- **Token CPI safety:** legacy SPL only (no Token-2022 hook reentrancy), transfer ordering safe, zero/dust rejected.
- **Funding zero-sum, mark-EMA band clamp (H5), haircut math (compute_h/convert), arithmetic overflow:** all Kani/test-proven sound. (The leaks are in *wire-in* sites — H-3, M-5 — not the proven math.)

## Honest caveats
- This is an **internal** audit; a real firm may find more or contest some severities.
- Several Critical/High require a **compromised sequencer** or **authority** (the threat model the protocol explicitly claims to defend against — so they count) or specific setups, not anonymous single-tx drains.
- The math/FV core is genuinely strong; the findings cluster in **enforcement wiring** and **incomplete fix propagation**, which are concentrated and fixable.

## Remediation priority
1. **C-1** (sticky required flag) — small change, restores the #35 guarantee.
2. **H-2 / H-3** (haircut enforce + dust conservation) — solvency-engine integrity.
3. **H-4 / H-5 / H-7** (propagate the margin-walk guard set to the 3 missing sites).
4. **H-1** (FLP staleness), **H-6** (vault withdraw gate), **H-8** (TWAP space).
5. Mediums, then Lows.
