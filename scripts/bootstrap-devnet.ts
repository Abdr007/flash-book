#!/usr/bin/env bun
// Devnet bootstrap script for Flash Book V3.
//
// Run AFTER `solana program deploy target/deploy/flash_book.so`
// (monolithic — wave 23 merged the 3 wrappers). The script:
//
//   1. Creates the protocol's quote_vault TokenAccount (USDC, owned by
//      the InsuranceFund PDA).
//   2. Initializes the InsuranceFund (singleton, [b"insurance_fund"]).
//   3. Initializes the FLP exposure singleton (singleton,
//      [b"flp_exposure"]).
//   4. Initializes the global FeeTiers (wave 22, 4-tier HL-style
//      schedule — authority can update later via `update_fee_tiers`).
//   5. For each of the configured markets (SOL/USDC, BTC/USDC,
//      ETH/USDC by default), runs:
//        a. initialize_market (with defaultMajorMarketParams + a
//           seeded oracle price)
//        b. init_market_book (allocates the 9864-byte hypertree PDA)
//
// Idempotent: each step checks for existing state via `getAccountInfo`
// and skips if already initialized. Safe to re-run after partial failure.
//
// USAGE
//   AUTHORITY_KEYPAIR=~/.config/solana/devnet.json \
//   RPC_URL=https://api.devnet.solana.com \
//   bun run scripts/bootstrap-devnet.ts
//
// ENV (all optional with sane defaults):
//   AUTHORITY_KEYPAIR  path to authority keypair (default ~/.config/solana/id.json)
//   RPC_URL            cluster RPC URL (default localnet)
//   QUOTE_MINT         USDC mint address (default devnet USDC)
//   MARKETS            comma-separated <BASE_MINT>:<INITIAL_PRICE_TICKS>
//                      (default = SOL:99950,BTC:6800000000,ETH:380000000)

import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
} from '@solana/web3.js';
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountInstruction,
} from '@solana/spl-token';
import { AnchorProvider, BN, Wallet } from '@coral-xyz/anchor';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  FlashBookClient,
  defaultInsuranceFundParams,
  defaultMajorMarketParams,
  feeTiersPda,
  flpExposurePda,
  insuranceFundPda,
  marketBookPda,
} from '../sdk-ts/src/index.ts';

// ─── Config ──────────────────────────────────────────────────────────

// Known-good Flash production endpoints (per flash-mobile project):
//   • Solana mainnet-beta:  https://api.mainnet-beta.solana.com
//   • Flash ER (mainnet):   https://flashtrade.magicblock.app
//
// CLUSTER env selects defaults safely:
//   CLUSTER=mainnet  → Solana mainnet-beta (sets up a LIVE deployment)
//   CLUSTER=devnet   → Solana devnet (safe testing — DEFAULT)
//   CLUSTER=local    → localnet
const CLUSTER = (process.env.CLUSTER ?? 'devnet').toLowerCase();
const RPC_URL =
  process.env.RPC_URL ??
  (CLUSTER === 'mainnet'
    ? 'https://api.mainnet-beta.solana.com'
    : CLUSTER === 'devnet'
      ? 'https://api.devnet.solana.com'
      : 'http://127.0.0.1:8899');
const AUTHORITY_KEYPAIR =
  process.env.AUTHORITY_KEYPAIR ?? path.join(os.homedir(), '.config', 'solana', 'id.json');

// Devnet USDC mint (Circle's testnet USDC). Override via env for other
// clusters / mock USDC mints.
const DEFAULT_QUOTE_MINT = '4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU';
const QUOTE_MINT = new PublicKey(process.env.QUOTE_MINT ?? DEFAULT_QUOTE_MINT);

// Markets to initialize: <BASE_MINT>:<INITIAL_PRICE_TICKS>
// price_ticks = USD_price × 10 (one tick = $0.10 at our default tick_size=1
// + USD_DECIMALS=6 + tick_size scaling).
const DEFAULT_MARKETS = [
  // SOL — assume $99.95
  { base: 'So11111111111111111111111111111111111111112', initialPriceTicks: 99_950n },
  // BTC — assume $68_000.00 (use a placeholder mint on devnet)
  { base: '9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E', initialPriceTicks: 6_800_000_00n },
  // ETH — assume $3_800.00
  { base: '7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs', initialPriceTicks: 380_000_00n },
];

const MARKETS_ENV = process.env.MARKETS;
const MARKETS = MARKETS_ENV
  ? MARKETS_ENV.split(',').map((s) => {
      const [base, ticks] = s.split(':');
      return { base, initialPriceTicks: BigInt(ticks) };
    })
  : DEFAULT_MARKETS;

// ─── Helpers ─────────────────────────────────────────────────────────

