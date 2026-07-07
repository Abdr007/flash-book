# Flash Book — Settlement Policy (pre-committed, algorithmic-only)

This is a **pre-commitment**, published before launch, on how markets settle,
delist, and liquidate. It exists so that the answer to *"can the operator reprice
a market to their advantage in a crisis?"* is a permanent, verifiable **no**.

## The commitments

1. **Settlement price is always the robust on-chain oracle.** Delisting or
   expiry settlement uses the same robust mark the protocol uses everywhere else
   (median of oracle+EMA / on-book mid / external reference for funding & display;
   the strict **worse-of** for liquidation). There is no "settlement price"
   input an operator can set by hand.

2. **No discretionary repricing, ever.** There is no instruction that lets any
   authority substitute a chosen price for the oracle on an open market. We
   **cannot** do a JELLY-style validator-put: force-settling a book at an
   off-market price to move value between traders.

3. **Liquidation uses the conservative price by construction.** The liquidation
   health check reads the worse-of the available marks, never the median and
   never a favorable pick. This is enforced in code, not policy.

4. **A trader cannot self-liquidate onto the insurance fund.** Machine-proven:
   Kani `withdraw_cannot_self_liquidate_below_maintenance` — any withdrawal the
   reserve-margin gate allows leaves collateral ≥ maintenance margin, so no one
   can withdraw into a liquidatable state and dump the loss on insurance.

5. **The insurance fund cannot be drained by an FLP (market-maker) loss.**
   Machine-proven: Kani `bad_debt_coverage_is_insurance_isolated_and_bounded` —
   the bad-debt waterfall debits insurance only as a function of its own balance
   and the shortfall; FLP capital is a separate bucket with no drain path. This
   is the structural fix for Hyperliquid's single-vault SPOF.

## Why this is credible here and not elsewhere

Each commitment above is either enforced in the deployed program or backed by a
machine-checked proof that ships in CI — not a terms-of-service promise. The code
is public; the proofs are runnable; the settlement path has no discretionary
input to abuse. That is the trust wedge: **you don't have to trust the operator,
because the operator cannot do the thing you'd need to trust them not to do.**

## Scope / honesty

This policy governs on-chain settlement, liquidation, and delisting mechanics.
The robust-median mark for funding/display (roadmap item 4.7) and the published
margin-tier table (4.4) are tracked in `docs/ROADMAP_TO_LAUNCH.md`; where an item
is not yet code-complete, the roadmap says so. No claim here outruns its evidence.
