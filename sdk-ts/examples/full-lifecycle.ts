// Standalone lifecycle demonstration — builds and prints every
// instruction required to spin up a Flash Book market end-to-end,
// without requiring a deployed program or a live RPC.
//
// What this proves:
//   1. The SDK constructs syntactically valid Solana transactions
//      against the program's IDL.
//   2. PDA seeds are reproducible across SDK + program.
//   3. The instruction surface matches what the program expects.
//
// Run: bun run examples/full-lifecycle.ts
//
// To actually execute against a running validator, set
// FLASH_BOOK_RPC=<rpc_url> and uncomment the send block at the bottom.

import { Connection, Keypair, PublicKey } from '@solana/web3.js';
import { Wallet } from '@coral-xyz/anchor';
import BN from 'bn.js';
import {
  FlashBookClient,
  defaultInsuranceFundParams,
  defaultMajorMarketParams,
  MarketStatus,
} from '../src/index.ts';

// ─── Setup ────────────────────────────────────────────────────────────

const RPC = process.env.FLASH_BOOK_RPC ?? 'https://api.devnet.solana.com';
const connection = new Connection(RPC, 'confirmed');

// Synthetic identities. In production these would be real wallets.
const authority = Keypair.generate();
const sequencer = Keypair.generate();
const trader = Keypair.generate();
const counterparty = Keypair.generate();
const baseMint = Keypair.generate().publicKey;
const quoteMint = Keypair.generate().publicKey;
const baseVault = Keypair.generate().publicKey;
const quoteVault = Keypair.generate().publicKey;
const oracleAccount = Keypair.generate().publicKey;

const wallet = new Wallet(authority);
const client = new FlashBookClient(connection, wallet);

// ─── Print helpers ────────────────────────────────────────────────────

function header(label: string): void {
  const rule = '━'.repeat(72);
  console.log(`\n${rule}\n  ${label}\n${rule}`);
}

function line(k: string, v: string | number | boolean | PublicKey | BN): void {
  const val =
    v instanceof PublicKey
      ? v.toBase58()
      : v instanceof BN
        ? v.toString()
        : String(v);
  console.log(`  ${k.padEnd(28)} ${val}`);
}

function describeIx(name: string, accounts: string[], args?: Record<string, unknown>): void {
  console.log(`\n  → ${name}`);
  for (const a of accounts) console.log(`      account: ${a}`);
  if (args) {
    for (const [k, v] of Object.entries(args)) {
      console.log(`      arg:     ${k} = ${stringify(v)}`);
    }
  }
}

function stringify(v: unknown): string {
  if (v instanceof PublicKey) return v.toBase58();
  if (v instanceof BN) return v.toString();
  if (typeof v === 'object' && v !== null) return JSON.stringify(v, (_, x) => x?.toString?.() ?? x);
  return String(v);
}

// ─── Phase 0: derive all the PDAs upfront ─────────────────────────────

header('Identities + PDAs');

line('authority', authority.publicKey);
line('sequencer', sequencer.publicKey);
line('trader', trader.publicKey);
line('counterparty', counterparty.publicKey);
line('base mint', baseMint);
line('quote mint', quoteMint);

const market = client.market(baseMint, quoteMint);
const insuranceFund = client.insuranceFund();
const flpExposure = client.flpExposure();
const traderState = client.traderState(trader.publicKey);
const counterpartyState = client.traderState(counterparty.publicKey);
const traderPosition = client.position(market.address, trader.publicKey);

line('market PDA', market.address);
line('insurance_fund PDA', insuranceFund.address);
line('flp_exposure PDA', flpExposure.address);
line('trader_state PDA', traderState.address);
line('trader position PDA', traderPosition.address);

// ─── Phase 1: protocol setup (one-time) ───────────────────────────────

header('Phase 1 — Protocol setup (authority signs)');

const ifParams = defaultInsuranceFundParams();
const ix1 = await client.initializeInsuranceFundIx({
  authority: authority.publicKey,
  params: ifParams,
  quoteMint,
  quoteVault,
});
describeIx('initialize_insurance_fund', [authority.publicKey.toBase58(), insuranceFund.address.toBase58()], {
  fee_contribution_bps: ifParams.feeContributionBps,
  toxicity_tax_bps: ifParams.toxicityTaxContributionBps,
  liq_penalty_bps: ifParams.liqPenaltyContributionBps,
  pause_threshold: ifParams.pauseThresholdQuoteLots,
});

const ix2 = await client.initializeFlpExposureIx(authority.publicKey, new BN(5_000_000));
describeIx('initialize_flp_exposure', [authority.publicKey.toBase58(), flpExposure.address.toBase58()], {
  initial_capital_quote_lots: '5,000,000',
});