function loadKeypair(p: string): Keypair {
  const raw = JSON.parse(fs.readFileSync(p, 'utf8')) as number[];
  return Keypair.fromSecretKey(new Uint8Array(raw));
}

/// Reject http:// when targeting mainnet — credentials would
/// transit cleartext + an MITM could swap pubkeys mid-bootstrap.
function validateRpcUrl(url: string) {
  if (url.includes('mainnet') && url.startsWith('http://')) {
    throw new Error(
      `Refusing http:// URL targeting mainnet: ${url}. ` +
        `Use https:// (TLS) for production endpoints.`,
    );
  }
}

async function exists(conn: Connection, pubkey: PublicKey): Promise<boolean> {
  return (await conn.getAccountInfo(pubkey)) !== null;
}

async function send(
  conn: Connection,
  payer: Keypair,
  ixs: any[],
  signers: Keypair[] = [],
  label = '',
) {
  if (ixs.length === 0) return null;
  const tx = new Transaction().add(...ixs);
  tx.feePayer = payer.publicKey;
  tx.recentBlockhash = (await conn.getLatestBlockhash('confirmed')).blockhash;
  tx.sign(payer, ...signers);
  const sig = await conn.sendRawTransaction(tx.serialize(), {
    skipPreflight: false,
    preflightCommitment: 'confirmed',
  });
  await conn.confirmTransaction(sig, 'confirmed');
  console.log(`  ✓ ${label || 'tx'}  sig=${sig}`);
  return sig;
}

// HL-style 4-tier fee schedule. Authority can rewrite via
// `update_fee_tiers` post-deploy.
function hlStyleTiers(): {
  minVolumeQuoteLots: BN;
  makerRebateBps: number;
  takerFeeBps: number;
}[] {
  // Volume thresholds in quote-lots (= USDC at 6 decimals × 10^6).
  // tier 0:           $0  → maker  -2 (2bp fee), taker  5
  // tier 1:           $1M → maker   0 (free),    taker  4
  // tier 2:           $5M → maker  +1 (1bp reb), taker  3
  // tier 3:           $25M→ maker  +2 (2bp reb), taker  2
  return [
    { minVolumeQuoteLots: new BN(0), makerRebateBps: 0, takerFeeBps: 5 },
    { minVolumeQuoteLots: new BN('1000000000000'), makerRebateBps: 0, takerFeeBps: 4 },
    { minVolumeQuoteLots: new BN('5000000000000'), makerRebateBps: 1, takerFeeBps: 3 },
    { minVolumeQuoteLots: new BN('25000000000000'), makerRebateBps: 2, takerFeeBps: 2 },
  ];
}

// ─── Main ────────────────────────────────────────────────────────────

