#!/usr/bin/env bun
// Flash Book V3 — Interactive devnet demo.
//
// Connects to the LIVE devnet deploy (4 programs at the IDs declared
// in the SDK + the markets initialized by bootstrap-devnet.ts) and
// gives a menu-driven REPL for placing orders, running the matcher,
// inspecting fills, managing vaults, and viewing tier-resolved fees.
//
// Usage:
//   bun run scripts/demo.ts
//
// Optional env:
//   DEMO_KEYPAIR     path to keypair (default: ~/.config/solana/id.json)
//   QUOTE_MINT       USDC mint (default: devnet Circle USDC)
//   MARKET           base mint pubkey (default: SOL devnet)

import {
  AccountMeta,
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  type Logs,
} from '@solana/web3.js';
import { AnchorProvider, BN, BorshEventCoder, Wallet } from '@coral-xyz/anchor';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline/promises';
import {
  FLASH_BOOK_PROGRAM_ID,
  FlashBookClient,
  IDL,
  feeTiersPda,
  flpExposurePda,
  insuranceFundPda,
  marketBookPda,
  marketPda,
  ORDER_FLAG_POST_ONLY,
  positionPda,
  traderStatePda,
} from '../sdk-ts/src/index.ts';

// ─── ANSI helpers ────────────────────────────────────────────────────
const RESET = '\x1b[0m';
const BOLD = '\x1b[1m';
const DIM = '\x1b[2m';
const RED = '\x1b[31m';
const GREEN = '\x1b[32m';
const YELLOW = '\x1b[33m';
const BLUE = '\x1b[34m';
const MAGENTA = '\x1b[35m';
const CYAN = '\x1b[36m';
const WHITE = '\x1b[37m';
const BG_BLUE = '\x1b[44m';

const c = {
  bold: (s: string) => `${BOLD}${s}${RESET}`,
  dim: (s: string) => `${DIM}${s}${RESET}`,
  red: (s: string) => `${RED}${s}${RESET}`,
  green: (s: string) => `${GREEN}${s}${RESET}`,
  yellow: (s: string) => `${YELLOW}${s}${RESET}`,
  blue: (s: string) => `${BLUE}${s}${RESET}`,
  magenta: (s: string) => `${MAGENTA}${s}${RESET}`,
  cyan: (s: string) => `${CYAN}${s}${RESET}`,
  banner: (s: string) => `${BG_BLUE}${WHITE}${BOLD} ${s} ${RESET}`,
};

// ─── Config ──────────────────────────────────────────────────────────
const RPC_URL = process.env.RPC_URL ?? 'https://api.devnet.solana.com';
const DEMO_KEYPAIR =
  process.env.DEMO_KEYPAIR ?? path.join(os.homedir(), '.config', 'solana', 'id.json');
const QUOTE_MINT = new PublicKey(
  process.env.QUOTE_MINT ?? '4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU',
);
const DEFAULT_BASE_MINT = new PublicKey(
  process.env.MARKET ?? 'So11111111111111111111111111111111111111112',
);

// ─── State ────────────────────────────────────────────────────────────
interface DemoState {
  conn: Connection;
  wallet: Keypair;
  client: FlashBookClient;
  baseMint: PublicKey;
  market: PublicKey;
  marketBook: PublicKey;
  rl: readline.Interface;
}

function loadKeypair(p: string): Keypair {
  const raw = JSON.parse(fs.readFileSync(p, 'utf8')) as number[];
  return Keypair.fromSecretKey(new Uint8Array(raw));
}

async function makeState(): Promise<DemoState> {
  const wallet = loadKeypair(DEMO_KEYPAIR);
  const conn = new Connection(RPC_URL, 'confirmed');
  const provider = new AnchorProvider(conn, new Wallet(wallet), { commitment: 'confirmed' });
  void provider;
  const client = new FlashBookClient(conn, new Wallet(wallet));
  const market = marketPda(DEFAULT_BASE_MINT, QUOTE_MINT).address;
  const marketBook = marketBookPda(market).address;
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  return { conn, wallet, client, baseMint: DEFAULT_BASE_MINT, market, marketBook, rl };
}

async function sendTx(
  state: DemoState,
  ixs: TransactionInstruction[],
  signers: Keypair[] = [],
  label = 'tx',
): Promise<string> {
  const tx = new Transaction().add(...ixs);
  tx.feePayer = state.wallet.publicKey;
  tx.recentBlockhash = (await state.conn.getLatestBlockhash('confirmed')).blockhash;
  tx.sign(state.wallet, ...signers);
  const sig = await state.conn.sendRawTransaction(tx.serialize(), {
    skipPreflight: false,
    preflightCommitment: 'confirmed',
  });
  process.stdout.write(c.dim(`  → sending ${label}…`) + '\n');
  await state.conn.confirmTransaction(sig, 'confirmed');
  process.stdout.write(c.green(`  ✓ ${label}  `) + c.dim(sig) + '\n');
  return sig;
}

