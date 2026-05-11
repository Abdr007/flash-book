#!/usr/bin/env bun
// Flash Book V3 Sequencer — bridges ER matcher ticks to mainnet
// settlement. Subscribes to `BatchFillIntentEvent` on the configured
// cluster (typically the MagicBlock ER), then dispatches the matching
// `apply_fill` (human maker) or `apply_flp_fill` (FLP maker) ix on
// mainnet to settle the fill into Position + TraderState PDAs.
//
// MVP scope:
//   • WebSocket subscribe to program logs
//   • Decode events via Anchor's BorshEventCoder
//   • Per-fill in-memory dedup (taker_id+maker_id+slot) — defeats
//     duplicate emits if WebSocket reconnects mid-batch
//   • Sign + send apply_fill / apply_flp_fill on mainnet
//   • Console-only logging — production hardens with Prometheus
//     metrics, structured logging, on-disk dedup, retry queues
//
// Production hardening (out of MVP scope):
//   • Persistent dedup (Redis/Postgres) so a sequencer restart
//     doesn't double-apply
//   • Bounded retry queue with exponential backoff
//   • Multi-sequencer coordination (leader election or sharded by market)
//   • Metrics: fills/sec, p50/p99 settle latency, mainnet errors
//
// USAGE
//   AUTHORITY_KEYPAIR=~/.config/solana/devnet.json \
//   ER_RPC=https://er.devnet.example/rpc \
//   ER_WS=wss://er.devnet.example/ws \
//   MAINNET_RPC=https://api.devnet.solana.com \
//   bun run scripts/sequencer.ts

import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  type Logs,
} from '@solana/web3.js';
import { BorshEventCoder, Wallet } from '@coral-xyz/anchor';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  FLASH_BOOK_PROGRAM_ID,
  FlashBookClient,
  IDL,
} from '../sdk-ts/src/index.ts';

// ─── Config ──────────────────────────────────────────────────────────

const AUTHORITY_KEYPAIR =
  process.env.AUTHORITY_KEYPAIR ?? path.join(os.homedir(), '.config', 'solana', 'id.json');

// Known-good Flash production endpoints (used by flash-mobile + the
// existing Flash V2 deploy):
//   • ER mainnet:        https://flashtrade.magicblock.app
//                        wss://flashtrade.magicblock.app  (WS path)
//   • Solana mainnet:    https://api.mainnet-beta.solana.com
//                        (replace with a paid RPC for production —
//                        the public endpoint is heavily rate-limited)
//
// CLUSTER env selects which set we default to:
//   CLUSTER=mainnet  → Flash ER + Solana mainnet
//   CLUSTER=devnet   → public devnet (safe testing)
//   CLUSTER=local    → localnet (default, fully offline)
const CLUSTER = (process.env.CLUSTER ?? 'local').toLowerCase();

let ER_RPC_DEFAULT = 'http://127.0.0.1:8899';
let ER_WS_DEFAULT = 'ws://127.0.0.1:8900';
let MAINNET_RPC_DEFAULT = ER_RPC_DEFAULT;
if (CLUSTER === 'mainnet') {
  ER_RPC_DEFAULT = 'https://flashtrade.magicblock.app';
  ER_WS_DEFAULT = 'wss://flashtrade.magicblock.app';
  MAINNET_RPC_DEFAULT = 'https://api.mainnet-beta.solana.com';
} else if (CLUSTER === 'devnet') {
  ER_RPC_DEFAULT = 'https://api.devnet.solana.com';
  ER_WS_DEFAULT = 'wss://api.devnet.solana.com';
  MAINNET_RPC_DEFAULT = 'https://api.devnet.solana.com';
}

const ER_RPC = process.env.ER_RPC ?? ER_RPC_DEFAULT;
const ER_WS = process.env.ER_WS ?? ER_WS_DEFAULT;
const MAINNET_RPC = process.env.MAINNET_RPC ?? MAINNET_RPC_DEFAULT;

// FLP marker — FLP fills carry maker == Pubkey::default. The sequencer
// detects this and routes to apply_flp_fill instead of apply_fill.
const FLP_MARKER = PublicKey.default;

interface BatchFillIntent {
  market: PublicKey;
  taker: PublicKey;
  maker: PublicKey;
  takerSide: number;
  sizeLots: bigint;
  priceTicks: bigint;
  takerId: bigint;
  makerId: bigint;
}

// ─── Helpers ─────────────────────────────────────────────────────────

function loadKeypair(p: string): Keypair {
  const raw = JSON.parse(fs.readFileSync(p, 'utf8')) as number[];
  return Keypair.fromSecretKey(new Uint8Array(raw));
}

function validateRpcUrl(label: string, url: string) {
  if (url.includes('mainnet') && url.startsWith('http://')) {
    throw new Error(
      `Refusing http:// URL for ${label} on mainnet: ${url}. Use https://`,
    );
  }
}

function fillKey(f: BatchFillIntent): string {
  return `${f.market.toBase58()}:${f.takerId}:${f.makerId}`;
}

// ─── Main ────────────────────────────────────────────────────────────

