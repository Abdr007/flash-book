# Flash Book — Security Architecture, Threat Model & Battle-Test Report

> Adversarial, code-grounded security review of the flash-book on-chain CLOB perps
> program. Every finding survived a **2-of-3 refutation panel against the real code**;
> the four **CRITICAL** findings were additionally **hand-verified line-by-line** by the
> lead reviewer. Verified-only, every claim cited to `file:line`. No individual named.

**Program:** `programs/flash-book/src` (Anchor 0.31.1; Pinocchio/no_std core migration in
progress). **Status: devnet only — NOT audit-ready, NOT mainnet-ready** (this document is
the gate to changing that).

---

## 0. Methodology & evidence discipline

This review fused three independent, code-grounded adversarial passes over the real
program (~30k LOC), each with its own verification panel:

| Pass | Lens | Attempts | Confirmed |
|---|---|---|---|
| Boundary audit | 10 trust boundaries, defensive | — | 22 (2 refuted) |
| Canonical red-team | 16 perps/DeFi exploit archetypes | 16 | 4 |
| Historical + novel | 26 real-incident classes + invented zero-days | 124 | 15 |

- **Adversarial verification:** every "exploitable" claim required **≥2 of 3** independent
  skeptics — each defaulting to *refute* — to confirm it against the code. Findings a guard
  already mitigates were dropped as false-positive (the panels dropped several). This is why
  an honest **DEFENDED (with the cited guard)** is a result here, not a gap.
- **Hand-verification of CRITICALs:** the reviewer personally re-opened and traced all four
  critical findings in source before they were admitted (see §4). A false critical is the
  one thing that destroys credibility; none shipped.
- **Tagging:** `[PROVEN]` = machine-proven/measured in flash-book; `[PROVEN by construction]`
  = enforced by a cited `require!`/constraint; `[PROPOSED]` = fix/invariant to build.

---

## 1. Executive summary

**The math/core layer is strong and partially machine-proven; the authentication
boundaries (who-may-call) are correct. The exposure is concentrated in three places:**

1. **Authenticity** — settlement fills are not yet bound to a real on-chain match (`F1`).
2. **Completeness** — one margin walk (`sweep_collateral`) trusts the caller's position
   set, re-introducing the 2026-06-21 critical's bug class in a handler the fix missed (`C2`).
3. **Destination binding** — the **v3 deposit** paths credit shares/collateral regardless of
   where the tokens actually landed (`C1`) — the one clean, permissionless fund-drain.

Everything else is either an **unwired-but-built backstop** (bad-debt waterfall, new-position
pause, JIT-LP hold-time — all written and unit-tested, just not called yet), a bounded
margin under-pricing, or design-hardening. **None breaks the formally-proven math core.**

**Battle-test scorecard:** of **26 historical exploit classes**, **15 DEFENDED**, **5 N-A**
(attack surface absent), **6 exploitable/partial** — and all 6 map to the confirmed findings
below, *not* to a historical mechanism being live verbatim (§3).

---

## 2. Threat model

### 2.1 Actors & trust

| Actor | Capability | Trust |
|---|---|---|
| Trader / MM | Permissionless: orders, collateral, positions, sweep | Untrusted; bounded by margin gates + PDA seeds |
| Liquidator / keeper | Permissionless: `liquidate_position_v2`, `liquidate_portfolio_v2`, `auto_deleverage` | Untrusted; no allowlist |
| **Sequencer** | Single per-market key; dispatches `apply_fill`/`apply_flp_fill` settlement | **Trusted + authenticated, NOT authenticity-verified — SPOF** |
| ER validator (MagicBlock) | Runs delegated matcher off base layer; commit/undelegate | Trusted delegation program; base-layer ownership is the backstop |
| Oracle publisher | Feeds mark via Pyth (permissionless caller) | Pyth gated by owner/feed_id/staleness/conf; envelope cap optional |
| LP | FLP/vault deposit, redeem shares | Untrusted; share↔funds invariant must hold |
| Admin / authority | init, params, delegate, set sequencer | Trusted for config; can misconfigure |

### 2.2 Assets
Trader collateral · insurance fund (shared `insurance_fund.quote_vault`, also backs FLP/vault
v3) · FLP/vault NAV · book integrity (price-time priority, `order_id` uniqueness, OI) · mark/
oracle price.

