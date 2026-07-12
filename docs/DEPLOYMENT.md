# Deployment runbook

How to take this from `cargo test` to a working market on devnet.
The same steps apply on mainnet with `--cluster mainnet-beta`.

## Prerequisites

```bash
solana --version          # 2.x (Agave)
anchor --version          # 0.31.x  ← the program's anchor-lang version
bun --version             # 1.3+
rustc --version           # 1.84+ for native; nightly for current cargo
```

A funded keypair on the target cluster:

```bash
solana-keygen new -o ~/.config/solana/devnet.json
solana config set -u devnet -k ~/.config/solana/devnet.json
solana airdrop 2
```

## 1. Build (BPF)

```bash
cd /path/to/clober
cargo build-sbf --tools-version v1.52 --manifest-path programs/clober/Cargo.toml --sbf-out-dir target/deploy
# platform-tools v1.52 (rustc 1.89) is required: earlier releases cannot
# compile edition2024 dependencies.
```

This produces:
- `target/sbf-solana-solana/release/clober.so` (or
  `target/deploy/clober.so` via Anchor) — the BPF program binary
- `target/idl/clober.json` — the Anchor IDL (or regenerate
  via `anchor idl build -p clober > idl/clober.json`)

The build is clean as of Phase 2j (commit `66bde61`). Earlier
versions of this runbook noted a `constant_time_eq` edition2024
dependency conflict — resolved by Solana platform-tools v1.49+.

## 2. Deploy

```bash
solana program deploy target/sbf-solana-solana/release/clober.so \
  --program-id keys/clober-keypair.json
```

Capture the printed program ID. The declared ID lives in
`programs/clober/src/lib.rs::declare_id!()` and in `Anchor.toml`;
both must match the deployed key. Current devnet ID:
`5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq`. If you need to
override, regenerate with `anchor keys sync` after building and
update `tests/integration.rs::PROGRAM_ID_STR` to match (it's pinned
at file scope for the runtime DeclaredProgramIdMismatch check).

## 3. Initialize protocol PDAs (one-time)

These two are global and create at the program ID's PDA seeds:
- `["insurance_fund"]`
- `["lp_exposure"]`

Using the SDK:

```ts
import { Connection, Keypair } from '@solana/web3.js';
import { Wallet } from '@coral-xyz/anchor';
import { CloberClient, defaultInsuranceFundParams } from './client';

const conn = new Connection('https://api.devnet.solana.com');
const authority = /* load deployer keypair */;
const client = new CloberClient(conn, new Wallet(authority));

const setupTx = await new Transaction()
  .add(await client.initializeInsuranceFundIx(authority.publicKey, defaultInsuranceFundParams()))
  .add(await client.initializeLiquidityPoolIx(authority.publicKey, new BN(5_000_000)));
await sendAndConfirmTransaction(conn, setupTx, [authority]);
```

## 4. Initialize a market

For each (base_mint, quote_mint) pair, plus pre-created token vaults
and an oracle account (Pyth price account on mainnet):

```ts
import { defaultMajorMarketParams } from './client';

const initTx = new Transaction().add(
  await client.initializeMarketIx({
    authority: authority.publicKey,
    baseMint, quoteMint, baseVault, quoteVault, oracleAccount,
    params: defaultMajorMarketParams(),
    initialOracleTicks: new BN(100_000),  // starting price in ticks
  }),
);
await sendAndConfirmTransaction(conn, initTx, [authority]);
```

The `defaultMajorMarketParams()` is calibrated for SOL/BTC/ETH-style
liquid markets. Use a different param set for long-tail markets
(narrower lot/tick, wider liq penalty, smaller LP cap per batch).

## 5. Onboard the first trader

```ts
const trader = /* load trader keypair */;
const traderClient = new CloberClient(conn, new Wallet(trader));

const tx = new Transaction()
  .add(await traderClient.openTraderStateIx(trader.publicKey))
  .add(await traderClient.depositCollateralIx(trader.publicKey, new BN(50_000)));
await sendAndConfirmTransaction(conn, tx, [trader]);
```

## 6. Place an order