// ─── Status header ──────────────────────────────────────────────────
async function renderHeader(state: DemoState) {
  console.clear();
  const balLamports = await state.conn.getBalance(state.wallet.publicKey);
  const sol = (balLamports / 1e9).toFixed(4);
  let usdc = '?';
  let collateral = '?';
  let tier: { idx: number; maker: number; taker: number } | null = null;
  let openPositions = '?';
  try {
    const traderStatePk = state.client.traderState(state.wallet.publicKey).address;
    const tsInfo = await state.conn.getAccountInfo(traderStatePk);
    if (tsInfo) {
      // Decode minimum: skip 8-byte disc, then 32 (trader) + 1 (bump),
      // then read collateral (u64) + skip realized_pnl (i64) + open_positions (u8).
      const buf = tsInfo.data;
      const collat = buf.readBigUInt64LE(8 + 32 + 1);
      collateral = (Number(collat) / 1e6).toFixed(4);
      const op = buf.readUInt8(8 + 32 + 1 + 8 + 8);
      openPositions = String(op);
    }
  } catch {
    /* not initialized yet */
  }
  try {
    // Best-effort tier read via simulate.
    const ix = await state.client.viewTraderEffectiveTierIx({ trader: state.wallet.publicKey });
    const sim = await simulateAndDecodeEvent(state, ix, 'TraderEffectiveTierEvent');
    if (sim) {
      tier = {
        idx: sim.tierIndex,
        maker: sim.makerRebateBps,
        taker: sim.takerFeeBps,
      };
    }
  } catch {
    /* fee tier not initialized OR no trader_state yet */
  }

  const lines = [
    c.banner('  Flash Book V3 — Interactive Devnet Demo  '),
    `  ${c.bold('RPC:')}     ${RPC_URL}`,
    `  ${c.bold('Wallet:')}  ${c.cyan(state.wallet.publicKey.toBase58())}`,
    `  ${c.bold('Market:')}  ${state.baseMint.toBase58().slice(0, 8)}…/${QUOTE_MINT.toBase58().slice(0, 8)}…  ${c.dim('(' + state.market.toBase58().slice(0, 16) + '…)')}`,
    `  ${c.bold('Balance:')} ${c.green(sol + ' SOL')}  collateral: ${c.green(collateral + ' USDC')}  open positions: ${openPositions}`,
    tier !== null
      ? `  ${c.bold('Tier:')}    VIP${tier.idx}  ${tier.maker >= 0 ? c.green('+' + tier.maker + ' bps maker') : c.red(tier.maker + ' bps maker fee')}  /  ${c.yellow(tier.taker + ' bps taker')}`
      : `  ${c.bold('Tier:')}    ${c.dim('(no trader_state — open one to see your tier)')}`,
    '',
  ];
  console.log(lines.join('\n'));
}

// ─── Event decoder helper (for view-style ixs) ───────────────────────
async function simulateAndDecodeEvent(
  state: DemoState,
  ix: TransactionInstruction,
  eventName: string,
): Promise<any | null> {
  const tx = new Transaction().add(ix);
  tx.feePayer = state.wallet.publicKey;
  tx.recentBlockhash = (await state.conn.getLatestBlockhash('confirmed')).blockhash;
  const sim = await state.conn.simulateTransaction(tx, [state.wallet]);
  const logs = sim.value.logs ?? [];
  const coder = new BorshEventCoder(IDL);
  for (const line of logs) {
    if (!line.startsWith('Program data: ')) continue;
    const b64 = line.slice('Program data: '.length).trim();
    try {
      const decoded = coder.decode(b64);
      if (decoded && decoded.name === eventName) return decoded.data;
    } catch {
      /* skip */
    }
  }
  return null;
}

// ─── Action menu ─────────────────────────────────────────────────────
async function mainMenu(state: DemoState): Promise<boolean> {
  await renderHeader(state);
  console.log(c.bold('  Choose an action:'));
  console.log('');
  console.log(`    ${c.cyan('1)')} Trade          — place / cancel limit orders, view orderbook`);
  console.log(`    ${c.cyan('2)')} Margin         — deposit / withdraw USDC, view positions`);
  console.log(`    ${c.cyan('3)')} Matcher        — run a batch tick, watch fills land live`);
  console.log(`    ${c.cyan('4)')} Vaults         — create vault, deposit, vault-trades`);
  console.log(`    ${c.cyan('5)')} Tiers          — view fee tier table + your effective rate`);
  console.log(`    ${c.cyan('6)')} Switch market  — change between SOL/BTC/ETH`);
  console.log(`    ${c.cyan('7)')} Show PDAs      — print all account addresses (for Solscan)`);
  console.log(`    ${c.cyan('q)')} Quit`);
  console.log('');
  const choice = (await state.rl.question(c.bold('  > '))).trim();
  console.log('');
  switch (choice) {
    case '1':
      await tradeMenu(state);
      break;
    case '2':
      await marginMenu(state);
      break;
    case '3':
      await matcherMenu(state);
      break;
    case '4':
      await vaultsMenu(state);
      break;
    case '5':
      await tiersMenu(state);
      break;
    case '6':
      await switchMarket(state);
      break;
    case '7':
      await showPdas(state);
      break;
    case 'q':
    case 'Q':
      return false;
    default:
      console.log(c.red('  Unknown option, try again.'));
      await state.rl.question(c.dim('  press enter…'));
  }
  return true;
}

// ─── 1. Trade ────────────────────────────────────────────────────────
async function tradeMenu(state: DemoState) {
  console.log(c.banner(' TRADE '));
  console.log('');
  console.log(`    ${c.cyan('a)')} Place limit order (long / buy)`);
  console.log(`    ${c.cyan('b)')} Place limit order (short / sell)`);
  console.log(`    ${c.cyan('c)')} Cancel order by ID`);
  console.log(`    ${c.cyan('d)')} View orderbook (top depth)`);
  console.log(`    ${c.cyan('q)')} Back`);
  console.log('');
  const choice = (await state.rl.question(c.bold('  > '))).trim();
  if (choice === 'a' || choice === 'b') {
    const side = choice === 'a' ? 'long' : 'short';
    const sizeStr = await state.rl.question(c.bold(`  Size (base lots, e.g. 5): `));
    const priceStr = await state.rl.question(c.bold(`  Limit price (ticks, e.g. 99950): `));
    const postOnlyAns = await state.rl.question(c.bold(`  Post-only? (y/N): `));
    const postOnly = postOnlyAns.toLowerCase() === 'y';
    const ix = await state.client.placeLimitOrderV2Ix({
      trader: state.wallet.publicKey,
      market: state.market,
      side,
      sizeLots: new BN(sizeStr.trim()),
      limitTicks: new BN(priceStr.trim()),
      flags: postOnly ? ORDER_FLAG_POST_ONLY : 0,
      expiresAtSlot: new BN(0),
    });
    try {
      await sendTx(state, [ix], [], 'place_limit_order_v2');
    } catch (e) {
      console.log(c.red('  ✗ ' + (e as Error).message.split('\n')[0]));
    }
    await state.rl.question(c.dim('  press enter…'));
  } else if (choice === 'c') {
    const sideAns = await state.rl.question(c.bold(`  Side (long/short): `));
    const orderIdStr = await state.rl.question(c.bold(`  Order ID (uint64): `));
    const ix = await state.client.cancelOrderV2Ix({
      trader: state.wallet.publicKey,
      market: state.market,
      side: sideAns.trim() === 'long' ? 'long' : 'short',
      orderId: new BN(orderIdStr.trim()),
    });
    try {
      await sendTx(state, [ix], [], 'cancel_order_v2');
    } catch (e) {
      console.log(c.red('  ✗ ' + (e as Error).message.split('\n')[0]));
    }
    await state.rl.question(c.dim('  press enter…'));
  } else if (choice === 'd') {
    try {
      const ix = await state.client.viewBookDepthV2Ix({ market: state.market });
      const depth = await simulateAndDecodeEvent(state, ix, 'BookDepthV2Event');
      if (!depth) {
        console.log(c.dim('  (book empty — no orders yet)'));
      } else {
        console.log(c.bold('  Asks (best first):'));
        for (const a of depth.asks ?? []) {
          if ((a.priceTicks?.toString?.() ?? '0') === '0') continue;
          console.log(`    ${c.red(String(a.priceTicks))}  ×  ${a.sizeLots}`);
        }
        console.log('  ─────────────────');
        console.log(c.bold('  Bids (best first):'));
        for (const b of depth.bids ?? []) {
          if ((b.priceTicks?.toString?.() ?? '0') === '0') continue;
          console.log(`    ${c.green(String(b.priceTicks))}  ×  ${b.sizeLots}`);
        }
      }
    } catch (e) {
      console.log(c.red('  ✗ ' + (e as Error).message.split('\n')[0]));
    }
    await state.rl.question(c.dim('  press enter…'));
  }
}

