# Latency & Compute-Unit Budget (6.4) — disclosed methodology + measurements

> All figures are from the **deployed devnet program** `BRtnEAZ6…` (bytecode
> sha256 == the merged-`main` artifact — see `docs/ROADMAP_TO_LAUNCH.md`), read
> from real transactions' `meta.computeUnitsConsumed`. Every row links a real
> Explorer signature you can independently verify. No synthetic numbers.

## Compute-unit cost of the hot money-path instructions

Measured on the live program, keyed devnet RPC. Solana's per-instruction default
is **200,000 CU**; the whole-transaction cap is **1.4M CU**. Every core
instruction lands **well under one default budget**, so a fill or liquidation
never needs a raised compute limit.

| Instruction | CU consumed | vs. 200k default | Signature |
|---|---:|---:|---|
| `apply_fill` (settlement, incl. 2.3 accrual leg) | **41,342** | 21% | `5T5q3NqW…4XM3` |
| `claim_fee_accrual` (vault→ATA payout) | **19,375** | 10% | `3RNef6KE…smg` |
| `liquidate_position_v2` (tranche inject, 4.5) | **52,693** | 26% | `47Rs7892…6A9bS` |
| `liquidate_position_v2` (OI surcharge on, 4.4) | **55,728** | 28% | `5nrKP3ns…pXkScU` |

Interpretation:
- **Fills are cheap** — settlement, PnL realization, OI update, fee waterfall,
  fill-commitment ring pop, AND the per-domain fee accrual all fit in ~41k CU
  (≈ 1/5 of one default budget). A batch of fills comfortably fits one 1.4M-CU tx.
- **Liquidations** (the heaviest path — stress-lattice margin walk + Dutch-auction
  close injection + book mutation) stay ~53–56k CU even with the 4.4 OI surcharge
  and 4.5 tranche logic active. The margin engine's per-market decomposition and
  the hypertree's O(log n) node ops keep this flat as the book grows.
- **Claims / withdrawals** are ~19k CU (a single PDA-signed SPL transfer + a
  solvency assertion).

### Methodology (CU)
1. Deploy merged `main` to the throwaway program; verify deployed hash == artifact.
2. Drive the real instruction on devnet (the acceptance harnesses in
   `er-acceptance/` do this — armed market → book crossing → `apply_fill`, etc.).
3. Read `getTransaction(sig).meta.computeUnitsConsumed` — the on-chain-measured
   cost, not an estimate. Signatures recorded above.

## ER execution latency

The MagicBlock **Ephemeral Rollup** is where resting orders live and fills
execute; L1 is the settlement/collateral domain. The full ER delegation
round-trip — `delegate_market_book` → match on the rollup → `commit` →
`commit_and_undelegate` → `process_undelegation` — is **proven end-to-end on the
live devnet ER** (the `er-acceptance` 19/19-stage run; see the live-ER session
notes). MagicBlock's ER targets **sub-50 ms** block execution by design.

A client-observed timed benchmark harness ships in
`er-acceptance/latency_benchmark.mjs` (methodology documented inline:
`performance.now()` around `placeTakerOrderV2` send→confirm on the ER
connection, which is an **upper bound** — it includes client↔ER network RTT
since the measuring client is not co-located with the sequencer). Running it for
a headline number requires the delegated ER market's oracle/anti-stuffing-band
config tuned to the sample order prices; the round-trip correctness itself is
already established by the acceptance run above.

**Honest disclosure:** we publish CU (on-chain-measured, verifiable) as the hard
efficiency number. The ER wall-clock figure depends on client location and the
MagicBlock sequencer, so we characterize it as "sub-50 ms class, round-trip
proven" rather than quoting a single latency our client geography would bias.
