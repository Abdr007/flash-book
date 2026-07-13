# LP Onboarding Guide

Clober's LP pool uses an ERC-4626-style **NAV-based vault**. Multiple LPs
deposit USDC; each receives shares proportional to their share of NAV at
deposit time. As the pool earns (maker rebates, realized PnL from LP fills,
toxicity tax), share value rises. Withdraws burn shares for proportional NAV.

## The math, in 4 lines

```
NAV               = total_capital_quote_lots + realized_pnl       (signed)
shares_to_mint    = amount_deposited × shares_outstanding / NAV    (1:1 if pool empty)
amount_returned   = shares_to_burn × NAV / shares_outstanding
share_value       = NAV / shares_outstanding
```

## Lifecycle

### 1. Pool initialization (governance, one-time)
The protocol authority calls `initialize_lp_exposure(initial_capital)`. The
authority's `LpPositionAccount` PDA is created and seeded with
`initial_capital` shares as a treasury endowment.

### 2. Your first deposit
Call `deposit_lp_capital_ix` with your USDC ATA + the amount. The chain:

1. Transfers USDC from your ATA into the protocol vault (SPL CPI).
2. Computes `shares_to_mint = amount × shares_outstanding / NAV`. If you're
   the first depositor and the pool is empty, you get 1:1 shares.
3. Creates your `LpPositionAccount` PDA via `init_if_needed` if you don't
   have one yet. Records `shares` + `total_deposited_quote_lots`.

After this, the pool's NAV = previous NAV + your amount. No PnL impact —
NAV/share stays constant.

### 3. Earning (passive)
Every time the LP pool wins a fill, your shares appreciate. Three sources:
- **Maker rebates** from `apply_lp_fill` flow into `total_capital`.
- **Realized PnL** from LP positions closing accumulates in `realized_pnl`.
- **Toxicity tax share** (when VPIN > 0) flows into `total_capital`.

You don't do anything; share value rises automatically.

### 4. Withdraw
Call `withdraw_lp_capital_ix` with the number of shares to burn. The chain:

1. Computes `amount = shares_to_burn × NAV / shares_outstanding`.
2. Validates that the pool can solvently part with that capital. If
   `markets_count > 0` (LP has open positions), you must pass each active
   market via `remaining_accounts`; the chain computes gross exposure at
   current marks and requires `(NAV - amount) ≥ gross_exposure`. If the
   LP is fully exposed, your withdraw rejects until exposure drops.
3. Transfers USDC from the protocol vault to your ATA (PDA-signed).
4. Burns `shares_to_burn` from your `LpPositionAccount`.

The amount you get back depends on the share value at withdraw time —
**not at deposit time**.

### 5. (Optional) Close ATA
After your final withdraw zeros out your position, you can call
`close_trader_ata_ix` to reclaim the ATA rent.

## Safety properties

These hold by construction (proven by 2_000-case proptest in
`programs/clober/tests/proptest_new_features.rs`):

- **No over-redemption.** Deposit X → immediate full withdraw → return ≤ X.
  The protocol always rounds in the pool's favor; truncation drift stays in
  the pool. An attacker cannot deposit-then-withdraw to drain.
- **Bootstrap is 1:1.** When the pool is empty (NAV ≤ 0 OR shares = 0), the
  first deposit mints `amount` shares. No share-price manipulation possible
  on bootstrap.
- **Withdraw guard scales with exposure.** Open LP positions block
  withdraws that would leave NAV below mark-to-market gross exposure.
  Phase 2: per-market position-specific guards via the same
  `remaining_accounts` pattern.

## What you lose

- **PnL is fungible across LPs.** If the pool takes a loss before you
  withdraw, your share value drops. There's no per-LP isolation.
- **No early redemption guarantee.** If the pool is fully exposed and the
  market is volatile, you may have to wait for LP to unwind before you can
  withdraw.
- **No per-LP fee tier.** All LPs receive PnL pro-rata by share count.
  Your discount tier (set via `set_trader_fee_tier`) only applies when YOU
  trade as a taker, not when the LP earns.

## Operator FAQ

**Q: How do I deposit?**
A: Use the SDK builder. From TypeScript:
```ts
import { CloberClient } from './client';
const ix = await client.depositLpCapitalIx({
  authority: myWallet.publicKey,
  amountQuoteLots: new BN(1_000_000_000), // 1000 USDC at 6 decimals
  quoteMint: USDC_MINT,
  quoteVault: protocol.quoteVault,
});
// Sign + send via your wallet.
```

**Q: How do I check my share value?**
A: Read `LiquidityPoolAccount.total_capital_quote_lots` and `.realized_pnl` and
`.lp_shares_outstanding`. Then `share_value = (total_capital + realized_pnl)
/ lp_shares_outstanding`. Multiply by your `LpPositionAccount.shares`.

**Q: Can I see my historical PnL?**
A: Your `LpPositionAccount` records `total_deposited_quote_lots` and
`total_withdrawn_quote_lots`. Your unrealized PnL = current share value -
average entry price (across all your deposits). Realized PnL = cumulative
withdrawn - cumulative deposited (when you've withdrawn portions).

**Q: What if the LP pool goes underwater?**
A: NAV can become negative if `realized_pnl < -total_capital`. In that case
new deposits still mint at 1:1 (bootstrap branch) and the depositor lifts
NAV back above zero. The previous LPs' shares become worthless if NAV stays
negative. The insurance fund is the first line of defense before this
happens — see `docs/SAFETY.md`.