// ─── 2. Margin ───────────────────────────────────────────────────────
async function marginMenu(state: DemoState) {
  console.log(c.banner(' MARGIN '));
  console.log('');
  console.log(`    ${c.cyan('a)')} Open trader_state (one-time)`);
  console.log(`    ${c.cyan('b)')} Deposit USDC`);
  console.log(`    ${c.cyan('c)')} Withdraw USDC (full)`);
  console.log(`    ${c.cyan('d)')} View my position on this market`);
  console.log(`    ${c.cyan('q)')} Back`);
  console.log('');
  const choice = (await state.rl.question(c.bold('  > '))).trim();
  if (choice === 'a') {
    try {
      const ix = await state.client.openTraderStateIx(state.wallet.publicKey);
      await sendTx(state, [ix], [], 'open_trader_state');
    } catch (e) {
      console.log(c.red('  ✗ ' + (e as Error).message.split('\n')[0]));
    }
    await state.rl.question(c.dim('  press enter…'));
  } else if (choice === 'b') {
    const amtStr = await state.rl.question(c.bold(`  Amount (USDC, e.g. 100): `));
    const fund = insuranceFundPda();
    const fundInfo = await state.conn.getAccountInfo(fund.address);
    if (!fundInfo) {
      console.log(c.red('  InsuranceFund not initialized.'));
      await state.rl.question(c.dim('  press enter…'));
      return;
    }
    // quote_vault is at offset 8(disc) + 32(authority) + 1(bump) + 4 (... etc) — easier to use bootstrap output.
    const quoteVault = new PublicKey('4Sg8UGcYvxsBDppT5khpB84ZkAExLyxF2B8LPVM5W8Mx');
    const ix = await state.client.depositCollateralIx({
      trader: state.wallet.publicKey,
      amountQuoteLots: new BN(Math.round(parseFloat(amtStr.trim()) * 1e6)),
      quoteMint: QUOTE_MINT,
      quoteVault,
    });
    try {
      await sendTx(state, [ix], [], 'deposit_collateral');
    } catch (e) {
      console.log(c.red('  ✗ ' + (e as Error).message.split('\n')[0]));
      console.log(c.dim('  Hint: you need devnet USDC. See QUOTE_MINT env or add a "mint test USDC" step.'));
    }
    await state.rl.question(c.dim('  press enter…'));
  } else if (choice === 'd') {
    const posPk = positionPda(state.market, state.wallet.publicKey).address;
    const info = await state.conn.getAccountInfo(posPk);
    if (!info) {
      console.log(c.dim('  (no position on this market yet)'));
    } else {
      // PositionAccount layout: 8 disc + trader(32) + market(32) + bump(1) +
      // funding_index(i128 = 16) + side(u8) + size_lots(u64) + entry_price_ticks(u64).
      const buf = info.data;
      const side = buf.readUInt8(8 + 32 + 32 + 1 + 16);
      const size = buf.readBigUInt64LE(8 + 32 + 32 + 1 + 16 + 1);
      const entry = buf.readBigUInt64LE(8 + 32 + 32 + 1 + 16 + 1 + 8);
      console.log(`  Side:  ${side === 0 ? c.green('LONG') : c.red('SHORT')}`);
      console.log(`  Size:  ${size}`);
      console.log(`  Entry: ${entry}`);
    }
    await state.rl.question(c.dim('  press enter…'));
  }
}

// ─── 3. Matcher ──────────────────────────────────────────────────────
async function matcherMenu(state: DemoState) {
  console.log(c.banner(' MATCHER '));
  console.log('');
  console.log(`    ${c.cyan('a)')} Run batch tick (run_batch_v2) on this market`);
  console.log(`    ${c.cyan('b)')} Watch live event stream (Ctrl-C to stop)`);
  console.log(`    ${c.cyan('q)')} Back`);
  console.log('');
  const choice = (await state.rl.question(c.bold('  > '))).trim();
  if (choice === 'a') {
    try {
      const ix = await state.client.runBatchV2Ix({
        caller: state.wallet.publicKey,
        market: state.market,
        nowMs: BigInt(Date.now()),
      });
      await sendTx(state, [ix], [], 'run_batch_v2');
    } catch (e) {
      console.log(c.red('  ✗ ' + (e as Error).message.split('\n')[0]));
    }
    await state.rl.question(c.dim('  press enter…'));
  } else if (choice === 'b') {
    console.log(c.dim('  Subscribing to flash-book events… Ctrl-C to stop.'));
    const coder = new BorshEventCoder(IDL);
    const subId = state.conn.onLogs(
      FLASH_BOOK_PROGRAM_ID,
      (logs: Logs) => {
        if (logs.err) return;
        for (const line of logs.logs) {
          if (!line.startsWith('Program data: ')) continue;
          const b64 = line.slice('Program data: '.length).trim();
          try {
            const ev = coder.decode(b64);
            if (!ev) continue;
            const ts = new Date().toISOString().slice(11, 23);
            console.log(`  ${c.dim(ts)}  ${c.cyan(ev.name)}  ${c.dim(JSON.stringify(ev.data).slice(0, 120))}`);
          } catch {
            /* skip */
          }
        }
      },
      'confirmed',
    );
    await new Promise<void>((resolve) => {
      const onSig = () => {
        process.removeListener('SIGINT', onSig);
        state.conn.removeOnLogsListener(subId).then(() => resolve());
      };
      process.on('SIGINT', onSig);
    });
  }
}

