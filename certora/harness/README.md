# Certora Solana harness — P-SOLV-4 global solvency

**Status: scaffold, not yet run.** Discharging `solvency.spec` needs a Certora
Solana license + the `cvlr` SDK. Nothing here is part of the production cargo
build or CI (the harness is `#[cfg(feature = "certora")]` and lives outside
`programs/clober/src`), so the green build is unaffected until a licensed
operator opts in.

## What is already done (no license required)

The **arithmetic** of the invariant and the **soundness** of the runtime
detector are machine-proven *today* by Kani — run them with:

```bash
cargo kani -p clober --harness full_solvent_iff_vault_covers_all_liabilities
cargo kani -p clober --harness partial_insolvency_detector_is_sound
```

and exercised at runtime by the permissionless `verify_collateral_solvency`
instruction (drift-free: it sums real on-chain collateral, so it cannot desync
from the 47 collateral-mutation sites the way a stored aggregate would).

## What Certora adds

The **all-paths preservation** proof: that *no reachable instruction path* drives
`vault ≥ Σ collateral + LP + insurance` from true to false. This is the part a
one-sided runtime sweep (which cannot enumerate unbounded traders) structurally
cannot give.

## Wiring it (license-time)

1. Add the `cvlr` SDK as a dev/verification dependency under a `certora` feature.
2. Implement the three summaries in `cvt_summaries.txt` so the ghost views in
   `specs/solvency.spec` resolve to live account reads:
   - `vaultAmount()`      → quote `TokenAccount.amount`
   - `lpCapital()`       → `LiquidityPoolAccount.total_capital_quote_lots`
   - `insuranceBalance()` → `InsuranceFundAccount.balance_quote_lots`
   - `totalCollateral()`  → Σ `TraderStateAccount.collateral_quote_lots`
                            + Σ isolated `PositionAccount.collateral_quote_lots`
3. Point `build.sh` at `cargo build-sbf` for the program crate.
4. Run:

```bash
certoraRun certora/solana_solvency.conf
```

## Proof-obligation set (the 19 balance-mutating instructions)

deposit_collateral, withdraw_collateral, partial_withdraw_collateral,
sweep_collateral, lp_deposit, lp_withdraw,
withdraw_insurance_fund, settle_funding, apply_fill, liquidate_position,
liquidate_portfolio, vault_place_order, vault_cancel_order,
settle_vault_perf_fee, lp_market_deposit, lp_market_withdraw, mature_position,
convert_position, cancel_order.

Every other handler is a view/admin/order-book op that does not move value across
the vault boundary and is closed trivially by `rule solvencyPreserved`.
