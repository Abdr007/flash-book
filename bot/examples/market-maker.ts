#!/usr/bin/env bun
// Reference market-maker bot for Flash Book V3.
//
// Usage:
//   bun run examples/market-maker.ts \
//     --rpc <URL> \
//     --keypair <PATH> \
//     --market <PUBKEY> \
//     --quote-mint <PUBKEY> \
//     --quote-vault <PUBKEY> \
//     [--dry-run] \
//     [--quote-size <LOTS>] \
//     [--max-inventory <LOTS>] \
//     [--max-drawdown <QUOTE_LOTS>] \
//     [--refresh-ms <MS>]
//
// Strategy: Avellaneda-Stoikov inventory-aware quoting + VPIN-scaled
// spread + drawdown kill switch. Uses post-only limits so we never pay
// taker fees, and re-quotes every batch interval.
//
// Venue is pluggable: this CLI binds to FlashBookVenue (V3). A Flash
// SDK v2 adapter implementing the same `Venue` contract can swap in
// without touching strategy code.

import {
  Connection,
  Keypair,
  PublicKey,
} from '@solana/web3.js';
import { Wallet } from '@coral-xyz/anchor';
import { readFileSync } from 'node:fs';
import {
  FlashBookClient,
  FlashBookVenue,
  MarketMaker,
  type MarketMakerConfig,
} from '../src/index.ts';

interface CliArgs {
  rpc: string;
  keypair: string;
  market: string;
  quoteMint: string;
  quoteVault: string;
  dryRun: boolean;
  quoteSize: bigint;
  maxInventory: bigint;
  maxDrawdown: bigint;
  refreshMs: number;
}

function parseArgs(): CliArgs {
  const argv = process.argv.slice(2);
  const get = (flag: string, fallback?: string): string => {
    const i = argv.indexOf(flag);
    if (i === -1 || i + 1 >= argv.length) {
      if (fallback === undefined) {
        throw new Error(`missing required flag ${flag}`);
      }
      return fallback;
    }
    return argv[i + 1] as string;
  };
  return {
    rpc: get('--rpc'),
    keypair: get('--keypair'),
    market: get('--market'),
    quoteMint: get('--quote-mint'),
    quoteVault: get('--quote-vault'),
    dryRun: argv.includes('--dry-run'),
    quoteSize: BigInt(get('--quote-size', '1')),
    maxInventory: BigInt(get('--max-inventory', '10')),
    maxDrawdown: BigInt(get('--max-drawdown', '-100000')),
    refreshMs: Number(get('--refresh-ms', '1000')),
  };
}

function loadKeypair(path: string): Keypair {
  const raw = JSON.parse(readFileSync(path, 'utf8')) as number[];
  return Keypair.fromSecretKey(Uint8Array.from(raw));
}

async function main(): Promise<void> {
  const args = parseArgs();
  const connection = new Connection(args.rpc, 'confirmed');
  const signer = loadKeypair(args.keypair);
  const wallet = new Wallet(signer);
  const market = new PublicKey(args.market);
  const quoteMint = new PublicKey(args.quoteMint);
  const quoteVault = new PublicKey(args.quoteVault);

  const client = new FlashBookClient(connection, wallet);
  const venue = new FlashBookVenue(client, connection, quoteMint, quoteVault);

  const config: MarketMakerConfig = {
    market,
    trader: signer.publicKey,
    signer,
    quoteParams: {
      // 10 bps half-spread baseline → 20 bps full spread on a calm book.
      baseSpreadBps: 10,
      // 50% spread widening at vpin=1.0; scales linearly.
      vpinSpreadAlpha: 0.5,
      // 100 bps skew per 100% inventory fraction; stronger pulls fair
      // more aggressively against our position.
      inventorySkewBpsPerUnit: 100,
      // 5% additional spread per unit of OI imbalance magnitude.
      oiImbalanceSpreadCoef: 0.05,
      quoteSizeLots: args.quoteSize,
    },
    riskLimits: {
      maxInventoryLots: args.maxInventory,
      maxDrawdownQuoteLots: args.maxDrawdown,
      // Floor at 100x quote size in collateral — leaves room for fees +
      // mark-to-market fluctuation. Tune per market.
      minCollateralQuoteLots: args.quoteSize * 100n,
    },
    quoteRefreshMs: args.refreshMs,
    dryRun: args.dryRun,
  };

  const bot = new MarketMaker(venue, config);

  console.log(`[mm] starting on ${venue.name}`);
  console.log(`[mm] trader=${signer.publicKey.toBase58()} market=${market.toBase58()}`);
  console.log(`[mm] dry_run=${args.dryRun} refresh=${args.refreshMs}ms`);
  console.log(`[mm] quote_size=${args.quoteSize} max_inv=${args.maxInventory} max_dd=${args.maxDrawdown}`);

  bot.start();

  // Periodic stats dump.
  const statsTimer = setInterval(() => {
    const s = bot.getStats();
    const q = s.lastQuote;
    console.log(
      `[mm] iter=${s.iterationsCompleted} placed=${s.ordersPlaced} cancelled=${s.ordersCancelled} ` +
        `txErr=${s.txErrors} inv=${s.lastInventory} pnl=${s.lastRealizedPnl} ` +
        `kill=${s.killSwitchActive} ` +
        (q && !q.empty
          ? `bid=${q.bidTicks} ask=${q.askTicks} fair=${q.fairValueTicks} spread_bps=${q.effectiveSpreadBps.toFixed(1)}`
          : 'no_quote') +
        (s.lastError ? ` err="${s.lastError}"` : ''),
    );
  }, Math.max(args.refreshMs, 1_000));

  // Graceful shutdown.
  const shutdown = (sig: string) => {
    console.log(`[mm] received ${sig}, stopping`);
    bot.stop();
    clearInterval(statsTimer);
    process.exit(0);
  };
  process.on('SIGINT', () => shutdown('SIGINT'));
  process.on('SIGTERM', () => shutdown('SIGTERM'));
}

main().catch((e) => {
  console.error('[mm] fatal:', e);
  process.exit(1);
});