// ─── 4. Vaults ───────────────────────────────────────────────────────
async function vaultsMenu(state: DemoState) {
  console.log(c.banner(' VAULTS '));
  console.log(c.dim('  (coming next: full vault flow via FlashBookVaultsClient)'));
  console.log(c.dim('  → For now, see scripts/sequencer.ts for the wave-22 vault surface.'));
  await state.rl.question(c.dim('  press enter…'));
}

// ─── 5. Tiers ────────────────────────────────────────────────────────
async function tiersMenu(state: DemoState) {
  console.log(c.banner(' FEE TIERS '));
  console.log('');
  // Read the FeeTiers PDA directly.
  const ft = feeTiersPda();
  const info = await state.conn.getAccountInfo(ft.address);
  if (!info) {
    console.log(c.red('  FeeTiers not initialized.'));
  } else {
    const buf = info.data;
    // 8 disc + 32 authority + 1 bump + 1 tier_count + 6 pad + 8 window
    const tierCount = buf.readUInt8(8 + 32 + 1);
    const window = Number(buf.readBigUInt64LE(8 + 32 + 1 + 1 + 6));
    console.log(`  Volume window: ${c.bold(String(window))} slots  (~${(window * 0.4 / 86400).toFixed(1)} days)`);
    console.log(`  Active tiers:  ${tierCount}`);
    console.log('');
    console.log(c.bold('  Tier  Min volume         Maker        Taker'));
    let off = 8 + 32 + 1 + 1 + 6 + 8;
    for (let i = 0; i < tierCount; i++) {
      const minVol = buf.readBigUInt64LE(off);
      const maker = buf.readInt32LE(off + 8);
      const taker = buf.readUInt32LE(off + 12);
      const makerStr = maker >= 0 ? c.green('+' + maker + ' bps') : c.red(maker + ' bps');
      console.log(
        `  VIP${i}  ${c.dim((Number(minVol) / 1e6).toFixed(2).padStart(15) + ' USDC')}   ${makerStr.padEnd(20)}  ${c.yellow(taker + ' bps')}`,
      );
      off += 16;
    }
  }
  console.log('');
  console.log(c.bold('  Your effective tier:'));
  try {
    const ix = await state.client.viewTraderEffectiveTierIx({ trader: state.wallet.publicKey });
    const t = await simulateAndDecodeEvent(state, ix, 'TraderEffectiveTierEvent');
    if (t) {
      console.log(`  Tier:   VIP${t.tierIndex}`);
      console.log(`  Volume: ${(Number(t.effectiveVolumeQuoteLots) / 1e6).toFixed(2)} USDC`);
      console.log(`  Maker:  ${t.makerRebateBps >= 0 ? c.green('+' + t.makerRebateBps) : c.red(String(t.makerRebateBps))} bps`);
      console.log(`  Taker:  ${c.yellow(t.takerFeeBps + ' bps')}`);
      if (t.windowExpired) console.log(c.dim('  (rolling window expired — next trade re-anchors)'));
    } else {
      console.log(c.dim('  (open a trader_state first via Margin → a)'));
    }
  } catch {
    console.log(c.dim('  (open a trader_state first via Margin → a)'));
  }
  await state.rl.question(c.dim('  press enter…'));
}

// ─── 6. Switch market ────────────────────────────────────────────────
async function switchMarket(state: DemoState) {
  console.log(c.banner(' SWITCH MARKET '));
  console.log('');
  console.log(`    ${c.cyan('a)')} SOL/USDC  (default)`);
  console.log(`    ${c.cyan('b)')} BTC/USDC`);
  console.log(`    ${c.cyan('c)')} ETH/USDC`);
  console.log('');
  const choice = (await state.rl.question(c.bold('  > '))).trim();
  let baseMint: PublicKey | null = null;
  if (choice === 'a') baseMint = new PublicKey('So11111111111111111111111111111111111111112');
  else if (choice === 'b') baseMint = new PublicKey('9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E');
  else if (choice === 'c') baseMint = new PublicKey('7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs');
  if (baseMint) {
    state.baseMint = baseMint;
    state.market = marketPda(baseMint, QUOTE_MINT).address;
    state.marketBook = marketBookPda(state.market).address;
    console.log(c.green(`  ✓ switched to market ${state.market.toBase58()}`));
    await state.rl.question(c.dim('  press enter…'));
  }
}

// ─── 7. Show PDAs (for Solscan) ──────────────────────────────────────
async function showPdas(state: DemoState) {
  console.log(c.banner(' PDAs (paste into solscan.io devnet) '));
  console.log('');
  console.log(`  ${c.bold('Programs:')}`);
  console.log(`    flash_book:        ${FLASH_BOOK_PROGRAM_ID.toBase58()}`);
  console.log('');
  console.log(`  ${c.bold('Globals:')}`);
  console.log(`    insurance_fund:    ${insuranceFundPda().address.toBase58()}`);
  console.log(`    flp_exposure:      ${flpExposurePda().address.toBase58()}`);
  console.log(`    fee_tiers:         ${feeTiersPda().address.toBase58()}`);
  console.log('');
  console.log(`  ${c.bold('Current market:')}`);
  console.log(`    market:            ${state.market.toBase58()}`);
  console.log(`    market_book:       ${state.marketBook.toBase58()}`);
  console.log('');
  console.log(`  ${c.bold('You:')}`);
  console.log(`    wallet:            ${state.wallet.publicKey.toBase58()}`);
  console.log(`    trader_state:      ${traderStatePda(state.wallet.publicKey).address.toBase58()}`);
  console.log(`    position[market]:  ${positionPda(state.market, state.wallet.publicKey).address.toBase58()}`);
  console.log('');
  await state.rl.question(c.dim('  press enter…'));
}

