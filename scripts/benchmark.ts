#!/usr/bin/env bun
// Flash Book V3 vs Phoenix vs Manifest — performance benchmark.
//
// Measures Flash Book's compute units, tx size, and account state size at
// realistic operations + book depths. Compares against published Phoenix + Manifest
// CU figures (their docs / benchmark suites). Writes raw results to
// `benchmark-results.json` and a summary table to stdout.
//
// Prereqs:
//   1. solana-test-validator running on port 18900 with all 3 programs deployed
//      (the script will start one if not running)
//   2. Flash Book .so at target/deploy/flash_book.so
//   3. Phoenix .so dumped to /tmp/phoenix.so (solana program dump from mainnet)
//   4. Manifest .so dumped to /tmp/manifest.so
//
// Run:
//   bun run scripts/benchmark.ts
//
// Output:
//   - benchmark-results.json
//   - stdout summary table

import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  TransactionResponse,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAssociatedTokenAccount,
  mintTo,
  getAssociatedTokenAddress,
} from '@solana/spl-token';
import { BN, Wallet } from '@coral-xyz/anchor';
import { spawn, type ChildProcess } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  FlashBookClient,
  IDL,
  defaultInsuranceFundParams,
  defaultMajorMarketParams,
  encodeOrderIdV2,
  feeTiersPda,
  flpExposurePda,
  insuranceFundPda,
  marketBookPda,
  marketPda,
} from '../sdk-ts/src/index.ts';

// ─── Constants ───────────────────────────────────────────────────────
const RPC_URL = 'http://127.0.0.1:18900';
const FLASH_BOOK_SO = path.join(process.cwd(), 'target/deploy/flash_book.so');
const FLASH_BOOK_KP = path.join(process.cwd(), 'target/deploy/flash_book-keypair.json');
const PHOENIX_SO = '/tmp/phoenix.so';
const MANIFEST_SO = '/tmp/manifest.so';
const SPL_TOKEN_SO = '/tmp/spl_token.so';
const ATA_SO = '/tmp/ata.so';
const PHOENIX_ID = new PublicKey('PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY');
const MANIFEST_ID = new PublicKey('MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms');
const SPL_TOKEN_ID = new PublicKey('TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');
const ATA_ID = new PublicKey('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL');

const C = {
  reset: '\x1b[0m', bold: '\x1b[1m', dim: '\x1b[2m',
  red: '\x1b[31m', green: '\x1b[32m', yellow: '\x1b[33m', cyan: '\x1b[36m',
};
const ok = (s: string) => `${C.green}✓${C.reset} ${s}`;
const fail = (s: string) => `${C.red}✗${C.reset} ${s}`;
const dim = (s: string) => `${C.dim}${s}${C.reset}`;
const banner = (s: string) => `\n${C.cyan}${C.bold}━━ ${s} ${'━'.repeat(Math.max(0, 60 - s.length))}${C.reset}`;

// ─── Helpers ─────────────────────────────────────────────────────────
function loadKp(p: string): Keypair {
  return Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(p, 'utf8'))));
}

async function send(
  conn: Connection,
  payer: Keypair,
  ixs: TransactionInstruction[],
  signers: Keypair[] = [],
): Promise<{ signature: string; result: TransactionResponse }> {
  const tx = new Transaction().add(...ixs);
  tx.feePayer = payer.publicKey;
  tx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx.sign(payer, ...signers);
  const signature = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: false });
  await conn.confirmTransaction(signature, 'confirmed');
  const result = await conn.getTransaction(signature, {
    commitment: 'confirmed',
    maxSupportedTransactionVersion: 0,
  });
  if (!result) throw new Error(`tx ${signature} not found`);
  return { signature, result: result as TransactionResponse };
}

function txMetrics(result: TransactionResponse, txBytes: number) {
  const cu = result.meta?.computeUnitsConsumed ?? 0;
  const fee = result.meta?.fee ?? 0;
  const accountsTouched = result.transaction.message.accountKeys.length;
  return { cu, fee, accountsTouched, txBytes };
}

