// update_oracle_from_pyth POSITIVE proof on devnet: discover a live Full-verified
// Pyth PriceUpdateV2, build a market whose oracle_config binds that feed, and crank
// the on-chain pull-oracle update. Prints the real devnet signature.
//
//   PIN_PROGRAM_ID=<id> KEYPAIR=<wallet> node pyth_crank.mjs
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
const { Connection, Keypair, PublicKey, Transaction, TransactionInstruction, SystemProgram, sendAndConfirmTransaction } = await import("@solana/web3.js");

const RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const PID = new PublicKey(process.env.PIN_PROGRAM_ID);
const RECEIVER = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const TOKEN = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const SYS = SystemProgram.programId;
const signer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(process.env.KEYPAIR || `${homedir()}/.config/solana/id.json`, "utf8"))));
const c = new Connection(RPC, "confirmed");

const seedBuf = (x) => (Buffer.isBuffer(x) ? x : typeof x === "string" ? Buffer.from(x) : x.toBuffer());
const pda = (seeds, p = PID) => PublicKey.findProgramAddressSync(seeds.map(seedBuf), p)[0];
const ix = (tag, data, keys) => new TransactionInstruction({ programId: PID, keys, data: Buffer.concat([Buffer.from([tag]), data]) });
const meta = (pk, s, w) => ({ pubkey: pk, isSigner: s, isWritable: w });
const le = (n, b) => { const x = Buffer.alloc(b); x.writeBigUInt64LE(BigInt(n)); return x; };
const u32 = (n) => { const x = Buffer.alloc(4); x.writeUInt32LE(n >>> 0); return x; };
const i32 = (n) => { const x = Buffer.alloc(4); x.writeInt32LE(n | 0); return x; };
const send = (instrs, signers) => sendAndConfirmTransaction(c, new Transaction().add(...instrs), signers, { commitment: "confirmed", skipPreflight: true });

// ── 1. discover a live Full-verified PriceUpdateV2 ──────────────────────────
// PriceUpdateV2 layout: disc[8] write_authority[32] verification_level@40 (1=Full)
// price_message@41 { feed_id[32], price i64, conf u64, exponent i32, publish_time i64, ... }
const FEED = new PublicKey(process.env.PYTH_FEED || "1121JSUgoCT514dycHuZRjPdDnXd1gvQ3wCixt8on1m");
const fa = await c.getAccountInfo(FEED, "confirmed");
if (!fa) { console.log("feed account not found:", FEED.toBase58()); process.exit(1); }
if (!fa.owner.equals(RECEIVER)) { console.log("feed not receiver-owned:", fa.owner.toBase58()); process.exit(1); }
const d0 = fa.data;
const best = {
  pubkey: FEED,
  feedId: d0.subarray(41, 73),
  price: d0.readBigInt64LE(73),
  exponent: d0.readInt32LE(89),
  age: Math.floor(Date.now() / 1000) - Number(d0.readBigInt64LE(93)),
  verif: d0[40],
};
console.log(`feed: ${FEED.toBase58()}  owner=${fa.owner.toBase58()}  verif@40=${best.verif}  price=${best.price} exp=${best.exponent} age=${best.age}s`);
console.log(`feed_id=${Buffer.from(best.feedId).toString("hex")}`);

// ── 2. build a market whose oracle_config binds this feed ───────────────────
const insurance = pda(["insurance_fund"]);
async function createMint(dec) {
  const mint = Keypair.generate();
  const rent = await c.getMinimumBalanceForRentExemption(82);
  const create = SystemProgram.createAccount({ fromPubkey: signer.publicKey, newAccountPubkey: mint.publicKey, lamports: rent, space: 82, programId: TOKEN });
  const data = Buffer.concat([Buffer.from([20, dec]), signer.publicKey.toBuffer(), Buffer.from([0])]);
  await send([create, new TransactionInstruction({ programId: TOKEN, keys: [meta(mint.publicKey, false, true)], data })], [signer, mint]);
  return mint.publicKey;
}
const base = await createMint(9), quote = await createMint(6);
const market = pda(["market", base, quote]);
const oracleCfg = pda(["oracle_config", market]);
const envelope = pda(["envelope", market]);

// tick_decimals = -exponent ⇒ pyth_price_to_ticks scale = 0 ⇒ new_ticks == price.
// Init the market mark to exactly that, so update_oracle_from_pyth's envelope move is 0.
const td = -best.exponent;
const newTicks = best.price; // BigInt, == pyth_price_to_ticks(price, exp, -exp)
console.log(`tick_decimals=${td}  expected mark(ticks)=${newTicks}`);
// initialize_market (mark = the pyth-derived ticks; crank re-sets the same value)
await send([ix(11, Buffer.concat([le(1, 8), le(newTicks, 8), u32(10), i32(2), le(1, 8), le(1000000000, 8), u32(500)]), [
  meta(signer.publicKey, true, true), meta(market, false, true), meta(base, false, false), meta(quote, false, false), meta(insurance, false, false), meta(SYS, false, false),
])], [signer]);

// init_market_oracle_config (tag 58): feed_id[32], max_staleness u32, max_confidence u32, tick_decimals i8 (source set to PYTH by the handler)
// max_staleness generous (devnet feeds are old-but-real); max_confidence 1000 (cap).
const oc = Buffer.concat([best.feedId, u32(4_000_000_000), u32(1000), Buffer.from([td & 0xff])]);
await send([ix(58, oc, [meta(signer.publicKey, true, true), meta(market, false, false), meta(oracleCfg, false, true), meta(SYS, false, false)])], [signer]);

// set_envelope_config (tag 56, 44B): move_bps, dt, funding, maintenance, liq_fee, min_liq, min_mm
const env = Buffer.concat([u32(1), le(1, 8), le(0, 8) /*i64 funding=0*/, u32(500), u32(0), le(0, 8), le(1, 8)]);
await send([ix(56, env, [meta(signer.publicKey, true, true), meta(market, false, false), meta(envelope, false, true), meta(SYS, false, false)])], [signer]);

// ── 3. crank update_oracle_from_pyth (tag 115) ──────────────────────────────
// accounts: [caller(s), market(w), oracle_config(r), price_update(r), envelope_config(r)]
try {
  const sig = await send([ix(115, Buffer.alloc(0), [
    meta(signer.publicKey, true, false), meta(market, false, true), meta(oracleCfg, false, false), meta(best.pubkey, false, false), meta(envelope, false, false),
  ])], [signer]);
  console.log(`\n✓ update_oracle_from_pyth (positive) on devnet:`);
  console.log(`  https://explorer.solana.com/tx/${sig}?cluster=devnet`);
  console.log(`  market=${market.toBase58()}  feed=${best.pubkey.toBase58()}`);
} catch (e) {
  console.log("crank failed:", String(e.message || e).slice(0, 160));
  if (e.getLogs) { try { console.log("logs:", (await e.getLogs(c) || []).join("\n  ")); } catch {} }
  if (e.transactionLogs) console.log("logs:", e.transactionLogs.join("\n  "));
  process.exit(1);
}