// ─── Main ────────────────────────────────────────────────────────────
/// Non-interactive showcase — runs through every read-only feature
/// of the demo for a quick "wow this is real" walkthrough. No tx
/// signing, no SOL spent. Run with `--showcase`.
async function showcase(state: DemoState) {
  console.clear();
  console.log(c.banner(' Flash Book V3 — DEVNET LIVE SHOWCASE '));
  console.log('');
  console.log(c.dim('  All reads against the live devnet deployment.'));
  console.log('');

  await renderHeader(state);

  console.log(c.banner(' [1/5]  PROGRAMS DEPLOYED '));
  await showPdasNonInteractive(state);

  console.log(c.banner(' [2/5]  GLOBAL FEE TIERS (HL pattern) '));
  await tiersDisplayOnly(state);

  console.log(c.banner(' [3/5]  ORDERBOOK — current depth on this market '));
  try {
    const ix = await state.client.viewBookDepthV2Ix({ market: state.market });
    const depth = await simulateAndDecodeEvent(state, ix, 'BookDepthV2Event');
    if (!depth) {
      console.log(c.dim('  (book empty — no orders placed yet)'));
    } else {
      console.log(c.bold('  Asks:'));
      let printed = 0;
      for (const a of depth.asks ?? []) {
        if ((a.priceTicks?.toString?.() ?? '0') === '0') continue;
        console.log(`    ${c.red(String(a.priceTicks))}  ×  ${a.sizeLots}`);
        printed++;
      }
      if (printed === 0) console.log(c.dim('    (no asks)'));
      console.log(c.bold('  Bids:'));
      printed = 0;
      for (const b of depth.bids ?? []) {
        if ((b.priceTicks?.toString?.() ?? '0') === '0') continue;
        console.log(`    ${c.green(String(b.priceTicks))}  ×  ${b.sizeLots}`);
        printed++;
      }
      if (printed === 0) console.log(c.dim('    (no bids)'));
    }
  } catch (e) {
    console.log(c.red('  ✗ ' + (e as Error).message.split('\n')[0]));
  }
  console.log('');

  console.log(c.banner(' [4/5]  EVENT STREAM — what the sequencer sees '));
  console.log(c.dim('  Listening for 8 seconds for any flash-book event…'));
  const coder = new BorshEventCoder(IDL);
  let eventCount = 0;
  const subId = state.conn.onLogs(
    FLASH_BOOK_PROGRAM_ID,
    (logs: Logs) => {
      if (logs.err) return;
      for (const line of logs.logs) {
        if (!line.startsWith('Program data: ')) continue;
        try {
          const ev = coder.decode(line.slice('Program data: '.length).trim());
          if (!ev) continue;
          const ts = new Date().toISOString().slice(11, 19);
          console.log(`  ${c.dim(ts)}  ${c.cyan(ev.name)}`);
          eventCount++;
        } catch {
          /* skip */
        }
      }
    },
    'confirmed',
  );
  await new Promise((r) => setTimeout(r, 8000));
  await state.conn.removeOnLogsListener(subId);
  if (eventCount === 0) console.log(c.dim('  (quiet right now — no fills happening)'));
  console.log('');

  console.log(c.banner(' [5/5]  WINS vs HL / DRIFT / PHOENIX '));
  console.log('');
  const wins: [string, string][] = [
    ['Sub-ms matcher tick (MagicBlock ER)', 'HL: 200 ms blocks'],
    ['Per-market FLP exposure (independently ER-delegatable)', 'HL: singleton FLP'],
    ['Multi-tier MMR (8 tiers, concentration penalty)', 'HL: 6 tiers'],
    ['NEGATIVE-fee retail tier 0 (i32 maker_rebate_bps)', 'Drift / Phoenix: positive only'],
    ['Volume-tier crystallized ON-CHAIN', 'HL: off-chain only'],
    ['Vol-adaptive oracle band (1+10×vol)', 'HL: fixed pct'],
    ['VPIN-gated FLP pause (toxicity ≥70%)', 'No other DEX has this'],
    ['EMA-blended funding (50/50 prior)', 'HL: per-block recompute'],
    ['Modular wrapper-CPI (4 programs, indep upgrade)', 'All others: monolith'],
    ['O(N log N) FBA matcher (256 orders/side)', 'Original O(N²): 64'],
  ];
  for (const [win, vs] of wins) {
    console.log(`  ${c.green('✓')} ${c.bold(win)}`);
    console.log(`     ${c.dim('vs ' + vs)}`);
  }
  console.log('');
  console.log(c.banner(' SHOWCASE COMPLETE '));
  console.log('');
  console.log(c.dim('  Try interactively: ') + c.bold('bun run scripts/demo.ts'));
  console.log('');
}

async function showPdasNonInteractive(state: DemoState) {
  console.log(`    flash_book:        ${c.cyan(FLASH_BOOK_PROGRAM_ID.toBase58())}`);
  console.log(`    insurance_fund:    ${c.cyan(insuranceFundPda().address.toBase58())}`);
  console.log(`    flp_exposure:      ${c.cyan(flpExposurePda().address.toBase58())}`);
  console.log(`    fee_tiers:         ${c.cyan(feeTiersPda().address.toBase58())}`);
  console.log(`    market (SOL/USDC): ${c.cyan(state.market.toBase58())}`);
  console.log(`    market_book:       ${c.cyan(state.marketBook.toBase58())}`);
  console.log('');
}

