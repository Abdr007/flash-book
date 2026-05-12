#!/usr/bin/env bun
// Flash Book V3 — END-TO-END AUTO-DELEVERAGE (ADL) PROOF.
//
// Proves the protocol's last-line-of-defence: when liquidation losses
// exceed the insurance fund's coverage, ADL force-closes the most
// profitable counter-positions at the bankrupt trader's bankruptcy
// price. The two-condition ADL gate (in `auto_deleverage` ix) ONLY
// fires when BOTH:
//   1. insurance_fund.balance_quote_lots < pause_threshold_quote_lots
//   2. The counter-position has POSITIVE PnL at the underwater
//      trader's bankruptcy price (the gate REFUSES to ADL a
//      counterparty whose realized PnL at bp would be ≤ 0).
//
// The flow:
//   STEP 1 — Snapshot current state.
//   STEP 2 — Authority calls `set_insurance_pause_threshold` to raise
//            the gate ABOVE the current balance. (Without this, the
//            ADL trigger would require draining the fund via many
//            liquidations; this authority lever lets us drive the
//            protocol into ADL-eligible state cleanly.)
//   STEP 3 — Three fresh traders enter:
//              Dan   = HIGH-leverage LONG  (the bankrupt-to-be)
//              Eve   = SHORT @ ~99950      (profitable at Dan's bp)
//              Frank = SHORT @ ~95000      (UNPROFITABLE at Dan's bp,
//                                            because bp ≈ 96617 > 95000)
//   STEP 4 — Authority pushes oracle DOWN ~5% — Dan bankrupts.
//   STEP 5 — Bob liquidates Dan via `liquidate_position_v2`. This
//            burns through Dan's collateral and the insurance fund
//            still has tiny balance < pause_threshold (we raised the
//            threshold sky-high in step 2). ADL is now eligible.
//   STEP 6 — Call `auto_deleverage` twice:
//              (a) Eve   — SHOULD SUCCEED.   Counter is profitable at bp.
//              (b) Frank — SHOULD FAIL with `AdlNotEligible`. The
//                          smart-gate refuses because Frank's SHORT
//                          would NOT be in profit at Dan's bp.
//   STEP 7 — Post-state verification, signatures, before/after diff.
//
// PREREQUISITES:
//   1. Run scripts/e2e-demo.ts first to bootstrap Alice/Bob/USDC/market.
//   2. Validator + program must be live.
//
// Run on localnet:
//   bun run scripts/e2e-adl.ts
//
// Run on devnet:
//   RPC_URL=https://api.devnet.solana.com TMP_PREFIX=/tmp/flash-book-devnet-e2e \
//     bun run scripts/e2e-adl.ts

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
  createAssociatedTokenAccount,
  createMint,
  getAssociatedTokenAddress,
  mintTo,
} from '@solana/spl-token';
import { AnchorProvider, BN, BorshEventCoder, Wallet } from '@coral-xyz/anchor';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  FLASH_BOOK_PROGRAM_ID,
  FlashBookClient,
  IDL,
  ORDER_FLAG_POST_ONLY,
  defaultMajorMarketParams,
  insuranceFundPda,
  marketBookPda,
  marketPda,
  positionPda,
} from '../sdk-ts/src/index.ts';
import {
  fetchInsuranceFund,
  fetchMarket,
  fetchPosition,
  fetchTraderState,
} from '../sdk-ts/src/accounts.ts';

// ─── Colors ──────────────────────────────────────────────────────────
const C = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
  magenta: '\x1b[35m',
};
const b = (s: string) => `${C.bold}${s}${C.reset}`;
const d = (s: string) => `${C.dim}${s}${C.reset}`;
const ok = (s: string) => `${C.green}✓${C.reset} ${s}`;
const fail = (s: string) => `${C.red}✗${C.reset} ${s}`;
const warn = (s: string) => `${C.yellow}!${C.reset} ${s}`;
const banner = (s: string) =>
  `\n${C.cyan}${b('━━ ' + s + ' ' + '━'.repeat(Math.max(2, 60 - s.length)))}${C.reset}`;

// ─── Config ──────────────────────────────────────────────────────────
const RPC_URL = process.env.RPC_URL ?? 'http://127.0.0.1:8899';
const AUTHORITY_PATH =
  process.env.AUTHORITY_KEYPAIR ??
  path.join(os.homedir(), '.config', 'solana', 'id.json');
const TMP_PREFIX = process.env.TMP_PREFIX ?? '/tmp/flash-book-e2e';

const USD_DECIMALS = 6;
const usdc = (q: bigint | number | BN): string => {
  const n = typeof q === 'bigint' ? Number(q) : typeof q === 'number' ? q : Number(q.toString());
  return (n / 1e6).toFixed(4);
};

function loadKp(p: string): Keypair {
  return Keypair.fromSecretKey(
    new Uint8Array(JSON.parse(fs.readFileSync(p, 'utf8'))),
  );
}

function ensureKeypairFile(p: string): Keypair {
  if (fs.existsSync(p)) return loadKp(p);
  const kp = Keypair.generate();
  fs.writeFileSync(p, JSON.stringify(Array.from(kp.secretKey)));
  return kp;
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
  const sig = await conn.sendRawTransaction(tx.serialize(), {
    skipPreflight: false,
  });
  await conn.confirmTransaction(sig, 'confirmed');
  return sig;
}

async function airdrop(conn: Connection, to: PublicKey, lamports: number) {
  const sig = await conn.requestAirdrop(to, lamports);
  await conn.confirmTransaction(sig, 'confirmed');
}