### 2.3 Trust boundaries — what breaks each (confirmed)

| Boundary | Assumes | Broken by |
|---|---|---|
| Settlement (sequencer→chain) | each `apply_fill` = a real, unconsumed match | no replay/nonce guard; args at face value (`F1`) |
| Margin walk (`remaining_accounts`) | caller supplies the *complete, authentic* position set | `sweep_collateral` (`C2`), `liquidate_portfolio_v2` (`H2`) omit guards |
| Vault deposit (token→accounting) | deposit destination IS the canonical vault | v3 deposit structs drop `address` + mint binding (`C1`) |
| Liquidation economics | reward paid only for realized close | reward paid per-injection, escalates, never closes (`H3`) |
| Oracle freshness | staleness re-check always armed | gate disabled when `oracle_staleness_max_seconds==0` (`M2`) |
| ER delegation | market recoverable to base | `undelegate_market` permanently uncallable (`M5`); no forced-exit (`M6`) |
| Protective flags | `reduce_only` can only shrink/close | accepted, enforced nowhere (`H4`) |

---

## 3. Historical-immunity matrix — "every attack that has happened on earth"

**DEFENDED** = a cited guard makes the *exact historical mechanism* impossible. **N-A** =
surface absent. **EXPLOITABLE/partial** = maps to a confirmed finding below.

| Real-world incident | Verdict | Guard / reason (`file:line`) |
|---|---|---|
| **Wormhole** $325M (sig-verify bypass) | **DEFENDED** | `require_keys_eq!(sequencer, market.sequencer)` `lib.rs:3114-3118,5757-5762` (fails closed) + per-fill `verify_trader_state_pda` `3132-3145` |
| **Cashio** $52M (type cosplay / fake accounts) | **DEFENDED** | margin walks owner-check + `try_deserialize` + `verify_position_pda` `13868-13887` |
| **Crema** (fake-tick / book substitution) | **DEFENDED** | `market_book` seeds+bump + disc + node invariants `state_v2.rs:380-398` |
| **Nirvana/Cashio** (vault destination swap) | **DEFENDED on withdraw / EXPLOITABLE on v3 deposit** | withdraw pinned `address=insurance_fund.quote_vault`; **v3 deposit not pinned → `C1`** |
| **Beanstalk** $182M (flashloan governance) | **N-A** | no token/stake voting; privileged ops gated by stored Pubkey signer, not a flashloanable balance |
| **bZx/Inverse/Rari/Fei** (AMM-as-oracle) | **N-A** | price only from Pyth `8913-8930` / quorum; no on-chain AMM consulted |
| **ERC-4626 first-depositor/inflation** | **DEFENDED** | NAV donation-immune `state.rs:631-633`; `require!(shares_to_mint>0)` `1291,8079,8550`; `pre_deposit_nav` excludes in-flight `8066` |
| **The DAO / Fei / Rari** (single-fn reentrancy) | **DEFENDED** | legacy SPL Token only (no transfer hook), transfer-before-debit safe; no Token-2022 |
| **Cream/Curve** (read-only reentrancy) | **N-A** | zero CPI in `apply_fill`/`liquidate_position_v2`/`place_taker_order_v2` |
| **Multichain** (zeroed-key authority bypass) | **DEFENDED** | `is_authorized` excludes default key `state.rs:906-908` |
| **Parity** (self-destruct brick) | **DEFENDED** | `set_market_sequencer`/`burn_market_authority` reject zero/burned `4547-4588` |
| **BeautyChain (BEC)** (mul overflow) | **DEFENDED** | u128 promotion + `checked_mul` `3173-3177`; `lot.rs:98-110` |
| **Compound** (rounding/dust theft) | **DEFENDED** | floors toward protocol `1284-1368`; haircut floor + Kani `haircut.rs:105-141` |
| **dYdX/Mango** (cross-margin contagion) | **DEFENDED** | `assess_margin_split/unified` isolates per-bucket `risk.rs:536-639`; ADL saturates isolated loss (invariant I-3) |
| **Drift** Apr-2026 $285M (mark cascade / self-liq) | **DEFENDED** | worse-of mark/oracle `6162-6181` + freshness `6138` + cooldown `6122-6126` + self-ADL forbidden `6649-6653` |
| **Solend whale** $108M (concentration cascade) | **DEFENDED** | concentration MMR `state.rs:61-67` + per-market OI cap `664-669` *(caveat: sub-account split `M9`)* |
| **bolt-terminal** decimal bug (own, 2026-03-07) | **N-A** | core is pure lot-space; `USD_DECIMALS=6` compile-time const |
| **Mango** $114M (perp mark pump) | **partial → `H5`** | mark clamp + stress lattice slow it, but EMA mark has no oracle-band ceiling |
| **Hyperliquid JELLY** (low-liq mark) | **DEFENDED oracle path / partial mark path → `H5`** | oracle fully gated; EMA-mark band missing |
| **Synthetix sKRW / GMX** (stale/single-source mark) | **partial → `H5`** | liquidation worse-of+fresh, but trade *intake* has no oracle-band check |
| **Venus LUNA** (oracle lag → bad debt) | **partial → `H6`** | staleness/conf gates present, but bankruptcy→insurance→ADL waterfall unwired |
| **Maker Black Thursday** ($0 auctions) | **partial → `H6`** | `auto_deleverage` is the backstop but permissionless-not-automatic, no Dutch widening |
| **Audius/Parity/Nomad** (init front-run) | **EXPLOITABLE → `M7`** | first caller of global singletons sets authority unconstrained |
| **Ronin/Harmony** (key compromise) | **partial → `F1`/`H1`** | sequencer key-gated + rotatable, but fills trusted with no on-chain re-match |
| **Euler/PancakeBunny** (donation + flash-deposit) | **DEFENDED donation / EXPLOITABLE JIT → `H9`** | NAV internal-only; but JIT-LP hold defense is dead code |
| **Tornado / admin-key abuse** | **DEFENDED** | `withdraw_collateral` not status-gated; MMR/IM capped <5000 `4289-4305` |

