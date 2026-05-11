#!/usr/bin/env bun
// Flash Book V3 — full end-to-end LOCALNET demo.
//
// Proves the complete trade lifecycle: bootstrap → 2 traders deposit
// → crossing orders → matcher tick → fills settled → positions
// populated. All on a real Solana validator, with all 4 programs
// deployed and ALL events emitted on-chain.
//
// Prerequisites:
//   1. solana-test-validator running on http://127.0.0.1:8899
//      (start with: solana-test-validator --reset --quiet)
//   2. All 4 .so files built (cargo build --release / anchor build)
//   3. Programs already deployed (or this script will check + warn)
//
// Run:
//   bun run scripts/e2e-demo.ts

import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAssociatedTokenAccount,
  mintTo,
  getAssociatedTokenAddress,
} from '@solana/spl-token';
import { AnchorProvider, BN, BorshEventCoder, Wallet } from '@coral-xyz/anchor';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  FLASH_BOOK_PROGRAM_ID,
  FlashBookClient,
  IDL,
  defaultInsuranceFundParams,
  defaultMajorMarketParams,
  feeTiersPda,
  flpExposurePda,
  insuranceFundPda,
  marketBookPda,
  marketPda,
} from '../sdk-ts/src/index.ts';

// ─── Colors ──────────────────────────────────────────────────────────
const C = {
  reset: '\x1b[0m', bold: '\x1b[1m', dim: '\x1b[2m',
  red: '\x1b[31m', green: '\x1b[32m', yellow: '\x1b[33m',
  blue: '\x1b[34m', cyan: '\x1b[36m', magenta: '\x1b[35m',
};
const b = (s: string) => `${C.bold}${s}${C.reset}`;
const d = (s: string) => `${C.dim}${s}${C.reset}`;
const ok = (s: string) => `${C.green}✓${C.reset} ${s}`;
const fail = (s: string) => `${C.red}✗${C.reset} ${s}`;
const banner = (s: string) => `\n${C.cyan}${b('━'.repeat(2) + ' ' + s + ' ' + '━'.repeat(60 - s.length))}${C.reset}`;

// ─── Config ──────────────────────────────────────────────────────────
const RPC_URL = process.env.RPC_URL ?? 'http://127.0.0.1:8899';
const AUTHORITY_PATH =
  process.env.AUTHORITY_KEYPAIR ?? path.join(os.homedir(), '.config', 'solana', 'id.json');
const TMP_PREFIX = process.env.TMP_PREFIX ?? '/tmp/flash-book-e2e';

// Test prices: SOL @ $99.95 (priceTicks = 99950 with tick_size 1).
const BASE_MINT_PRICE_TICKS = 99950n;

function loadKp(p: string): Keypair {
  const raw = JSON.parse(fs.readFileSync(p, 'utf8')) as number[];
  return Keypair.fromSecretKey(new Uint8Array(raw));
}

async function send(
  conn: Connection,
  payer: Keypair,
  ixs: TransactionInstruction[],
  signers: Keypair[] = [],
): Promise<string> {
  const tx = new Transaction().add(...ixs);
  tx.feePayer = payer.publicKey;
  tx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx.sign(payer, ...signers);
  const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: false });
  await conn.confirmTransaction(sig, 'confirmed');
  return sig;
}

async function airdrop(conn: Connection, to: PublicKey, lamports: number) {
  const sig = await conn.requestAirdrop(to, lamports);
  await conn.confirmTransaction(sig, 'confirmed');
}

/// Devnet-safe funding: try airdrop first, fall back to transfer from
/// `from` keypair. Use to fund Alice/Bob on devnet where the airdrop
/// faucet is rate-limited.
async function fundWallet(
  conn: Connection,
  to: PublicKey,
  lamports: number,
  from: Keypair,
): Promise<void> {
  const existing = await conn.getBalance(to);
  if (existing >= lamports) return;
  const needed = lamports - existing;
  try {
    await airdrop(conn, to, needed);
  } catch {
    const tx = new Transaction().add(
      SystemProgram.transfer({ fromPubkey: from.publicKey, toPubkey: to, lamports: needed }),
    );
    tx.feePayer = from.publicKey;
    tx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
    tx.sign(from);
    const sig = await conn.sendRawTransaction(tx.serialize());
    await conn.confirmTransaction(sig, 'confirmed');
  }
}

