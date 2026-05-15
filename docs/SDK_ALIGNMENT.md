# SDK Alignment — Flash V2 Beta

How `@flash-book/sdk` aligns with the Flash V2 beta SDK
(`@flash_trade/magic-trade-client@1.x`) for projects already on Flash V2
that want to integrate Flash Book without dependency conflicts.

## Dependency matrix

| Dependency | Flash V2 (`@flash_trade/magic-trade-client@1.0.22`) | Flash Book (`@flash-book/sdk`) | Compatible |
|---|---|---|---|
| `@coral-xyz/anchor` | `^0.32.1` | `^0.32.1` | ✅ |
| `@solana/web3.js` | `^1.95.0` | `^1.95.0` | ✅ |
| `@solana/spl-token` | `^0.4.14` | `^0.4.14` | ✅ |
| `bn.js` | `^5.2.1` | `^5.2.1` | ✅ |
| `bignumber.js` | `^9.1.2` | (not used) | ✅ |
| `pyth-solana-receiver-sdk` (Rust) | `0.6.1` | `0.6.1` | ✅ |

The two SDKs can coexist in the same project's `package.json` without
peer-dependency warnings.

## Side-by-side primitives

| Concept | Flash V2 surface | Flash Book surface |
|---|---|---|
| Open position | `client.openPosition(...)` (single CPI) | `placeLimitOrderV2Ix(...)` then `apply_fill` (matcher dispatch) |
| Close position | `client.closePosition(...)` | Opposite-side `placeLimitOrderV2Ix` (matcher walk) |
| Set leverage | `client.setMaxLeverage(...)` | `setPositionLeverageIx(...)` (per-position cap) |
| Oracle pull | Pyth Lazer via off-chain client | `updateOracleFromPythIx(...)` (on-chain CPI) |
| Liquidation | Internal keeper bot | `liquidatePositionV2Ix(...)` (permissionless) |

## Coexistence pattern

For a Solana app already integrated with Flash V2 that wants to offer
Flash Book as an alternative venue:

```ts
import { FlashSdk } from '@flash_trade/magic-trade-client';
import { FlashBookClient } from '@flash-book/sdk';
import { AnchorProvider, Wallet } from '@coral-xyz/anchor';
import { Connection } from '@solana/web3.js';

const connection = new Connection(RPC_URL);
const wallet = new Wallet(keypair);
const provider = new AnchorProvider(connection, wallet, {});

// Flash V2 (LP pool model)
const flashV2 = new FlashSdk({ provider, environment: 'mainnet-beta' });

// Flash Book (CLOB model)
const flashBook = new FlashBookClient(provider);

// Route order based on user preference / liquidity:
async function routeOrder(side: 'long' | 'short', sizeUsd: bigint) {
  const v2Quote = await flashV2.previewOpen({ side, sizeUsd });
  const bookQuote = await flashBook.previewMarketOrder({ side, sizeUsd });

  if (bookQuote.expectedPrice < v2Quote.expectedPrice) {
    return flashBook.placeMarketOrder({ side, sizeUsd });
  } else {
    return flashV2.openPosition({ side, sizeUsd });
  }
}
```

## Account model differences

Flash V2 (`@flash_trade/magic-trade-client@1.x`) uses:
- Single global pool (per-pool `Pool` account)
- Per-side position custody under `Custody` account
- LP token (FLP) for pool shares

Flash Book uses:
- Per-market `MarketAccount` + hypertree `MarketBook`
- Per-trader-per-market `PositionAccount` PDA
- Optional sibling PDAs: `MarketHaircutState`, `MarketSideAccrual`,
  `MarketEnvelopeConfig`, `PositionHaircutState`
- FLP shares via `FlpExposureAccount` + `LpPositionAccount`

These models do **not** share state. A trader's Flash V2 position is
independent of their Flash Book position; collateral is in distinct
vault accounts.

## Sub-account compatibility

Flash V2's session-key pattern (web-share delegation) is compatible
with Flash Book's sub-account model:

```ts
// Both SDKs accept a delegated signer.
const delegated = new AnchorProvider(connection, delegatedWallet, {});
const bookClient = new FlashBookClient(delegated);

// Place on sub-account index 1 (Flash Book Phase 2c-2f).
await bookClient.placeLimitOrderV2Ix({
  trader: ownerPublicKey,        // identity from owner
  market: solPerpMarket,
  side: 'long',
  sizeLots: 10n,
  limitTicks: 95_000n,
  subIndex: 1,                   // routed to TraderState [SEED, owner, 1]
});
```

## Pyth + MagicBlock ER alignment

Both SDKs target:
- Pyth Solana Receiver (`pyth_solana_receiver_sdk = "0.6.1"`)
- MagicBlock ER for sub-100ms execution (mainnet endpoint
  `https://flashtrade.magicblock.app`)

Flash Book's ER delegation (`delegate_market_book`, `delegate_market`,
`delegate_commit_buffer`) is documented in `docs/V3_STATUS.md`. Wave 19b
shipped the delegation infrastructure; runtime tested on devnet.

## Risk parameter parity

Flash V2 risk params (`Custody.PricingParams`) map to Flash Book
`MarketParams` as follows:

| Flash V2 field | Flash Book equivalent |
|---|---|
| `maxLeverage` | `MarketParams.max_leverage` |
| `maxGlobalLongSizes` | `MarketParams.max_oi_base_lots` (per side) |
| `tradeSpreadLong/Short` | `MarketEnvelopeConfig.max_price_move_bps_per_slot` (different model) |
| `swapSpread` | not applicable (CLOB has explicit limit prices) |
| `maxBorrowRateBps` | `matcher/borrow_fee.rs` constants (Wave 35; not yet wired into ix) |

Operators migrating risk profiles from Flash V2 should consult
`docs/PARAMETER_PLAYBOOK.md` for per-asset recommended values.

## What's missing (vs Flash V2)

- **Pyth Lazer integration** — Flash V2 uses Lazer for sub-ms feeds.
  Flash Book uses Pyth Solana Receiver pull-oracle. Roadmap item.
- **Native ER auto-delegation on first trade** — Flash V2's
  `magic-trade-client` handles ER session lifecycle transparently.
  Flash Book exposes the delegation ix surface but doesn't auto-manage
  sessions yet.
- **Web-share P&L cards** — Flash V2 has `apps/web-share` for OG image
  rendering. Flash Book has no equivalent (yet).

## Future convergence

The intent is for Flash Book to become an interchangeable backend
behind a Flash V2-compatible UI layer. The closer the SDK surfaces
align, the easier it is for existing Flash V2 frontends to add Flash
Book as a venue with one branch on the routing layer.

Tracking items:
- Match Flash V2's `Side`, `OrderType`, `PositionStatus` enum naming.
- Expose preview helpers (`previewOpen`, `previewClose`) on
  `FlashBookClient` that return the same shape as Flash V2.
- Adopt Flash V2's `SessionContext` for ER delegation.

These are SDK-layer changes; the on-chain program needs no further
work for parity.