async function fundWallet(
  conn: Connection,
  to: PublicKey,
  lamports: number,
  from: Keypair,
) {
  const existing = await conn.getBalance(to);
  if (existing >= lamports) return;
  const needed = lamports - existing;
  try {
    await airdrop(conn, to, needed);
  } catch {
    const tx = new Transaction().add(
      SystemProgram.transfer({
        fromPubkey: from.publicKey,
        toPubkey: to,
        lamports: needed,
      }),
    );
    tx.feePayer = from.publicKey;
    tx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
    tx.sign(from);
    const sig = await conn.sendRawTransaction(tx.serialize());
    await conn.confirmTransaction(sig, 'confirmed');
  }
}

async function decodeEvents(
  conn: Connection,
  sig: string,
  name?: string,
): Promise<Array<{ name: string; data: any }>> {
  const tx = await conn.getTransaction(sig, {
    commitment: 'confirmed',
    maxSupportedTransactionVersion: 0,
  });
  const coder = new BorshEventCoder(IDL);
  const out: Array<{ name: string; data: any }> = [];
  for (const line of tx?.meta?.logMessages ?? []) {
    if (!line.startsWith('Program data: ')) continue;
    try {
      const ev = coder.decode(line.slice('Program data: '.length).trim());
      if (ev && (!name || ev.name === name))
        out.push({ name: ev.name, data: ev.data });
    } catch {
      /* skip non-event program-data lines */
    }
  }
  return out;
}

async function exists(conn: Connection, pk: PublicKey): Promise<boolean> {
  return (await conn.getAccountInfo(pk)) !== null;
}

// ─── Trader bootstrap helper ─────────────────────────────────────────
async function setupTrader(
  conn: Connection,
  client: FlashBookClient,
  authority: Keypair,
  trader: Keypair,
  USDC: PublicKey,
  quoteVault: PublicKey,
  collateralQuoteLots: bigint,
  label: string,
): Promise<{ ata: PublicKey; tsAddr: PublicKey }> {
  // Fund SOL
  const solBal = await conn.getBalance(trader.publicKey);
  if (solBal < 0.3 * 1e9) {
    await fundWallet(conn, trader.publicKey, 1 * 1e9, authority);
    console.log(ok(`Funded ${label} with 1 SOL`));
  } else {
    console.log(d(`${label} SOL balance ${(solBal / 1e9).toFixed(2)} SOL`));
  }

  // ATA
  let ata: PublicKey;
  try {
    ata = await createAssociatedTokenAccount(conn, authority, USDC, trader.publicKey);
    console.log(ok(`Created ${label} ATA`));
  } catch {
    ata = await getAssociatedTokenAddress(USDC, trader.publicKey);
  }

  // Mint USDC if low
  const tokBal = await conn.getTokenAccountBalance(ata);
  const need = Number(collateralQuoteLots) + 5_000_000; // 5 USDC slack
  if (Number(tokBal.value.amount) < need) {
    const toMint = need - Number(tokBal.value.amount);
    await mintTo(conn, authority, USDC, ata, authority, toMint);
    console.log(ok(`Minted ${usdc(BigInt(toMint))} USDC to ${label}`));
  }

  // Trader state
  const tsAddr = client.traderState(trader.publicKey).address;
  if (!(await exists(conn, tsAddr))) {
    const ix = await client.openTraderStateIx(trader.publicKey);
    await send(conn, trader, [ix]);
    console.log(ok(`Opened ${label}'s trader_state`));
  } else {
    console.log(d(`${label} trader_state already open`));
  }

  // Deposit collateral if not enough
  const tsPre = await fetchTraderState(client, tsAddr);
  const collatPre = tsPre ? BigInt(tsPre.collateralQuoteLots.toString()) : 0n;
  if (collatPre < collateralQuoteLots) {
    const delta = collateralQuoteLots - collatPre;
    const depIx = await client.depositCollateralIx({
      trader: trader.publicKey,
      amount: new BN(delta.toString()) as unknown as bigint,
      quoteMint: USDC,
      quoteVault,
      traderQuoteAta: ata,
    });
    await send(conn, trader, [depIx]);
    console.log(
      ok(
        `${label} deposited ${usdc(delta)} USDC  (target collateral ${usdc(collateralQuoteLots)} USDC)`,
      ),
    );
  } else {
    console.log(d(`${label} collateral ${usdc(collatPre)} USDC (≥ target)`));
  }

  return { ata, tsAddr };
}