async function main() {
  console.log(`▶ Flash Book V3 devnet bootstrap`);
  console.log(`  RPC:        ${RPC_URL}`);
  console.log(`  Authority:  ${AUTHORITY_KEYPAIR}`);
  console.log(`  Quote mint: ${QUOTE_MINT.toBase58()}`);
  validateRpcUrl(RPC_URL);

  const authority = loadKeypair(AUTHORITY_KEYPAIR);
  const conn = new Connection(RPC_URL, 'confirmed');
  const wallet = new Wallet(authority);
  const _provider = new AnchorProvider(conn, wallet, { commitment: 'confirmed' });
  const client = new FlashBookClient(conn, wallet);

  console.log(`  Authority pubkey: ${authority.publicKey.toBase58()}`);
  const balance = await conn.getBalance(authority.publicKey);
  console.log(`  Authority SOL:    ${balance / 1e9}`);

  // ─── 1. Quote vault TokenAccount keypair
  // Anchor's `initialize_insurance_fund` uses `init` (not init_if_needed
  // or ATA) for the quote_vault, so we generate a fresh keypair and
  // pass it as a signer alongside the authority. The init creates the
  // TokenAccount with InsuranceFund PDA as token::authority.
  const fund = insuranceFundPda();
  console.log(`\n[1/5] InsuranceFund PDA: ${fund.address.toBase58()}`);

  const QUOTE_VAULT_PATH = path.join(os.homedir(), '.flash', 'devnet-quote-vault.json');
  let quoteVaultKp: Keypair;
  if (fs.existsSync(QUOTE_VAULT_PATH)) {
    quoteVaultKp = loadKeypair(QUOTE_VAULT_PATH);
    console.log(`  → reusing existing quote_vault keypair: ${quoteVaultKp.publicKey.toBase58()}`);
  } else {
    quoteVaultKp = Keypair.generate();
    fs.mkdirSync(path.dirname(QUOTE_VAULT_PATH), { recursive: true });
    fs.writeFileSync(QUOTE_VAULT_PATH, JSON.stringify(Array.from(quoteVaultKp.secretKey)));
    console.log(`  → generated new quote_vault keypair: ${quoteVaultKp.publicKey.toBase58()}`);
    console.log(`  → saved to ${QUOTE_VAULT_PATH}`);
  }
  const quoteVault = quoteVaultKp.publicKey;

  // ─── 2. Initialize InsuranceFund
  console.log(`\n[2/5] Initialize InsuranceFund`);
  if (await exists(conn, fund.address)) {
    console.log(`  → already initialized, skipping`);
  } else {
    const ix = await client.initializeInsuranceFundIx({
      authority: authority.publicKey,
      quoteMint: QUOTE_MINT,
      quoteVault,
      params: defaultInsuranceFundParams(),
    });
    await send(conn, authority, [ix], [quoteVaultKp], 'init InsuranceFund');
  }

  // ─── 3. Initialize FLP exposure singleton
  const flp = flpExposurePda();
  console.log(`\n[3/5] Initialize FLP exposure singleton: ${flp.address.toBase58()}`);
  if (await exists(conn, flp.address)) {
    console.log(`  → already initialized, skipping`);
  } else {
    const ix = await client.initializeFlpExposureIx(authority.publicKey);
    await send(conn, authority, [ix], [], 'init FlpExposure');
  }

  // ─── 4. Initialize FeeTiers (wave 22)
  const ft = feeTiersPda();
  console.log(`\n[4/5] Initialize FeeTiers: ${ft.address.toBase58()}`);
  if (await exists(conn, ft.address)) {
    console.log(`  → already initialized, skipping`);
  } else {
    const ix = await client.initFeeTiersIx({
      authority: authority.publicKey,
      // 14 days @ 0.4s/slot = 3_024_000 slots (HL standard window).
      volumeWindowSlots: new BN(3_024_000),
      tiers: hlStyleTiers(),
    });
    await send(conn, authority, [ix], [], 'init FeeTiers');
  }

  // ─── 5. Initialize each market
  console.log(`\n[5/5] Initialize ${MARKETS.length} market(s)`);
  for (const m of MARKETS) {
    const baseMint = new PublicKey(m.base);
    const market = client.market(baseMint, QUOTE_MINT);
    const book = marketBookPda(market.address);
    console.log(`\n  ─ ${baseMint.toBase58().slice(0, 8)}.../${QUOTE_MINT.toBase58().slice(0, 8)}...`);
    console.log(`    market:        ${market.address.toBase58()}`);
    console.log(`    market_book:   ${book.address.toBase58()}`);

    // The base_vault + oracle_account aren't strictly used by the v2
    // hypertree path but are required by the existing
    // initialize_market ix signature. Use the InsuranceFund's
    // quote_vault as a placeholder for base_vault (unused on perps);
    // oracle_account is the trader-supplied price feed pubkey (for
    // testing we use authority pubkey as a stand-in — production
    // wires Pyth here).
    const baseVault = quoteVault; // placeholder; perps don't use base_vault
    const oracleAccount = authority.publicKey; // placeholder for testing

    if (await exists(conn, market.address)) {
      console.log(`    → market already initialized, skipping init`);
    } else {
      const ix = await client.initializeMarketIx({
        authority: authority.publicKey,
        baseMint,
        quoteMint: QUOTE_MINT,
        baseVault,
        quoteVault,
        oracleAccount,
        params: defaultMajorMarketParams(),
        initialOracleTicks: new BN(m.initialPriceTicks.toString()) as unknown as bigint,
      });
      await send(conn, authority, [ix], [], 'initialize_market');
    }

    if (await exists(conn, book.address)) {
      console.log(`    → market_book already initialized, skipping`);
    } else {
      const ix = await client.initMarketBookIx({
        authority: authority.publicKey,
        market: market.address,
      });
      await send(conn, authority, [ix], [], 'init_market_book');
    }
  }

  // ─── Summary
  console.log(`\n✅ Bootstrap complete.`);
  console.log(`\nGlobal PDAs (record these for client config):`);
  console.log(`  insurance_fund: ${fund.address.toBase58()}`);
  console.log(`  flp_exposure:   ${flp.address.toBase58()}`);
  console.log(`  fee_tiers:      ${ft.address.toBase58()}`);
  console.log(`  quote_vault:    ${quoteVault.toBase58()}`);
  console.log(`\nPer-market PDAs (above). Markets are now LIVE.`);
  console.log(
    `\nNext: start the sequencer (scripts/sequencer.ts) to drive ` +
      `apply_fill / apply_flp_fill from MagicBlock ER tick logs.`,
  );
}

main().catch((e) => {
  console.error(`\n❌ Bootstrap failed:`, e);
  process.exit(1);
});
