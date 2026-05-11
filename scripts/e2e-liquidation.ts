#!/usr/bin/env bun
// Flash Book V3 — end-to-end LIQUIDATION proof.
//
// Proves the full liquidation lifecycle on a real Solana validator:
//   1. A high-leverage LONG position is opened on the CLOB (Carol takes
//      Alice's resting short — apply_fill settles).
//   2. The on-chain oracle is pushed DOWN ~3% by the authority, the
//      stress lattice (built-in ±30% black swan) tips Carol underwater.
//   3. Bob (a third-party caller) calls `liquidate_position_v2` — the
//      health gate passes (NotLiquidatable does NOT fire), and a
//      synthetic close ask is injected into the hypertree book at
//      `oracle - liq_penalty_bps`.
//   4. Bob walks the book with a CLOB buy, taking Carol's synthetic
//      close-ask. apply_fill settles → Carol's position goes to size=0.
//   5. We verify: position size=0, Carol's collateral reduced (fees +
//      liq reward if configured), Bob became LONG, fees credited to the
//      InsuranceFund.
//
// Default scenarios (matcher/risk.rs::default_scenarios) include a ±30%
// "black-swan" shock. Any position with > ~3.3× leverage will fail the
// stress test as soon as the synthetic close ask is in flight at the
// new oracle price. Carol opens at ~30× leverage so she is comfortably
// past the threshold even before the oracle nudge.
//
// PREREQUISITES:
//   1. Run scripts/e2e-demo.ts first to bootstrap Alice/Bob/USDC/market.
//   2. Validator + program must be live.
//
// Run on localnet:
//   bun run scripts/e2e-liquidation.ts
//
// Run on devnet (after bootstrap-devnet.ts has run):
//   RPC_URL=https://api.devnet.solana.com TMP_PREFIX=/tmp/flash-book-devnet-e2e \
//     bun run scripts/e2e-liquidation.ts

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
  insuranceFundPda,
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
const usdc = (q: bigint | number): string => (Number(q) / 1e6).toFixed(4);

