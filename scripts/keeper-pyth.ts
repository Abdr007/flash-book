#!/usr/bin/env bun
// Flash Book V3 — Pyth oracle keeper bot.
//
// Continuously pulls fresh SOL/USD prices from Pyth Hermes and writes them
// into the market's oracle_* fields via `update_oracle_from_pyth`. This is
// the operational layer for P0.1 of MAINNET_READINESS.md.
//
// What it does each tick:
//   1. Hermes WebSocket gives us the latest signed VAA for our feed
//   2. PythSolanaReceiver posts the VAA → creates a fresh PriceUpdateV2 account
//   3. We call `update_oracle_from_pyth(market, oracle_config, price_update)`
//   4. The PriceUpdateV2 ephemeral account is closed (rent reclaimed)
//   5. Sleep N ms, repeat.
//
// Run:
//   RPC_URL=https://api.devnet.solana.com \
//     AUTHORITY_KEYPAIR=~/.config/solana/id.json \
//     MARKET=<base58 market pubkey> \
//     bun run scripts/keeper-pyth.ts
//
// Flags:
//   --feed-id <hex>           Pyth feed ID (default SOL/USD mainnet ID)
//   --interval-ms <ms>        Poll interval (default 5000)
//   --once                    Pull once and exit (useful for cron / testing)
//   --install-config          Run init_market_oracle_config first (authority only)
//   --tick-decimals <n>       Override the ticks-per-USD scaling (default 3)
//   --max-staleness <s>       Reject pulls older than N seconds (default 30)
//   --max-conf-bps <n>        Reject pulls with conf/price > N bps (default 100)

import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
} from '@solana/web3.js';
import { AnchorProvider, Wallet } from '@coral-xyz/anchor';
import { PythSolanaReceiver } from '@pythnetwork/pyth-solana-receiver';
import { HermesClient } from '@pythnetwork/hermes-client';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  FlashBookClient,
  insuranceFundPda,
} from '../sdk-ts/src/index.ts';

// ─── Args + env ──────────────────────────────────────────────────────
const ARGS = process.argv.slice(2);
const argFlag = (name: string) => ARGS.includes(name);
const argVal = (name: string, def?: string) => {
  const i = ARGS.indexOf(name);
  return i >= 0 && i + 1 < ARGS.length ? ARGS[i + 1] : def;
};

const RPC_URL = process.env.RPC_URL ?? 'https://api.devnet.solana.com';
const HERMES_URL = process.env.HERMES_URL ?? 'https://hermes.pyth.network';
const AUTHORITY_PATH =
  process.env.AUTHORITY_KEYPAIR ?? path.join(os.homedir(), '.config', 'solana', 'id.json');
const MARKET_STR = process.env.MARKET ?? argVal('--market');
const FEED_ID_HEX = (argVal('--feed-id') ??
  '0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d'  // SOL/USD mainnet
).replace(/^0x/, '');
const INTERVAL_MS = Number(argVal('--interval-ms', '5000'));
const ONCE = argFlag('--once');
const INSTALL_CONFIG = argFlag('--install-config');
const TICK_DECIMALS = Number(argVal('--tick-decimals', '3'));
const MAX_STALENESS = Number(argVal('--max-staleness', '30'));
const MAX_CONF_BPS = Number(argVal('--max-conf-bps', '100'));

if (!MARKET_STR) {
  console.error('Missing required: MARKET env or --market <pubkey>');
  process.exit(1);
}
const market = new PublicKey(MARKET_STR);

const C = {
  reset: '\x1b[0m', bold: '\x1b[1m', dim: '\x1b[2m',
  red: '\x1b[31m', green: '\x1b[32m', yellow: '\x1b[33m', cyan: '\x1b[36m',
};
const ok = (s: string) => `${C.green}✓${C.reset} ${s}`;
const warn = (s: string) => `${C.yellow}⚠${C.reset} ${s}`;
const dim = (s: string) => `${C.dim}${s}${C.reset}`;
const bold = (s: string) => `${C.bold}${s}${C.reset}`;

function loadKp(p: string): Keypair {
  return Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(p, 'utf8'))));
}