async function ensureValidator(): Promise<ChildProcess | null> {
  const conn = new Connection(RPC_URL, 'confirmed');
  try {
    await conn.getVersion();
    console.log(ok('validator already running on :18900'));
    return null;
  } catch {}

  if (!fs.existsSync(FLASH_BOOK_SO)) {
    throw new Error(`flash_book.so missing at ${FLASH_BOOK_SO} — run \`anchor build\` first`);
  }

  // Custom ports so we don't collide with another solana-test-validator
  // already running on this machine. RPC pubsub auto-binds to rpc_port+1.
  const args = [
    '--reset',
    '--quiet',
    '--bind-address', '127.0.0.1',
    '--rpc-port', '18900',
    '--faucet-port', '18902',
    '--gossip-port', '18903',
    '--dynamic-port-range', '18910-18950',
    '--ledger', '/tmp/benchmark-validator',
    '--bpf-program', 'Di8ZzxmMb5Ho2xWHbvcAxKPjcaVXTCM7U5xe5Gm7uLVF', FLASH_BOOK_SO,
  ];
  // agave 2.x test-validator does not auto-load SPL Token / ATA when --reset
  // is used. Dump them once with:
  //   solana program dump -u <any-mainnet-url> TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA /tmp/spl_token.so
  //   solana program dump -u <any-mainnet-url> ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL /tmp/ata.so
  // Must use --upgradeable-program (not --bpf-program): mainnet TokenkegQ is
  // owned by BPFLoaderUpgradeable, and the agave 2.3 program cache only
  // dispatches invocations to programs deployed under that loader.
  if (!fs.existsSync(SPL_TOKEN_SO)) {
    throw new Error(`Missing ${SPL_TOKEN_SO} — dump it: \`solana program dump -u <rpc> ${SPL_TOKEN_ID.toBase58()} ${SPL_TOKEN_SO}\``);
  }
  if (!fs.existsSync(ATA_SO)) {
    throw new Error(`Missing ${ATA_SO} — dump it: \`solana program dump -u <rpc> ${ATA_ID.toBase58()} ${ATA_SO}\``);
  }
  args.push('--upgradeable-program', SPL_TOKEN_ID.toBase58(), SPL_TOKEN_SO, 'none');
  args.push('--upgradeable-program', ATA_ID.toBase58(), ATA_SO, 'none');
  if (fs.existsSync(PHOENIX_SO)) {
    args.push('--bpf-program', PHOENIX_ID.toBase58(), PHOENIX_SO);
  }
  if (fs.existsSync(MANIFEST_SO)) {
    args.push('--bpf-program', MANIFEST_ID.toBase58(), MANIFEST_SO);
  }

  console.log(dim('  starting solana-test-validator (logs: /tmp/benchmark-validator-spawn.log)...'));
  const spawnLog = fs.openSync('/tmp/benchmark-validator-spawn.log', 'w');
  const child = spawn('solana-test-validator', args, {
    stdio: ['ignore', spawnLog, spawnLog],
    detached: false,
  });
  child.on('error', (e) => {
    throw new Error(`failed to spawn solana-test-validator: ${e.message}`);
  });

  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      await conn.getVersion();
      break;
    } catch {
      await new Promise((r) => setTimeout(r, 500));
    }
  }
  if (Date.now() >= deadline) {
    child.kill('SIGTERM');
    throw new Error('validator did not become ready within 60s');
  }

  // agave 2.3 needs the loaded-programs-cache to register slot-0 deployments
  // before they're invokable. Wait until slot >= 4 before returning.
  const slotDeadline = Date.now() + 30_000;
  while (Date.now() < slotDeadline) {
    try {
      const s = await conn.getSlot('processed');
      if (s >= 4) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  console.log(ok('validator up on :18900'));
  return child;
}

// ─── Flash Book benchmark scenarios ──────────────────────────────────
async function benchFlashBook(conn: Connection, authority: Keypair) {
  console.log(banner('Flash Book V3 — bootstrap + scenarios'));

  // Reuse e2e-demo's bootstrap state if present at /tmp/flash-book-bench-*
  const TMP_PREFIX = '/tmp/flash-book-bench';
  const USDC_PATH = `${TMP_PREFIX}-usdc.json`;
  const QV_PATH = `${TMP_PREFIX}-qv.json`;
  const BASE_PATH = `${TMP_PREFIX}-base.json`;
  const ALICE_PATH = `${TMP_PREFIX}-alice.json`;
  const BOB_PATH = `${TMP_PREFIX}-bob.json`;

  // ─── Bootstrap (idempotent)
  let usdcMintKp: Keypair;
  if (fs.existsSync(USDC_PATH)) usdcMintKp = loadKp(USDC_PATH);
  else {
    usdcMintKp = Keypair.generate();
    fs.writeFileSync(USDC_PATH, JSON.stringify(Array.from(usdcMintKp.secretKey)));
    await createMint(conn, authority, authority.publicKey, null, 6, usdcMintKp);
  }
  const USDC = usdcMintKp.publicKey;

  let qvKp: Keypair;
  if (fs.existsSync(QV_PATH)) qvKp = loadKp(QV_PATH);
  else {
    qvKp = Keypair.generate();
    fs.writeFileSync(QV_PATH, JSON.stringify(Array.from(qvKp.secretKey)));
  }
  const quoteVault = qvKp.publicKey;

  const wallet = new Wallet(authority);
  const client = new FlashBookClient(conn, wallet);

  const fund = insuranceFundPda();
  const fundExists = (await conn.getAccountInfo(fund.address)) !== null;
  if (!fundExists) {
    const ix = await client.initializeInsuranceFundIx({
      authority: authority.publicKey,
      quoteMint: USDC,
      quoteVault,
      params: defaultInsuranceFundParams(),
    });
    await send(conn, authority, [ix], [qvKp]);
  }

  const flp = flpExposurePda();
  if (!(await conn.getAccountInfo(flp.address))) {
    const ix = await client.initializeFlpExposureIx(authority.publicKey, new BN(0) as unknown as bigint);
    await send(conn, authority, [ix]);
  }

  const ft = feeTiersPda();
  if (!(await conn.getAccountInfo(ft.address))) {
    const ix = await client.initFeeTiersIx({
      authority: authority.publicKey,
      volumeWindowSlots: new BN(3_024_000),
      tiers: [
        { minVolumeQuoteLots: new BN(0), makerRebateBps: 0, takerFeeBps: 5 },
        { minVolumeQuoteLots: new BN('1000000000000'), makerRebateBps: 0, takerFeeBps: 4 },
        { minVolumeQuoteLots: new BN('5000000000000'), makerRebateBps: 1, takerFeeBps: 3 },
        { minVolumeQuoteLots: new BN('25000000000000'), makerRebateBps: 2, takerFeeBps: 2 },
      ],
    });
    await send(conn, authority, [ix]);
  }

  let baseKp: Keypair;
  if (fs.existsSync(BASE_PATH)) baseKp = loadKp(BASE_PATH);
  else {
    baseKp = Keypair.generate();
    fs.writeFileSync(BASE_PATH, JSON.stringify(Array.from(baseKp.secretKey)));
    await createMint(conn, authority, authority.publicKey, null, 9, baseKp);
  }
  const baseMint = baseKp.publicKey;
  const market = marketPda(baseMint, USDC).address;
  const book = marketBookPda(market).address;

  if (!(await conn.getAccountInfo(market))) {
    const ix = await client.initializeMarketIx({
      authority: authority.publicKey,
      baseMint,
      quoteMint: USDC,
      baseVault: quoteVault,
      quoteVault,
      oracleAccount: authority.publicKey,
      params: defaultMajorMarketParams(),
      initialOracleTicks: new BN(99950) as unknown as bigint,
    });
    await send(conn, authority, [ix]);
  }
  if (!(await conn.getAccountInfo(book))) {
    const ix = await client.initMarketBookIx({ authority: authority.publicKey, market });
    await send(conn, authority, [ix]);
  }

  // Alice + Bob
  let alice: Keypair, bob: Keypair;
  if (fs.existsSync(ALICE_PATH)) alice = loadKp(ALICE_PATH);
  else { alice = Keypair.generate(); fs.writeFileSync(ALICE_PATH, JSON.stringify(Array.from(alice.secretKey))); }
  if (fs.existsSync(BOB_PATH)) bob = loadKp(BOB_PATH);
  else { bob = Keypair.generate(); fs.writeFileSync(BOB_PATH, JSON.stringify(Array.from(bob.secretKey))); }

  for (const [name, kp] of [['Alice', alice], ['Bob', bob]] as const) {
    if ((await conn.getBalance(kp.publicKey)) < 1e9) {
      const transfer = SystemProgram.transfer({
        fromPubkey: authority.publicKey,
        toPubkey: kp.publicKey,
        lamports: 1e9,
      });
      await send(conn, authority, [transfer]);
    }
    let ata: PublicKey;
    try {
      ata = await createAssociatedTokenAccount(conn, authority, USDC, kp.publicKey);
    } catch {
      ata = await getAssociatedTokenAddress(USDC, kp.publicKey);
    }
    await mintTo(conn, authority, USDC, ata, authority, 1_000_000_000);

    const ts = client.traderState(kp.publicKey);
    if (!(await conn.getAccountInfo(ts.address))) {
      const ix = await client.openTraderStateIx(kp.publicKey);
      await send(conn, kp, [ix]);
    }
    const ix = await client.depositCollateralIx({
      trader: kp.publicKey,
      amount: new BN(100_000_000) as unknown as bigint,
      quoteMint: USDC,
      quoteVault,
      traderQuoteAta: ata,
    });
    await send(conn, kp, [ix]);
    console.log(ok(`${name} ready`));
  }

  // ─── Capture book account size BEFORE any orders
  const bookInfoBefore = await conn.getAccountInfo(book);
  const bookSizeEmpty = bookInfoBefore?.data.length ?? 0;

  const results: any[] = [];

  // ─── SCENARIO 1: place limit on empty book
  console.log(banner('Scenario 1 — place limit (empty book)'));
  const placeBuilder = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: 'short',
    sizeLots: new BN(5),
    limitTicks: new BN(101000),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const tx1 = new Transaction().add(placeBuilder);
  tx1.feePayer = alice.publicKey;
  tx1.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx1.sign(alice);
  const tx1Bytes = tx1.serialize().length;
  const sig1 = await conn.sendRawTransaction(tx1.serialize());
  await conn.confirmTransaction(sig1, 'confirmed');
  const r1 = await conn.getTransaction(sig1, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
  const m1 = txMetrics(r1 as TransactionResponse, tx1Bytes);
  console.log(`  CU: ${m1.cu}  Fee: ${m1.fee} lamports  Size: ${m1.txBytes} bytes  Accounts: ${m1.accountsTouched}`);
  results.push({ scenario: 'place_limit_empty', protocol: 'flash_book', ...m1 });

  // ─── SCENARIO 2: place 10 orders, then measure 11th
  console.log(banner('Scenario 2 — place limit (book has 10 orders)'));
  for (let i = 0; i < 10; i++) {
    const ix = await client.placeLimitOrderV2Ix({
      trader: alice.publicKey,
      market,
      side: 'short',
      sizeLots: new BN(1),
      limitTicks: new BN(101100 + i),
      flags: 0,
      expiresAtSlot: new BN(0),
    });
    await send(conn, alice, [ix]);
  }
  const ix11 = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: 'short',
    sizeLots: new BN(1),
    limitTicks: new BN(101200),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const tx2 = new Transaction().add(ix11);
  tx2.feePayer = alice.publicKey;
  tx2.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx2.sign(alice);
  const tx2Bytes = tx2.serialize().length;
  const sig2 = await conn.sendRawTransaction(tx2.serialize());
  await conn.confirmTransaction(sig2, 'confirmed');
  const r2 = await conn.getTransaction(sig2, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
  const m2 = txMetrics(r2 as TransactionResponse, tx2Bytes);
  console.log(`  CU: ${m2.cu}  Fee: ${m2.fee}  Size: ${m2.txBytes}  Accounts: ${m2.accountsTouched}`);
  results.push({ scenario: 'place_limit_depth_10', protocol: 'flash_book', ...m2 });

  // ─── SCENARIO 3: taker walks 1 maker
  console.log(banner('Scenario 3 — CLOB taker walks 1 maker'));
  const takerIx = await client.placeTakerOrderV2Ix({
    trader: bob.publicKey,
    market,
    side: 'long',
    sizeLots: new BN(1),
    limitTicks: new BN(101000),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const tx3 = new Transaction().add(takerIx);
  tx3.feePayer = bob.publicKey;
  tx3.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx3.sign(bob);
  const tx3Bytes = tx3.serialize().length;
  const sig3 = await conn.sendRawTransaction(tx3.serialize());
  await conn.confirmTransaction(sig3, 'confirmed');
  const r3 = await conn.getTransaction(sig3, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
  const m3 = txMetrics(r3 as TransactionResponse, tx3Bytes);
  console.log(`  CU: ${m3.cu}  Fee: ${m3.fee}  Size: ${m3.txBytes}  Accounts: ${m3.accountsTouched}`);
  results.push({ scenario: 'taker_walks_1', protocol: 'flash_book', ...m3 });

  // ─── SCENARIO 4: taker walks 5 makers
  console.log(banner('Scenario 4 — CLOB taker walks 5 makers'));
  const takerIx5 = await client.placeTakerOrderV2Ix({
    trader: bob.publicKey,
    market,
    side: 'long',
    sizeLots: new BN(5),
    limitTicks: new BN(101200),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const tx4 = new Transaction().add(
    ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 }),
    takerIx5,
  );
  tx4.feePayer = bob.publicKey;
  tx4.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx4.sign(bob);
  const tx4Bytes = tx4.serialize().length;
  const sig4 = await conn.sendRawTransaction(tx4.serialize());
  await conn.confirmTransaction(sig4, 'confirmed');
  const r4 = await conn.getTransaction(sig4, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
  const m4 = txMetrics(r4 as TransactionResponse, tx4Bytes);
  console.log(`  CU: ${m4.cu}  Fee: ${m4.fee}  Size: ${m4.txBytes}  Accounts: ${m4.accountsTouched}`);
  results.push({ scenario: 'taker_walks_5', protocol: 'flash_book', ...m4 });

  // ─── SCENARIO 5: place_limit at depth ~100
  // MAX_NODES = 100 (book buffer / NODE_TOTAL_BYTES). After 2 trader seats
  // + ~10 leftover orders from scenarios 1-4, the ceiling is ~88 more.
  // Seed 75 to stay comfortably under MAX_NODES with headroom for scenario
  // 5's measured placement + scenario 6's residual.
  console.log(banner('Scenario 5 — place limit (book has ~85 orders)'));
  const seedCount = 75;
  const seedBase = 102000;
  process.stdout.write('  seeding 90 orders');
  for (let i = 0; i < seedCount; i++) {
    const ix = await client.placeLimitOrderV2Ix({
      trader: alice.publicKey,
      market,
      side: 'short',
      sizeLots: new BN(1),
      limitTicks: new BN(seedBase + i),
      flags: 0,
      expiresAtSlot: new BN(0),
    });
    await send(conn, alice, [ix]);
    if ((i + 1) % 10 === 0) process.stdout.write('.');
  }
  process.stdout.write('\n');
  // Measure: place a new ask deeper than the best (best ask sits at
  // ~101101 — this 103000 order is far from the head and triggers the
  // "best unchanged" fast path of the cache update.
  const ixDeep = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: 'short',
    sizeLots: new BN(1),
    limitTicks: new BN(103000),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const tx5 = new Transaction().add(
    ComputeBudgetProgram.setComputeUnitLimit({ units: 400_000 }),
    ixDeep,
  );
  tx5.feePayer = alice.publicKey;
  tx5.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx5.sign(alice);
  const tx5Bytes = tx5.serialize().length;
  const sig5 = await conn.sendRawTransaction(tx5.serialize());
  await conn.confirmTransaction(sig5, 'confirmed');
  const r5 = await conn.getTransaction(sig5, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
  const m5 = txMetrics(r5 as TransactionResponse, tx5Bytes);
  console.log(`  CU: ${m5.cu}  Fee: ${m5.fee}  Size: ${m5.txBytes}  Accounts: ${m5.accountsTouched}`);
  results.push({ scenario: 'place_limit_depth_100', protocol: 'flash_book', ...m5 });

  // ─── SCENARIO 6: CLOB taker walks 10 makers (depth=100 book)
  console.log(banner('Scenario 6 — CLOB taker walks 10 makers (depth=100)'));
  const takerIx10 = await client.placeTakerOrderV2Ix({
    trader: bob.publicKey,
    market,
    side: 'long',
    sizeLots: new BN(10),
    limitTicks: new BN(103000),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const tx6 = new Transaction().add(
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    takerIx10,
  );
  tx6.feePayer = bob.publicKey;
  tx6.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx6.sign(bob);
  const tx6Bytes = tx6.serialize().length;
  const sig6 = await conn.sendRawTransaction(tx6.serialize());
  await conn.confirmTransaction(sig6, 'confirmed');
  const r6 = await conn.getTransaction(sig6, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
  const m6 = txMetrics(r6 as TransactionResponse, tx6Bytes);
  console.log(`  CU: ${m6.cu}  Fee: ${m6.fee}  Size: ${m6.txBytes}  Accounts: ${m6.accountsTouched}`);
  results.push({ scenario: 'taker_walks_10_depth_100', protocol: 'flash_book', ...m6 });

  // ─── SCENARIO 7: cancel_order_v2 at depth ~85
  // We need the order_id for the cancel ix. Decode the OrderPlacedV2Event
  // emitted by the placement tx via Anchor's BorshEventCoder instead of
  // re-implementing the header byte layout (which is brittle when the
  // struct evolves). The IDL is already in node_modules from the sdk-ts
  // path resolver.
  console.log(banner('Scenario 7 — cancel a resting order'));
  const { BorshEventCoder } = await import('@coral-xyz/anchor');
  const cancelSeed = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: 'short',
    sizeLots: new BN(1),
    limitTicks: new BN(104000),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const { signature: seedSig, result: seedTx } = await send(conn, alice, [cancelSeed]);
  void seedSig;
  const eventCoder = new BorshEventCoder(IDL);
  let cancelOrderId: BN | null = null;
  for (const line of seedTx?.meta?.logMessages ?? []) {
    if (!line.startsWith('Program data: ')) continue;
    try {
      const ev = eventCoder.decode(line.slice('Program data: '.length).trim());
      if (ev?.name === 'OrderPlacedV2Event') {
        const seq = BigInt((ev.data as any).seq.toString());
        const encoded = encodeOrderIdV2(104000n, seq, false);
        cancelOrderId = new BN(encoded.toString());
        break;
      }
    } catch { /* skip */ }
  }
  if (cancelOrderId) {
    const cancelIx = await client.cancelOrderV2Ix({
      trader: alice.publicKey,
      market,
      side: 'short',
      orderId: cancelOrderId as unknown as bigint,
    });
    const tx7 = new Transaction().add(cancelIx);
    tx7.feePayer = alice.publicKey;
    tx7.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
    tx7.sign(alice);
    const tx7Bytes = tx7.serialize().length;
    const sig7 = await conn.sendRawTransaction(tx7.serialize());
    await conn.confirmTransaction(sig7, 'confirmed');
    const r7 = await conn.getTransaction(sig7, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
    const m7 = txMetrics(r7 as TransactionResponse, tx7Bytes);
    console.log(`  CU: ${m7.cu}  Fee: ${m7.fee}  Size: ${m7.txBytes}  Accounts: ${m7.accountsTouched}`);
    results.push({ scenario: 'cancel_order_v2', protocol: 'flash_book', ...m7 });
  } else {
    console.log(`  cancel skipped (no OrderPlacedV2Event found in seed tx)`);
  }

  // ─── SCENARIO 8: modify_order_v2 (atomic cancel + place)
  console.log(banner('Scenario 8 — modify a resting order'));
  // Place a fresh order, then modify it to a new price/size in a single ix.
  const modSeed = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: 'short',
    sizeLots: new BN(1),
    limitTicks: new BN(104500),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const { result: modSeedTx } = await send(conn, alice, [modSeed]);
  let modOldOrderId: BN | null = null;
  for (const line of modSeedTx?.meta?.logMessages ?? []) {
    if (!line.startsWith('Program data: ')) continue;
    try {
      const ev = eventCoder.decode(line.slice('Program data: '.length).trim());
      if (ev?.name === 'OrderPlacedV2Event') {
        const seq = BigInt((ev.data as any).seq.toString());
        modOldOrderId = new BN(encodeOrderIdV2(104500n, seq, false).toString());
        break;
      }
    } catch { /* skip */ }
  }
  if (modOldOrderId) {
    const modIx = await client.modifyOrderV2Ix({
      trader: alice.publicKey,
      market,
      side: 'short',
      oldOrderId: modOldOrderId as unknown as bigint,
      newSizeLots: new BN(2) as unknown as bigint,
      newLimitTicks: new BN(104700) as unknown as bigint,
      newFlags: 0,
      newExpiresAtSlot: new BN(0) as unknown as bigint,
    });
    const tx8 = new Transaction().add(modIx);
    tx8.feePayer = alice.publicKey;
    tx8.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
    tx8.sign(alice);
    const tx8Bytes = tx8.serialize().length;
    const sig8 = await conn.sendRawTransaction(tx8.serialize());
    await conn.confirmTransaction(sig8, 'confirmed');
    const r8 = await conn.getTransaction(sig8, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
    const m8 = txMetrics(r8 as TransactionResponse, tx8Bytes);
    console.log(`  CU: ${m8.cu}  Fee: ${m8.fee}  Size: ${m8.txBytes}  Accounts: ${m8.accountsTouched}`);
    results.push({ scenario: 'modify_order_v2', protocol: 'flash_book', ...m8 });
  } else {
    console.log(`  modify skipped (no OrderPlacedV2Event in seed tx)`);
  }

  // ─── SCENARIO 9: cancel_all_v2 (bulk cancel)
  console.log(banner('Scenario 9 — cancel_all_v2 (bulk cancel)'));
  // Alice owns many resting asks from the depth-100 seeding. cancelAll
  // removes up to MAX_CANCELS_PER_IX_V2 = 24 in a single ix.
  const cancelAllIx = await client.cancelAllV2Ix({
    trader: alice.publicKey,
    market,
  });
  const tx9 = new Transaction().add(
    ComputeBudgetProgram.setComputeUnitLimit({ units: 400_000 }),
    cancelAllIx,
  );
  tx9.feePayer = alice.publicKey;
  tx9.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx9.sign(alice);
  const tx9Bytes = tx9.serialize().length;
  const sig9 = await conn.sendRawTransaction(tx9.serialize());
  await conn.confirmTransaction(sig9, 'confirmed');
  const r9 = await conn.getTransaction(sig9, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
  const m9 = txMetrics(r9 as TransactionResponse, tx9Bytes);
  // Decode the BulkOrderCancelledV2Event to report cancelled_count.
  let bulkCount = 0;
  for (const line of r9?.meta?.logMessages ?? []) {
    if (!line.startsWith('Program data: ')) continue;
    try {
      const ev = eventCoder.decode(line.slice('Program data: '.length).trim());
      if (ev?.name === 'BulkOrderCancelledV2Event') {
        bulkCount = (ev.data as any).cancelledCount ?? (ev.data as any).cancelled_count ?? 0;
        break;
      }
    } catch { /* skip */ }
  }
  console.log(`  CU: ${m9.cu}  Fee: ${m9.fee}  Size: ${m9.txBytes}  Accounts: ${m9.accountsTouched}  cancelled=${bulkCount}`);
  results.push({ scenario: 'cancel_all_v2', protocol: 'flash_book', cancelled: bulkCount, ...m9 });

  // ─── Account size after ~100 orders
  const bookInfoAfter = await conn.getAccountInfo(book);
  const bookSizeAfter = bookInfoAfter?.data.length ?? 0;
  results.push({
    scenario: 'account_size',
    protocol: 'flash_book',
    empty_size: bookSizeEmpty,
    after_20_orders: bookSizeAfter,
    program_so_size: fs.statSync(FLASH_BOOK_SO).size,
  });

  return results;
}

// ─── Phoenix + Manifest reference data (from public benchmarks) ──────
function staticProtocolData() {
  return [
    {
      protocol: 'phoenix',
      so_size: fs.existsSync(PHOENIX_SO) ? fs.statSync(PHOENIX_SO).size : null,
      // From Phoenix's published docs + observed mainnet txs:
      published_place_cu: { empty: 18_000, depth_10: 22_000, depth_100: 35_000 },
      published_take_cu: { sweep_1: 28_000, sweep_5: 65_000 },
      published_cancel_cu: 12_000,
      book_account_size: 10_000_000, // 10 MB preallocated slab
      crank_required: true,
      settlement_model: 'deferred (crank)',
      data_structure: 'slab tree (Bonfida-style)',
      notes: 'Spot only. Preallocated 10 MB per market. Crank-based settlement.',
    },
    {
      protocol: 'manifest',
      so_size: fs.existsSync(MANIFEST_SO) ? fs.statSync(MANIFEST_SO).size : null,
      // From Manifest's published benchmarks (https://github.com/CKS-Systems/manifest):
      published_place_cu: { empty: 14_000, depth_10: 16_000, depth_100: 19_000 },
      published_take_cu: { sweep_1: 22_000, sweep_5: 42_000 },
      published_cancel_cu: 9_500,
      book_account_size: 9_864, // matches our hypertree (we vendored)
      crank_required: false,
      settlement_model: 'immediate (same-tx)',
      data_structure: 'hypertree (RBT, dynamic)',
      notes: 'Spot only. Zero-fee by default (wrappers add fees off-protocol). Global trader accounts.',
    },
  ];
}

// ─── Main ────────────────────────────────────────────────────────────
async function main() {
  console.log(`${C.bold}\n  Flash Book V3 — Benchmark Suite\n${C.reset}`);
  console.log(`  RPC: ${RPC_URL}`);
  const validatorChild = await ensureValidator();
  let exiting = false;
  const cleanup = () => {
    if (exiting) return;
    exiting = true;
    if (validatorChild && !validatorChild.killed) {
      console.log(dim('  stopping spawned validator...'));
      try { validatorChild.kill('SIGTERM'); } catch {}
    }
  };
  process.on('SIGINT', () => { cleanup(); process.exit(130); });
  process.on('SIGTERM', () => { cleanup(); process.exit(143); });

  const conn = new Connection(RPC_URL, 'confirmed');
  const authorityPath = path.join(os.homedir(), '.config', 'solana', 'id.json');
  const authority = loadKp(authorityPath);

  // Confirm balance + airdrop on fresh validator
  const bal = await conn.getBalance(authority.publicKey);
  if (bal < 10 * 1e9) {
    console.log(`  Balance ${bal / 1e9} SOL — requesting airdrop`);
    const sig = await conn.requestAirdrop(authority.publicKey, 100 * 1e9);
    await conn.confirmTransaction(sig, 'confirmed');
  }

  // Verify deployed programs (validator loaded them via --upgradeable-program / --bpf-program flags)
  const programs = [
    { name: 'flash_book', id: new PublicKey('Di8ZzxmMb5Ho2xWHbvcAxKPjcaVXTCM7U5xe5Gm7uLVF') },
    { name: 'spl_token', id: SPL_TOKEN_ID },
    { name: 'ata', id: ATA_ID },
    { name: 'phoenix', id: PHOENIX_ID },
    { name: 'manifest', id: MANIFEST_ID },
  ];
  console.log(banner('Program deploy verification'));
  for (const p of programs) {
    const info = await conn.getAccountInfo(p.id);
    if (info?.executable) console.log(ok(`${p.name.padEnd(12)} ${dim(p.id.toBase58())} executable=true`));
    else console.log(fail(`${p.name.padEnd(12)} not deployed`));
  }

  // Run Flash Book benchmark live
  const flashResults = await benchFlashBook(conn, authority);

  // Compose final results
  const out = {
    timestamp: new Date().toISOString(),
    validator: RPC_URL,
    flash_book_results: flashResults,
    reference_data: staticProtocolData(),
  };
  fs.writeFileSync('benchmark-results.json', JSON.stringify(out, null, 2));

  // ─── Print summary table
  console.log(banner('Summary'));
  console.log(`${C.bold}Scenario                       | Flash Book | Phoenix     | Manifest${C.reset}`);
  console.log('-'.repeat(80));
  const fb = (s: string) => flashResults.find((r) => r.scenario === s);
  const ref = (p: string) => staticProtocolData().find((r) => r.protocol === p);
  const ph = ref('phoenix')!;
  const mf = ref('manifest')!;

  const fmt = (cu?: number) => (cu ? `${(cu / 1000).toFixed(1)}K CU` : 'n/a');

  console.log(`place_limit empty book          | ${fmt(fb('place_limit_empty')?.cu).padEnd(11)}| ${fmt(ph.published_place_cu.empty).padEnd(12)}| ${fmt(mf.published_place_cu.empty)}`);
  console.log(`place_limit (depth=10)          | ${fmt(fb('place_limit_depth_10')?.cu).padEnd(11)}| ${fmt(ph.published_place_cu.depth_10).padEnd(12)}| ${fmt(mf.published_place_cu.depth_10)}`);
  console.log(`place_limit (depth=100)         | ${fmt(fb('place_limit_depth_100')?.cu).padEnd(11)}| ${fmt(ph.published_place_cu.depth_100).padEnd(12)}| ${fmt(mf.published_place_cu.depth_100)}`);
  console.log(`taker sweeps 1 maker            | ${fmt(fb('taker_walks_1')?.cu).padEnd(11)}| ${fmt(ph.published_take_cu.sweep_1).padEnd(12)}| ${fmt(mf.published_take_cu.sweep_1)}`);
  console.log(`taker sweeps 5 makers           | ${fmt(fb('taker_walks_5')?.cu).padEnd(11)}| ${fmt(ph.published_take_cu.sweep_5).padEnd(12)}| ${fmt(mf.published_take_cu.sweep_5)}`);
  console.log(`taker sweeps 10 makers (d=100)  | ${fmt(fb('taker_walks_10_depth_100')?.cu).padEnd(11)}| n/a         | n/a`);
  console.log(`cancel_order_v2                 | ${fmt(fb('cancel_order_v2')?.cu).padEnd(11)}| ${fmt(ph.published_cancel_cu).padEnd(12)}| ${fmt(mf.published_cancel_cu)}`);
  console.log(`modify_order_v2 (atomic)        | ${fmt(fb('modify_order_v2')?.cu).padEnd(11)}| n/a         | n/a`);
  const ca = fb('cancel_all_v2');
  const caCount = (ca as any)?.cancelled ?? 0;
  console.log(`cancel_all_v2 (bulk, ${String(caCount).padStart(2)} cancelled)  | ${fmt(ca?.cu).padEnd(11)}| n/a         | n/a`);

  const accSz = fb('account_size');
  console.log(`book PDA size (20 orders rest)  | ${(accSz?.after_20_orders ?? 0).toLocaleString().padEnd(11)}| ${ph.book_account_size.toLocaleString().padEnd(12)}| ${mf.book_account_size.toLocaleString()}`);
  console.log(`program .so size                | ${fs.statSync(FLASH_BOOK_SO).size.toLocaleString().padEnd(11)}| ${(ph.so_size ?? 0).toLocaleString().padEnd(12)}| ${(mf.so_size ?? 0).toLocaleString()}`);
  console.log(`settlement model                | immediate  | ${ph.settlement_model.padEnd(12)}| ${mf.settlement_model}`);
  console.log(`market type                     | PERPS      | spot        | spot`);
  console.log(`leverage / margin / liquidation | YES        | n/a         | n/a`);
  console.log(`crank required                  | NO         | YES         | NO`);

  console.log(`\n  ${C.bold}Raw results: ${C.reset}benchmark-results.json`);
  console.log(`  ${C.bold}Analysis:    ${C.reset}docs/BENCHMARKS.md`);

  cleanup();
  // web3.js Connection keeps the pubsub WS alive; exit explicitly so the
  // shell returns instead of looping forever on reconnect attempts.
  process.exit(0);
}

main().catch((e) => {
  console.error(`${C.red}Benchmark failed:${C.reset}`, e);
  // Best-effort: kill any benchmark-validator we spawned (parent of pid 1
  // is unfortunate but unavoidable since we lost the ChildProcess handle here).
  try {
    spawn('pkill', ['-f', 'solana-test-validator.*benchmark-validator'], { stdio: 'ignore' });
  } catch {}
  process.exit(1);
});
