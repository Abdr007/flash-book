#!/usr/bin/env bun
// Reference keeper runner. Wires all four keepers (liquidation, funding,
// invariant monitor, ATA cleanup) against a JSON config.
//
// Usage:
//   bun run sdk-ts/examples/keepers.ts --rpc <URL> --keypair <PATH> --config <JSON>
//
// Config JSON shape (top-level fields are all optional — only the
// keepers you configure will run):
//
//   {
//     "liquidation": {
//       "watchlist": [{ "market": "<pubkey>", "trader": "<pubkey>" }],
//       "refreshMs": 5000,
//       "healthThreshold": 1.0
//     },
//     "funding": {
//       "watchlist": [{ "market": "<pubkey>", "trader": "<pubkey>" }],
//       "refreshMs": 60000,
//       "minOwedQuoteLots": "1000"
//     },
//     "invariant": {
//       "markets": ["<pubkey>"],
//       "refreshMs": 30000
//     },
//     "ataCleanup": {
//       "watchlist": [{ "trader": "<pubkey>", "quoteMint": "<pubkey>" }],
//       "refreshMs": 600000
//     }
//   }
//
// Set --dry-run to compute decisions without sending tx.

import { Connection, Keypair, PublicKey } from '@solana/web3.js';
import { Wallet } from '@coral-xyz/anchor';
import { readFileSync } from 'node:fs';
import {
  AtaCleanupKeeper,
  FlashBookClient,
  FundingKeeper,
  InvariantMonitor,
  Keeper,
  LiquidationKeeper,
} from '../src/index.ts';

interface CliArgs {
  rpc: string;
  keypair: string;
  config: string;
  dryRun: boolean;
}

function parseArgs(): CliArgs {
  const argv = process.argv.slice(2);
  const get = (flag: string, fallback?: string): string => {
    const i = argv.indexOf(flag);
    if (i === -1 || i + 1 >= argv.length) {
      if (fallback === undefined) throw new Error(`missing required flag ${flag}`);
      return fallback;
    }
    return argv[i + 1] as string;
  };
  return {
    rpc: get('--rpc'),
    keypair: get('--keypair'),
    config: get('--config'),
    dryRun: argv.includes('--dry-run'),
  };
}

function loadKeypair(path: string): Keypair {
  const raw = JSON.parse(readFileSync(path, 'utf8')) as number[];
  return Keypair.fromSecretKey(Uint8Array.from(raw));
}

interface KeeperConfig {
  liquidation?: {
    watchlist: { market: string; trader: string }[];
    refreshMs: number;
    healthThreshold?: number;
  };
  funding?: {
    watchlist: { market: string; trader: string }[];
    refreshMs: number;
    minOwedQuoteLots?: string;
  };
  invariant?: {
    markets: string[];
    refreshMs: number;
  };
  ataCleanup?: {
    watchlist: { trader: string; quoteMint: string; rentDestination?: string }[];
    refreshMs: number;
  };
}

async function main(): Promise<void> {
  const args = parseArgs();
  const conn = new Connection(args.rpc, 'confirmed');
  const signer = loadKeypair(args.keypair);
  const wallet = new Wallet(signer);
  const client = new FlashBookClient(conn, wallet);
  const cfg = JSON.parse(readFileSync(args.config, 'utf8')) as KeeperConfig;

  const keepers: Keeper[] = [];

  if (cfg.liquidation) {
    const k = new LiquidationKeeper(client, conn, {
      signer,
      refreshMs: cfg.liquidation.refreshMs,
      dryRun: args.dryRun,
      watchlist: cfg.liquidation.watchlist.map((w) => ({
        market: new PublicKey(w.market),
        trader: new PublicKey(w.trader),
      })),
      ...(cfg.liquidation.healthThreshold !== undefined
        ? { healthThreshold: cfg.liquidation.healthThreshold }
        : {}),
    });
    keepers.push(k);
  }

  if (cfg.funding) {
    const k = new FundingKeeper(client, conn, {
      signer,
      refreshMs: cfg.funding.refreshMs,
      dryRun: args.dryRun,
      watchlist: cfg.funding.watchlist.map((w) => ({
        market: new PublicKey(w.market),
        trader: new PublicKey(w.trader),
      })),
      ...(cfg.funding.minOwedQuoteLots !== undefined
        ? { minOwedQuoteLots: BigInt(cfg.funding.minOwedQuoteLots) }
        : {}),
    });
    keepers.push(k);
  }

  if (cfg.invariant) {
    const k = new InvariantMonitor(client, conn, {
      signer,
      refreshMs: cfg.invariant.refreshMs,
      dryRun: args.dryRun,
      markets: cfg.invariant.markets.map((m) => new PublicKey(m)),
      onAlert: ({ market, error }) => {
        console.error(`[invariant] BREACH market=${market.toBase58()} err=${error}`);
      },
    });
    keepers.push(k);
  }

  if (cfg.ataCleanup) {
    const k = new AtaCleanupKeeper(client, conn, {
      signer,
      refreshMs: cfg.ataCleanup.refreshMs,
      dryRun: args.dryRun,
      watchlist: cfg.ataCleanup.watchlist.map((w) => ({
        trader: new PublicKey(w.trader),
        quoteMint: new PublicKey(w.quoteMint),
        ...(w.rentDestination ? { rentDestination: new PublicKey(w.rentDestination) } : {}),
      })),
    });
    keepers.push(k);
  }

  if (keepers.length === 0) {
    console.error('[keepers] no keepers configured — exiting');
    process.exit(1);
  }

  for (const k of keepers) {
    k.start();
    console.log(`[keepers] started ${k.name} dryRun=${args.dryRun}`);
  }

  // Stats dump every 30s.
  const statsTimer = setInterval(() => {
    for (const k of keepers) {
      const s = k.getStats();
      console.log(
        `[${k.name}] iter=${s.iterationsCompleted} acted=${s.actionsTaken} ` +
          `txErr=${s.txErrors}` +
          (s.lastError ? ` err="${s.lastError}"` : ''),
      );
    }
  }, 30_000);

  const shutdown = (sig: string) => {
    console.log(`[keepers] received ${sig}, stopping`);
    for (const k of keepers) k.stop();
    clearInterval(statsTimer);
    process.exit(0);
  };
  process.on('SIGINT', () => shutdown('SIGINT'));
  process.on('SIGTERM', () => shutdown('SIGTERM'));
}

main().catch((e) => {
  console.error('[keepers] fatal:', e);
  process.exit(1);
});