async function tiersDisplayOnly(state: DemoState) {
  const ft = feeTiersPda();
  const info = await state.conn.getAccountInfo(ft.address);
  if (!info) {
    console.log(c.red('  FeeTiers not initialized.'));
    return;
  }
  const buf = info.data;
  const tierCount = buf.readUInt8(8 + 32 + 1);
  const window = Number(buf.readBigUInt64LE(8 + 32 + 1 + 1 + 6));
  console.log(`  Volume window: ${c.bold(String(window))} slots  (~${(window * 0.4 / 86400).toFixed(1)} days)`);
  console.log('');
  console.log(c.bold('  Tier  Min volume         Maker            Taker'));
  let off = 8 + 32 + 1 + 1 + 6 + 8;
  for (let i = 0; i < tierCount; i++) {
    const minVol = buf.readBigUInt64LE(off);
    const maker = buf.readInt32LE(off + 8);
    const taker = buf.readUInt32LE(off + 12);
    const makerStr = maker >= 0 ? c.green('+' + maker + ' bps rebate') : c.red(maker + ' bps fee');
    console.log(
      `  VIP${i}  ${(Number(minVol) / 1e6).toFixed(2).padStart(15)} USDC   ${makerStr.padEnd(25)}  ${c.yellow(taker + ' bps')}`,
    );
    off += 16;
  }
  console.log('');
}

/// Live TUI dashboard — split-pane view that refreshes every 2 sec
/// with on-chain state. The "wow" mode for first-time viewers.
///   ┌─────────── ORDERBOOK ────────────┬─────── MY ACCOUNT ───────┐
///   │ asks → top-of-book               │ wallet, SOL, collateral  │
///   │ bids ← top-of-book               │ open positions           │
///   ├──────────── EVENTS (live) ───────┼─────── FEE TIER ─────────┤
///   │ every program log decoded        │ VIP rank + maker / taker │
///   └──────────────────────────────────┴──────────────────────────┘
async function splashBanner() {
  console.clear();
  const SPLASH = `
    ${c.cyan('███████')} ${c.cyan('██')}      ${c.cyan('█████')}  ${c.cyan('███████')} ${c.cyan('██')}  ${c.cyan('██')}     ${c.bold('██████')}   ${c.bold('██████')}   ${c.bold('██████')}  ${c.bold('██')}  ${c.bold('██')}
    ${c.cyan('██')}      ${c.cyan('██')}     ${c.cyan('██')}   ${c.cyan('██')} ${c.cyan('██')}      ${c.cyan('██')}  ${c.cyan('██')}     ${c.bold('██')}   ${c.bold('██')} ${c.bold('██')}    ${c.bold('██')} ${c.bold('██')}    ${c.bold('██')} ${c.bold('██')} ${c.bold('██')}
    ${c.cyan('█████')}   ${c.cyan('██')}     ${c.cyan('███████')} ${c.cyan('███████')} ${c.cyan('███████')}      ${c.bold('██████')}  ${c.bold('██')}    ${c.bold('██')} ${c.bold('██')}    ${c.bold('██')} ${c.bold('████')}
    ${c.cyan('██')}      ${c.cyan('██')}     ${c.cyan('██')}   ${c.cyan('██')}      ${c.cyan('██')} ${c.cyan('██')}  ${c.cyan('██')}     ${c.bold('██   ██')} ${c.bold('██')}    ${c.bold('██')} ${c.bold('██')}    ${c.bold('██')} ${c.bold('██')} ${c.bold('██')}
    ${c.cyan('██')}      ${c.cyan('███████')} ${c.cyan('██')}  ${c.cyan('██')} ${c.cyan('███████')} ${c.cyan('██')}  ${c.cyan('██')}     ${c.bold('██████')}   ${c.bold('██████')}   ${c.bold('██████')}  ${c.bold('██')}  ${c.bold('██')}
                                                                          ${c.dim('v3 · live on devnet')}
`;
  console.log(SPLASH);
  await new Promise((r) => setTimeout(r, 1000));
  const tagline = '  ⚡  Sub-ms matcher · MagicBlock ER · 4-program modular · 10 wins over HL/Drift/Phoenix';
  for (let i = 0; i < tagline.length; i++) {
    process.stdout.write(c.cyan(tagline[i]));
    await new Promise((r) => setTimeout(r, 6));
  }
  console.log('\n');
  await new Promise((r) => setTimeout(r, 600));
}

