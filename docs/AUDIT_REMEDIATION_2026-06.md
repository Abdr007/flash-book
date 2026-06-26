# Flash Book — Audit Remediation (2026-06)

Remediation of the findings in `docs/INTERNAL_AUDIT_2026-06.md`. Done on branch
`audit-remediation` (off `d61dc14`) in an isolated worktree so it does **not**
disturb parallel work on `feat/safety-9.5-hardening`. Every fix is built
(`cargo build-sbf`), the full host suite passes, and each binds to the real handler.

## Status: CRITICAL + all 8 HIGH remediated ✅

| # | Finding | Fix | Verified |
|---|---|---|---|
| **C-1** | Fill-commitment ring bypassable (optional account) | Sticky `MarketAccount.fill_commitment_required` (set at `init_fill_commitment`); `apply_fill` hard-requires the account when armed (`FillCommitmentMissing`). **+ regression test** `armed_apply_fill_rejects_when_commitment_account_omitted` (0x200e). | ✅ |
| **H-1** | `apply_flp_fill` no oracle-staleness check | Staleness gate before the band (fail-closed; `published_at==0` = stale) when a bound is configured. | ✅ |
| **H-2** | Haircut solvency gate bypassable (optional accounts) | Sticky `MarketAccount.haircut_enabled` (set at `initialize_haircut_state`); both settlement handlers require the haircut accounts when enabled (`HaircutNotInitialized`). | ✅ |
| **H-3** | `flush_haircut_dust` breaks Residual conservation | Debit `residual_quote_lots` by dust (ΔResidual=−dust per the contract table). | ✅ |
| **H-4** | `liquidate_position_v2` single cross-leg vs full pool | Route multi-position cross traders to the portfolio path: `require!(isolated || open_positions<=1)` (`CrossLiquidationNeedsPortfolio`). | ✅ |
| **H-5** | `auto_deleverage` same single-leg defect | Same guard on the underwater position. | ✅ |
| **H-6** | `vault_withdraw_v3` no margin gate | Require the vault FLAT (`open_positions==0`) for redemptions (mirrors `settle_vault_perf_fee`). | ✅ |
| **H-7** | N-leg basket missing `verify_position_pda` | Bind each leg's position to the canonical PDA via `#[inline(never)] require_canonical_position_pda` (own BPF frame — inline derivation overflowed the stack). | ✅ |
| **H-8** | `TwapOrderAccountV3::space()` 4 bytes short | `8+144` → `8+148` (body is 148B). | ✅ |

**Systemic pattern closed:** C-1 and H-2 were the same class — an opt-in solvency/
authenticity control disable-able by the tx-builder. Both now use a sticky
`MarketAccount` flag set at arm/enable time and hard-enforced at settlement.

## ⚠️ MERGE NOTES (read before integrating into `feat/safety-9.5-hardening`)
1. **Two new `MarketAccount` fields** (`fill_commitment_required`, `haircut_enabled`).
   The parallel branch added others (e.g. `book_delegated_at_slot`). **Agree a single
   field ORDER** when merging so the Borsh account layout is identical on both sides.
2. **`init_fill_commitment` / `initialize_haircut_state` now take `market` as `mut`.**
   Off-chain callers (and tests) must pass the market account **writable** — the test
   callers in this branch were updated; production clients need the same.
3. Branch base is `d61dc14`; the parallel branch is ahead, so the liquidation
   (`H-4/H-5`) and `apply_flp_fill`/`apply_fill` edits will need conflict resolution
   against any parallel edits to those handlers.

## MEDIUM — partial

