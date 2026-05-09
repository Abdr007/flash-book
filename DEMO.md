# Flash Book V3 — Demo

## What is Flash Book V3

A frequent-batch-auction perpetual orderbook for Solana, **specially built for
Flash Trade**: every quote level is backed by real FLP capital, every batch
clears via Walrasian uniform pricing inside a MagicBlock ER, and the matcher
participates in its own book as the maker-of-last-resort. It ships with feature
parity-or-better vs Hyperliquid (native trigger / TWAP / bracket / trailing /
iceberg orders, HIP-3 permissionless markets with slashable bond, user-managed
vaults, ADL, builder codes, multi-threshold margin alerts) and three pieces of
math that are smarter than HL: FLP-keyed concentration tier, funding-premium
TWAP dampener, and symmetric-OI funding dampener (balanced book → 0 funding).

## Try it locally (3 commands)

```bash
# Terminal 1 — build the Anchor program and start a local validator
anchor build && anchor localnet

# Terminal 2 — run the demo (fresh wallet, auto-airdrops 5 SOL)
bun run scripts/demo.ts
```

That's it. The demo prints a guided tour of every flagship instruction,
simulates the read-only view ixs, and exits clean.

## Try it on devnet

```bash
FLASH_BOOK_RPC=https://api.devnet.solana.com bun run scripts/demo.ts
```

> ⚠️ Devnet costs **test SOL**. Fund the demo wallet from the
> [Solana faucet](https://faucet.solana.com/) before running, or wire your
> own keypair into `scripts/demo.ts`. You also need a deployed program at
> the configured `FLASH_BOOK_PROGRAM_ID` (`anchor deploy --provider.cluster devnet`).

## What you'll see

- Wallet derivation + auto-airdrop on localnet
- Init-flow ixs built and printed: `initializeInsuranceFundIx`,
  `initializeFlpExposureIx`, `initializeMarketIx`, `openTraderStateIx`
- Native order-type ixs built and printed:
  - `placeLimitOrderIx` (GTC + a GTT/reduce-only variant)
  - `placeTriggerOrderIx` (stop-loss + a trailing-stop variant)
  - `placeBracketOrderIx` (atomic parent + TP + SL with OCO link)
  - `placeIcebergOrderIx` (1_000-lot order, 50 visible at a time)
  - `cancelAllOrdersInMarketIx` (single-tx flatten)
- View ixs simulated against the live RPC:
  - `viewPredictedFundingIx` → `PredictedFundingEvent` in logs
  - `viewQuoteLadderIx` → `QuoteLadderSnapshotEvent` in logs
  - `viewPortfolioRiskIx` → `PortfolioRiskEvent` in logs
- Account counts + data sizes per ix (so you can sanity-check tx-fit
  on Solana's 1232-byte cap)

## Highlights vs Hyperliquid

The chain code lives in `programs/flash-book/src/`. Every claim below
points at the line where it's implemented:

- **FBA Walrasian clearing** (no other Solana DEX has this) —
  [`programs/flash-book/src/matcher/fba.rs:43`](programs/flash-book/src/matcher/fba.rs)
  `clear_batch()` finds the uniform price that maximizes cleared volume.
- **FLP-backed CLOB** (Flash V2 pool participates in its own book) —
  [`programs/flash-book/src/matcher/flp_quoter.rs:54`](programs/flash-book/src/matcher/flp_quoter.rs)
  `generate_quotes()` synthesizes orders from real LP capital.
- **In-loop liquidations** (no external race; injected in the same batch
  that detected unhealthy state) —
  [`programs/flash-book/src/lib.rs:4517`](programs/flash-book/src/lib.rs)
  `liquidate_position()`.
- **Auto-Deleverage with bankruptcy-price math** —
  [`programs/flash-book/src/lib.rs:4877`](programs/flash-book/src/lib.rs)
  `auto_deleverage()`. Counter-eligibility re-checked on chain.
- **HIP-3 permissionless markets + safe envelope** (anyone deploys; bond
  is slashable; the envelope blocks the obvious griefing patterns) —
  [`programs/flash-book/src/lib.rs:5321`](programs/flash-book/src/lib.rs)
  `permissionless_initialize_market()`.
- **Native bracket order** (atomic parent + 2 OCO triggers in one tx —
  HL only has child-trigger-after-parent-fill; ours is one ix) —
  [`programs/flash-book/src/lib.rs:2813`](programs/flash-book/src/lib.rs)
  `place_bracket_order()`.
- **User-managed trading vaults with MTM NAV** —
  [`programs/flash-book/src/lib.rs:726`](programs/flash-book/src/lib.rs)
  `deposit_to_vault()` (markets passed via remaining_accounts for
  mark-to-market NAV during deposit).
- **Funding math smarter than HL**: FLP-keyed concentration tier
  (`MarketSnapshot::effective_mmr_bps`), funding-premium TWAP, and
  symmetric-OI dampener (balanced book → 0 funding) —
  [`programs/flash-book/src/matcher/risk.rs:142`](programs/flash-book/src/matcher/risk.rs)
  `assess_margin()` consumes the per-position effective MMR.

## How to integrate

The TypeScript SDK is a single file with one builder per ix:
[`sdk-ts/src/client.ts`](sdk-ts/src/client.ts). Every flagship
operation in the demo is a one-liner — derive the wallet, call the
builder, sign+send. The 8 production keepers (liquidation, funding,
invariant, ATA cleanup, ADL, trailing-stop, iceberg, bond-monitor) live
in [`bot/src/keepers.ts`](bot/src/keepers.ts) and share a single
`Keeper` base class.

## Next step

Want a sync to walk the Flash team through the program, the SDK, and
the keeper deployment? Reply to whoever sent you this and we'll set up
a 30-min review. Bring your own use case — we'll point at the exact
file:line that supports it.