async function dashboard(state: DemoState) {
  await splashBanner();
  console.clear();
  process.stdout.write('\x1b[?25l'); // hide cursor

  let recentEvents: { ts: string; name: string; data: any }[] = [];
  let totalEventsSeen = 0;
  const startedAt = Date.now();
  const coder = new BorshEventCoder(IDL);
  const subId = state.conn.onLogs(
    FLASH_BOOK_PROGRAM_ID,
    (logs: Logs) => {
      if (logs.err) return;
      for (const line of logs.logs) {
        if (!line.startsWith('Program data: ')) continue;
        try {
          const ev = coder.decode(line.slice('Program data: '.length).trim());
          if (!ev) continue;
          recentEvents.unshift({
            ts: new Date().toISOString().slice(11, 19),
            name: ev.name,
            data: ev.data,
          });
          totalEventsSeen++;
          if (recentEvents.length > 8) recentEvents = recentEvents.slice(0, 8);
        } catch { /* skip */ }
      }
    },
    'confirmed',
  );

  const cleanup = async () => {
    process.stdout.write('\x1b[?25h'); // show cursor
    await state.conn.removeOnLogsListener(subId);
    console.clear();
    console.log(c.green('\n  bye 👋  Run again: ') + c.bold('bun run scripts/demo.ts') + '\n');
    process.exit(0);
  };
  process.on('SIGINT', cleanup);
  process.on('SIGTERM', cleanup);

  const refresh = async () => {
    // Gather state in parallel.
    const [solLamports, depthEv, tierEv, tsInfo, posInfo] = await Promise.all([
      state.conn.getBalance(state.wallet.publicKey).catch(() => 0),
      (async () => {
        try {
          const ix = await state.client.viewBookDepthV2Ix({ market: state.market });
          return await simulateAndDecodeEvent(state, ix, 'BookDepthV2Event');
        } catch { return null; }
      })(),
      (async () => {
        try {
          const ix = await state.client.viewTraderEffectiveTierIx({ trader: state.wallet.publicKey });
          return await simulateAndDecodeEvent(state, ix, 'TraderEffectiveTierEvent');
        } catch { return null; }
      })(),
      state.conn.getAccountInfo(state.client.traderState(state.wallet.publicKey).address).catch(() => null),
      state.conn.getAccountInfo(positionPda(state.market, state.wallet.publicKey).address).catch(() => null),
    ]);

    const sol = (solLamports / 1e9).toFixed(4);
    let collateral = '—';
    let openPos = '—';
    let volume = '—';
    if (tsInfo) {
      try {
        collateral = (Number(tsInfo.data.readBigUInt64LE(8 + 32 + 1)) / 1e6).toFixed(2);
        openPos = String(tsInfo.data.readUInt8(8 + 32 + 1 + 8 + 8));
        // volume_30d_quote_lots is at the END of TraderState body.
        // Body order: trader(32) + bump(1) + collat(8) + realized_pnl(8) + open_positions(1) +
        //   toxicity(4) + orders_this_batch(4) + last_batch(8) + fee_discount(4) + delegate(32) +
        //   referrer(32) + builder(32) + builder_max(4) = 170 bytes pre-volume.
        const volOff = 8 + 32 + 1 + 8 + 8 + 1 + 4 + 4 + 8 + 4 + 32 + 32 + 32 + 4;
        if (tsInfo.data.length >= volOff + 8) {
          volume = (Number(tsInfo.data.readBigUInt64LE(volOff)) / 1e6).toFixed(2);
        }
      } catch { /* layout drift */ }
    }

    let posLine = c.dim('— no position —');
    if (posInfo) {
      try {
        // PositionAccount: 8 disc + 32 trader + 32 market + 1 bump + 1 side + 8 size + 8 entry
        const off = 8 + 32 + 32 + 1;
        const side = posInfo.data.readUInt8(off);
        const size = posInfo.data.readBigUInt64LE(off + 1);
        const entry = posInfo.data.readBigUInt64LE(off + 1 + 8);
        if (size > 0n) {
          const sideStr = side === 0 ? c.green('LONG') : c.red('SHORT');
          posLine = `${sideStr} ${size}  @  ${entry}`;
        }
      } catch { /* skip */ }
    }

    // Multi-market quick read — for the "ALL markets" header strip.
    const otherMarkets = [
      { sym: 'SOL', mint: new PublicKey('So11111111111111111111111111111111111111112') },
      { sym: 'BTC', mint: new PublicKey('9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E') },
      { sym: 'ETH', mint: new PublicKey('7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs') },
    ];
    const marketBookExists = await Promise.all(
      otherMarkets.map(async (m) => {
        const mp = marketPda(m.mint, QUOTE_MINT).address;
        const bp = marketBookPda(mp).address;
        const info = await state.conn.getAccountInfo(bp).catch(() => null);
        return { ...m, exists: !!info, addr: mp };
      }),
    );

    // Stats: uptime, current slot, events seen, refresh rate.
    let slot = 0;
    try { slot = await state.conn.getSlot('confirmed'); } catch { /* skip */ }
    const uptimeSec = Math.floor((Date.now() - startedAt) / 1000);
    const uptimeStr = `${Math.floor(uptimeSec / 60)}m${(uptimeSec % 60).toString().padStart(2, '0')}s`;

    // Render.
    process.stdout.write('\x1b[H'); // cursor home
    const w = process.stdout.columns || 100;
    const sep = c.dim('═'.repeat(w));

    const lines: string[] = [];
    lines.push(c.banner('  ⚡  Flash Book V3 — LIVE devnet dashboard  ') + c.dim('   Ctrl-C to exit  ·  --interactive for menu  ·  --showcase for walkthrough'));
    lines.push(sep);
    // Multi-market strip.
    const marketStrip = marketBookExists
      .map((m) => {
        const isActive = m.addr.equals(state.market);
        const sym = isActive ? c.bold(c.green(m.sym)) : c.dim(m.sym);
        const status = m.exists ? c.green('●') : c.dim('○');
        return `${status} ${sym}/USDC`;
      })
      .join('   ');
    lines.push(`  ${c.bold('Markets:')} ${marketStrip}     ${c.dim('· slot ' + slot + ' · uptime ' + uptimeStr + ' · events seen ' + totalEventsSeen)}`);
    lines.push(sep);
    lines.push(`  ${c.bold('Wallet')} ${c.cyan(state.wallet.publicKey.toBase58().slice(0, 12) + '…')}    ${c.bold('SOL')} ${c.green(sol)}    ${c.bold('Collateral')} ${c.green(collateral + ' USDC')}    ${c.bold('Open positions')} ${openPos}`);
    lines.push(`  ${c.bold('Market')} ${c.cyan(state.market.toBase58().slice(0, 12) + '…')}   ${c.bold('Position')} ${posLine}`);
    if (tierEv) {
      const m = tierEv.makerRebateBps;
      const makerStr = m >= 0 ? c.green('+' + m + ' bps rebate') : c.red(m + ' bps fee');
      lines.push(`  ${c.bold('Tier')} VIP${tierEv.tierIndex}   maker ${makerStr}   taker ${c.yellow(tierEv.takerFeeBps + ' bps')}   30d-vol ${c.cyan(volume + ' USDC')}`);
    } else {
      lines.push(`  ${c.bold('Tier')} ${c.dim('— open trader_state to see —')}`);
    }
    lines.push(sep);

    const halfW = Math.max(40, Math.floor(w / 2) - 1);
    // Build orderbook + events as parallel columns
    const bookLines: string[] = [];
    bookLines.push(c.bold('  📖 ORDERBOOK'));
    bookLines.push('');
    if (depthEv) {
      const asks = (depthEv.asks ?? []).filter((a: any) => (a.priceTicks?.toString?.() ?? '0') !== '0');
      const bids = (depthEv.bids ?? []).filter((b: any) => (b.priceTicks?.toString?.() ?? '0') !== '0');
      if (asks.length === 0 && bids.length === 0) {
        bookLines.push(c.dim('    book empty — no orders placed'));
      } else {
        for (const a of asks.slice().reverse()) {
          bookLines.push(`     ${c.red(String(a.priceTicks).padStart(12))}  ×  ${a.sizeLots}`);
        }
        bookLines.push(c.dim('    ─────────────'));
        for (const b of bids) {
          bookLines.push(`     ${c.green(String(b.priceTicks).padStart(12))}  ×  ${b.sizeLots}`);
        }
      }
    } else {
      bookLines.push(c.dim('    (failed to load book)'));
    }

    const evLines: string[] = [];
    evLines.push(c.bold('  📡 EVENT STREAM (live)'));
    evLines.push('');
    if (recentEvents.length === 0) {
      evLines.push(c.dim('    (waiting for events…)'));
    } else {
      for (const e of recentEvents) {
        evLines.push(`    ${c.dim(e.ts)}  ${c.cyan(e.name)}`);
      }
    }

    const maxRows = Math.max(bookLines.length, evLines.length);
    for (let i = 0; i < maxRows; i++) {
      const left = (bookLines[i] ?? '').padEnd(halfW + 10); // +10 because ANSI codes
      const right = evLines[i] ?? '';
      lines.push(left + c.dim(' │') + right);
    }
    lines.push('');
    lines.push(sep);
    lines.push(`  ${c.dim('Solscan: https://explorer.solana.com/address/' + state.market.toBase58() + '?cluster=devnet')}`);
    lines.push(`  ${c.dim('Refreshing every 2 s · streaming events live · Ctrl-C to exit · or run with --interactive for menu')}`);

    // Clear from cursor down + write
    process.stdout.write('\x1b[J');
    process.stdout.write(lines.join('\n') + '\n');
  };

  await refresh();
  setInterval(refresh, 2000);
  await new Promise(() => { /* run forever until Ctrl-C */ });
}

