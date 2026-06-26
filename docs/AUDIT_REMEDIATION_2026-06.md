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

## Remaining (not in this branch)
- **8 Medium** (M-1 FLP band width, M-2 liquidator-reward pre-skim, M-3 latent v3-FLP
  guards, M-4 cross-domain solvency invariant, M-5 negative-fee mint, M-6 sybil
  arena DoS, M-7 basket scope, M-8 O(N²) position DoS) and **~8 Low/Info**.
- **Dedicated regression tests** for H-1/H-2/H-3/H-4/H-5/H-6/H-7/H-8 (C-1 has one).
  The fixes are verified by build + the full suite passing + inspection against each
  auditor scenario; per-finding negative tests are the recommended follow-up.
- These remediations should themselves be **re-reviewed** (ideally by the external
  audit) — a fix can introduce new issues.

## Reproduce
```bash
cargo build-sbf                                   # clean (no stack overflow)
cargo test -p flash-book                          # full host suite, 0 failed
cargo test -p flash-book --test integration \
  armed_apply_fill_rejects_when_commitment_account_omitted   # C-1 regression
```