---

## 4. CRITICAL findings (hand-verified by the reviewer)

### C1 — v3 deposit paths credit shares/collateral without binding the destination vault or source mint *(Cashio-class, permissionless full-vault drain)*
**`vault_deposit_v3`** (`lib.rs:8033-8113`, struct `14626-14668`) and **`flp_deposit_v3`**
(`lib.rs:8528-8581`, struct `14839-14869`). `quote_vault` and the depositor ATA are bare
`#[account(mut)]` — **no `address = insurance_fund.quote_vault`, no `token::mint`/`authority`
binding** (verified: `14651-14656`, `14860-14865`). The handler transfers depositor tokens to
the *attacker-supplied* `quote_vault` (`8044-8053`) yet still credits
`vault_trader_state.collateral_quote_lots` and mints shares regardless (`8058-8099`).
Redemption uses the **address-bound** withdraw vault (`VaultWithdrawV3.quote_vault` pinned at
`14701`; FLP at `14900`) — so the attacker withdraws the *real* pooled funds of honest
depositors.

- **Exploit:** `create_vault_v3`/`init_flp_per_market_v3` (both permissionless) → deposit with
  `quote_vault` = an account you control (self-transfer; or a worthless-mint ATA) → receive real
  shares → `*_withdraw_v3` drains the real `insurance_fund.quote_vault`. Repeatable, zero cost.
- **Reviewer hand-verification:** confirmed the deposit handler credits by `amount` independent
  of destination (`8044-8099`), and that the *withdraw* path IS bound (`14701`) — the asymmetry
  is the smoking gun. The legacy `DepositFlpCapital` path is correctly bound (`10464`), so this
  is a **v3 regression**, not a design choice.
- **Fix:** add `#[account(mut, address = insurance_fund.quote_vault)]` to both v3 deposit
  `quote_vault`s, include `insurance_fund` in both contexts, and constrain the source ATA
  `associated_token::mint = quote_mint, authority = depositor/lp`. Optionally assert
  post-transfer `quote_vault.reload()?.amount` increased by `amount` (precedent: `1418`).
- **Blast radius:** full drain of the shared insurance/FLP/vault USDC. **Single highest priority.**

