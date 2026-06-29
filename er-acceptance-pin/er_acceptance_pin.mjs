// Live-ER acceptance harness — Pinocchio port (flash-book-pin).
//
// Validates the ONE thing solana-program-test structurally cannot: the real
// MagicBlock CPI delegation round-trip — i.e. PR #190's `cpi_delegate` WAVE-24i
// staging (create buffer → copy → zero → assign → CPI Delegate → close) and the
// undelegate path (`commit_and_undelegate_market_book` → `process_undelegation`,
// which re-opens the book program-owned and runs the #188 `validate_node_links`
// defense on the committed state).
//
// The pin program has NO Anchor IDL — it is a raw 1-byte-Ix-tag program — so this
// harness builds raw `TransactionInstruction`s (tag byte + LE data + account metas),
// unlike the Anchor `../er-acceptance` suite.
//
// GATED: runs only when ER_RPC + PIN_PROGRAM_ID + MARKET are set; otherwise it
// SKIPS cleanly (exit 0) so it never breaks CI (like the SBF benches skip without
// BPF_OUT_DIR). Run:
//
//   npm install
//   L1_RPC=https://api.devnet.solana.com \
//   ER_RPC=https://devnet-as.magicblock.app \
//   PIN_PROGRAM_ID=<deployed pin program id> \
//   MARKET=<an initialized, active, mark-set market pubkey> \
//     npm run acceptance
//
// Prerequisites (one-time, on L1, before running — see README): deploy the pin
// program; initialize the insurance fund; `initialize_market`; `init_market_book`;
// `update_oracle` (set a non-zero mark). This harness then drives the delegation
// round-trip on that market's book.

import { readFileSync } from "node:fs";
import { homedir } from "node:os";

// ── env gate (BEFORE importing web3.js, so an unset run skips cleanly even
//    before `npm install`) ────────────────────────────────────────────────────
const ER_RPC = process.env.ER_RPC;
const PIN_PROGRAM_ID = process.env.PIN_PROGRAM_ID;
const MARKET_STR = process.env.MARKET;
if (!ER_RPC || !PIN_PROGRAM_ID || !MARKET_STR) {
  console.log("SKIP live-ER acceptance (pin): set ER_RPC, PIN_PROGRAM_ID and MARKET to run.");
  console.log("  ER_RPC          = the ER validator endpoint (e.g. https://devnet-as.magicblock.app)");
  console.log("  PIN_PROGRAM_ID  = the deployed flash-book-pin program id");
  console.log("  MARKET          = an initialized, active, mark-set market pubkey (book already init'd)");
  process.exit(0);
}

// Imported only when actually running (so the skip path needs no node_modules).
const {
  Connection, Keypair, PublicKey, Transaction, TransactionInstruction,
  SystemProgram, sendAndConfirmTransaction, ComputeBudgetProgram,
} = await import("@solana/web3.js");
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const PID = new PublicKey(PIN_PROGRAM_ID);
const M = new PublicKey(MARKET_STR);

// The MagicBlock devnet ER validator the accounts are delegated to (pin it so the
// ER match stage lands on the SAME validator). Set ER_RPC to its endpoint.
const ER_VALIDATOR = new PublicKey(process.env.ER_VALIDATOR || "MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");

// MagicBlock / DLP ids — from the bytes hard-coded in `src/er.rs` / `er_permission.rs`
// (using the byte arrays avoids any base58 transcription error).
const DELEG = new PublicKey(Uint8Array.from([181,183,0,225,242,87,58,192,204,6,34,1,52,74,207,151,184,53,6,235,140,229,25,152,204,98,126,24,147,128,167,62]));
const MAGIC_PROGRAM = new PublicKey(Uint8Array.from([5,69,180,36,176,218,112,149,236,185,214,222,195,119,215,40,145,182,231,142,146,234,18,214,223,187,58,64,0,0,0,0]));
const MAGIC_CONTEXT = new PublicKey(Uint8Array.from([5,69,180,36,196,165,40,191,95,180,3,47,68,82,130,142,187,56,171,193,210,220,151,247,63,139,148,84,128,0,0,0]));

// ── pin Ix tags (from src/lib.rs `Ix` enum) ─────────────────────────────────
const IX = {
  PLACE_LIMIT: 2,
  DELEGATE_MARKET_BOOK: 120,
  COMMIT_MARKET_BOOK: 125,
  COMMIT_AND_UNDELEGATE_MARKET_BOOK: 126,
};
const SYS = SystemProgram.programId;

// ── keypair ─────────────────────────────────────────────────────────────────
function loadKeypair() {
  const path = process.env.KEYPAIR || `${homedir()}/.config/solana/id.json`;
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))));
}
const signer = loadKeypair();
const l1 = new Connection(L1_RPC, "confirmed");
const er = new Connection(ER_RPC, "confirmed");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ── helpers ─────────────────────────────────────────────────────────────────
const seedBuf = (x) => (Buffer.isBuffer(x) ? x : typeof x === "string" ? Buffer.from(x) : x.toBuffer());
const pda = (seeds, p = PID) => PublicKey.findProgramAddressSync(seeds.map(seedBuf), p)[0];

function le(n, bytes) { const b = Buffer.alloc(bytes); b.writeBigUInt64LE(BigInt(n) & ((1n << 64n) - 1n), 0); return b.subarray(0, bytes); }
function u32le(n) { const b = Buffer.alloc(4); b.writeUInt32LE(n >>> 0, 0); return b; }

function ix(tag, data, keys) {
  return new TransactionInstruction({ programId: PID, keys, data: Buffer.concat([Buffer.from([tag]), data]) });
}
const meta = (pk, isSigner, isWritable) => ({ pubkey: pk, isSigner, isWritable });

