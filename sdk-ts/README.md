# @flash-book/sdk

TypeScript client for the Flash Book Anchor program.

## What's in here

- **PDA derivation** — `marketPda`, `marketBookPda`, `insuranceFundPda`,
  `flpExposurePda`, `traderStatePda`, `positionPda`.
- **Typed parameter shapes** — `MarketParamsRaw`, `InsuranceFundInitParams`,
  with `defaultMajorMarketParams()` / `defaultInsuranceFundParams()`
  helpers calibrated to the Rust program's defaults.
- **Anchor program client** — `FlashBookClient` wraps `@coral-xyz/anchor`'s
  `Program<Idl>` against the embedded `idl.json` and exposes one async
  builder per CLOB instruction:
    - `initializeInsuranceFundIx`
    - `initializeMarketIx` / `initMarketBookIx`
    - `openTraderStateIx`
    - `depositCollateralIx` / `withdrawCollateralIx`
    - `placeLimitOrderV2Ix` (maker rests in the hypertree book)
    - `placeTakerOrderV2Ix` (CLOB taker walks the book inline)
    - `cancelOrderV2Ix`
    - `applyFillIx` / `applyFlpFillIx`
- **Event types** — `BatchFillIntentEvent`, `TakerOrderClearedEvent`,
  `FillAppliedEvent`, etc.
- **Error code enum** — `FlashBookErrorCode` with `errorFamily()` and
  `errorName()` helpers for client-side classification.

## Usage sketch

```ts
import { Connection, Keypair } from '@solana/web3.js';
import { Wallet } from '@coral-xyz/anchor';
import {
  FlashBookClient,
  defaultMajorMarketParams,
  defaultInsuranceFundParams,
} from '@flash-book/sdk';

const connection = new Connection('https://api.devnet.solana.com');
const wallet = new Wallet(Keypair.generate());
const client = new FlashBookClient(connection, wallet);

// Setup
const ix1 = await client.initializeInsuranceFundIx(
  authority,
  defaultInsuranceFundParams(),
);
const ix2 = await client.initializeMarketIx({
  authority,
  baseMint, quoteMint, baseVault, quoteVault, oracleAccount,
  params: defaultMajorMarketParams(),
  initialOracleTicks: 100_000n,
});

// Trader flow — maker rests
const ix3 = await client.openTraderStateIx(trader);
const ix4 = await client.depositCollateralIx({
  trader, amount: 1_000_000n, quoteMint, quoteVault,
});
const ix5 = await client.placeLimitOrderV2Ix({
  trader,
  market: client.market(baseMint, quoteMint).address,
  side: 'short',
  sizeLots: 10n,
  limitTicks: 99_950n,
});

// Trader flow — CLOB taker walks the book inline (no batch tick needed)
const ix6 = await client.placeTakerOrderV2Ix({
  trader: otherTrader,
  market: client.market(baseMint, quoteMint).address,
  side: 'long',
  sizeLots: 10n,
  limitTicks: 99_950n,
});
```

## Strong typing caveat

This SDK consumes the Anchor IDL as JSON, which gives `Program<Idl>` —
the loose form. For fully-typed `program.methods.x(...)` builders the
recommended path is to import the IDL as a TypeScript file (see Anchor
0.30+ codegen). This package's per-instruction `*Ix()` helpers fill the
gap by hand-typing each call site.

## License

MIT (see repo root).