### C2 — `sweep_collateral` margin walk is defeatable by position omission *(the 2026-06-21 critical's bug class, unported to this handler)*
`sweep_collateral`'s walk (`lib.rs:1987-2026`) checks only `owner==program_id` (`1990-1991`),
`position.trader==from_trader` (`1999`), `position.market==market_ai.key()` (`2000`), and the
exact count (`1979-1982`). It **omits** the three guards `partial_withdraw_collateral` carries
(`2427-2441`): `verify_position_pda` (PDA-bind each position to the *trader_state* being swept),
market-dedupe, and `size_lots>0`. Because positions are PDA-keyed on `trader_state.key()` but
the walk only checks the *wallet* `.trader` field, **a different sub-account of the same wallet**
(or a duplicated safe position) is accepted.

- **Exploit:** wallet stages a safe decoy on sub-account B, holds risky positions on
  sub-account A, calls `sweep_collateral(A→C)` supplying B/duplicates → understated margin passes
  `assess_margin_unified` → collateral swept out → A becomes bad debt to insurance/ADL.
- **Reviewer hand-verification:** confirmed `verify_position_pda` is absent from `1987-2026` and
  present in the sibling `partial_withdraw_collateral` at `2427-2441`; `market_keys` is built
  (`2025`) but only used for scenario generation (`2027`), never to dedupe. The C-2 comment at
  `partial_withdraw` (`2395-2398`) explicitly states "% 2 == 0 alone was the bug."
- **Fix:** insert all three guards after `lib.rs:2000`, mirroring `2427-2441`.
- **Blast radius:** self-inflicted bad debt to protocol/insurance (high, not critical-on-theft,
  but the panel adjusted severity to critical given the prior incident class).

---

## 5. HIGH findings (panel-verified ≥2/3)

| # | Title | Location | Fix |
|---|---|---|---|
| **H1** | `apply_fill`/`apply_flp_fill` have no on-chain replay or fill-authenticity binding — a compromised *or crash-restarting* sequencer can replay/fabricate fills | `lib.rs:3092-3145,5746-5773`; `current_batch` only `=0` at `9603`, never incremented; `last_settlement_batch` written, never read | monotonic per-market `fill_seq` (`require!` strictly increasing) + bind settlement to a per-tx match commitment (fill-buffer/Merkle) consumed-and-cleared; interim HSM + idempotent outbox keyed on `taker_id` |
| **H2** | `liquidate_portfolio_v2` cross walk lets a liquidator OMIT risk-reducing positions → wrongful liquidation of a healthy trader | `lib.rs:6939-6997`, struct `11802-11835` | `len==(open_positions-1)*2` + `verify_position_pda` each pair + market-dedupe + `size_lots>0` |
| **H3** | Repeated liquidator-reward extraction drains liquidatee without closing the position (reward paid per-injection, escalates) | `lib.rs:6121-6126,6400-6462,6464-6514` | pay reward only in fill/settlement path proportional to size closed; dedupe resting liq orders; cap injected ≤ `size_lots`; reject `cooldown==0` |
| **H4** | `reduce_only` accepted on v2 limit/taker/modify but enforced nowhere (`check_reduce_only` has zero call sites) → "protective" close can OPEN/FLIP | `lib.rs:498-499,519/639/1053,3092-3700` | enforce via Position in v2 context + thread flag into `apply_fill`; or **fail loud** (reject bit1) until implemented. Fix doc inconsistency `lib.rs:499` vs `state_v2.rs:154` |
| **H5** | EMA mark has no oracle-band ceiling — a walked-down mark becomes the worse-of `health_price` → wrongful liquidation (Mango/JELLY/sKRW class) | `lib.rs:3826-3841` (clamp to prior mark, not oracle), gate `6162-6181`; `oracle_band_bps` validated `4279` but never enforced | clamp `new_mark` into `oracle*(1±oracle_band_bps)` when oracle fresh; enforce in the liquidation gate; add trade-intake band |
| **H6** | Bad-debt socialization gap — bankruptcy reverts instead of drawing insurance/ADL (`cover_shortfall`/`compute_shortfall` exist, **zero call sites**) | `lib.rs:13791-13793`; `insurance.rs:80`, `liquidation.rs:122` uncalled | wire `compute_realized_pnl_routing` → `InsuranceFund::cover_shortfall` → ADL before reverting; debit the fund vault |
| **H7** | Haircut "Residual ratchet-down" — gains debit Residual, losses never credit it (`Residual += |loss|` missing) → `h→0`, breaks `V−C_tot−I` | `lib.rs:13738-13739`; `realized_loss_total` dead (`9015/9480`); `apply_loss_to_capital` referenced but nonexistent `haircut.rs:209-210` | thread `MarketHaircutState` into `apply_realized_pnl_delta_v2`; loss branch `residual += |delta|` + `realized_loss_total += |delta|` (mirror `settle_funding` `3042-3061`) |
| **H8** | JIT-LP windfall — min-hold defense (`jit_lp_defense.rs`) is orphaned dead code; `withdraw_flp_capital` has no hold check; `deposited_at_slot` never persisted | `lib.rs:1340-1460`; `state.rs:819-829`; module zero call sites | add `deposited_at_slot`; set in deposit via `extend_lock_on_deposit`; gate withdraw on `can_withdraw(...)` + `flp_min_hold_slots` |
| **H9** | `convert_position` value destruction — debits Residual by `credit` but never credits trader collateral → matured PnL burned | `lib.rs:9129`, comment `9086-9090`; `HAIRCUT_MATH.md:87` | add `collateral += credit` alongside the Residual debit; consider authority gate on permissionless convert |

