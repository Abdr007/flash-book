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
const BASE_MINT_STR = process.env.BASE_MINT;
const QUOTE_MINT_STR = process.env.QUOTE_MINT;
if (!ER_RPC || !PIN_PROGRAM_ID || !MARKET_STR || !BASE_MINT_STR || !QUOTE_MINT_STR) {
  console.log("SKIP live-ER acceptance (pin): set ER_RPC, PIN_PROGRAM_ID, MARKET, BASE_MINT and QUOTE_MINT to run.");
  console.log("  ER_RPC          = the ER validator endpoint (e.g. https://devnet-as.magicblock.app)");
  console.log("  PIN_PROGRAM_ID  = the deployed flash-book-pin program id");
  console.log("  MARKET          = an initialized, active, mark-set market pubkey (book already init'd)");
  console.log("  BASE_MINT       = the base mint used to derive MARKET");
  console.log("  QUOTE_MINT      = the quote mint used to derive MARKET");
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
const BASE_MINT = new PublicKey(BASE_MINT_STR);
const QUOTE_MINT = new PublicKey(QUOTE_MINT_STR);

// The MagicBlock devnet ER validator the accounts are delegated to (pin it so the
// ER match stage lands on the SAME validator). Set ER_RPC to its endpoint.
const ER_VALIDATOR = new PublicKey(process.env.ER_VALIDATOR || "MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");

// MagicBlock / DLP ids — from the bytes hard-coded in `src/er.rs` / `er_permission.rs`
// (using the byte arrays avoids any base58 transcription error).
const DELEG = new PublicKey(Uint8Array.from([181,183,0,225,242,87,58,192,204,6,34,1,52,74,207,151,184,53,6,235,140,229,25,152,204,98,126,24,147,128,167,62]));
const MAGIC_PROGRAM = new PublicKey(Uint8Array.from([5,69,180,36,176,218,112,149,236,185,214,222,195,119,215,40,145,182,231,142,146,234,18,214,223,187,58,64,0,0,0,0]));
const MAGIC_CONTEXT = new PublicKey(Uint8Array.from([5,69,180,36,196,165,40,191,95,180,3,47,68,82,130,142,187,56,171,193,210,220,151,247,63,139,148,84,128,0,0,0]));
const PERMISSION_PROGRAM = new PublicKey(Uint8Array.from([136,161,10,196,33,152,1,214,246,106,29,60,6,152,192,102,169,175,212,217,180,252,231,71,151,141,209,5,168,212,103,82]));
const EPHEMERAL_VAULT = new PublicKey(Uint8Array.from([5,69,180,36,224,197,24,97,240,41,76,112,66,34,84,78,202,127,133,79,194,135,136,166,123,118,113,80,62,224,143,184]));

// ── pin Ix tags (from src/lib.rs `Ix` enum) ─────────────────────────────────
const IX = {
  PLACE_LIMIT: 2,
  STAMP_BOOK_LIVENESS_BASELINE: 116,
  INIT_BOOK_PERMISSION: 117,
  SET_BOOK_PRIVACY: 118,
  CLOSE_BOOK_PERMISSION: 119,
  DELEGATE_MARKET_BOOK: 120,
  DELEGATE_MARKET: 122,
  COMMIT_MARKET_BOOK: 125,
  COMMIT_AND_UNDELEGATE_MARKET_BOOK: 126,
  INIT_FILL_COMMITMENT: 127,
  DELEGATE_FILL_COMMITMENT: 128,
  COMMIT_FILL_COMMITMENT: 129,
  COMMIT_AND_UNDELEGATE_FILL_COMMITMENT: 130,
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

// 1.0M CU default (under the 1.4M/tx cap): the delegate stages copy the full ~10KB
// book into a buffer + zero + assign + CPI Delegate, which can exceed a 400k budget.
// A higher limit is free here (no priority price set) and avoids a CU-exhaustion fail.
async function send(conn, instructions, cu = 1_000_000) {
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: cu }), ...instructions);
  return sendAndConfirmTransaction(conn, tx, [signer], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
}