```ts
const market = client.market(baseMint, quoteMint).address;
await sendAndConfirmTransaction(
  conn,
  new Transaction().add(
    await traderClient.placeLimitOrderIx({
      trader: trader.publicKey,
      market,
      side: 'long',
      sizeLots: new BN(10),
      limitTicks: new BN(99_950),
      postOnly: false,
    }),
  ),
  [trader],
);
```

The first order on a (market, trader) pair pays rent for the position
PDA. Subsequent orders are zero-rent.

## 7. Run a batch

The sequencer (in production: an MagicBlock ER node; in dev: a cron
job calling the program every 50 ms) submits:

```ts
await sendAndConfirmTransaction(
  conn,
  new Transaction().add(
    await sequencerClient.runBatchIx({
      sequencer: sequencer.publicKey,
      market,
      nowMs: new BN(Date.now()),
    }),
  ),
  [sequencer],
);
```

Subscribe to events to consume fills:

```ts
import { subscribeToProgramEvents } from './client';

subscribeToProgramEvents(conn, (event, slot, sig) => {
  if (event.name === 'BatchClearedEvent') {
    console.log(`batch ${event.data.batchNum}: ${event.data.fillCount} fills`);
  }
});
```

For each `FillAppliedEvent` in a batch's logs, the sequencer (or any
authorized actor) submits an `apply_fill` or `apply_lp_fill` tx to
mutate the affected Position PDAs.

## 8. Run a liquidation bot

```ts
import { previewPortfolioRisk, fetchPosition, fetchTraderState, fetchMarket } from './client';

async function checkAndLiquidate(traderPk: PublicKey) {
  const position = await fetchPosition(client, client.position(market, traderPk).address);
  if (!position || position.sizeLots.isZero()) return;
  const traderState = await fetchTraderState(client, client.traderState(traderPk).address);
  const marketAcct = await fetchMarket(client, market);
  if (!traderState || !marketAcct) return;

  const preview = previewPortfolioRisk(
    [position],
    new Map([[market.toBase58(), marketAcct]]),
    traderState.collateralQuoteLots.toNumber(),
  );

  if (!preview.isHealthy) {
    // Submit liquidate_position. The on-chain matcher will re-verify and
    // reject if the trader isn't actually unhealthy at execution time.
    await sendAndConfirmTransaction(
      conn,
      new Transaction().add(
        await client.liquidatePositionIx({
          caller: liquidator.publicKey,
          market,
          trader: traderPk,
        }),
      ),
      [liquidator],
    );
  }
}
```

## 9. Operational pause (circuit breaker)

```ts
import { MarketStatus } from './client';

await sendAndConfirmTransaction(
  conn,
  new Transaction().add(
    await client.setMarketStatusIx({
      authority: authority.publicKey,
      market,
      newStatus: MarketStatus.Paused,
    }),
  ),
  [authority],
);
```

Status `Paused` blocks all `place_limit_order`. Existing positions can
be closed via `apply_fill` (no status gate) or liquidated.

## 10. Authority transfer (key rotation)

```ts
await sendAndConfirmTransaction(
  conn,
  new Transaction().add(
    await client.transferMarketAuthorityIx({
      authority: authority.publicKey,
      market,
      newAuthority: newKey.publicKey,
    }),
  ),
  [authority],
);
```

The old authority can no longer call `update_oracle`, `set_market_status`,
`update_market_params`, or `transfer_market_authority` again. Verified
in `transfer_market_authority_rotates_keys` E2E test.

## 11. Health monitoring

Run `examples/live-monitor.ts` as a background service:

```bash
CLOBER_LIVE=1 \
CLOBER_RPC=<your_rpc> \
  bun run examples/live-monitor.ts <market_pda> <trader_pubkey>
```

Prints state snapshots and live-streams events with timestamps + sigs.

## Acceptance gates before mainnet

These all map to entries in `docs/SAFETY.md` § "Audit checklist":

- [x] All matcher math integer with checked overflow
- [x] PDA seed validation on every account access
- [x] Status circuit breaker
- [x] Authority transfer auditable
- [x] 17 E2E integration tests pass
- [x] 12K-case property tests pass
- [ ] BPF build successful (blocked upstream)
- [ ] Mainnet shadow mode (Phase 2)
- [ ] Independent third-party audit