async function main() {
  console.log(`▶ Flash Book V3 Sequencer`);
  console.log(`  Authority:   ${AUTHORITY_KEYPAIR}`);
  console.log(`  ER RPC:      ${ER_RPC}`);
  console.log(`  ER WS:       ${ER_WS}`);
  console.log(`  Mainnet RPC: ${MAINNET_RPC}`);

  validateRpcUrl('ER_RPC', ER_RPC);
  validateRpcUrl('MAINNET_RPC', MAINNET_RPC);

  const sequencer = loadKeypair(AUTHORITY_KEYPAIR);
  console.log(`  Sequencer pubkey: ${sequencer.publicKey.toBase58()}`);

  // Two connections: ER (subscribe to events), Mainnet (send settlement ixs).
  const erConn = new Connection(ER_RPC, { wsEndpoint: ER_WS, commitment: 'confirmed' });
  const mainnetConn = new Connection(MAINNET_RPC, 'confirmed');
  const wallet = new Wallet(sequencer);
  const mainnetClient = new FlashBookClient(mainnetConn, wallet);

  // Anchor's event coder decodes Borsh-encoded `emit!()` payloads from
  // the program log line `Program data: <base64>`.
  const eventCoder = new BorshEventCoder(IDL);

  // In-memory dedup. Production: persist to disk so a restart doesn't
  // double-apply. Bounded LRU keeps memory under control during long
  // sessions; a fill key only needs to live long enough for any
  // duplicate WebSocket emit to clear.
  const seen = new Set<string>();
  const SEEN_BOUND = 100_000;

  let appliedCount = 0;
  let skippedCount = 0;
  let errorCount = 0;
  let logsPerSec = 0;
  setInterval(() => {
    if (logsPerSec > 0 || appliedCount > 0 || errorCount > 0) {
      console.log(
        `[stats] applied=${appliedCount} skipped=${skippedCount} errors=${errorCount} logs/s=${logsPerSec}`,
      );
      logsPerSec = 0;
    }
  }, 5_000);

  const handleLogs = async (logs: Logs) => {
    logsPerSec++;
    if (logs.err) return;
    for (const line of logs.logs) {
      if (!line.startsWith('Program data: ')) continue;
      const b64 = line.slice('Program data: '.length).trim();
      let event: any;
      try {
        event = eventCoder.decode(b64);
      } catch {
        continue; // not a flash-book event
      }
      if (!event || event.name !== 'BatchFillIntentEvent') continue;
      const data = event.data as BatchFillIntent;
      const key = fillKey(data);
      if (seen.has(key)) {
        skippedCount++;
        continue;
      }
      seen.add(key);
      if (seen.size > SEEN_BOUND) {
        // Trim oldest 25%; fine for MVP since dups window is small.
        const keep = Array.from(seen).slice(SEEN_BOUND / 4);
        seen.clear();
        for (const k of keep) seen.add(k);
      }

      try {
        await dispatch(mainnetClient, sequencer, data);
        appliedCount++;
      } catch (e) {
        errorCount++;
        console.error(`  ✗ dispatch failed for ${key}:`, (e as Error).message);
        // Drop from dedup so a manual retry can re-process.
        seen.delete(key);
      }
    }
  };

  // Subscribe to logs. `mentions` filter limits firehose to flash-book.
  const subId = erConn.onLogs(FLASH_BOOK_PROGRAM_ID, handleLogs, 'confirmed');
  console.log(`  ✓ subscribed (subId=${subId})  →  watching for BatchFillIntentEvent…`);

  process.on('SIGINT', async () => {
    console.log(`\n▶ Shutting down — applied=${appliedCount} errors=${errorCount}`);
    await erConn.removeOnLogsListener(subId);
    process.exit(0);
  });

  // Keep process alive.
  await new Promise(() => {});
}

async function dispatch(
  client: FlashBookClient,
  sequencer: Keypair,
  fill: BatchFillIntent,
): Promise<void> {
  const taker = new PublicKey(fill.taker);
  const market = new PublicKey(fill.market);
  const isFlpFill = new PublicKey(fill.maker).equals(FLP_MARKER);

  let ix;
  if (isFlpFill) {
    ix = await client.applyFlpFillIx({
      sequencer: sequencer.publicKey,
      market,
      takerTrader: taker,
      sizeLots: fill.sizeLots,
      priceTicks: fill.priceTicks,
      takerSide: fill.takerSide === 0 ? 'long' : 'short',
      useFeeTiers: true,
    });
  } else {
    const maker = new PublicKey(fill.maker);
    ix = await client.applyFillIx({
      sequencer: sequencer.publicKey,
      market,
      takerTrader: taker,
      makerTrader: maker,
      sizeLots: fill.sizeLots,
      priceTicks: fill.priceTicks,
      takerSide: fill.takerSide === 0 ? 'long' : 'short',
      // takerWasJit: derived from order flags off-chain in production;
      // MVP defaults to false.
      takerWasJit: false,
      useFeeTiers: true,
    });
  }

  const tx = new Transaction().add(ix);
  tx.feePayer = sequencer.publicKey;
  tx.recentBlockhash = (await client.connection.getLatestBlockhash('confirmed')).blockhash;
  tx.sign(sequencer);
  const sig = await client.connection.sendRawTransaction(tx.serialize(), {
    skipPreflight: false,
    preflightCommitment: 'confirmed',
  });
  await client.connection.confirmTransaction(sig, 'confirmed');
  const fillType = isFlpFill ? 'apply_flp_fill' : 'apply_fill';
  console.log(
    `  ✓ ${fillType}  ${market.toBase58().slice(0, 8)}…  ` +
      `size=${fill.sizeLots} px=${fill.priceTicks} side=${fill.takerSide === 0 ? 'L' : 'S'}  ${sig}`,
  );
}

main().catch((e) => {
  console.error(`\n❌ Sequencer crashed:`, e);
  process.exit(1);
});