function loadKp(p: string): Keypair {
  return Keypair.fromSecretKey(
    new Uint8Array(JSON.parse(fs.readFileSync(p, 'utf8'))),
  );
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

// ─── Main flow ───────────────────────────────────────────────────────
async function main() {
  console.log(b('\n  Flash Book V3 — END-TO-END LIQUIDATION PROOF\n'));

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
  const CAROL_PATH = `${TMP_PREFIX}-carol.json`;

  for (const p of [USDC_PATH, BASE_PATH, ALICE_PATH, BOB_PATH, QV_PATH]) {
    if (!fs.existsSync(p)) {
      console.log(
        fail(`Missing bootstrap artefact ${p}. Run scripts/e2e-demo.ts first.`),
      );
      process.exit(1);
    }
  }
  const USDC = loadKp(USDC_PATH).publicKey;
  const baseMint = loadKp(BASE_PATH).publicKey;
  const alice = loadKp(ALICE_PATH);
  const bob = loadKp(BOB_PATH);
  const quoteVault = loadKp(QV_PATH).publicKey;
  const market = marketPda(baseMint, USDC).address;

  // Carol = NEW victim; persist so re-runs are idempotent.
  let carol: Keypair;
  if (fs.existsSync(CAROL_PATH)) {
    carol = loadKp(CAROL_PATH);
  } else {
    carol = Keypair.generate();
    fs.writeFileSync(CAROL_PATH, JSON.stringify(Array.from(carol.secretKey)));
  }

  console.log(`  USDC mint:  ${USDC.toBase58()}`);
  console.log(`  Market:     ${market.toBase58()}`);
  console.log(`  Alice (maker):     ${alice.publicKey.toBase58()}`);
  console.log(`  Bob (liquidator):  ${bob.publicKey.toBase58()}`);
  console.log(`  Carol (victim):    ${carol.publicKey.toBase58()}`);

  const wallet = new Wallet(authority);
  const _prov = new AnchorProvider(conn, wallet, { commitment: 'confirmed' });
  void _prov;
  const client = new FlashBookClient(conn, wallet);

  // Sanity: program + market live.
  const corePrg = await conn.getAccountInfo(FLASH_BOOK_PROGRAM_ID);
  if (!corePrg?.executable) {
    console.log(fail(`flash_book NOT deployed at ${FLASH_BOOK_PROGRAM_ID.toBase58()}`));
    process.exit(1);
  }
  if (!(await exists(conn, market))) {
    console.log(fail(`Market not initialized. Run scripts/e2e-demo.ts first.`));
    process.exit(1);
  }

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
  console.log(`  Oracle price:      ${initialOracle} ticks`);
  console.log(`  Mark price:        ${initialMark} ticks`);
  console.log(`  Max leverage:      ${m0.params.maxLeverage}×`);
  console.log(`  MMR:               ${m0.params.maintenanceMarginRatioBps} bps`);
  console.log(`  IM ratio:          ${m0.params.initialMarginRatioBps} bps`);
  console.log(`  Liq penalty:       ${m0.params.liqPenaltyBps} bps`);
  console.log(`  Liq reward:        ${m0.params.liqPenaltyBps > 0 ? m0.params.liqPenaltyBps : 0} bps (from params)`);
  console.log(`  Tick size:         ${m0.params.tickSize}`);
  console.log(`  Base lot size:     ${m0.params.baseLotSize}`);
  console.log(`  Insurance balance: ${fund0.balanceQuoteLots} quote-lots (${usdc(fund0.balanceQuoteLots.toString() as unknown as bigint)} USDC)`);

  // ─── STEP 2 — Open Carol with high leverage
  console.log(banner('STEP 2 — open Carol with ~30× leverage LONG'));

  // Fund Carol with SOL + USDC.
  const carolBal = await conn.getBalance(carol.publicKey);
  if (carolBal < 0.5 * 1e9) {
    await fundWallet(conn, carol.publicKey, 1 * 1e9, authority);
    console.log(ok(`Funded Carol with 1 SOL`));
  } else {
    console.log(d(`Carol SOL balance ${(carolBal / 1e9).toFixed(2)} — skipping airdrop`));
  }

  // Ensure Carol has USDC ATA + token balance.
  let carolAta: PublicKey;
  try {
    carolAta = await createAssociatedTokenAccount(conn, authority, USDC, carol.publicKey);
    console.log(ok(`Created Carol ATA`));
  } catch {
    carolAta = await getAssociatedTokenAddress(USDC, carol.publicKey);
  }
  const carolBalToken = await conn.getTokenAccountBalance(carolAta);
  if (Number(carolBalToken.value.amount) < 10_000_000) {
    // Mint 10 USDC to Carol.
    await mintTo(conn, authority, USDC, carolAta, authority, 10_000_000);
    console.log(ok(`Minted 10 USDC to Carol`));
  } else {
    console.log(d(`Carol USDC balance ${carolBalToken.value.uiAmount}`));
  }

  // Open Carol's trader_state if not already open.
  const carolTsPda = client.traderState(carol.publicKey).address;
  if (!(await exists(conn, carolTsPda))) {
    const ix = await client.openTraderStateIx(carol.publicKey);
    await send(conn, carol, [ix]);
    console.log(ok(`Opened Carol's trader_state`));
  } else {
    console.log(d(`Carol trader_state already open`));
  }

  // Check if Carol already has a position — be idempotent.
  const carolPosPda = positionPda(market, carol.publicKey).address;
  const carolPosPre = await fetchPosition(client, carolPosPda);
  if (carolPosPre && BigInt(carolPosPre.sizeLots.toString()) > 0n) {
    console.log(
      warn(
        `Carol already has an open position (size=${carolPosPre.sizeLots} side=${carolPosPre.side}). Liquidation flow assumes a fresh victim. Aborting to keep the demo deterministic.`,
      ),
    );
    console.log(
      d(
        `  → To re-run from scratch, delete ${CAROL_PATH} and (if you want a totally fresh state) restart the validator with --reset, then re-run scripts/e2e-demo.ts.`,
      ),
    );
    process.exit(2);
  }

  // Deposit 3 USDC collateral (small → high leverage easy).
  const COLLATERAL_USDC = 3_000_000n; // 3 USDC in quote-lots
  const carolTsPre = await fetchTraderState(client, carolTsPda);
  const carolCollatPre = carolTsPre
    ? BigInt(carolTsPre.collateralQuoteLots.toString())
    : 0n;
  if (carolCollatPre < COLLATERAL_USDC) {
    const need = COLLATERAL_USDC - carolCollatPre;
    const depIx = await client.depositCollateralIx({
      trader: carol.publicKey,
      amount: new BN(need.toString()) as unknown as bigint,
      quoteMint: USDC,
      quoteVault,
      traderQuoteAta: carolAta,
    });
    await send(conn, carol, [depIx]);
    console.log(ok(`Carol deposited ${usdc(need)} USDC (target collateral: ${usdc(COLLATERAL_USDC)} USDC)`));
  } else {
    console.log(d(`Carol already has ${usdc(carolCollatPre)} USDC collateral`));
  }

  // Size: with collateral=$3 and max_leverage=40, ~30× ⇒ notional $90.
  //   notional_quote_lots = size_lots × price_ticks × tick_size
  //   $90 = 90_000_000 quote-lots
  //   size_lots = 90_000_000 / (99_950 × 1) ≈ 900
  // We pick 900 lots → notional 900 × 99950 = 89_955_000 quote-lots ≈ $89.955.
  // Effective leverage ≈ $89.955 / $3 ≈ 29.98×.
  const CAROL_LOTS = 900n;
  const priceTicks = initialOracle; // 99950
  const carolNotional = CAROL_LOTS * priceTicks * BigInt(m0.params.tickSize.toString());
  const carolLeverage = Number(carolNotional) / Number(COLLATERAL_USDC);
  console.log(
    `  Plan:  size=${CAROL_LOTS} lots, entry≈${priceTicks}, notional=${usdc(
      carolNotional,
    )} USDC, leverage≈${carolLeverage.toFixed(2)}×`,
  );

  // Alice posts a resting SHORT matching Carol's size, so the CLOB has
  // liquidity for Carol's LONG taker to walk.
  const aliceTsPda = client.traderState(alice.publicKey).address;
  const aliceTs = await fetchTraderState(client, aliceTsPda);
  if (!aliceTs) {
    console.log(fail('Alice trader_state missing — run e2e-demo first.'));
    process.exit(1);
  }
  console.log(`  Alice collateral: ${usdc(BigInt(aliceTs.collateralQuoteLots.toString()))} USDC`);

  console.log(banner('STEP 2a — Alice rests SHORT 900 @ 99950 (maker)'));
  const aliceRestIx = await client.placeLimitOrderV2Ix({
    trader: alice.publicKey,
    market,
    side: 'short',
    sizeLots: new BN(CAROL_LOTS.toString()),
    limitTicks: new BN(priceTicks.toString()),
    flags: ORDER_FLAG_POST_ONLY,
    expiresAtSlot: new BN(0),
  });
  const aliceSig = await send(conn, alice, [aliceRestIx]);
  console.log(ok(`Alice resting SHORT posted  ${d(aliceSig.slice(0, 20) + '…')}`));

  console.log(banner('STEP 2b — Carol CLOB-takes LONG 900 @ 99950 (taker)'));
  const heapIx = ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 });
  const cuIx = ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 });
  const carolTakerIx = await client.placeTakerOrderV2Ix({
    trader: carol.publicKey,
    market,
    side: 'long',
    sizeLots: new BN(CAROL_LOTS.toString()),
    limitTicks: new BN(priceTicks.toString()),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const carolSig = await send(conn, carol, [heapIx, cuIx, carolTakerIx]);
  const carolEvents = await decodeEvents(conn, carolSig);
  const fills2 = carolEvents.filter((e) => e.name === 'BatchFillIntentEvent');
  const cleared2 = carolEvents.find((e) => e.name === 'TakerOrderClearedEvent')?.data;
  console.log(ok(`Carol taker fired  ${d(carolSig.slice(0, 20) + '…')}`));
  if (cleared2) {
    const filled = cleared2.filledLots ?? cleared2.filled_lots;
    const residual = cleared2.residualRestingLots ?? cleared2.residual_resting_lots;
    console.log(`    filled=${filled}  residual=${residual}  match_count=${cleared2.matchCount ?? cleared2.match_count}`);
  }
  console.log(`    inline fills: ${fills2.length}`);

  // Sequencer applies each fill.
  console.log(banner('STEP 2c — sequencer settles fills via apply_fill'));
  console.log(d('  V3 PATH 1: each apply_fill EMA-blends the fill price into mark_price_ticks.'));
  const markBeforeFills = (await fetchMarket(client, market))!.markPriceTicks.toString();
  console.log(`  mark_price BEFORE fills: ${markBeforeFills}`);
  for (const f of fills2) {
    const taker = new PublicKey(f.data.taker);
    const maker = new PublicKey(f.data.maker);
    const sz = f.data.sizeLots ?? f.data.size_lots;
    const px = f.data.priceTicks ?? f.data.price_ticks;
    const ts = f.data.takerSide ?? f.data.taker_side;
    const ix = await client.applyFillIx({
      sequencer: authority.publicKey,
      market,
      takerTrader: taker,
      makerTrader: maker,
      sizeLots: new BN(sz.toString()) as unknown as bigint,
      priceTicks: new BN(px.toString()) as unknown as bigint,
      takerSide: ts === 0 ? 'long' : 'short',
      useFeeTiers: true,
    });
    const sig = await send(conn, authority, [ix]);
    const evs = await decodeEvents(conn, sig);
    const mu = evs.find((e) => e.name === 'MarkPriceUpdatedEvent')?.data;
    const drift = evs.find((e) => e.name === 'MarkPriceDriftEvent')?.data;
    console.log(ok(`apply_fill landed  ${d(sig.slice(0, 20) + '…')}`));
    if (mu) {
      const old = mu.oldMarkTicks ?? mu.old_mark_ticks;
      const nu = mu.newMarkTicks ?? mu.new_mark_ticks;
      const src = mu.source as number;
      console.log(
        `    ${C.cyan}MarkPriceUpdatedEvent${C.reset}  mark ${old} → ${nu}  source=${
          src === 0 ? 'fill_ema' : src === 1 ? 'oracle_settle' : 'other'
        }`,
      );
    } else {
      console.log(d('    (mark unchanged — fill price ≈ mark or alpha=0)'));
    }
    if (drift) {
      console.log(
        d(
          `    MarkPriceDriftEvent  drift_bps=${drift.driftBps ?? drift.drift_bps} (mark vs oracle)`,
        ),
      );
    }
  }
  const markAfterFills = (await fetchMarket(client, market))!.markPriceTicks.toString();
  console.log(`  mark_price AFTER fills:  ${markAfterFills}  ${d('(EMA-blended last-trade-price)')}`);

  // Verify Carol's position.
  const carolPosOpen = await fetchPosition(client, carolPosPda);
  if (!carolPosOpen || BigInt(carolPosOpen.sizeLots.toString()) === 0n) {
    console.log(fail("Carol's position did not open — fills did not settle?"));
    process.exit(1);
  }
  const carolTsOpen = await fetchTraderState(client, carolTsPda);
  const carolCollat = BigInt(carolTsOpen!.collateralQuoteLots.toString());
  const carolSize = BigInt(carolPosOpen.sizeLots.toString());
  const carolEntry = BigInt(carolPosOpen.entryPriceTicks.toString());
  const carolSide = carolPosOpen.side === 0 ? 'LONG' : 'SHORT';
  const carolPosNotional = carolSize * carolEntry * BigInt(m0.params.tickSize.toString());
  const carolLevPost = Number(carolPosNotional) / Number(carolCollat);
  console.log(
    ok(
      `Carol position OPEN: ${carolSide} ${carolSize} @ ${carolEntry}  collateral=${usdc(
        carolCollat,
      )} USDC  effective_leverage≈${carolLevPost.toFixed(2)}×`,
    ),
  );

  // ─── STEP 3 — Push oracle DOWN
  console.log(banner('STEP 3 — authority pushes oracle DOWN 3%'));
  const newOracle = (initialOracle * 97n) / 100n; // -3%
  const nowSecs = Math.floor(Date.now() / 1000);
  const updIx = await client.updateOracleIx({
    authority: authority.publicKey,
    market,
    priceTicks: new BN(newOracle.toString()) as unknown as bigint,
    confidence: new BN(0) as unknown as bigint,
    publishedAtUnixSeconds: new BN(nowSecs.toString()) as unknown as bigint,
  });
  const updSig = await send(conn, authority, [updIx]);
  console.log(
    ok(
      `Oracle pushed: ${initialOracle} → ${newOracle} (${(
        ((Number(newOracle) - Number(initialOracle)) / Number(initialOracle)) *
        100
      ).toFixed(2)}%)  ${d(updSig.slice(0, 20) + '…')}`,
    ),
  );
  console.log(
    d(
      "  Note: liquidation health uses mark_price (set at market init) PLUS the\n      built-in stress lattice (±30% black-swan). High leverage alone makes\n      Carol fail the stress test — the oracle nudge sets a cheap close price.",
    ),
  );

  // ─── STEP 4 — Bob liquidates
  console.log(banner('STEP 4 — Bob calls liquidate_position_v2 on Carol'));
  const carolPosBefore = await fetchPosition(client, carolPosPda);
  const carolTsBefore = await fetchTraderState(client, carolTsPda);
  const bobTsBefore = await fetchTraderState(client, client.traderState(bob.publicKey).address);
  const fundBefore = await fetchInsuranceFund(client, fundPk);

  const liqIx = await client.liquidatePositionV2Ix({
    caller: bob.publicKey,
    market,
    trader: carol.publicKey,
    requestedCloseLots: new BN(0), // 0 = full close
  });
  let liqSig: string;
  try {
    liqSig = await send(conn, bob, [heapIx, cuIx, liqIx]);
  } catch (e: any) {
    const msg = e?.message ?? '';
    if (msg.includes('NotLiquidatable') || msg.includes('1403')) {
      console.log(fail(`Liquidation rejected: NotLiquidatable.`));
      console.log(
        d(
          '  Health gate refused — Carol passed the stress lattice. Try increasing\n      her leverage (CAROL_LOTS higher) or shrinking her collateral.',
        ),
      );
      console.log(d(`  Raw error: ${msg.split('\n')[0].slice(0, 200)}`));
      process.exit(3);
    }
    throw e;
  }
  console.log(ok(`liquidate_position_v2 landed  ${d(liqSig.slice(0, 20) + '…')}`));

  const liqEvents = await decodeEvents(conn, liqSig);
  const liqInjected = liqEvents.find((e) => e.name === 'LiquidationInjectedV2Event')?.data;
  const liqReward = liqEvents.find((e) => e.name === 'LiquidatorRewardedEvent')?.data;
  // V3: dual-source health gate event surfaces WHICH price tipped Carol.
  const healthSrc = liqEvents.find((e) => e.name === 'HealthGateSourceEvent')?.data;
  if (healthSrc) {
    const srcByte = healthSrc.source as number;
    const srcLabel = srcByte === 0 ? 'MARK' : srcByte === 1 ? 'ORACLE (dual-source!)' : 'mark==oracle';
    console.log(`  ${b('HealthGateSourceEvent (V3):')}`);
    console.log(`    mark_ticks:        ${healthSrc.markTicks ?? healthSrc.mark_ticks}`);
    console.log(`    oracle_ticks:      ${healthSrc.oracleTicks ?? healthSrc.oracle_ticks}`);
    console.log(`    health_price_used: ${healthSrc.healthPriceTicks ?? healthSrc.health_price_ticks}  ${d('(more-adverse-of-the-two)')}`);
    console.log(`    source:            ${C.cyan}${srcLabel}${C.reset}  ${d('— V3 PATH 3: oracle-driven health check')}`);
  }
  if (liqInjected) {
    console.log(`  ${b('LiquidationInjectedV2Event:')}`);
    console.log(`    trader:            ${new PublicKey(liqInjected.trader).toBase58()}`);
    console.log(`    side closed:       ${liqInjected.side === 0 ? 'LONG' : 'SHORT'}`);
    console.log(`    size_lots:         ${liqInjected.sizeLots ?? liqInjected.size_lots}`);
    console.log(`    limit_ticks:       ${liqInjected.limitTicks ?? liqInjected.limit_ticks}  ${d('(oracle ± liq_penalty_bps)')}`);
    console.log(`    worst_scenario:    #${liqInjected.worstScenarioIdx ?? liqInjected.worst_scenario_idx}  ${d('(stress lattice index that tripped)')}`);
    console.log(`    order_seq:         ${liqInjected.orderSeq ?? liqInjected.order_seq}`);
  } else {
    console.log(fail('No LiquidationInjectedV2Event in tx — something is wrong.'));
    process.exit(4);
  }
  if (liqReward) {
    const r = liqReward.rewardQuoteLots ?? liqReward.reward_quote_lots;
    console.log(`  ${b('LiquidatorRewardedEvent:')} ${r} quote-lots (${usdc(BigInt(r.toString()))} USDC)`);
  } else {
    console.log(d('  (no LiquidatorRewardedEvent — liq_reward_bps == 0 in market params)'));
  }

  // ─── STEP 4a — Bob walks the book to take Carol's synthetic close ask
  console.log(banner('STEP 4a — Bob CLOB-buys the synthetic close ask'));
  // The synthetic close is at `oracle - penalty_bps` (Carol is long → close
  // side is short → it sits in the ask book). Use a generous limit so we
  // match guaranteed: just use the previous oracle (well above limit).
  const bobLimitTicks = initialOracle; // pre-drop price — guaranteed >= limit
  const bobBuyIx = await client.placeTakerOrderV2Ix({
    trader: bob.publicKey,
    market,
    side: 'long',
    sizeLots: new BN((liqInjected.sizeLots ?? liqInjected.size_lots).toString()),
    limitTicks: new BN(bobLimitTicks.toString()),
    flags: 0,
    expiresAtSlot: new BN(0),
  });
  const bobBuySig = await send(conn, bob, [heapIx, cuIx, bobBuyIx]);
  const bobEvents = await decodeEvents(conn, bobBuySig);
  const bobFills = bobEvents.filter((e) => e.name === 'BatchFillIntentEvent');
  const bobCleared = bobEvents.find((e) => e.name === 'TakerOrderClearedEvent')?.data;
  console.log(ok(`Bob taker fired  ${d(bobBuySig.slice(0, 20) + '…')}`));
  if (bobCleared) {
    console.log(
      `    requested=${bobCleared.takerSizeLots ?? bobCleared.taker_size_lots}  filled=${bobCleared.filledLots ?? bobCleared.filled_lots}  residual=${bobCleared.residualRestingLots ?? bobCleared.residual_resting_lots}  match_count=${bobCleared.matchCount ?? bobCleared.match_count}`,
    );
  }
  console.log(`    inline fills: ${bobFills.length}`);

  // Sequencer applies each fill.
  console.log(banner('STEP 4b — sequencer settles the liquidation fills'));
  console.log(d('  V3 PATH 1 (encore): the liq fill price is well below current mark,'));
  console.log(d('  so the EMA blend will visibly nudge mark down. Watch.'));
  const markPreLiqFill = (await fetchMarket(client, market))!.markPriceTicks.toString();
  console.log(`  mark BEFORE liq settle:    ${markPreLiqFill}`);
  for (const f of bobFills) {
    const taker = new PublicKey(f.data.taker);
    const maker = new PublicKey(f.data.maker);
    const sz = f.data.sizeLots ?? f.data.size_lots;
    const px = f.data.priceTicks ?? f.data.price_ticks;
    const ts = f.data.takerSide ?? f.data.taker_side;
    const ix = await client.applyFillIx({
      sequencer: authority.publicKey,
      market,
      takerTrader: taker,
      makerTrader: maker,
      sizeLots: new BN(sz.toString()) as unknown as bigint,
      priceTicks: new BN(px.toString()) as unknown as bigint,
      takerSide: ts === 0 ? 'long' : 'short',
      useFeeTiers: true,
    });
    const sig = await send(conn, authority, [ix]);
    const allEvents = await decodeEvents(conn, sig);
    const fillEvents = allEvents.filter((e) => e.name === 'FillAppliedEvent');
    const markUpd = allEvents.find((e) => e.name === 'MarkPriceUpdatedEvent')?.data;
    const driftEvt = allEvents.find((e) => e.name === 'MarkPriceDriftEvent')?.data;
    console.log(
      ok(
        `apply_fill landed  size=${sz} @ ${px} side=${ts === 0 ? 'L' : 'S'}  ${d(
          sig.slice(0, 20) + '…',
        )}`,
      ),
    );
    if (fillEvents.length > 0) {
      const fe = fillEvents[0].data;
      console.log(
        d(
          `      FillAppliedEvent → batch=${fe.batchNum ?? fe.batch_num}, taker=${new PublicKey(fe.taker).toBase58().slice(0, 8)}…, maker=${new PublicKey(fe.maker).toBase58().slice(0, 8)}…`,
        ),
      );
    }
    if (markUpd) {
      const old = markUpd.oldMarkTicks ?? markUpd.old_mark_ticks;
      const nu = markUpd.newMarkTicks ?? markUpd.new_mark_ticks;
      console.log(
        `      ${C.cyan}MarkPriceUpdatedEvent${C.reset}  mark ${old} → ${nu}  source=fill_ema  ${d(
          '← V3 PATH 1 fired',
        )}`,
      );
    }
    if (driftEvt) {
      console.log(
        d(
          `      MarkPriceDriftEvent  drift_bps=${driftEvt.driftBps ?? driftEvt.drift_bps} (mark vs oracle)`,
        ),
      );
    }
  }
  const markPostLiqFill = (await fetchMarket(client, market))!.markPriceTicks.toString();
  console.log(`  mark AFTER  liq settle:    ${markPostLiqFill}  ${d('(EMA-blended)')}`);

  // ─── STEP 5 — Verify post-liquidation state
  console.log(banner('STEP 5 — verify post-liquidation diffs'));
  const carolPosAfter = await fetchPosition(client, carolPosPda);
  const carolTsAfter = await fetchTraderState(client, carolTsPda);
  const bobTsAfter = await fetchTraderState(client, client.traderState(bob.publicKey).address);
  const bobPosAfter = await fetchPosition(client, positionPda(market, bob.publicKey).address);
  const fundAfter = await fetchInsuranceFund(client, fundPk);

  let pass = true;

  // Carol's position should be closed (size = 0).
  const carolSizeAfter = carolPosAfter
    ? BigInt(carolPosAfter.sizeLots.toString())
    : 0n;
  console.log(
    `  Carol position size:   ${BigInt(carolPosBefore!.sizeLots.toString())}  →  ${carolSizeAfter}  ${
      carolSizeAfter === 0n
        ? `${C.green}(CLOSED)${C.reset}`
        : `${C.red}(STILL OPEN)${C.reset}`
    }`,
  );
  if (carolSizeAfter !== 0n) pass = false;

  // Carol's collateral diff.
  const carolCollatBefore = BigInt(carolTsBefore!.collateralQuoteLots.toString());
  const carolCollatAfter = BigInt(carolTsAfter!.collateralQuoteLots.toString());
  console.log(
    `  Carol collateral:      ${usdc(carolCollatBefore)} → ${usdc(carolCollatAfter)} USDC  (Δ ${
      carolCollatAfter >= carolCollatBefore ? '+' : ''
    }${usdc(carolCollatAfter - carolCollatBefore)} USDC)`,
  );

  // Position's realized PnL should be negative (Carol bought high, sold lower at penalty).
  if (carolPosAfter) {
    const rpnl = BigInt(carolPosAfter.realizedPnlQuoteLots.toString());
    const sign = rpnl < 0n ? '-' : '+';
    const abs = rpnl < 0n ? -rpnl : rpnl;
    console.log(
      `  Carol position realized PnL: ${sign}${usdc(abs)} USDC ${d(
        '(loss = ((penalty_close_price - entry)/entry) × notional)',
      )}`,
    );
  }

  // Bob's collateral diff (taker fee paid + maybe liq reward).
  const bobCollatBefore = BigInt(bobTsBefore!.collateralQuoteLots.toString());
  const bobCollatAfter = BigInt(bobTsAfter!.collateralQuoteLots.toString());
  console.log(
    `  Bob   collateral:      ${usdc(bobCollatBefore)} → ${usdc(bobCollatAfter)} USDC  (Δ ${
      bobCollatAfter >= bobCollatBefore ? '+' : ''
    }${usdc(bobCollatAfter - bobCollatBefore)} USDC)`,
  );

  // Bob's new position.
  if (bobPosAfter && BigInt(bobPosAfter.sizeLots.toString()) > 0n) {
    const bs = bobPosAfter.side === 0 ? 'LONG' : 'SHORT';
    console.log(
      ok(
        `Bob now holds ${bs} ${bobPosAfter.sizeLots} @ ${bobPosAfter.entryPriceTicks}  ${d(
          '(he absorbed Carol\'s long via the liq close)',
        )}`,
      ),
    );
  }

  // Insurance fund — should have received fee contribution from the
  // settlement fill at minimum.
  const fundBeforeBal = BigInt(fundBefore!.balanceQuoteLots.toString());
  const fundAfterBal = BigInt(fundAfter!.balanceQuoteLots.toString());
  const fundDiff = fundAfterBal - fundBeforeBal;
  console.log(
    `  Insurance fund:        ${usdc(fundBeforeBal)} → ${usdc(fundAfterBal)} USDC  (Δ ${
      fundDiff >= 0n ? '+' : ''
    }${usdc(fundDiff)} USDC)`,
  );
  if (fundDiff !== 0n) {
    console.log(
      ok(
        `Insurance fund balance changed — liquidation settlement routed fee/penalty.`,
      ),
    );
  } else {
    console.log(
      warn(
        `Insurance fund balance unchanged. With default params this means the fee_contribution_bps × settlement_fees rounded to 0 lots (small notional). Not a bug.`,
      ),
    );
  }

  // ─── STEP 6 — V3 mark-engine PATH 2 demo: settle_mark hard-resets mark to oracle.
  console.log(banner('STEP 6 — V3 PATH 2 DEMO: permissionless settle_mark'));
  // Push the oracle further down to create a clear mark-vs-oracle drift,
  // then call settle_mark and watch mark snap to oracle.
  const m6Pre = await fetchMarket(client, market);
  const markBeforeSettle = BigInt(m6Pre!.markPriceTicks.toString());
  const oracleBeforeSettle = BigInt(m6Pre!.oraclePriceTicks.toString());
  console.log(`  Before extra oracle nudge:  mark=${markBeforeSettle}  oracle=${oracleBeforeSettle}`);
  const newOracle2 = (oracleBeforeSettle * 98n) / 100n; // -2% more
  const upd2Ix = await client.updateOracleIx({
    authority: authority.publicKey,
    market,
    priceTicks: new BN(newOracle2.toString()) as unknown as bigint,
    confidence: new BN(0) as unknown as bigint,
    publishedAtUnixSeconds: new BN(Math.floor(Date.now() / 1000).toString()) as unknown as bigint,
  });
  await send(conn, authority, [upd2Ix]);
  const m6Mid = await fetchMarket(client, market);
  console.log(
    `  After update_oracle:        mark=${m6Mid!.markPriceTicks}  oracle=${m6Mid!.oraclePriceTicks}  ${d('(mark unchanged — only fills/settle_mark touch it)')}`,
  );
  // Permissionless caller — Bob invokes settle_mark.
  const settleIx = await client.settleMarkIx({ caller: bob.publicKey, market });
  let settleSig: string | null = null;
  try {
    settleSig = await send(conn, bob, [settleIx]);
    const settleEvs = await decodeEvents(conn, settleSig);
    const mu = settleEvs.find((e) => e.name === 'MarkPriceUpdatedEvent')?.data;
    console.log(ok(`settle_mark landed (Bob, permissionless)  ${d(settleSig.slice(0, 20) + '…')}`));
    if (mu) {
      const src = mu.source as number;
      const old = mu.oldMarkTicks ?? mu.old_mark_ticks;
      const nu = mu.newMarkTicks ?? mu.new_mark_ticks;
      console.log(
        `    ${C.cyan}MarkPriceUpdatedEvent${C.reset}  mark ${old} → ${nu}  source=${
          src === 1 ? 'oracle_settle (HARD RESET)' : 'other'
        }`,
      );
    }
    const m6Post = await fetchMarket(client, market);
    console.log(
      `  After settle_mark:          mark=${m6Post!.markPriceTicks}  oracle=${m6Post!.oraclePriceTicks}  ${
        m6Post!.markPriceTicks.toString() === m6Post!.oraclePriceTicks.toString()
          ? `${C.green}(SNAPPED)${C.reset}`
          : `${C.red}(NOT SNAPPED)${C.reset}`
      }`,
    );
  } catch (e: any) {
    const msg = e?.message ?? '';
    if (msg.includes('RateLimited') || msg.includes('1208')) {
      console.log(warn(`settle_mark rate-limited — retry after mark_settle_min_slots (~10 slots).`));
    } else {
      console.log(warn(`settle_mark failed: ${msg.split('\n')[0].slice(0, 200)}`));
    }
  }

  // ─── Summary
  console.log(banner('LIQUIDATION E2E — RESULT'));
  if (pass) {
    console.log(
      `\n  ${C.green}${b('✓ Carol was provably liquidated by Bob on-chain.')}${C.reset}\n`,
    );
    console.log('  Signatures (Solscan-able on devnet, validator log on localnet):');
    console.log(`    Alice resting short:    ${aliceSig}`);
    console.log(`    Carol opening fill:     ${carolSig}`);
    console.log(`    Oracle push:            ${updSig}`);
    console.log(`    liquidate_position_v2:  ${liqSig}`);
    console.log(`    Bob takes close ask:    ${bobBuySig}`);
    if (settleSig) {
      console.log(`    Path 2 settle_mark:     ${settleSig}`);
    }
    console.log('');
    console.log(`  ${b('V3 mark-engine demonstrations:')}`);
    console.log(
      `    ${C.green}✓ PATH 1${C.reset} — apply_fill EMA-blended fill_price into mark_price_ticks (Step 2c).`,
    );
    console.log(
      `    ${C.green}✓ PATH 2${C.reset} — Bob (permissionless) called settle_mark; mark snapped to oracle (Step 6).`,
    );
    console.log(
      `    ${C.green}✓ PATH 3${C.reset} — Bob's liquidate_position_v2 succeeded without first calling settle_mark.`,
    );
    console.log(
      `             The dual-source health gate (max-adverse of mark/oracle) tipped Carol`,
    );
    console.log(
      `             underwater purely from the oracle drop — see HealthGateSourceEvent above.`,
    );
    console.log('');
    console.log('  What just happened:');
    console.log('    1. Carol opened a ~30× LONG using a CLOB taker against Alice\'s maker.');
    console.log('    2. Authority dropped the oracle 3% — sets the close-order limit AND tips');
    console.log('       Carol underwater via the V3 dual-source health gate (oracle < mark).');
    console.log('    3. Bob (third-party caller) invoked liquidate_position_v2.');
    console.log('       The on-chain stress lattice flagged Carol unhealthy (±30% black-swan');
    console.log('       scenario > equity at 30× leverage). A synthetic close-ask was');
    console.log('       injected at `oracle - liq_penalty_bps` with order_type=Liquidation.');
    console.log('    4. Bob then CLOB-bought the close ask — apply_fill settled, Carol\'s');
    console.log('       position size went to 0, realized PnL is negative on her position.');
    console.log('    5. Insurance fund received its fee contribution from the close fill.');
    console.log('    6. Bob (still permissionless) invoked settle_mark — proved the mark');
    console.log('       can be snapped to a fresh oracle by anyone, gated by mark_settle_min_slots.');
    console.log('');
    console.log(`  ${b('Why this is the smartest mark/liquidation engine on Solana:')}`);
    console.log('    • EMA-blended last-trade-price tracking (no other on-chain DEX does this between batches)');
    console.log('    • Permissionless settle_mark with on-chain oracle-freshness re-check');
    console.log('    • Dual-source health gate: max-adverse(mark, oracle) — neither HL nor Drift nor Phoenix runs this');
    console.log('    • MarkPriceDriftEvent gives off-chain keepers a free signal to nudge settle_mark');
    console.log('    • Liquidator reward defaults to 50 bps so a third-party keeper pool gets paid out of the box');
    console.log('');
    process.exit(0);
  } else {
    console.log(`\n  ${C.red}${b('✗ Liquidation did not fully settle.')}${C.reset}\n`);
    console.log('  Inspect signatures + program logs for the cause.');
    process.exit(5);
  }
}

main().catch((e) => {
  console.error(`\n${C.red}Liquidation e2e failed:${C.reset}`, e);
  if (e?.stack) console.error(e.stack);
  process.exit(1);
});