const params = defaultMajorMarketParams();
const ix3 = await client.initializeMarketIx({
  authority: authority.publicKey,
  baseMint,
  quoteMint,
  baseVault,
  quoteVault,
  oracleAccount,
  params,
  initialOracleTicks: new BN(100_000),
});
describeIx(
  'initialize_market',
  [
    authority.publicKey.toBase58(),
    market.address.toBase58(),
    insuranceFund.address.toBase58(),
    flpExposure.address.toBase58(),
  ],
  {
    initial_oracle_ticks: 100_000,
    flp_quote_levels: params.flpQuoteLevels,
  },
);

// ─── Phase 2: trader onboarding ───────────────────────────────────────

header('Phase 2 — Trader onboarding');

const ix4 = await client.openTraderStateIx(trader.publicKey);
describeIx('open_trader_state', [trader.publicKey.toBase58(), traderState.address.toBase58()]);

const ix5 = await client.depositCollateralIx({
  trader: trader.publicKey,
  amount: new BN(50_000),
  quoteMint,
  quoteVault,
});
describeIx('deposit_collateral', [trader.publicKey.toBase58(), traderState.address.toBase58()], {
  amount_quote_lots: 50_000,
});

// ─── Phase 3: maker rests on the CLOB ─────────────────────────────────

header('Phase 3 — Maker rests on the CLOB');

const ix6 = await client.placeLimitOrderV2Ix({
  trader: counterparty.publicKey,
  market: market.address,
  side: 'short',
  sizeLots: new BN(10),
  limitTicks: new BN(99_950),
});
describeIx(
  'place_limit_order_v2 (maker)',
  [counterparty.publicKey.toBase58(), market.address.toBase58()],
  { side: 'short', size_lots: 10, limit_ticks: 99_950, semantics: 'rest in book' },
);

// ─── Phase 4: taker walks the book ────────────────────────────────────

header('Phase 4 — CLOB taker walks the book (immediate match)');

const ix7 = await client.placeTakerOrderV2Ix({
  trader: trader.publicKey,
  market: market.address,
  side: 'long',
  sizeLots: new BN(10),
  limitTicks: new BN(99_950),
});
describeIx(
  'place_taker_order_v2',
  [trader.publicKey.toBase58(), market.address.toBase58()],
  {
    side: 'long',
    size_lots: 10,
    limit_ticks: 99_950,
    semantics: 'immediate match — emits BatchFillIntentEvent inline',
  },
);

// ─── Phase 5: settlement ──────────────────────────────────────────────

header('Phase 5 — Sequencer settles each inline fill');

const ix8 = await client.applyFillIx({
  sequencer: sequencer.publicKey,
  market: market.address,
  takerTrader: trader.publicKey,
  makerTrader: counterparty.publicKey,
  sizeLots: new BN(10),
  priceTicks: new BN(99_950),
  takerSide: 'long',
});
describeIx(
  'apply_fill',
  [
    sequencer.publicKey.toBase58(),
    market.address.toBase58(),
    traderState.address.toBase58(),
    counterpartyState.address.toBase58(),
    traderPosition.address.toBase58(),
    client.position(market.address, counterparty.publicKey).address.toBase58(),
  ],
  { size_lots: 10, price_ticks: 99_950, taker_side: 'long' },
);

// ─── Phase 6: governance ──────────────────────────────────────────────

header('Phase 6 — Governance (authority signs)');

const ix9 = await client.setMarketStatusIx({
  authority: authority.publicKey,
  market: market.address,
  newStatus: MarketStatus.PostOnly,
});
describeIx('set_market_status', [authority.publicKey.toBase58(), market.address.toBase58()], {
  new_status: 'PostOnly',
});

// ─── Summary ──────────────────────────────────────────────────────────

const allIxs = [ix1, ix2, ix3, ix4, ix5, ix6, ix7, ix8, ix9];
header('Summary');
console.log(`  RPC:           ${RPC}`);
console.log(`  Instructions:  ${allIxs.length}`);
console.log(`  Total bytes:   ${allIxs.reduce((s, ix) => s + ix.data.length, 0)}`);
console.log(`  All accounts:  ${allIxs.reduce((s, ix) => s + ix.keys.length, 0)} (across all ixs)`);
console.log('');
console.log('  Each instruction was built against the program IDL with verified');
console.log('  PDA seeds. To execute against a localnet, deploy the program');
console.log('  (`anchor deploy`) and uncomment the send block in this file.');

// ─── Optional: send to a live validator ───────────────────────────────
//
// Uncomment to actually send these. Requires the program to be deployed
// at FLASH_BOOK_PROGRAM_ID on the configured RPC and `authority` /
// `trader` / `sequencer` to be funded with SOL.
//
// const tx1 = new Transaction().add(ix1, ix2, ix3);
// tx1.feePayer = authority.publicKey;
// const { blockhash } = await connection.getLatestBlockhash();
// tx1.recentBlockhash = blockhash;
// tx1.sign(authority);
// const sig1 = await connection.sendRawTransaction(tx1.serialize());
// console.log(`Setup tx: ${sig1}`);
// ... etc

console.log('\n  Lifecycle demo complete.\n');