const stages = [];
async function stage(name, fn) {
  try {
    const r = await fn();
    stages.push({ name, ok: true, sig: typeof r === "string" ? r : undefined });
    console.log(`  ✓ ${name}${typeof r === "string" ? `  https://explorer.solana.com/tx/${r}?cluster=devnet` : ""}`);
    return r;
  }
  catch (e) { const msg = String(e.message || e).slice(0, 200); stages.push({ name, ok: false, err: msg }); console.log(`  ✗ ${name}: ${msg}`); throw e; }
}

async function expectReject(name, fn, needle) {
  let sig;
  try {
    sig = await fn();
  } catch (e) {
    const msg = String(e.message || e);
    if (!msg.includes(needle)) {
      stages.push({ name, ok: false, err: msg.slice(0, 200) });
      throw e;
    }
    stages.push({ name, ok: true, note: `correctly rejected: ${needle}` });
    console.log(`  ✓ ${name}  (correctly rejected: ${needle})`);
    return;
  }
  stages.push({ name, ok: false, err: `expected rejection containing ${needle}, got success ${sig}` });
  throw new Error(`expected rejection containing ${needle}, got success ${sig}`);
}

// ── derived accounts ────────────────────────────────────────────────────────
const BOOK = pda(["market_book", M]);
const FILL_COMMITMENT = pda(["fill_commit", M]);
const PERMISSION = pda(["permission:", BOOK], PERMISSION_PROGRAM);
// DLP staging PDAs for the book (buffer is under PID; record/metadata under DLP).
const dlpAccts = (delegated) => ({
  buf: pda(["buffer", delegated], PID),
  rec: pda(["delegation", delegated], DELEG),
  meta: pda(["delegation-metadata", delegated], DELEG),
});

console.log(`live-ER acceptance (pin) — L1=${L1_RPC} ER=${ER_RPC}`);
console.log(`  program=${PID.toBase58()} market=${M.toBase58()} book=${BOOK.toBase58()} fill_commitment=${FILL_COMMITMENT.toBase58()}`);

async function run() {
  const delegateData = Buffer.concat([u32le(30_000), Buffer.from([1]), ER_VALIDATOR.toBuffer()]);
  const commit = (tag, acct) => ix(tag, Buffer.alloc(0), [
    meta(signer.publicKey, true, false),
    meta(acct, false, true),
    meta(MAGIC_CONTEXT, false, true),
    meta(MAGIC_PROGRAM, false, false),
  ]);

  await stage("L1 precheck: market/book exist and are program-owned", async () => {
    const m = await l1.getAccountInfo(M);
    if (!m) throw new Error("market account not found");
    if (!m.owner.equals(PID)) throw new Error(`market owned by ${m.owner.toBase58()} (already delegated?) — expected ${PID.toBase58()}`);
    const a = await l1.getAccountInfo(BOOK);
    if (!a) throw new Error("book account not found — run init_market_book first (see README)");
    if (!a.owner.equals(PID)) throw new Error(`book owned by ${a.owner.toBase58()} (already delegated?) — expected ${PID.toBase58()}`);
  });

  await stage("L1 ensure init_fill_commitment ring exists", async () => {
    const existing = await l1.getAccountInfo(FILL_COMMITMENT);
    if (existing) {
      if (!existing.owner.equals(PID)) throw new Error(`fill commitment owned by ${existing.owner.toBase58()} before delegation`);
      return;
    }
    return send(l1, [ix(IX.INIT_FILL_COMMITMENT, Buffer.alloc(0), [
      meta(signer.publicKey, true, true),
      meta(M, false, true),
      meta(FILL_COMMITMENT, false, true),
      meta(SYS, false, false),
    ])]);
  });

  await stage("L1 delegate_market_book → DLP (WAVE-24i staging)", async () => {
    const b = dlpAccts(BOOK);
    return send(l1, [ix(IX.DELEGATE_MARKET_BOOK, delegateData, [
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

  await expectReject("L1 stamp_book_liveness_baseline already stamped by delegate", async () => {
    return send(l1, [ix(IX.STAMP_BOOK_LIVENESS_BASELINE, Buffer.alloc(0), [
      meta(signer.publicKey, true, false),
      meta(M, false, true),
      meta(BOOK, false, false),
    ])]);
  }, "0xc9");

  await stage("L1 init_book_permission on delegated book", async () => {
    return send(l1, [ix(IX.INIT_BOOK_PERMISSION, Buffer.alloc(0), [
      meta(signer.publicKey, true, false),
      meta(M, false, false),
      meta(BOOK, false, true),
      meta(PERMISSION, false, true),
      meta(EPHEMERAL_VAULT, false, true),
      meta(MAGIC_PROGRAM, false, false),
      meta(PERMISSION_PROGRAM, false, false),
    ])]);
  });

  await stage("L1 set_book_privacy private allow signer", async () => {
    const data = Buffer.concat([Buffer.from([1, 1]), signer.publicKey.toBuffer()]);
    return send(l1, [ix(IX.SET_BOOK_PRIVACY, data, [
      meta(signer.publicKey, true, false),
      meta(M, false, false),
      meta(BOOK, false, true),
      meta(PERMISSION, false, true),
      meta(EPHEMERAL_VAULT, false, true),
      meta(MAGIC_PROGRAM, false, false),
      meta(PERMISSION_PROGRAM, false, false),
    ])]);
  });

  await stage("L1 close_book_permission", async () => {
    return send(l1, [ix(IX.CLOSE_BOOK_PERMISSION, Buffer.alloc(0), [
      meta(signer.publicKey, true, false),
      meta(M, false, false),
      meta(BOOK, false, true),
      meta(PERMISSION, false, true),
      meta(EPHEMERAL_VAULT, false, true),
      meta(MAGIC_PROGRAM, false, false),
      meta(PERMISSION_PROGRAM, false, false),
    ])]);
  });

  await stage("L1 delegate_fill_commitment → DLP", async () => {
    const b = dlpAccts(FILL_COMMITMENT);
    return send(l1, [ix(IX.DELEGATE_FILL_COMMITMENT, delegateData, [
      meta(signer.publicKey, true, true),
      meta(M, false, false),
      meta(FILL_COMMITMENT, false, true),
      meta(PID, false, false),
      meta(b.buf, false, true),
      meta(b.rec, false, true),
      meta(b.meta, false, true),
      meta(SYS, false, false),
      meta(DELEG, false, false),
    ])]);
  });

  await stage("L1 delegate_market → DLP (market last)", async () => {
    const b = dlpAccts(M);
    return send(l1, [ix(IX.DELEGATE_MARKET, delegateData, [
      meta(signer.publicKey, true, true),
      meta(M, false, true),
      meta(BASE_MINT, false, false),
      meta(QUOTE_MINT, false, false),
      meta(PID, false, false),
      meta(b.buf, false, true),
      meta(b.rec, false, true),
      meta(b.meta, false, true),
      meta(SYS, false, false),
      meta(DELEG, false, false),
    ])]);
  });

  await sleep(5000); // let the ER validator pick up the delegated writable set

  await stage("ER place_limit_order (rest a bid on the delegated book)", async () => {
    // [side=0 bid][size u64][limit u64][expires u64][flags u8][sub_index u8]
    const data = Buffer.concat([Buffer.from([0]), le(1, 8), le(1, 8), le(0, 8), Buffer.from([0, 0])]);
    return send(er, [ix(IX.PLACE_LIMIT, data, [
      meta(signer.publicKey, true, false), // trader
      meta(M, false, false),
      meta(BOOK, false, true),
    ])]);
  });

  await stage("ER commit_market_book → L1 snapshot", async () => {
    return send(er, [commit(IX.COMMIT_MARKET_BOOK, BOOK)]);
  });

  await stage("ER commit_fill_commitment → L1 snapshot", async () => {
    return send(er, [commit(IX.COMMIT_FILL_COMMITMENT, FILL_COMMITMENT)]);
  });
  await sleep(5000); // commit propagation to L1

  await stage("L1 assert book/ring/market are still delegated after commit-only", async () => {
    for (const [name, key] of [["book", BOOK], ["fill_commitment", FILL_COMMITMENT], ["market", M]]) {
      const a = await l1.getAccountInfo(key);
      if (!a || !a.owner.equals(DELEG)) throw new Error(`${name} owner ${a?.owner?.toBase58()} != DLP`);
    }
  });

  await stage("ER commit_and_undelegate_fill_commitment → L1 finalize", async () => {
    return send(er, [commit(IX.COMMIT_AND_UNDELEGATE_FILL_COMMITMENT, FILL_COMMITMENT)]);
  });
  await sleep(8000);

  await stage("L1 assert fill_commitment back program-owned", async () => {
    const a = await l1.getAccountInfo(FILL_COMMITMENT);
    if (!a || !a.owner.equals(PID)) throw new Error(`fill_commitment owner ${a?.owner?.toBase58()} != program after undelegate`);
  });

  await stage("ER commit_and_undelegate_market_book → L1 finalize", async () => {
    return send(er, [commit(IX.COMMIT_AND_UNDELEGATE_MARKET_BOOK, BOOK)]);
  });
  await sleep(8000);

  await stage("L1 assert book back program-owned + non-empty (validate_node_links accepted)", async () => {
    const a = await l1.getAccountInfo(BOOK);
    if (!a) throw new Error("book vanished after undelegate");
    if (!a.owner.equals(PID)) throw new Error(`book owner ${a.owner.toBase58()} != program after undelegate (round-trip incomplete)`);
    if (a.data.length < 8 || a.data.subarray(0, 4).every((x) => x === 0)) throw new Error("book data looks empty/uninitialized after undelegate");
  });

  await stage("ER commit_and_undelegate_market → L1 finalize", async () => {
    return send(er, [commit(IX.COMMIT_AND_UNDELEGATE_MARKET_BOOK, M)]);
  });
  await sleep(8000);

  await stage("L1 assert market back program-owned", async () => {
    const a = await l1.getAccountInfo(M);
    if (!a || !a.owner.equals(PID)) throw new Error(`market owner ${a?.owner?.toBase58()} != program after undelegate`);
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