// ─── Main ────────────────────────────────────────────────────────────
async function main() {
  console.log(bold('\n  Flash Book V3 — Pyth Oracle Keeper\n'));
  console.log(`  RPC:       ${RPC_URL}`);
  console.log(`  Hermes:    ${HERMES_URL}`);
  console.log(`  Market:    ${market.toBase58()}`);
  console.log(`  Feed ID:   ${FEED_ID_HEX}`);
  console.log(`  Interval:  ${INTERVAL_MS}ms (${ONCE ? 'ONCE' : 'looping'})`);

  const conn = new Connection(RPC_URL, 'confirmed');
  const authority = loadKp(AUTHORITY_PATH);
  const wallet = new Wallet(authority);
  const provider = new AnchorProvider(conn, wallet, { commitment: 'confirmed' });

  // Verify wallet has funds.
  const bal = await conn.getBalance(authority.publicKey);
  console.log(`  Wallet:    ${authority.publicKey.toBase58()}  (${(bal / 1e9).toFixed(3)} SOL)`);
  if (bal < 0.05 * 1e9) {
    console.error('Wallet balance too low (need at least 0.05 SOL for posting + closing ephemeral accounts)');
    process.exit(1);
  }

  const client = new FlashBookClient(conn, wallet);
  const cfgPda = client.marketOracleConfigPda(market);

  // ─── Optional one-time setup: install the MarketOracleConfig PDA.
  if (INSTALL_CONFIG) {
    const cfgInfo = await conn.getAccountInfo(cfgPda.address);
    if (cfgInfo) {
      console.log(dim(`  MarketOracleConfig already initialized: ${cfgPda.address.toBase58()}`));
    } else {
      console.log(`\n  Installing MarketOracleConfig for the market…`);
      const feedIdBytes = Buffer.from(FEED_ID_HEX, 'hex');
      const ix = await client.initMarketOracleConfigIx({
        authority: authority.publicKey,
        market,
        pythPriceFeedId: feedIdBytes,
        maxStalenessSeconds: MAX_STALENESS,
        maxConfidenceBps: MAX_CONF_BPS,
        tickDecimals: TICK_DECIMALS,
      });
      const tx = new Transaction().add(ix);
      tx.feePayer = authority.publicKey;
      tx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
      tx.sign(authority);
      const sig = await conn.sendRawTransaction(tx.serialize());
      await conn.confirmTransaction(sig, 'confirmed');
      console.log(ok(`Installed: ${cfgPda.address.toBase58()}  sig=${dim(sig)}`));
    }
  } else {
    const cfgInfo = await conn.getAccountInfo(cfgPda.address);
    if (!cfgInfo) {
      console.error(
        `MarketOracleConfig PDA does not exist at ${cfgPda.address.toBase58()}. ` +
          `Run with --install-config first (authority signs).`,
      );
      process.exit(1);
    }
  }

  // ─── Build the Pyth helpers
  const hermes = new HermesClient(HERMES_URL);
  const pythReceiver = new PythSolanaReceiver({ connection: conn, wallet });
  const feedIdStr = `0x${FEED_ID_HEX}`;

  // ─── Pull loop
  let iteration = 0;
  const runOnce = async () => {
    iteration++;
    const t0 = Date.now();
    try {
      // 1. Fetch the latest signed VAA for SOL/USD.
      const update = await hermes.getLatestPriceUpdates([feedIdStr], { encoding: 'base64' });
      if (!update?.binary?.data?.[0]) {
        console.log(warn(`#${iteration}  no VAA returned by Hermes; skipping`));
        return;
      }
      const vaaB64 = update.binary.data[0];
      const parsed = update.parsed?.[0];
      const priceUsd = parsed
        ? Number(parsed.price.price) * Math.pow(10, parsed.price.expo)
        : NaN;
      const fetchMs = Date.now() - t0;

      // 2. Build a TransactionBuilder via PythSolanaReceiver. It will:
      //    - close any existing ephemeral PriceUpdateV2 account
      //    - post the new VAA (creates the PriceUpdateV2)
      //    - run our update_oracle_from_pyth ix
      //    - close the ephemeral after our ix (rent reclaimed)
      const txBuilder = pythReceiver.newTransactionBuilder({ closeUpdateAccounts: true });
      await txBuilder.addPostPriceUpdates([vaaB64]);

      await txBuilder.addPriceConsumerInstructions(
        async (
          getPriceUpdateAccount: (feedId: string) => PublicKey,
        ): Promise<{ instruction: TransactionInstruction; signers: Keypair[] }[]> => {
          const priceUpdateAccount = getPriceUpdateAccount(feedIdStr);
          const ix = await client.updateOracleFromPythIx({
            caller: authority.publicKey,
            market,
            priceUpdate: priceUpdateAccount,
          });
          return [{ instruction: ix, signers: [] }];
        },
      );

      // 3. Send + confirm.
      const versionedTxs = await txBuilder.buildVersionedTransactions({
        computeUnitPriceMicroLamports: 100_000,
      });

      let mainSig = '';
      for (let i = 0; i < versionedTxs.length; i++) {
        const vt = versionedTxs[i];
        vt.tx.sign([authority, ...vt.signers]);
        const sig = await conn.sendTransaction(vt.tx, {
          skipPreflight: false,
          maxRetries: 3,
        });
        await conn.confirmTransaction(sig, 'confirmed');
        if (i === versionedTxs.length - 1) mainSig = sig;
      }

      const totalMs = Date.now() - t0;
      console.log(
        ok(
          `#${iteration}  price=$${priceUsd.toFixed(4)}  ` +
            `hermes=${fetchMs}ms total=${totalMs}ms  ${dim(mainSig.slice(0, 20) + '…')}`,
        ),
      );
    } catch (e: any) {
      console.log(warn(`#${iteration}  ${e.message?.split('\n')[0] ?? e}`));
    }
  };

  await runOnce();
  if (ONCE) return;

  console.log(dim(`\n  Looping every ${INTERVAL_MS}ms. Ctrl-C to stop.\n`));
  const interval = setInterval(runOnce, INTERVAL_MS);
  process.on('SIGINT', () => {
    clearInterval(interval);
    console.log(`\n  Keeper stopped after ${iteration} iterations.`);
    process.exit(0);
  });
}

main().catch((e) => {
  console.error(`\n${C.red}Keeper failed:${C.reset}`, e);
  process.exit(1);
});