---

## 6. MEDIUM / LOW / INFO (condensed)

**MEDIUM** — `M1` JIT offers depletable for free, maker never bound as counterparty
(`6261-6381`) · `M2` liquidation staleness gate silently disabled by `oracle_staleness_max_seconds==0`
(`6138-6143`; init/update don't `require!>0`) · `M3` `partial_withdraw_collateral` itself lacks
an oracle-staleness gate that every other consumer has — over-withdraw against a frozen-favorable
mark while liquidation is paused (`2375-2539`) · `M4` FLP v3 share NAV ignores `realized_pnl`
(`8543-8604`) · `M5` `undelegate_market` permanently uncallable once delegated (`Account` owner
check on a delegation-owned PDA; `10268-10294`) · `M6` no permissionless forced-exit during a
stalled/censoring ER (`DelegationExpired=1702` unused) · `M7` init front-run / authority grab on
global singletons (`initialize_insurance_fund`/`init_fee_tiers`/`initialize_flp_exposure`) · `M8`
`order_id` collision via 24-bit `seq` wrap (ceiling never enforced; `state_v2.rs:226-237`) · `M9`
concentration-MMR dodgeable by splitting size across sub-accounts (`risk.rs:101-123`) · `M10`
insurance-depletion doesn't pause new position opening (`new_positions_allowed()` zero call sites)
· `M11` basket-intake margin ignores non-basket open positions · `M12` split-delegation base-vs-ER
phantom write (runtime-rejected, no explicit guard) · `M13` dust over-accrual without Residual
decrement (`flush_haircut_dust` `9157-9190`).

**LOW** — `L1` per-slot oracle envelope cap optional/omittable · `L2` ADL counter chosen by
arbitrary caller with no rank/cap proof · `L3` stress lattice omits divergent (anti-correlated)
cross-market scenarios — hedges under-margined (`risk.rs:646-681`; ~1.4× under-pricing,
conditional on listing correlated markets) · `L4` basket size==0 leg reads attacker-chosen
collateral (bounded; `apply_fill` re-derives) · `L5` `limit_ticks` not bounded vs encodable max.

**INFO** — base-layer trade on delegated book blocked only by runtime rule (add explicit owner
guard) · delegate market/book not atomic · OI-scaled crowded-MMR inert (conservative-safe) ·
`OrderType::Liquidation` priority promotion is dead code in the v2 matcher.

**Considered and refuted (NOT vulnerabilities):** unchecked `get_helper`/`get_mut_helper`
(reachable only via internally-derived indices); `from_account_data` header tampering after ER
round-trip (program-owned seeds-bound PDA, attacker bytes unreachable).

---

## 7. Novel zero-days defended by construction (the unique primitives are hardened)

Proof the bespoke primitives are battle-tested, not just untested surface — each attack *attempted*
and stopped by a cited guard:

- **ER undelegation callback forgery** → `buffer.is_signer` + `owner==DELEGATION_PROGRAM_ID` +
  PDA re-derive + size guard (`er.rs:364-397`).
- **Double-delegate / re-pin validator** → `*market_book.owner==program_id` re-assert (`259-263`).
- **JIT offer spoofing / cancel front-run / sandwich the synthetic close** → owner+disc+market+
  side+expiry gates (`6263-6330`); `cancel` requires `maker==signer` (`8737-8741`); close rests
  passively at `limit` (no slippage leg).
- **Hypertree taker-walk double-free via `order_id` collision** → matcher walks by *node index*,
  each node enumerated once (`state_v2.rs:547-571`); free-list sentinels coincide + slot zeroed
  (`454-466`).
- **Dual-stale wrongful liquidation** → refuses to liquidate when oracle stale (`6138-6143`).
- **Isolate-a-losing-position to wall off cross pool** → post-transition `assess_margin_split` +
  `require!(is_healthy)` (`2725-2736`).
- **Funding-rate manipulation** → premium clamped `clamp(K·premium,±r_max)·Δt` (`funding.rs:52-65`).
- **Residual mint/burn via funding sign** → `checked_sub`/`checked_add` with underflow error
  (`3050-3060`).

---

## 8. Security invariants — enforce or PROVE

| Invariant | Status |
|---|---|
| **INV-V1** Shares minted only against funds in the canonical vault | **[PROPOSED]** `address`-bind v3 deposit (`C1`) — highest priority |
| **INV-V2** Source mint == quote_mint, authority == depositor | **[PROPOSED]** (`C1`) |
| **INV-V4** Haircut bound on FLP/insurance shortfall | **[PROVEN]** Lean at real 1e9 divisor (axiom-clean) + Kani |
| **INV-M1** Every margin walk sees the complete, authentic, distinct, live position set | **[PROPOSED]** uniform helper → `sweep_collateral`(`C2`), `liquidate_portfolio_v2`(`H2`), basket(`M11`); `partial_withdraw` is the reference |
| **INV-M4** `effective_mmr_bps ≥ base maintenance_margin_bps` | **[PROVEN]** (saturating_add of non-negative extras) |
| **INV-S1/S2** Each fill settles a real, not-yet-consumed match; no double-apply | **[PROPOSED]** `fill_seq` + match commitment + Kani idempotency (`H1`) |
| **INV-L1** Liquidator reward paid only for realized close, once per quantity | **[PROPOSED]** (`H3`) |
| **INV-O1** Liquidation staleness gate can never be fully disabled | **[PROPOSED]** `require!(staleness>0)` at init+update (`M2`) |
| **INV-O3** Written oracle price is fresh/in-confidence/correct-feed | **[PROVEN by construction]** (`8919-8962`) |
| **INV-B4** Settlement accounts PDA-bound; cross-trader mutation impossible | **[PROVEN by construction]** (`order.trader==trader_pk` `943/1108`) |
| **INV-E1** A delegated market is always recoverable to base | **[PROPOSED]** fix `undelegate_market` + forced-exit (`M5/M6`) |

---

## 9. Decentralization / liveness security

- **Sequencer SPOF (central trust):** authenticated (`require_keys_eq!`, fails closed) but not
  authenticity-verified — `apply_fill` takes amounts at face value with no replay guard (`H1`).
  A compromised key *or a benign crash-restart re-emitting a settled batch* drains the vault.
  **Mainnet requires:** on-chain fill-authenticity commitment + monotonic `fill_seq`, interim
  HSM/idempotent-outbox, and the path to permissionless settlement (architecture §3.2).
- **ER fraud-proof boundary:** double-spend backstopped by the delegation program + base-layer
  ownership rule — works today but is *runtime-enforced, not program-asserted*; add explicit
  owner guards (`M12`) and make delegation atomic.
- **Forced-exit escape hatch — currently broken (`M5/M6`):** `undelegate_market` is uncallable
  once delegated and there is no permissionless forced-exit, so collateral backing *open*
  positions is trapped if the ER stalls (flat traders can still `withdraw_collateral` on base).
  **A working escape hatch + a live delegate→trade→undelegate→withdraw test is a mainnet prereq.**

---

## 10. Remediation roadmap — the mainnet/audit gate

**Blocking (must fix + enforce/prove before any mainnet or audit claim):**
1. **C1** — bind v3 deposit `quote_vault` to `insurance_fund.quote_vault` + constrain source ATA
   mint/authority *(permissionless full-vault drain)*.
2. **C2** — add the three margin-walk guards to `sweep_collateral` *(bad-debt extraction)*.
3. **H1** — on-chain fill authenticity + replay guard *(removes the sequencer-key drain)*.
4. **H2** — uniform `verify_position_pda + dedupe + exact-count + size>0` on the portfolio liq walk.
5. **H3** — reward only on realized close + liq-order dedup.
6. **H6** — wire the bad-debt waterfall (insurance → ADL) instead of reverting.
7. **M5/M6** — repair the ER forced-exit escape hatch + lifecycle test.
8. **H4** — enforce or fail-loud on `reduce_only`.

**Should-fix (pre-audit):** H5, H7, H8, H9, M1–M4, M7, M9, M10.
**Accept-or-harden (low/info):** L1–L5 + the INFO dead-code cleanup (so verification tooling and
refactors cannot be misled by inaccurate invariant comments — e.g. `state_v2.rs` `MAX_SEQ`/
`MAX_PRICE` ceilings claimed "enforced" but not).

**Cross-cutting:** the **v3 instruction-plumbing layer is unfinished** — `C1`, `H4`, `H6`, `H7`,
`H8`, `M4`, `M10` cluster there; treat the entire v3 deposit/vault/order surface as untrusted
until the missing constraints + adversarial tests land. Only the **haircut bound** and
**`effective_mmr ≥ base`** are machine-proven today; all settlement-authenticity, margin-
completeness, and vault-solvency invariants remain `[PROPOSED]`.

---

*All findings cite real source. The four CRITICALs were hand-verified by re-opening the code;
all other findings survived a 2-of-3 adversarial-refutation panel. False-positives were dropped,
not reported. No individual is named anywhere in this document.*

---

## 11. Remediation status & verification ledger (as of 2026-06-25)

Branch `fix-security-c1-c2` (off `main`). 573 tests + `build-sbf` green at every step.

### Fixed (committed + pushed)
| Finding | Severity | Fix summary | Commit |
|---|---|---|---|
| **C1** | CRITICAL | v3 deposit (`vault_deposit_v3`/`flp_deposit_v3`) `quote_vault`+ATA bound to `insurance_fund.quote_vault`/`quote_mint` | `24f1426` |
| **C2** | CRITICAL | `sweep_collateral` walk: `verify_position_pda`+dedupe+`size>0` | `24f1426` |
| **H2** | HIGH | `liquidate_portfolio_v2` exact-count + PDA-bind + dedupe complete-portfolio | `2580fdf` |
| **H4** | HIGH | `reduce_only` (bit1) fail-loud reject on all 4 v2-book entry points | `a405515`,`e3e6f7c` |
| **H6** | HIGH | bad-debt waterfall → insurance (`cover_bad_debt`); no revert/strand | `c82163f` |
| **H3** | HIGH | duplicate-liquidation block in `liquidate_position_v2` … | `5336605` |
| **H3 (portfolio)** | HIGH | …and ported to `liquidate_portfolio_v2` (holistic re-verify found the gap) | `be3aec9` |
| **H9** | HIGH | `convert_position` credits collateral + trader-gated (no PnL burn / griefing) | `b5b398a` |
| **H5** | HIGH | EMA-mark clamped into the oracle band (band-then-maxchange) | `260d380` |
| **H7** | HIGH | realized losses credit haircut Residual (≤ loss; no over-credit) | `7685b6a` |
| **H8** | HIGH | FLP minimum-hold (`deposited_at_slot` + `jit_lp_defense`) | `c280516` |
| **H1 (part A)** | HIGH | monotonic settlement replay guard (`fill_seq`/`last_settlement_seq`) | `8a41078` |

**Closed: 2 CRITICAL + 10 HIGH** (+ the uniformity gaps the re-verify caught).

### Formal verification added (machine-checked)
- **Kani** (`lib.rs #[cfg(kani)] mod h6_h7_solvency_proofs`, `VERIFICATION: SUCCESSFUL`):
  `cross_loss_shortfall` conservation + **no over-credit** (`removed ≤ loss` → `h`
  never inflated → no value mint); `compute_realized_pnl_routing` gain/loss exact +
  one-sided. Commits `7f0dbc7`, `34af992`. Runs in the CI Kani job.
- Pre-existing: haircut bound in **Lean** (real 1e9 divisor) + **5 Kani** proofs.

### Adversarial verification
Every fix re-attacked + cross-checked (workflows `wv774res1`, `wvc3kk3be`,
`wxc04gk3p`); the holistic combined-vector pass caught the H3-portfolio gap (now
fixed). 156+ distinct attacks attempted and survived across all passes.

### Open (remaining work)

*Refreshed after #35/#36/FV-sweep landed and PR #34 merged to `main`.*

**Closed since this doc's first draft** (no longer open): **H1 part B /
settlement authenticity (#35)** — a compromised sequencer can no longer fabricate
fills on either path (book consume-and-clear commitment ring + FLP oracle band,
both proven on-chain); the **FV sweep** (now **31 Kani proofs + 7 Lean theorems**,
all CI-gated); **IDL regen**; **PR #34 opened and merged to `main`**.

**Still open:**
- **External audit (#39)** — *the single largest remaining security item.* The code
  is **unaudited**; a third-party audit is the mainnet-upgrade gate and is not
  self-certifiable. Scope package ready (`docs/AUDIT_SCOPE.md`).
- **Book-stuffing sybil residual (#36)** — price-band + permissionless
  expiry-reaper shipped (the cheap far-from-market vector is closed). A
  **per-trader cap** (needs `ClaimedSeat` lifecycle wiring) **or per-order economic
  cost** to stop a *sybil* (N wallets × near-market orders) is the deferred 3rd fix.
- **Whole-program invariants** (`[CERTORA-TARGET]`, runtime-enforced today, not yet
  machine-proven — need the Certora Prover + a license): **P-MARGIN-4** margin-walk
  exhaustiveness, **P-SETTLE-3** no settlement path bypasses the sequencer gate,
  **P-LIQ-2** no duplicate liquidation. Prime audit targets.
- **ER trust boundary** — #35 removed the single sequencer *key* as a fabrication
  point, but on the ER path trust shifts to the **MagicBlock validator set**
  (trust-*minimized*, not trust-*less*). Full SPOF removal is a multi-validator /
  shared-sequencer redesign.
- **#37** — enumerate mediums M1–M13 / lows L1–L5 from the review record into
  discrete findings (blocked on the transcripts); H3 deeper "reward on realized
  close" refinement.

---

## Security posture — strict self-rating

**Overall: 6 / 10 — strong pre-audit engineering, NOT yet production-secure.**

This is a deliberately strict self-assessment, not a certification. For a
fund-custody perps DEX the dominant gates are *external validation* and *production
exposure* — and Flash Book has **neither** yet. No internal rigor substitutes for
an audit; that single fact caps a responsible self-rating well below the engineering
quality.

| Dimension | Score | Why |
|---|---|---|
| Formal-verification rigor | **9/10** | 31 Kani + 7 Lean, bound to deployed code, CI-gated. Genuinely top-percentile — most *audited* protocols have zero FV. |
| Known-vuln hardening (post-fix) | **8/10** | 2 CRIT + 10 HIGH closed + adversarially re-verified. (Caveat: that 12 serious issues *existed* signals real complexity/attack surface.) |
| Settlement integrity | **8/10** | Replay + fabrication closed on both paths, proven on-chain. |
| Proof completeness | **7/10** | Money-paths proven; 3 whole-program invariants (P-MARGIN-4/SETTLE-3/LIQ-2) runtime-enforced only. |
| DoS resistance | **6/10** | Cheap book-stuffing closed; sybil residual open (per-trader cap deferred). |
| Trust-minimization | **5/10** | Sequencer *key* can't fabricate, but the ER path trusts the MagicBlock validator set — trust-*minimized*, not trust-*less*. |
| **External audit** | **1/10** | **None.** The biggest gap for fund custody. |
| Production battle-testing | **2/10** | Devnet only; no bug bounty, no mainnet TVL, no incident history. |

**What would move the number:** a clean external audit (→ ~7–8), then a live bug
bounty + guarded mainnet with real TVL surviving over time (→ 8+). Until then, the
honest ceiling is ~6 regardless of internal quality. The engineering is
best-in-class for a *pre-audit* protocol; the *security posture* is mid — exactly
because "unaudited + never on mainnet" dominates.