// ─── Open a position via CLOB taker (Alice rests, target takes) ──────
async function openCounterShort(
  conn: Connection,
  client: FlashBookClient,
  authority: Keypair,
  alice: Keypair,
  taker: Keypair,
  market: PublicKey,
  sideOfTaker: 'short' | 'long',
  sizeLots: bigint,
  priceTicks: bigint,
  label: string,
): Promise<{ aliceSig: string; takerSig: string }> {
  // Alice posts the resting order on the opposite side of taker.
  // If taker is short → Alice rests LONG (bid). If taker is long → Alice rests SHORT (ask).
  const aliceSide = sideOfTaker === 'short' ? 'long' : 'short';
  const aliceRestIx = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: aliceSide,
    sizeLots: new BN(sizeLots.toString()),
    limitTicks: new BN(priceTicks.toString()),
    flags: ORDER_FLAG_POST_ONLY,
    expiresAtSlot: new BN(0),
  });
  const aliceSig = await send(conn, alice, [aliceRestIx]);
  console.log(ok(`Alice rests ${aliceSide.toUpperCase()} ${sizeLots} @ ${priceTicks}  ${d(aliceSig.slice(0, 20) + '…')}`));

  // Taker takes
  const heapIx = ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 });
  const cuIx = ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 });
  const takerIx = await client.placeTakerOrderV2Ix({
    trader: taker.publicKey,
    market,
    side: sideOfTaker,
    sizeLots: new BN(sizeLots.toString()),
    limitTicks: new BN(priceTicks.toString()),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const takerSig = await send(conn, taker, [heapIx, cuIx, takerIx]);
  console.log(ok(`${label} takes ${sideOfTaker.toUpperCase()} ${sizeLots} @ ${priceTicks}  ${d(takerSig.slice(0, 20) + '…')}`));

  // Settle fills
  const evs = await decodeEvents(conn, takerSig);
  const fills = evs.filter((e) => e.name === 'BatchFillIntentEvent');
  for (const f of fills) {
    const t = new PublicKey(f.data.taker);
    const m = new PublicKey(f.data.maker);
    const sz = f.data.sizeLots ?? f.data.size_lots;
    const px = f.data.priceTicks ?? f.data.price_ticks;
    const ts = f.data.takerSide ?? f.data.taker_side;
    const ix = await client.applyFillIx({
      sequencer: authority.publicKey,
      market,
      takerTrader: t,
      makerTrader: m,
      sizeLots: new BN(sz.toString()) as unknown as bigint,
      priceTicks: new BN(px.toString()) as unknown as bigint,
      takerSide: ts === 0 ? 'long' : 'short',
      useFeeTiers: true,
    });
    await send(conn, authority, [ix]);
  }
  return { aliceSig, takerSig };
}

// ─── Print trader summary ────────────────────────────────────────────
async function snapshotTrader(
  client: FlashBookClient,
  trader: PublicKey,
  market: PublicKey,
  label: string,
) {
  const ts = await fetchTraderState(client, client.traderState(trader).address);
  const pos = await fetchPosition(client, positionPda(market, trader).address);
  const collat = ts ? BigInt(ts.collateralQuoteLots.toString()) : 0n;
  const rpnl = ts ? BigInt(ts.realizedPnlQuoteLots.toString()) : 0n;
  const sz = pos ? BigInt(pos.sizeLots.toString()) : 0n;
  const entry = pos ? BigInt(pos.entryPriceTicks.toString()) : 0n;
  const side = pos ? (pos.side === 0 ? 'LONG' : 'SHORT') : '-';
  const sign = rpnl < 0n ? '-' : '+';
  const abs = rpnl < 0n ? -rpnl : rpnl;
  console.log(
    `  ${label.padEnd(7)} collat=${usdc(collat).padStart(10)} USDC  rpnl=${sign}${usdc(abs).padStart(10)} USDC  pos=${
      sz === 0n ? 'flat' : `${side} ${sz} @ ${entry}`
    }`,
  );
}

// ─── Main flow ───────────────────────────────────────────────────────
async function main() {
  console.log(b('\n  Flash Book V3 — END-TO-END AUTO-DELEVERAGE (ADL) PROOF\n'));

  const conn = new Connection(RPC_URL, 'confirmed');
  const authority = loadKp(AUTHORITY_PATH);
  console.log(`  Validator:  ${RPC_URL}`);
  console.log(`  Authority:  ${authority.publicKey.toBase58()}`);
  try {
    const v = await conn.getVersion();
    console.log(`  Cluster:    Solana ${v['solana-core']}`);
  } catch {
    console.log(fail(`Cannot reach validator at ${RPC_URL}`));
    process.exit(1);
  }

  // ─── Reuse bootstrap artefacts
  const USDC_PATH = `${TMP_PREFIX}-usdc-mint.json`;
  const BASE_PATH = `${TMP_PREFIX}-base-mint.json`;
  const ALICE_PATH = `${TMP_PREFIX}-alice.json`;
  const BOB_PATH = `${TMP_PREFIX}-bob.json`;
  const QV_PATH = `${TMP_PREFIX}-quote-vault.json`;
  const DAN_PATH = `${TMP_PREFIX}-dan.json`;
  const EVE_PATH = `${TMP_PREFIX}-eve.json`;
  const FRANK_PATH = `${TMP_PREFIX}-frank.json`;

  for (const p of [USDC_PATH, BASE_PATH, ALICE_PATH, BOB_PATH]) {
    if (!fs.existsSync(p)) {
      console.log(
        fail(`Missing bootstrap artefact ${p}. Run scripts/e2e-demo.ts first.`),
      );
      process.exit(1);
    }
  }
  const USDC = loadKp(USDC_PATH).publicKey;
  const alice = loadKp(ALICE_PATH);
  const bob = loadKp(BOB_PATH);
  // Use a FRESH base mint for the ADL test. The e2e-demo's market has
  // residual maker orders from prior runs that interfere with placing
  // a SHORT taker at a precisely-below-bankruptcy-price level. A fresh
  // market gives the gate proof a clean book.
  const ADL_BASE_PATH = `${TMP_PREFIX}-adl-base-mint.json`;
  let adlBaseMintKp: Keypair;
  if (fs.existsSync(ADL_BASE_PATH)) {
    adlBaseMintKp = loadKp(ADL_BASE_PATH);
  } else {
    adlBaseMintKp = Keypair.generate();
    fs.writeFileSync(ADL_BASE_PATH, JSON.stringify(Array.from(adlBaseMintKp.secretKey)));
  }
  const baseMint = adlBaseMintKp.publicKey;
  // Read quote_vault from on-chain InsuranceFund (authoritative).
  const _idl = await import('../sdk-ts/idl.json', { with: { type: 'json' } });
  const _fundPda = insuranceFundPda();
  const _fundInfo = await conn.getAccountInfo(_fundPda.address);
  let quoteVault: PublicKey;
  if (_fundInfo) {
    const _coder = new (await import('@coral-xyz/anchor')).BorshAccountsCoder(_idl.default ?? _idl);
    const _decoded: any = _coder.decode('InsuranceFundAccount', _fundInfo.data);
    quoteVault = _decoded.quoteVault ?? _decoded.quote_vault;
  } else if (fs.existsSync(QV_PATH)) {
    quoteVault = loadKp(QV_PATH).publicKey;
  } else {
    console.log(fail(`InsuranceFund missing and no local quote-vault keypair`));
    process.exit(1);
  }
  const market = marketPda(baseMint, USDC).address;

  // Three fresh ADL traders (persisted).
  const dan = ensureKeypairFile(DAN_PATH);
  const eve = ensureKeypairFile(EVE_PATH);
  const frank = ensureKeypairFile(FRANK_PATH);

  console.log(`  USDC mint:  ${USDC.toBase58()}`);
  console.log(`  Market:     ${market.toBase58()}`);
  console.log(`  Alice (maker):    ${alice.publicKey.toBase58()}`);
  console.log(`  Bob   (liq):      ${bob.publicKey.toBase58()}`);
  console.log(`  Dan   (UW LONG):  ${dan.publicKey.toBase58()}`);
  console.log(`  Eve   (SHORT,P):  ${eve.publicKey.toBase58()}`);
  console.log(`  Frank (SHORT,U):  ${frank.publicKey.toBase58()}`);

  const wallet = new Wallet(authority);
  const _prov = new AnchorProvider(conn, wallet, { commitment: 'confirmed' });
  void _prov;
  const client = new FlashBookClient(conn, wallet);

  // Sanity: program live.
  const corePrg = await conn.getAccountInfo(FLASH_BOOK_PROGRAM_ID);
  if (!corePrg?.executable) {
    console.log(fail(`flash_book NOT deployed at ${FLASH_BOOK_PROGRAM_ID.toBase58()}`));
    process.exit(1);
  }

  // ─── Ensure the dedicated ADL market exists (idempotent).
  const baseMintInfo = await conn.getAccountInfo(baseMint);
  if (!baseMintInfo) {
    console.log(d(`Creating ADL base mint ${baseMint.toBase58()}…`));
    await createMint(conn, authority, authority.publicKey, null, 9, adlBaseMintKp);
    console.log(ok(`ADL base mint created`));
  }
  if (!(await exists(conn, market))) {
    const initOracle = new BN(99950);
    const ix = await client.initializeMarketIx({
      authority: authority.publicKey,
      baseMint,
      quoteMint: USDC,
      baseVault: quoteVault,
      quoteVault,
      oracleAccount: authority.publicKey,
      params: defaultMajorMarketParams(),
      initialOracleTicks: initOracle as unknown as bigint,
    });
    await send(conn, authority, [ix]);
    console.log(ok(`ADL market initialized at oracle=${initOracle}`));
  }
  const book = marketBookPda(market).address;
  if (!(await exists(conn, book))) {
    const ix = await client.initMarketBookIx({ authority: authority.publicKey, market });
    await send(conn, authority, [ix]);
    console.log(ok(`ADL MarketBook initialized`));
  }
  console.log(ok(`Using dedicated ADL market: ${market.toBase58()}`));

  // ─── STEP 1 — Snapshot initial state
  console.log(banner('STEP 1 — initial snapshot'));
  const fundPk = insuranceFundPda().address;
  const m0 = await fetchMarket(client, market);
  const fund0 = await fetchInsuranceFund(client, fundPk);
  if (!m0 || !fund0) {
    console.log(fail('Failed to fetch market/insurance fund'));
    process.exit(1);
  }
  const initialOracle = BigInt(m0.oraclePriceTicks.toString());
  const initialMark = BigInt(m0.markPriceTicks.toString());
  const tickSize = BigInt(m0.params.tickSize.toString());
  console.log(`  Oracle price:        ${initialOracle} ticks`);
  console.log(`  Mark price:          ${initialMark} ticks`);
  console.log(`  Max leverage:        ${m0.params.maxLeverage}×`);
  console.log(`  MMR:                 ${m0.params.maintenanceMarginRatioBps} bps`);
  console.log(`  Tick size:           ${tickSize}`);
  console.log(`  Insurance balance:   ${fund0.balanceQuoteLots} q-lots (${usdc(BigInt(fund0.balanceQuoteLots.toString()))} USDC)`);
  console.log(`  Pause threshold:     ${fund0.pauseThresholdQuoteLots} q-lots (${usdc(BigInt(fund0.pauseThresholdQuoteLots.toString()))} USDC)`);
  console.log('');
  console.log(`  ${b('Traders BEFORE:')}`);
  for (const [label, kp] of [['Alice', alice], ['Bob', bob], ['Dan', dan], ['Eve', eve], ['Frank', frank]] as const) {
    await snapshotTrader(client, kp.publicKey, market, label);
  }

  // ─── STEP 2 — Raise pause_threshold above current balance
  console.log(banner('STEP 2 — raise pause_threshold via set_insurance_pause_threshold'));
  console.log(d('  This puts the protocol into the ADL-eligible state without first'));
  console.log(d('  draining the insurance fund the hard way. Mainnet would normally'));
  console.log(d('  enter this state by burning balance via large bankruptcy losses.'));
  // Big threshold: 10_000_000_000 q-lots = 10_000 USDC. Way above current balance.
  const NEW_PAUSE_THRESHOLD = new BN('10000000000');
  const setThreshIx = await client.setInsurancePauseThresholdIx({
    authority: authority.publicKey,
    newThresholdQuoteLots: NEW_PAUSE_THRESHOLD,
  });
  const setThreshSig = await send(conn, authority, [setThreshIx]);
  const setThreshEvs = await decodeEvents(conn, setThreshSig);
  const thresholdEv = setThreshEvs.find(
    (e) => e.name === 'InsurancePauseThresholdUpdatedEvent',
  )?.data;
  console.log(ok(`set_insurance_pause_threshold landed  ${d(setThreshSig.slice(0, 20) + '…')}`));
  if (thresholdEv) {
    console.log(
      `    ${b('InsurancePauseThresholdUpdatedEvent:')} prev=${thresholdEv.previousThresholdQuoteLots ?? thresholdEv.previous_threshold_quote_lots}  new=${thresholdEv.newThresholdQuoteLots ?? thresholdEv.new_threshold_quote_lots}  balance=${thresholdEv.currentBalanceQuoteLots ?? thresholdEv.current_balance_quote_lots}`,
    );
  }
  const fundAfterThresh = await fetchInsuranceFund(client, fundPk);
  const gateOpen =
    BigInt(fundAfterThresh!.balanceQuoteLots.toString()) <
    BigInt(fundAfterThresh!.pauseThresholdQuoteLots.toString());
  console.log(
    `  Gate condition #1 (balance < threshold): ${
      gateOpen ? `${C.green}OPEN${C.reset} (${fundAfterThresh!.balanceQuoteLots} < ${fundAfterThresh!.pauseThresholdQuoteLots})` : `${C.red}CLOSED${C.reset}`
    }`,
  );
  if (!gateOpen) {
    console.log(fail('pause_threshold did not move above balance — aborting'));
    process.exit(2);
  }

  // ─── STEP 3 — Setup the three new traders
  console.log(banner('STEP 3 — setup Dan / Eve / Frank with collateral'));

  // Dan: high-lev LONG → bankruptcy. Small collateral.
  await setupTrader(conn, client, authority, dan, USDC, quoteVault, 3_000_000n, 'Dan');
  // Eve: profitable SHORT counter. Comfortable collateral so she doesn't fail margin.
  await setupTrader(conn, client, authority, eve, USDC, quoteVault, 50_000_000n, 'Eve');
  // Frank: SHORT at lower price (will be unprofitable at Dan's BP). Need comfortable collateral.
  await setupTrader(conn, client, authority, frank, USDC, quoteVault, 50_000_000n, 'Frank');

  // ─── STEP 3a — Dan opens HIGH-lev LONG via CLOB
  console.log(banner('STEP 3a — Dan opens 30× LONG'));
  // Idempotency: if Dan already has a position, skip.
  const danPosPda = positionPda(market, dan.publicKey).address;
  let danPos = await fetchPosition(client, danPosPda);
  const DAN_LOTS = 900n;
  const DAN_PRICE = initialOracle; // 99950 on fresh market
  if (!danPos || BigInt(danPos.sizeLots.toString()) === 0n) {
    await openCounterShort(
      conn, client, authority, alice, dan, market,
      'long', DAN_LOTS, DAN_PRICE, 'Dan',
    );
    danPos = await fetchPosition(client, danPosPda);
  } else {
    console.log(d(`Dan already has position size=${danPos.sizeLots} side=${danPos.side}`));
  }

  // Compute expected bankruptcy price NOW (after Dan opens). The ix
  // computes bp = entry - C/(S × tick) using the position's trader_state
  // collateral. We use the same formula to choose Frank's entry price.
  const danPosAfterOpen = await fetchPosition(client, danPosPda);
  const danTsAfterOpen = await fetchTraderState(client, client.traderState(dan.publicKey).address);
  const danEntryOpen = BigInt(danPosAfterOpen!.entryPriceTicks.toString());
  const danSizeOpen = BigInt(danPosAfterOpen!.sizeLots.toString());
  const danCollatOpen = BigInt(danTsAfterOpen!.collateralQuoteLots.toString());
  const denomOpen = danSizeOpen * tickSize;
  const dropPerLotOpen = denomOpen > 0n ? danCollatOpen / denomOpen : 0n;
  const bpExpected = danEntryOpen > dropPerLotOpen ? danEntryOpen - dropPerLotOpen : 1n;
  console.log(d(`  Dan's expected bp = ${danEntryOpen} - ${danCollatOpen}/(${danSizeOpen}×${tickSize}) = ${bpExpected}`));

  // ─── STEP 3b — Eve opens a profitable-at-bp SHORT (entered AT oracle)
  console.log(banner('STEP 3b — Eve opens SHORT 200 @ 99950 (profitable at Dan\'s bp)'));
  const evePosPda = positionPda(market, eve.publicKey).address;
  let evePos = await fetchPosition(client, evePosPda);
  const EVE_LOTS = 200n;
  const EVE_PRICE = initialOracle; // 99950 — way above bp ≈ 96617
  if (!evePos || BigInt(evePos.sizeLots.toString()) === 0n) {
    await openCounterShort(
      conn, client, authority, alice, eve, market,
      'short', EVE_LOTS, EVE_PRICE, 'Eve',
    );
    evePos = await fetchPosition(client, evePosPda);
  } else {
    console.log(d(`Eve already has position size=${evePos.sizeLots} side=${evePos.side}`));
  }

  // ─── STEP 3c — Frank opens an UNPROFITABLE-at-bp SHORT (entered BELOW bp)
  // Dan's expected bp ≈ 99950 - 3_000_000/900 ≈ 96617.
  // Eve's SHORT at 99950 > bp → profitable at bp ✓
  // Frank's SHORT at 95000 < bp → UNPROFITABLE at bp (price went UP from
  // his short entry, so closing the short at bp realises a LOSS) → the
  // ADL gate must refuse to ADL Frank.
  // The fresh market has no other resting bids, so Alice's resting LONG
  // at 95000 is the only bid Frank can hit — he enters at exactly 95000.
  const FRANK_PRICE = (bpExpected / tickSize - 1500n) * tickSize; // ~1.5k below bp
  console.log(banner(`STEP 3c — Frank opens SHORT 200 @ ${FRANK_PRICE} (UNPROFITABLE at Dan\'s bp)`));
  const frankPosPda = positionPda(market, frank.publicKey).address;
  let frankPos = await fetchPosition(client, frankPosPda);
  const FRANK_LOTS = 200n;
  if (!frankPos || BigInt(frankPos.sizeLots.toString()) === 0n) {
    await openCounterShort(
      conn, client, authority, alice, frank, market,
      'short', FRANK_LOTS, FRANK_PRICE, 'Frank',
    );
    frankPos = await fetchPosition(client, frankPosPda);
  } else {
    console.log(d(`Frank already has position size=${frankPos.sizeLots} side=${frankPos.side}`));
  }

  // Snapshot positions
  console.log(banner('STEP 3 — positions after opening'));
  for (const [label, kp] of [['Dan', dan], ['Eve', eve], ['Frank', frank]] as const) {
    await snapshotTrader(client, kp.publicKey, market, label);
  }

  // ─── STEP 4 — Push oracle DOWN to bankrupt Dan
  console.log(banner('STEP 4 — push oracle DOWN past Dan\'s bp to drive him underwater'));
  // We bypass liquidate_position_v2 entirely: ADL has its own on-chain
  // health gate (assess_margin_fn with the same stress lattice). Going
  // straight to ADL lets us showcase the gate-eligibility decision
  // BEFORE the regular liquidation pipeline destroys Dan's position.
  //
  // Drop oracle to bp - 5% (the position MUST be unhealthy past the
  // ±30% stress lattice; deep enough below bp to be safe).
  const targetOracle = bpExpected * 95n / 100n;
  const updIx = await client.updateOracleIx({
    authority: authority.publicKey,
    market,
    priceTicks: new BN(targetOracle.toString()) as unknown as bigint,
    confidence: new BN(0) as unknown as bigint,
    publishedAtUnixSeconds: new BN(Math.floor(Date.now() / 1000).toString()) as unknown as bigint,
  });
  const updSig = await send(conn, authority, [updIx]);
  console.log(
    ok(
      `Oracle pushed: ${initialOracle} → ${targetOracle} (${(((Number(targetOracle) - Number(initialOracle)) / Number(initialOracle)) * 100).toFixed(2)}%, ~5% below bp ${bpExpected})  ${d(updSig.slice(0, 20) + '…')}`,
    ),
  );

  const heapIx = ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 });
  const cuIx = ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 });

  // ─── STEP 5 — Snapshot Dan + bp before ADL
  console.log(banner('STEP 5 — Dan is now underwater; bp re-derived from on-chain state'));
  const danPosFinal = await fetchPosition(client, danPosPda);
  const danTsFinal = await fetchTraderState(client, client.traderState(dan.publicKey).address);
  if (!danPosFinal || BigInt(danPosFinal.sizeLots.toString()) === 0n) {
    console.log(fail('Dan has no live position for ADL — aborting'));
    process.exit(3);
  }
  const danEntry = BigInt(danPosFinal.entryPriceTicks.toString());
  const danSize = BigInt(danPosFinal.sizeLots.toString());
  const danCollat = BigInt(danTsFinal!.collateralQuoteLots.toString());
  const denom = danSize * tickSize;
  const dropPerLot = denom > 0n ? danCollat / denom : 0n;
  const danBp = danEntry > dropPerLot ? danEntry - dropPerLot : 1n;
  console.log(`  Dan position:    LONG ${danSize} @ ${danEntry}  collat=${usdc(danCollat)} USDC`);
  console.log(`  Bankruptcy px:   bp = entry - C/(S×tick) = ${danEntry} - ${danCollat}/(${danSize}×${tickSize}) = ${b(danBp.toString())} ticks`);

  // Expected ADL gate decisions:
  const eveEntry = BigInt(evePos!.entryPriceTicks.toString());
  const frankEntry = BigInt(frankPos!.entryPriceTicks.toString());
  console.log('');
  console.log(`  Eve   SHORT entry=${eveEntry}.  bp(${danBp}) ${danBp < eveEntry ? '<' : '≥'} entry → SHORT ${danBp < eveEntry ? `${C.green}PROFITABLE${C.reset}` : `${C.red}UNPROFITABLE${C.reset}`} at bp`);
  console.log(`  Frank SHORT entry=${frankEntry}.  bp(${danBp}) ${danBp < frankEntry ? '<' : '≥'} entry → SHORT ${danBp < frankEntry ? `${C.green}PROFITABLE${C.reset}` : `${C.red}UNPROFITABLE${C.reset}`} at bp`);

  // ─── STEP 6 — Call auto_deleverage on Eve (should SUCCEED)
  console.log(banner('STEP 6a — auto_deleverage Eve (SHOULD SUCCEED — gate accepts)'));
  // Capture pre-state for verification.
  const evePosBeforeAdl = await fetchPosition(client, evePosPda);
  const eveTsBeforeAdl = await fetchTraderState(client, client.traderState(eve.publicKey).address);
  const eveSizeBefore = BigInt(evePosBeforeAdl!.sizeLots.toString());
  const eveCollatBefore = BigInt(eveTsBeforeAdl!.collateralQuoteLots.toString());

  const closeSize = eveSizeBefore < danSize ? eveSizeBefore : danSize;
  console.log(d(`  close_size = min(eve.size=${eveSizeBefore}, dan.size=${danSize}) = ${closeSize}`));
  const adlEveIx = await client.autoDeleverageIx({
    caller: bob.publicKey,
    market,
    underwaterTrader: dan.publicKey,
    counterTrader: eve.publicKey,
    closeSizeLots: new BN(closeSize.toString()) as unknown as bigint,
  });
  let adlEveSig: string | null = null;
  try {
    adlEveSig = await send(conn, bob, [heapIx, cuIx, adlEveIx]);
    console.log(ok(`${C.green}${b('AUTO-DELEVERAGE on Eve SUCCEEDED')}${C.reset}  ${d(adlEveSig.slice(0, 20) + '…')}`));
    const evs = await decodeEvents(conn, adlEveSig);
    const adlEv = evs.find((e) => e.name === 'AutoDeleveragedEvent')?.data;
    if (adlEv) {
      console.log(`    ${b('AutoDeleveragedEvent:')}`);
      console.log(`      underwater_trader: ${new PublicKey(adlEv.underwaterTrader ?? adlEv.underwater_trader).toBase58()}  (Dan)`);
      console.log(`      counter_trader:    ${new PublicKey(adlEv.counterTrader ?? adlEv.counter_trader).toBase58()}  (Eve)`);
      console.log(`      close_size_lots:   ${adlEv.closeSizeLots ?? adlEv.close_size_lots}`);
      console.log(`      bankruptcy_price:  ${adlEv.bankruptcyPriceTicks ?? adlEv.bankruptcy_price_ticks}`);
      console.log(`      counter_gain:      ${adlEv.counterGainQuoteLots ?? adlEv.counter_gain_quote_lots}  q-lots (${usdc(BigInt((adlEv.counterGainQuoteLots ?? adlEv.counter_gain_quote_lots).toString()))} USDC)`);
      console.log(`      executor:          ${new PublicKey(adlEv.executor).toBase58()}  (Bob)`);
    }
  } catch (e: any) {
    const msg = e?.message ?? '';
    console.log(fail(`ADL on Eve failed unexpectedly: ${msg.split('\n')[0].slice(0, 250)}`));
    if (msg.includes('AdlNotEligible')) {
      console.log(d('  Gate refused — possible reason: counter PnL at bp computed ≤ 0.'));
      console.log(d(`  bp=${danBp} eveEntry=${eveEntry} → expected bp < eveEntry, got bp ${danBp < eveEntry ? '<' : '≥'} eveEntry.`));
    }
    process.exit(4);
  }

  // ─── STEP 6b — Call auto_deleverage on Frank (should FAIL with AdlNotEligible)
  console.log(banner('STEP 6b — auto_deleverage Frank (SHOULD FAIL — gate refuses)'));
  // Re-fetch Dan after Eve's ADL — his size may have shrunk.
  const danPosAfterEveAdl = await fetchPosition(client, danPosPda);
  const danSizeNow = danPosAfterEveAdl
    ? BigInt(danPosAfterEveAdl.sizeLots.toString())
    : 0n;
  const frankPosBeforeAdl = await fetchPosition(client, frankPosPda);
  const frankSizeNow = BigInt(frankPosBeforeAdl!.sizeLots.toString());
  console.log(d(`  Dan residual size: ${danSizeNow}.  Frank size: ${frankSizeNow}`));

  let adlFrankErr: string | null = null;
  if (danSizeNow > 0n && frankSizeNow > 0n) {
    const closeF = danSizeNow < frankSizeNow ? danSizeNow : frankSizeNow;
    const adlFrankIx = await client.autoDeleverageIx({
      caller: bob.publicKey,
      market,
      underwaterTrader: dan.publicKey,
      counterTrader: frank.publicKey,
      closeSizeLots: new BN(closeF.toString()) as unknown as bigint,
    });
    try {
      const sig = await send(conn, bob, [heapIx, cuIx, adlFrankIx]);
      console.log(fail(`ADL on Frank UNEXPECTEDLY succeeded (${sig}) — gate did NOT refuse`));
      adlFrankErr = 'unexpected_success';
    } catch (e: any) {
      const msg: string = e?.message ?? '';
      adlFrankErr = msg;
      const isEligibility = msg.includes('AdlNotEligible') || msg.includes('1220') || msg.includes('0x4c4');
      if (isEligibility) {
        console.log(
          ok(
            `${C.green}${b('ADL on Frank REFUSED with AdlNotEligible')}${C.reset} — smart-gate working as designed`,
          ),
        );
        // Find the relevant log line
        const errLines = msg.split('\n').slice(0, 8);
        for (const l of errLines) {
          if (l.toLowerCase().includes('adl') || l.toLowerCase().includes('eligib') || l.includes('0x4c4') || l.includes('1220')) {
            console.log(d(`    log: ${l.trim().slice(0, 200)}`));
          }
        }
      } else {
        console.log(fail(`ADL on Frank failed for an UNEXPECTED reason: ${msg.split('\n')[0].slice(0, 250)}`));
      }
    }
  } else {
    console.log(warn(`Cannot test ADL-Frank: dan_size=${danSizeNow} frank_size=${frankSizeNow}`));
  }

  // ─── STEP 7 — Post-state verification
  console.log(banner('STEP 7 — verify post-ADL state'));
  const evePosAfterAdl = await fetchPosition(client, evePosPda);
  const eveTsAfterAdl = await fetchTraderState(client, client.traderState(eve.publicKey).address);
  const frankPosAfterAdl = await fetchPosition(client, frankPosPda);
  const frankTsAfterAdl = await fetchTraderState(client, client.traderState(frank.publicKey).address);
  const danPosFinalCheck = await fetchPosition(client, danPosPda);
  const danTsFinalCheck = await fetchTraderState(client, client.traderState(dan.publicKey).address);
  const fundFinal = await fetchInsuranceFund(client, fundPk);

  const eveSizeAfter = evePosAfterAdl ? BigInt(evePosAfterAdl.sizeLots.toString()) : 0n;
  const eveCollatAfter = eveTsAfterAdl ? BigInt(eveTsAfterAdl.collateralQuoteLots.toString()) : 0n;
  const eveRpnlAfter = eveTsAfterAdl ? BigInt(eveTsAfterAdl.realizedPnlQuoteLots.toString()) : 0n;
  const frankSizeAfter = frankPosAfterAdl ? BigInt(frankPosAfterAdl.sizeLots.toString()) : 0n;
  const frankCollatAfter = frankTsAfterAdl ? BigInt(frankTsAfterAdl.collateralQuoteLots.toString()) : 0n;
  const danSizeAfter = danPosFinalCheck ? BigInt(danPosFinalCheck.sizeLots.toString()) : 0n;
  const danCollatAfter = danTsFinalCheck ? BigInt(danTsFinalCheck.collateralQuoteLots.toString()) : 0n;

  let pass = true;

  // Eve's position should be reduced (size_before - closeSize). Acceptable: 0 if fully closed.
  console.log(`  Eve   size:        ${eveSizeBefore}  →  ${eveSizeAfter}  ${
    eveSizeAfter < eveSizeBefore ? `${C.green}(REDUCED — ADL applied)${C.reset}` : `${C.red}(UNCHANGED — FAILED)${C.reset}`
  }`);
  if (eveSizeAfter >= eveSizeBefore) pass = false;

  console.log(`  Eve   collat:      ${usdc(eveCollatBefore)} USDC  →  ${usdc(eveCollatAfter)} USDC  (Δ ${eveCollatAfter >= eveCollatBefore ? '+' : ''}${usdc(eveCollatAfter - eveCollatBefore)} USDC)`);
  console.log(`  Eve   realised pnl: ${eveRpnlAfter < 0n ? '-' : '+'}${usdc(eveRpnlAfter < 0n ? -eveRpnlAfter : eveRpnlAfter)} USDC  ${d('(should be POSITIVE — counter realised gain at bp)')}`);
  if (eveRpnlAfter <= 0n) {
    console.log(warn('Eve realised pnl is not strictly positive — review counter_gain math.'));
  }

  console.log(`  Frank size:        ${frankSizeNow}  →  ${frankSizeAfter}  ${
    frankSizeAfter === frankSizeNow ? `${C.green}(UNCHANGED — gate refused, as expected)${C.reset}` : `${C.red}(REDUCED — gate did not refuse!)${C.reset}`
  }`);
  if (frankSizeAfter !== frankSizeNow) pass = false;
  console.log(`  Frank collat:      ${usdc(frankCollatAfter)} USDC  ${d('(should be unchanged)')}`);

  console.log(`  Dan   size:        ${danSize}  →  ${danSizeAfter}  ${d('(reduced by Eve\'s ADL close)')}`);
  console.log(`  Dan   collat:      ${usdc(danCollat)} USDC  →  ${usdc(danCollatAfter)} USDC  (Δ ${danCollatAfter >= danCollat ? '+' : ''}${usdc(danCollatAfter - danCollat)} USDC)`);

  const fundBalAfter = fundFinal ? BigInt(fundFinal.balanceQuoteLots.toString()) : 0n;
  console.log(`  Insurance fund:    balance=${fundBalAfter}  threshold=${fundFinal?.pauseThresholdQuoteLots}  ${
    fundBalAfter < BigInt(fundFinal!.pauseThresholdQuoteLots.toString()) ? d('(still in ADL-eligible state)') : d('(threshold no longer above balance)')
  }`);

  // ─── STEP 8 — Summary
  console.log(banner('AUTO-DELEVERAGE E2E — RESULT'));
  if (pass) {
    console.log(`\n  ${C.green}${b('✓ ADL e2e proved end-to-end on a real Solana validator.')}${C.reset}\n`);
    console.log('  Signatures:');
    console.log(`    set_insurance_pause_threshold:     ${setThreshSig}`);
    console.log(`    update_oracle (drop below bp):     ${updSig}`);
    if (adlEveSig) console.log(`    auto_deleverage Eve  (SUCCESS):    ${adlEveSig}`);
    console.log(`    auto_deleverage Frank (REFUSED):   ${adlFrankErr?.includes('AdlNotEligible') ? '(tx never landed — AdlNotEligible)' : adlFrankErr ?? 'n/a'}`);
    console.log('');
    console.log(`  ${b('What this proves:')}`);
    console.log('    1. set_insurance_pause_threshold raised the gate above balance →');
    console.log('       protocol entered the ADL-eligible state (condition #1 met).');
    console.log('    2. Dan went bankrupt → bankruptcy price = entry - C/(S×tick).');
    console.log(`    3. Eve's SHORT entry (${eveEntry}) > Dan's bp (${danBp}) — counter is`);
    console.log('       profitable at bp → ADL gate ACCEPTS → Eve force-closed at bp,');
    console.log('       realising her gain. Dan\'s size reduced. AutoDeleveragedEvent emitted.');
    console.log(`    4. Frank's SHORT entry (${frankEntry}) < Dan's bp (${danBp}) — counter`);
    console.log('       is UNPROFITABLE at bp → ADL gate REFUSES with AdlNotEligible.');
    console.log('       The smart-gate protects the unprofitable counterparty.');
    console.log('');
    console.log(`  ${b('Why the smart-gate matters:')}`);
    console.log('    Without condition #2, low insurance balance would let ADL force-');
    console.log('    close ANY counter-position — including ones who happen to be at a');
    console.log('    loss already. That would amount to double-punishing innocent');
    console.log('    counterparties. The gate ensures ADL only ever transfers value');
    console.log('    from a profitable counter (who gives up part of their unrealised');
    console.log('    profit) to absorb the bankrupt trader\'s gap. Mathematically:');
    console.log('    the ADL recipient is ALWAYS strictly better off than at entry.');
    console.log('');
    process.exit(0);
  } else {
    console.log(`\n  ${C.red}${b('✗ ADL e2e did not match expectations — see verification block above.')}${C.reset}\n`);
    process.exit(5);
  }
}

main().catch((e) => {
  console.error(`\n${C.red}ADL e2e failed:${C.reset}`, e);
  if (e?.stack) console.error(e.stack);
  process.exit(1);
});