async function send(conn, instructions, cu = 400_000) {
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: cu }), ...instructions);
  return sendAndConfirmTransaction(conn, tx, [signer], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
}

const stages = [];
async function stage(name, fn) {
  try { const r = await fn(); stages.push({ name, ok: true }); console.log(`  ✓ ${name}`); return r; }
  catch (e) { const msg = String(e.message || e).slice(0, 200); stages.push({ name, ok: false, err: msg }); console.log(`  ✗ ${name}: ${msg}`); throw e; }
}

// ── derived accounts ────────────────────────────────────────────────────────
const BOOK = pda(["market_book", M]);
// DLP staging PDAs for the book (buffer is under PID; record/metadata under DLP).
const dlpAccts = (delegated) => ({
  buf: pda(["buffer", delegated], PID),
  rec: pda(["delegation", delegated], DELEG),
  meta: pda(["delegation-metadata", delegated], DELEG),
});

console.log(`live-ER acceptance (pin) — L1=${L1_RPC} ER=${ER_RPC}`);
console.log(`  program=${PID.toBase58()} market=${M.toBase58()} book=${BOOK.toBase58()}`);

async function run() {
  // Sanity: the book must exist and be program-owned (not already delegated).
  await stage("L1 precheck: book exists and is program-owned", async () => {
    const a = await l1.getAccountInfo(BOOK);
    if (!a) throw new Error("book account not found — run init_market_book first (see README)");
    if (!a.owner.equals(PID)) throw new Error(`book owned by ${a.owner.toBase58()} (already delegated?) — expected ${PID.toBase58()}`);
  });

  // ── L1 → DLP: delegate the book to the ER (PR #190 cpi_delegate staging) ──
  await stage("L1 delegate_market_book → DLP (WAVE-24i staging)", async () => {
    const b = dlpAccts(BOOK);
    // data: [commit_frequency_ms u32][has_validator u8][validator 32]
    const data = Buffer.concat([u32le(30_000), Buffer.from([1]), ER_VALIDATOR.toBuffer()]);
    await send(l1, [ix(IX.DELEGATE_MARKET_BOOK, data, [
      meta(signer.publicKey, true, true), // authority (= market.authority)
      meta(M, false, false),
      meta(BOOK, false, true),            // delegated_account (PDA signs internally)
      meta(PID, false, false),            // owner_program
      meta(b.buf, false, true),
      meta(b.rec, false, true),
      meta(b.meta, false, true),
      meta(SYS, false, false),
      meta(DELEG, false, false),
    ])]);
  });
  await sleep(4000); // let the ER validator pick up the delegated book

  // ── ER: mutate the delegated book ON the rollup (rest a bid) ──────────────
  await stage("ER place_limit_order (rest a bid on the delegated book)", async () => {
    // [side=0 bid][size u64][limit u64][expires u64][flags u8][sub_index u8]
    const data = Buffer.concat([Buffer.from([0]), le(1, 8), le(1, 8), le(0, 8), Buffer.from([0, 0])]);
    await send(er, [ix(IX.PLACE_LIMIT, data, [
      meta(signer.publicKey, true, false), // trader
      meta(M, false, false),
      meta(BOOK, false, true),
    ])]);
  });

  // ── ER → L1: commit the book snapshot (no undelegate) ─────────────────────
  await stage("ER commit_market_book → L1 snapshot", async () => {
    await send(er, [ix(IX.COMMIT_MARKET_BOOK, Buffer.alloc(0), [
      meta(signer.publicKey, true, false), // payer
      meta(BOOK, false, true),             // committed
      meta(MAGIC_CONTEXT, false, true),
      meta(MAGIC_PROGRAM, false, false),
    ])]);
  });
  await sleep(5000); // commit propagation to L1

  await stage("L1 assert book is delegated (owned by the DLP)", async () => {
    const a = await l1.getAccountInfo(BOOK);
    if (!a || !a.owner.equals(DELEG)) throw new Error(`book owner ${a?.owner?.toBase58()} != DLP (commit should keep it delegated)`);
  });

  // ── ER → L1: commit-and-undelegate → process_undelegation finalizes ───────
  await stage("ER commit_and_undelegate_market_book → L1 finalize", async () => {
    await send(er, [ix(IX.COMMIT_AND_UNDELEGATE_MARKET_BOOK, Buffer.alloc(0), [
      meta(signer.publicKey, true, false),
      meta(BOOK, false, true),
      meta(MAGIC_CONTEXT, false, true),
      meta(MAGIC_PROGRAM, false, false),
    ])]);
  });
  await sleep(8000); // undelegation callback (process_undelegation) lands on L1

  await stage("L1 assert book back program-owned + non-empty (validate_node_links accepted)", async () => {
    const a = await l1.getAccountInfo(BOOK);
    if (!a) throw new Error("book vanished after undelegate");
    if (!a.owner.equals(PID)) throw new Error(`book owner ${a.owner.toBase58()} != program after undelegate (round-trip incomplete)`);
    if (a.data.length < 8 || a.data.subarray(0, 4).every((x) => x === 0)) throw new Error("book data looks empty/uninitialized after undelegate");
  });
}

run()
  .then(() => {
    const ok = stages.filter((s) => s.ok).length;
    console.log(`\nlive-ER acceptance (pin): ${ok}/${stages.length} stages green`);
    process.exit(ok === stages.length ? 0 : 1);
  })
  .catch(() => {
    const ok = stages.filter((s) => s.ok).length;
    console.log(`\nlive-ER acceptance (pin): FAILED at stage ${ok + 1}/${stages.length}`);
    process.exit(1);
  });