/// Print every menu's banner + options to stdout sequentially, no
/// readline. Lets you see what the interactive REPL looks like
/// without typing.
async function screenshots(state: DemoState) {
  console.clear();
  console.log(c.banner(' ALL MENUS — what the interactive REPL looks like '));
  console.log('');

  // Main menu
  console.log(c.banner(' MAIN MENU '));
  console.log('');
  console.log(`    ${c.cyan('1)')} Trade          — place / cancel limit orders, view orderbook`);
  console.log(`    ${c.cyan('2)')} Margin         — deposit / withdraw USDC, view positions`);
  console.log(`    ${c.cyan('3)')} Matcher        — run a batch tick, watch fills land live`);
  console.log(`    ${c.cyan('4)')} Vaults         — create vault, deposit, vault-trades`);
  console.log(`    ${c.cyan('5)')} Tiers          — view fee tier table + your effective rate`);
  console.log(`    ${c.cyan('6)')} Switch market  — change between SOL/BTC/ETH`);
  console.log(`    ${c.cyan('7)')} Show PDAs      — print all account addresses (for Solscan)`);
  console.log(`    ${c.cyan('q)')} Quit`);
  console.log('');

  console.log(c.banner(' [1] TRADE '));
  console.log(`    ${c.cyan('a)')} Place limit order (long / buy)`);
  console.log(`    ${c.cyan('b)')} Place limit order (short / sell)`);
  console.log(`    ${c.cyan('c)')} Cancel order by ID`);
  console.log(`    ${c.cyan('d)')} View orderbook (top depth)`);
  console.log('');

  console.log(c.banner(' [2] MARGIN '));
  console.log(`    ${c.cyan('a)')} Open trader_state (one-time)`);
  console.log(`    ${c.cyan('b)')} Deposit USDC`);
  console.log(`    ${c.cyan('c)')} Withdraw USDC (full)`);
  console.log(`    ${c.cyan('d)')} View my position on this market`);
  console.log('');

  console.log(c.banner(' [3] MATCHER '));
  console.log(`    ${c.cyan('a)')} Run batch tick (run_batch_v2) on this market`);
  console.log(`    ${c.cyan('b)')} Watch live event stream (Ctrl-C to stop)`);
  console.log('');

  console.log(c.banner(' [5] TIERS — live decoded from FeeTiers PDA '));
  await tiersDisplayOnly(state);

  console.log(c.banner(' [6] SWITCH MARKET '));
  console.log(`    ${c.cyan('a)')} SOL/USDC  (default)`);
  console.log(`    ${c.cyan('b)')} BTC/USDC`);
  console.log(`    ${c.cyan('c)')} ETH/USDC`);
  console.log('');

  console.log(c.banner(' [7] SHOW PDAs '));
  console.log(`  ${c.bold('Programs:')}`);
  console.log(`    flash_book:        ${FLASH_BOOK_PROGRAM_ID.toBase58()}`);
  console.log(`  ${c.bold('Globals:')}`);
  console.log(`    insurance_fund:    ${insuranceFundPda().address.toBase58()}`);
  console.log(`    flp_exposure:      ${flpExposurePda().address.toBase58()}`);
  console.log(`    fee_tiers:         ${feeTiersPda().address.toBase58()}`);
  console.log(`  ${c.bold('Current market:')}`);
  console.log(`    market:            ${state.market.toBase58()}`);
  console.log(`    market_book:       ${state.marketBook.toBase58()}`);
  console.log(`  ${c.bold('You:')}`);
  console.log(`    wallet:            ${state.wallet.publicKey.toBase58()}`);
  console.log(`    trader_state:      ${traderStatePda(state.wallet.publicKey).address.toBase58()}`);
  console.log(`    position[market]:  ${positionPda(state.market, state.wallet.publicKey).address.toBase58()}`);
  console.log('');
}

async function main() {
  const state = await makeState();
  if (process.argv.includes('--showcase')) {
    await showcase(state);
    state.rl.close();
    return;
  }
  if (process.argv.includes('--screenshots')) {
    await screenshots(state);
    state.rl.close();
    return;
  }
  if (process.argv.includes('--interactive')) {
    try {
      while (await mainMenu(state)) {
        /* loop */
      }
    } catch (e) {
      if ((e as Error).message?.includes('readline was closed')) {
        // EOF — treat as quit.
      } else {
        throw e;
      }
    }
    state.rl.close();
    console.log(c.green('\n  bye 👋\n'));
    return;
  }
  // Default: dashboard mode.
  await dashboard(state);
}

main().catch((e) => {
  console.error(c.red('\n  Demo crashed: ') + e.message);
  process.exit(1);
});
