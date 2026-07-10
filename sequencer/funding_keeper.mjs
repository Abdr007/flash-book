// Funding keeper — the permissionless service half of the funding mechanism.
//
// `crank_funding` advances a market's `cum_funding_index` from the live
// (mark, oracle) premium. It is permissionless, rate-capped, oracle-gated, and
// clamps Δt to one funding period — the on-chain proof suite
// (`funding_index_delta_is_gated_and_safe`) guarantees no caller-reachable input
// drives the index past those bounds. So a keeper cannot harm the protocol; it
// only keeps the index current. This service cranks every configured market on an
// interval.
//
// The keeper NEVER moves value: `crank_funding` only advances the index. Positions
// realize funding later, per position, through the Kani-proven `settle_funding` /
// `route_funding` path (Δcollateral == −Δresidual). Running or not running this
// keeper cannot mint or burn value — a stale index just means funding is applied
// in fewer, larger steps (each still clamped to one period).
//
// Safe to run many instances: same-second cranks are no-ops on chain, and the
// worst case of two keepers racing is one wasted (reverted / zero-delta) tx.
//
// Usage:
//   L1_RPC=<rpc> MARKETS=<mkt1,mkt2,…> node funding_keeper.mjs
//   optional: KEYPAIR=~/.config/solana/id.json  INTERVAL_MS=15000  ONCE=1
//             MIN_DT_SECONDS=5   (skip a market cranked within the last N seconds)
//             DRY_RUN=1          (log what would be cranked; send nothing)
//
// The keypair only pays fees — crank_funding takes no privileged signer.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";

const { AnchorProvider, Program, Wallet } = anchor;

// ── config ──────────────────────────────────────────────────────────────────
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const MARKETS = (process.env.MARKETS || "").split(",").map((s) => s.trim()).filter(Boolean);
const INTERVAL_MS = Number(process.env.INTERVAL_MS || 15000);
const MIN_DT_SECONDS = Number(process.env.MIN_DT_SECONDS || 0);
const ONCE = !!process.env.ONCE;
const DRY_RUN = !!process.env.DRY_RUN;

if (MARKETS.length === 0) {
  console.error("MARKETS=<comma-separated market pubkeys> is required");
  process.exit(1);
}

const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const keypairPath = process.env.KEYPAIR || `${os.homedir()}/.config/solana/id.json`;
const payer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(keypairPath))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(payer), { commitment: "confirmed" }));

const markets = MARKETS.map((m) => new PublicKey(m));

// Whether a market is worth cranking now: skip if it was cranked within
// MIN_DT_SECONDS (saves fees; the on-chain Δt clamp makes this purely an
// optimization, never a correctness requirement).
function shouldCrank(marketAcct, nowUnix) {
  if (MIN_DT_SECONDS <= 0) return true;
  const last = Number(marketAcct.lastFundingCrankUnix ?? 0);
  if (last === 0) return true; // never cranked (first call only seeds the clock)
  return nowUnix - last >= MIN_DT_SECONDS;
}

async function crankOne(marketPk) {
  const nowUnix = Math.floor(Date.now() / 1000);
  let acct;
  try {
    acct = await program.account.marketAccount.fetch(marketPk);
  } catch (e) {
    console.warn(`[skip] ${marketPk.toBase58()}: cannot fetch market (${e.message})`);
    return;
  }
  if (!shouldCrank(acct, nowUnix)) {
    console.log(`[hold] ${marketPk.toBase58()}: cranked <${MIN_DT_SECONDS}s ago`);
    return;
  }
  if (DRY_RUN) {
    console.log(`[dry ] ${marketPk.toBase58()}: would crank_funding (last=${acct.lastFundingCrankUnix})`);
    return;
  }
  try {
    const sig = await program.methods
      .crankFunding()
      .accounts({ caller: payer.publicKey, market: marketPk })
      .rpc();
    console.log(`[crank] ${marketPk.toBase58()}: ${sig}`);
  } catch (e) {
    console.warn(`[fail] ${marketPk.toBase58()}: ${e.message}`);
  }
}

async function pass() {
  // Cranks are independent per market — run them concurrently, isolate failures.
  await Promise.allSettled(markets.map((m) => crankOne(m)));
}

async function main() {
  console.log(
    `funding_keeper: ${markets.length} market(s) on ${L1_RPC}` +
      `${DRY_RUN ? " [DRY_RUN]" : ""}${ONCE ? " [ONCE]" : ` every ${INTERVAL_MS}ms`}`
  );
  await pass();
  if (ONCE) return;
  // eslint-disable-next-line no-constant-condition
  while (true) {
    await new Promise((r) => setTimeout(r, INTERVAL_MS));
    await pass();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