| # | Finding | Status |
|---|---|---|
| **M-1** | FLP fill band 20% wide → ≤20% notional extraction per fill by a compromised sequencer | **FIXED** — `FLP_MAX_FILL_DEVIATION_BPS` 2000→300 bps (3%). Still clears any realistic FLP spread; caps per-fill extraction at 3%. |
| **M-2** | Self-liquidation skims the Dutch-auction reward ahead of the insurance `cover_bad_debt` draw | **FIXED** — `liquidate_position_v2` requires `caller != liquidatee` (`SelfLiquidationForbidden = 2208`). |
| **M-3** | v3 FLP (`flp_deposit_v3`/`flp_withdraw_v3`) missing H8 min-hold + undercollateralization guard | **DEFERRED (latent)** — v3 FLP exposure not yet wired into matching. Port `deposited_at_slot`/`can_withdraw` + `FlpWithdrawUndercollateralized` *before* wiring. Becomes High the moment it is wired. |
| **M-4** | No global cross-domain solvency invariant on the shared `quote_vault` | **DEFERRED (design)** — needs a documented `Σledgers == vault.amount` invariant (proof/test) and/or pool segregation. Structural; not a one-line guard. |
| **M-5** | Negative-fee tier (`MAX_FEE_DISCOUNT_BPS=12000`) mints unbacked rebate credit | **DEFERRED (product decision)** — two options: (a) cap to `10_000` (disables negative fees entirely — removes a maker-incentive feature), or (b) source the rebate from a real insurance/Residual debit and revert if uncovered. Authority-gated footgun; needs the team's call on (a) vs (b). |
| **M-6** | Arena-exhaustion DoS / no per-trader order cap (#36 sybil) | **DEFERRED (larger)** — wire `ClaimedSeatV2.open_orders_count` (currently dead code) or add per-order rent. Multi-site change to the place/reap paths. |
| **M-7** | N-leg basket assesses only touched markets → cross-margin understatement | **DEFERRED (scope rework)** — assess against the trader's full `open_positions`, not just the basket legs; changes the assess call's account set. |
| **M-8** | `MAX_POSITIONS_PER_TRADER`/`MAX_STRESS_SCENARIOS` unenforced → O(N²) un-liquidatable trader | **DEFERRED (placement vs settlement)** — enforce at order **placement** (not in `apply_fill`: a revert there would strand a committed fill in the #35 ring → DoS). Needs the trader's open-position count + same-market check at intake. |

The two FIXED Mediums (M-1, M-2) are constant/guard changes — safe and verified
(build-sbf clean, full host suite 0 failed). The six DEFERRED are each structural,
a product decision, or carry a settlement/ring-safety risk that a one-line guard
can't satisfy — they're documented with the exact recommended fix above rather than
rushed in. **None is an anonymous single-tx drain.**

## Low / Info
- **~8 Low/Info** (L-1 seq ceiling, L-2 price ceiling, L-3 portfolio-mark conservatism,
  L-4 funding truncation, L-5 oracle-gate config floor, L-6 `process_undelegation`
  defense-in-depth, L-7 singleton init front-run, L-8 `ClaimedSeatV2` dead code) — not
  yet addressed; all are footguns / defense-in-depth, none a direct theft.

## Regression-test status (dedicated BanksClient/host tests)
- **C-1** — `armed_apply_fill_rejects_when_commitment_account_omitted` → `Custom(8206)`.
- **H-1** — `apply_flp_fill_rejects_stale_oracle_h1`: market with `oracle_staleness_max_seconds=60`, clock warped past the bound so the init-time oracle publish is stale → `Custom(7800)` OracleTooStale; asserts no position is created.
- **H-4** — `liquidate_position_v2_rejects_multi_leg_cross_h4`: 2-leg cross trader, single leg via the single-position path → `Custom(8207)`.
- **H-5** — `auto_deleverage_rejects_multi_leg_cross_h5`: same defect on the ADL path → `Custom(8207)`.
- **H-6** — `vault_withdraw_v3_rejects_when_vault_has_open_position_h6`: position opened on the VAULT's own trader_state via a real `apply_fill` (open_positions==1) → `Custom(7214)` SweepRequiresFlat.
- **H-7** — `place_basket_order_n_v2_rejects_noncanonical_position_h7`: attacker basket leg references a victim's real position (non-canonical for the attacker) → `Custom(7104)` WrongTrader.
- **H-8** — `twap_v3_space_matches_borsh_serialized_len`: pins the EXACT 148-byte Borsh body (stronger than the sibling `>= 8+body` checks).
- **H-3** — `flush_haircut_dust_debits_residual_h3`: drives the FULL real haircut pipeline (2 cross positions → enable haircut residual=1000 → release 1000 each → mature both, matured_total=2000 → convert one at h=0.5 ⇒ credit=500/dust=500, residual 1000→500 → flush) and asserts `residual_after == residual_before − dust` exactly (plus dust→0 and insurance +=dust). **No byte injection.** Reachable only after the Phase-2c re-key below.
- **M-2** — `liquidate_position_v2_rejects_self_liquidation_m2`: `caller == liquidatee` → `Custom(8208)`. (The self-liquidation case collapses `caller_trader_state`/`trader_state` onto one PDA; confirmed the guard fires *before* any mutation, so `8208` surfaces — not an Anchor collision.)
- H-1/H-3/H-4/H-5/H-6 + M-2 reuse a shared `open_cross_position` helper (unarmed `apply_fill` opens a zero-collateral cross leg). All are real-pipeline tests — **no byte injection**.

### Haircut Phase-2c re-key (FIXED — was a latent blocker found while testing H-3)
While building the H-3 test I found the entire interim haircut pipeline was
**unreachable on-chain**: `init_position_haircut_state` / `release_gain_to_haircut` /
`mature_position` / `convert_position` derived the position PDA from
`position.trader` (the **wallet**), e.g. `seeds = [PositionAccount::SEED,
position.market, position.trader]`. But `apply_fill` (Phase 2c) keys positions by
**`trader_state.key()`** and stores the wallet in `position.trader` (lib.rs:3736), so
wallet ≠ trader_state PDA ⇒ every haircut instruction reverted with Anchor
`ConstraintSeeds` (Custom **2006**). This made `convert_position` (the only producer
of `dust_accrued`) — and therefore H-3's flush path — dead code on any real position.

**Fix:** re-keyed all four contexts to the established Phase-2d relaxed-`trader_state`
pattern (same as `liquidate_position_v2` / `apply_fill`): a `trader_state`
`AccountLoader` is declared **before** `position` (relaxed — no seed), the position
seeds use `trader_state.key()`, and `constraint = position.trader ==
trader_state.trader` re-checks identity. A wrong `trader_state` derives a different
PDA → `ConstraintSeeds`, so it's bound canonically. Handlers are unchanged (the
`trader_state` field name is preserved; `ConvertPosition`/`ReleaseGainToHaircut`
already used it, just reordered + relaxed). Account-list order for these four
instructions changed — safe, since they had no working callers. `build-sbf` clean
(no stack regression); H-3 now passes end-to-end; full host suite 0 failed.

- **H-2** — `apply_fill_requires_haircut_accounts_when_enabled_h2`: enable the haircut engine, then settle with the haircut accounts omitted (None sentinels) → `Custom(7904)` HaircutNotInitialized; asserts no position is created.
- **M-1** — `apply_flp_fill_band_tightened_rejects_ten_percent_m1`: an FLP fill priced +10% from the fresh oracle (inside the OLD 20% band, outside the new 3%) → `Custom(8205)` FlpPriceOutsideBand; asserts no position is created.
- **Every Critical/High + both remediated Mediums (M-1, M-2) now has a dedicated test.** Only the constants/inspection items remain test-free by nature.
- These remediations should themselves be **re-reviewed** (ideally by the external audit) — a fix can introduce new issues.

## Reproduce
```bash
cargo build-sbf                                   # clean (no stack overflow)
cargo test -p flash-book                          # full host suite, 0 failed
cargo test -p flash-book --test integration \
  armed_apply_fill_rejects_when_commitment_account_omitted   # C-1 regression
```