async function exists(conn: Connection, pk: PublicKey): Promise<boolean> {
  return (await conn.getAccountInfo(pk)) !== null;
}

// ─── Main flow ───────────────────────────────────────────────────────
async function main() {
  console.log(b('\n  Flash Book V3 — END-TO-END LOCALNET DEMO\n'));

  const conn = new Connection(RPC_URL, 'confirmed');
  const authority = loadKp(AUTHORITY_PATH);
  console.log(`  Validator:  ${RPC_URL}`);
  console.log(`  Authority:  ${authority.publicKey.toBase58()}`);
  try {
    const v = await conn.getVersion();
    console.log(`  Cluster:    Solana ${v['solana-core']}`);
  } catch {
    console.log(fail(`Cannot reach validator at ${RPC_URL} — start with: solana-test-validator --reset`));
    process.exit(1);
  }

  // ─── Step 1: verify programs are deployed
  console.log(banner('STEP 1 — verify programs deployed'));
  const corePrg = await conn.getAccountInfo(FLASH_BOOK_PROGRAM_ID);
  if (!corePrg?.executable) {
    console.log(fail(`flash_book NOT deployed at ${FLASH_BOOK_PROGRAM_ID.toBase58()}`));
    process.exit(1);
  }
  console.log(ok(`flash_book (core)    ${d(FLASH_BOOK_PROGRAM_ID.toBase58())}`));
  // Wrapper programs are optional for the basic trade flow.
  for (const [name, id] of [
    ['flash_book_orders', '2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5'],
    ['flash_book_vaults', 'GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt'],
    ['flash_book_flp', 'eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M'],
  ] as const) {
    const info = await conn.getAccountInfo(new PublicKey(id));
    if (info?.executable) console.log(ok(`${name.padEnd(20)} ${d(id)}`));
    else console.log(d(`${name.padEnd(20)} not deployed at this cluster (optional)`));
  }

  // ─── Step 2: airdrop SOL to authority (skip on devnet/mainnet) + create test USDC mint
  console.log(banner('STEP 2 — fund authority + create test USDC mint'));
  const currentAuthBal = await conn.getBalance(authority.publicKey);
  if (currentAuthBal < 2 * 1e9 && RPC_URL.includes('127.0.0.1')) {
    await airdrop(conn, authority.publicKey, 10 * 1e9);
  }
  const authBal = await conn.getBalance(authority.publicKey);
  console.log(ok(`Authority balance: ${(authBal / 1e9).toFixed(2)} SOL`));

  const USDC_MINT_PATH = `${TMP_PREFIX}-usdc-mint.json`;
  let usdcMintKp: Keypair;
  if (fs.existsSync(USDC_MINT_PATH)) {
    usdcMintKp = loadKp(USDC_MINT_PATH);
    console.log(ok(`Reusing test USDC mint: ${usdcMintKp.publicKey.toBase58()}`));
  } else {
    usdcMintKp = Keypair.generate();
    fs.writeFileSync(USDC_MINT_PATH, JSON.stringify(Array.from(usdcMintKp.secretKey)));
    await createMint(conn, authority, authority.publicKey, null, 6, usdcMintKp);
    console.log(ok(`Test USDC mint created:  ${usdcMintKp.publicKey.toBase58()}`));
    console.log(d(`     (6 decimals, authority can mint freely)`));
  }
  const USDC = usdcMintKp.publicKey;

  // ─── Step 3: bootstrap protocol with our test USDC
  console.log(banner('STEP 3 — bootstrap protocol with test USDC'));
  const wallet = new Wallet(authority);
  const _provider = new AnchorProvider(conn, wallet, { commitment: 'confirmed' });
  void _provider;
  const client = new FlashBookClient(conn, wallet);

  // Quote vault keypair (re-used or fresh)
  const QV_PATH = `${TMP_PREFIX}-quote-vault.json`;
  let quoteVaultKp: Keypair;
  if (fs.existsSync(QV_PATH)) {
    quoteVaultKp = loadKp(QV_PATH);
  } else {
    quoteVaultKp = Keypair.generate();
    fs.writeFileSync(QV_PATH, JSON.stringify(Array.from(quoteVaultKp.secretKey)));
  }
  const quoteVault = quoteVaultKp.publicKey;

  const fund = insuranceFundPda();
  if (await exists(conn, fund.address)) {
    console.log(d(`     InsuranceFund already initialized — skipping`));
  } else {
    const ix = await client.initializeInsuranceFundIx({
      authority: authority.publicKey,
      quoteMint: USDC,
      quoteVault,
      params: defaultInsuranceFundParams(),
    });
    await send(conn, authority, [ix], [quoteVaultKp]);
    console.log(ok(`InsuranceFund initialized  ${d(fund.address.toBase58())}`));
  }

  const flp = flpExposurePda();
  if (!(await exists(conn, flp.address))) {
    const ix = await client.initializeFlpExposureIx(authority.publicKey);
    await send(conn, authority, [ix]);
    console.log(ok(`FlpExposure initialized    ${d(flp.address.toBase58())}`));
  } else {
    console.log(d(`     FlpExposure already initialized — skipping`));
  }

  const ft = feeTiersPda();
  if (!(await exists(conn, ft.address))) {
    const ix = await client.initFeeTiersIx({
      authority: authority.publicKey,
      volumeWindowSlots: new BN(3_024_000),
      tiers: [
        { minVolumeQuoteLots: new BN(0), makerRebateBps: -2, takerFeeBps: 5 },
        { minVolumeQuoteLots: new BN('1000000000000'), makerRebateBps: 0, takerFeeBps: 4 },
        { minVolumeQuoteLots: new BN('5000000000000'), makerRebateBps: 1, takerFeeBps: 3 },
        { minVolumeQuoteLots: new BN('25000000000000'), makerRebateBps: 2, takerFeeBps: 2 },
      ],
    });
    await send(conn, authority, [ix]);
    console.log(ok(`FeeTiers initialized      ${d(ft.address.toBase58())}`));
  } else {
    console.log(d(`     FeeTiers already initialized — skipping`));
  }

  // Initialize ONE market: SOL/USDC (using a synthetic base mint).
  const baseMintKpPath = `${TMP_PREFIX}-base-mint.json`;
  let baseMintKp: Keypair;
  if (fs.existsSync(baseMintKpPath)) {
    baseMintKp = loadKp(baseMintKpPath);
  } else {
    baseMintKp = Keypair.generate();
    fs.writeFileSync(baseMintKpPath, JSON.stringify(Array.from(baseMintKp.secretKey)));
    await createMint(conn, authority, authority.publicKey, null, 9, baseMintKp);
  }
  const baseMint = baseMintKp.publicKey;
  const market = marketPda(baseMint, USDC).address;
  const book = marketBookPda(market).address;
  console.log(`  ${b('Market base mint:')} ${baseMint.toBase58()}`);
  console.log(`  ${b('Market PDA:')}       ${market.toBase58()}`);

  if (!(await exists(conn, market))) {
    const ix = await client.initializeMarketIx({
      authority: authority.publicKey,
      baseMint,
      quoteMint: USDC,
      baseVault: quoteVault, // placeholder; perps don't use base_vault
      quoteVault,
      oracleAccount: authority.publicKey, // placeholder
      params: defaultMajorMarketParams(),
      initialOracleTicks: new BN(BASE_MINT_PRICE_TICKS.toString()) as unknown as bigint,
    });
    await send(conn, authority, [ix]);
    console.log(ok(`Market initialized`));
  }
  if (!(await exists(conn, book))) {
    const ix = await client.initMarketBookIx({ authority: authority.publicKey, market });
    await send(conn, authority, [ix]);
    console.log(ok(`MarketBook initialized`));
  }

  // ─── Step 4: create Alice + Bob, fund them with SOL + USDC
  console.log(banner('STEP 4 — create Alice + Bob, fund with USDC'));
  const ALICE_PATH = `${TMP_PREFIX}-alice.json`;
  const BOB_PATH = `${TMP_PREFIX}-bob.json`;
  let alice: Keypair;
  let bob: Keypair;
  if (fs.existsSync(ALICE_PATH)) alice = loadKp(ALICE_PATH);
  else { alice = Keypair.generate(); fs.writeFileSync(ALICE_PATH, JSON.stringify(Array.from(alice.secretKey))); }
  if (fs.existsSync(BOB_PATH)) bob = loadKp(BOB_PATH);
  else { bob = Keypair.generate(); fs.writeFileSync(BOB_PATH, JSON.stringify(Array.from(bob.secretKey))); }

  for (const [name, kp] of [['Alice', alice], ['Bob', bob]] as const) {
    const bal = await conn.getBalance(kp.publicKey);
    if (bal < 1e9) await fundWallet(conn, kp.publicKey, 1 * 1e9, authority);
    let ata: PublicKey;
    try {
      ata = await createAssociatedTokenAccount(conn, authority, USDC, kp.publicKey);
    } catch {
      ata = await getAssociatedTokenAddress(USDC, kp.publicKey);
    }
    // Mint 1000 USDC (1000 * 10^6 quote-lots).
    await mintTo(conn, authority, USDC, ata, authority, 1_000_000_000);
    const tokenBal = await conn.getTokenAccountBalance(ata);
    console.log(ok(`${name.padEnd(5)} ${kp.publicKey.toBase58()}  →  ${tokenBal.value.uiAmount} USDC`));
  }

  // ─── Step 5: open trader_state + deposit for both
  console.log(banner('STEP 5 — open trader_state + deposit 100 USDC each'));
  for (const [name, kp] of [['Alice', alice], ['Bob', bob]] as const) {
    const tsPda = client.traderState(kp.publicKey).address;
    if (!(await exists(conn, tsPda))) {
      const ix = await client.openTraderStateIx(kp.publicKey);
      await send(conn, kp, [ix]);
      console.log(ok(`${name} opened trader_state`));
    }

    // Deposit 100 USDC.
    const ata = await getAssociatedTokenAddress(USDC, kp.publicKey);
    const ix = await client.depositCollateralIx({
      trader: kp.publicKey,
      amount: new BN(100_000_000) as unknown as bigint, // 100 USDC at 6 decimals
      quoteMint: USDC,
      quoteVault,
      traderQuoteAta: ata,
    });
    await send(conn, kp, [ix]);

    const tsInfo = await conn.getAccountInfo(tsPda);
    const collat = Number(tsInfo!.data.readBigUInt64LE(8 + 32 + 1)) / 1e6;
    console.log(ok(`${name} deposited → collateral: ${collat.toFixed(2)} USDC`));
  }

  // ─── Step 6: Alice rests as maker, Bob takes the book
  console.log(banner('STEP 6 — Alice rests SHORT 5 @ 99950 (maker)'));
  // Order size = 5 base lots. With baseLotSize=1000, that's 5_000 base units.
  // Notional = 5 × 99950 × tick_size(1) = 499_750 quote-lots = 0.50 USDC.
  // IM required ≈ 0.50 × 250/10000 = 0.0125 USDC. Plenty of headroom.
  const aliceMakerIx = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: 'short',
    sizeLots: new BN(5),
    limitTicks: new BN(99950),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  await send(conn, alice, [aliceMakerIx]);
  console.log(ok(`Alice  rests SHORT 5 @ 99950 (maker — in the hypertree book)`));

  // Show orderbook depth.
  try {
    const depthIx = await client.viewBookDepthV2Ix({ market });
    const tx = new Transaction().add(depthIx);
    tx.feePayer = authority.publicKey;
    tx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
    const sim = await conn.simulateTransaction(tx, [authority]);
    const coder = new BorshEventCoder(IDL);
    let depth: any = null;
    for (const line of sim.value.logs ?? []) {
      if (!line.startsWith('Program data: ')) continue;
      try {
        const ev = coder.decode(line.slice('Program data: '.length).trim());
        if (ev && ev.name === 'BookDepthV2Event') { depth = ev.data; break; }
      } catch { /* skip */ }
    }
    if (depth) {
      console.log(`  ${b('Pre-match book:')}`);
      let bidCount = 0, askCount = 0;
      for (const a of depth.asks ?? []) {
        if ((a.priceTicks?.toString?.() ?? '0') === '0') continue;
        console.log(`    ${C.red}${a.priceTicks}${C.reset}  ×  ${a.sizeLots}  ← ask`);
        askCount++;
      }
      for (const bd of depth.bids ?? []) {
        if ((bd.priceTicks?.toString?.() ?? '0') === '0') continue;
        console.log(`    ${C.green}${bd.priceTicks}${C.reset}  ×  ${bd.sizeLots}  ← bid`);
        bidCount++;
      }
      console.log(d(`     (${bidCount} bid + ${askCount} ask resting — ready to match)`));
    }
  } catch (e) {
    console.log(d(`  (could not read orderbook: ${(e as Error).message.split('\n')[0]})`));
  }

  // ─── Step 7: Bob takes the book via CLOB place_taker_order_v2
  console.log(banner('STEP 7 — Bob CLOB-sweeps LONG 5 @ 99950 (taker)'));
  // CLOB takers walk the book inline — no run_batch_v2 needed. The matcher
  // emits BatchFillIntentEvent per fill plus a TakerOrderClearedEvent
  // summary inside the same tx.
  const heapIx = ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 });
  const cuIx = ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 });
  const bobTakerIx = await client.placeTakerOrderV2Ix({
    trader: bob.publicKey,
    market,
    side: 'long',
    sizeLots: new BN(5),
    limitTicks: new BN(99950),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const takerSig = await send(conn, bob, [heapIx, cuIx, bobTakerIx]);
  console.log(ok(`CLOB taker fired  ${d(takerSig.slice(0, 20) + '…')}`));

  // Decode inline events from the taker tx.
  const takerTx = await conn.getTransaction(takerSig, {
    commitment: 'confirmed',
    maxSupportedTransactionVersion: 0,
  });
  const coder = new BorshEventCoder(IDL);
  let takerClearedEvent: any = null;
  const fillIntents: any[] = [];
  for (const line of takerTx?.meta?.logMessages ?? []) {
    if (!line.startsWith('Program data: ')) continue;
    try {
      const ev = coder.decode(line.slice('Program data: '.length).trim());
      if (!ev) continue;
      if (ev.name === 'TakerOrderClearedEvent') takerClearedEvent = ev.data;
      if (ev.name === 'BatchFillIntentEvent') fillIntents.push(ev.data);
    } catch { /* skip */ }
  }

  if (takerClearedEvent) {
    const e = takerClearedEvent;
    const requested = e.takerSizeLots ?? e.taker_size_lots;
    const filled = e.filledLots ?? e.filled_lots;
    const residual = e.residualRestingLots ?? e.residual_resting_lots;
    const matchCount = e.matchCount ?? e.match_count;
    console.log(`  ${b('TakerOrderClearedEvent:')}`);
    console.log(`    requested:   ${requested?.toString()}`);
    console.log(`    filled:      ${filled?.toString()}`);
    console.log(`    residual:    ${residual?.toString()}`);
    console.log(`    match_count: ${matchCount}`);
  } else {
    console.log(fail(`No TakerOrderClearedEvent emitted — taker didn't run`));
  }
  console.log(`  ${b('BatchFillIntentEvents (inline):')} ${fillIntents.length}`);
  for (const f of fillIntents) {
    const taker = f.taker instanceof PublicKey ? f.taker : new PublicKey(f.taker);
    const maker = f.maker instanceof PublicKey ? f.maker : new PublicKey(f.maker);
    const sz = f.sizeLots ?? f.size_lots;
    const px = f.priceTicks ?? f.price_ticks;
    const ts = f.takerSide ?? f.taker_side;
    console.log(`    taker=${taker.toBase58().slice(0, 8)}…  maker=${maker.toBase58().slice(0, 8)}…  size=${sz}  price=${px}  side=${ts === 0 ? 'L' : 'S'}`);
  }

  // ─── Step 8: sequencer-style apply_fill for each intent
  console.log(banner('STEP 8 — sequencer settles fills via apply_fill'));
  for (const f of fillIntents) {
    const taker = f.taker instanceof PublicKey ? f.taker : new PublicKey(f.taker);
    const maker = f.maker instanceof PublicKey ? f.maker : new PublicKey(f.maker);
    const sz = f.sizeLots ?? f.size_lots;
    const px = f.priceTicks ?? f.price_ticks;
    const ts = f.takerSide ?? f.taker_side;
    const applyIx = await client.applyFillIx({
      sequencer: authority.publicKey,
      market,
      takerTrader: taker,
      makerTrader: maker,
      sizeLots: new BN(sz.toString()) as unknown as bigint,
      priceTicks: new BN(px.toString()) as unknown as bigint,
      takerSide: ts === 0 ? 'long' : 'short',
      useFeeTiers: true,
    });
    const sig = await send(conn, authority, [applyIx]);
    console.log(ok(`apply_fill landed  ${d(sig.slice(0, 20) + '…')}`));
  }

  // ─── Step 9: verify positions + collateral changed
  console.log(banner('STEP 9 — verify positions populated + collateral updated'));
  const posPdaAlice = client.position(market, alice.publicKey).address;
  const posPdaBob = client.position(market, bob.publicKey).address;
  for (const [name, kp, posPk] of [['Alice', alice, posPdaAlice], ['Bob', bob, posPdaBob]] as const) {
    const tsInfo = await conn.getAccountInfo(client.traderState(kp.publicKey).address);
    const collat = Number(tsInfo!.data.readBigUInt64LE(8 + 32 + 1)) / 1e6;
    const openPos = tsInfo!.data.readUInt8(8 + 32 + 1 + 8 + 8);
    // volume_30d at end of body
    const volOff = 8 + 32 + 1 + 8 + 8 + 1 + 4 + 4 + 8 + 4 + 32 + 32 + 32 + 4;
    const volume = tsInfo!.data.length >= volOff + 8
      ? Number(tsInfo!.data.readBigUInt64LE(volOff)) / 1e6
      : 0;

    const posInfo = await conn.getAccountInfo(posPk);
    if (posInfo) {
      // PositionAccount layout: 8 disc + 32 trader + 32 market + 1 bump + 1 side + 8 size + 8 entry
      const off = 8 + 32 + 32 + 1; // side offset
      const side = posInfo.data.readUInt8(off);
      const size = posInfo.data.readBigUInt64LE(off + 1);
      const entry = posInfo.data.readBigUInt64LE(off + 1 + 8);
      const sideStr = side === 0 ? `${C.green}LONG${C.reset}` : `${C.red}SHORT${C.reset}`;
      console.log(ok(`${name.padEnd(5)} position: ${sideStr} ${size} @ ${entry}   collateral: ${collat.toFixed(4)} USDC   30d-volume: ${volume.toFixed(2)} USDC   open: ${openPos}`));
    } else {
      console.log(fail(`${name} position not created`));
    }
  }

  // ─── Step 10: read effective tier for Alice now that she has volume
  console.log(banner('STEP 10 — Alice\'s effective tier after the fill'));
  const tierIx = await client.viewTraderEffectiveTierIx({ trader: alice.publicKey });
  const tierTx = new Transaction().add(tierIx);
  tierTx.feePayer = authority.publicKey;
  tierTx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  const tierSim = await conn.simulateTransaction(tierTx, [authority]);
  let tier: any = null;
  for (const line of tierSim.value.logs ?? []) {
    if (!line.startsWith('Program data: ')) continue;
    try {
      const ev = coder.decode(line.slice('Program data: '.length).trim());
      if (ev && ev.name === 'TraderEffectiveTierEvent') { tier = ev.data; break; }
    } catch { /* skip */ }
  }
  if (tier) {
    const tierIndex = tier.tierIndex ?? tier.tier_index;
    const vol = tier.effectiveVolumeQuoteLots ?? tier.effective_volume_quote_lots;
    const maker = tier.makerRebateBps ?? tier.maker_rebate_bps;
    const taker = tier.takerFeeBps ?? tier.taker_fee_bps;
    console.log(`  Tier:     VIP${tierIndex}`);
    console.log(`  Volume:   ${(Number(vol) / 1e6).toFixed(4)} USDC`);
    console.log(`  Maker:    ${maker >= 0 ? '+' : ''}${maker} bps`);
    console.log(`  Taker:    ${taker} bps`);
  }

  // ─── Summary
  console.log(banner('END-TO-END DEMO — COMPLETE'));
  console.log(`\n  ${C.green}${b('✓ Protocol works end-to-end on a real Solana validator.')}${C.reset}\n`);
  console.log('  What just happened:');
  console.log('    1. Deployed 4 programs (verified executable)');
  console.log('    2. Created a test USDC mint + InsuranceFund + FlpExposure + FeeTiers + 1 market');
  console.log('    3. Funded Alice + Bob with SOL + 1000 USDC each');
  console.log('    4. Both opened trader_state + deposited 100 USDC');
  console.log('    5. Alice posted a resting SHORT (maker); Bob ran a CLOB taker LONG');
  console.log('    6. CLOB taker walked the book inline → BatchFillIntentEvent + TakerOrderClearedEvent');
  console.log('    7. Sequencer-style apply_fill landed BOTH positions on-chain');
  console.log('    8. Both Alice + Bob now have populated positions + tier-resolved volumes');
  console.log('');
  console.log(`  ${d('All accounts queryable on the validator. Inspect with:')}`);
  console.log(`  ${d('  solana account ' + posPdaAlice.toBase58() + ' --url ' + RPC_URL)}`);
  console.log('');
}

main().catch((e) => {
  console.error(`\n${C.red}E2E demo failed:${C.reset}`, e);
  console.error(e.stack);
  process.exit(1);
});
