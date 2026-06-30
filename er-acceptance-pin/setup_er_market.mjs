// One-time L1 setup for the live-ER acceptance run: create base+quote mints, a
// market, its book, and set a mark — then print MARKET / BASE_MINT / QUOTE_MINT to
// feed into er_acceptance_pin.mjs. Assumes the insurance-fund singleton already
// exists under PIN_PROGRAM_ID (the local_exercise devnet run creates it; its
// authority must equal this signer for the CR-1 gate on initialize_market).
//
//   PIN_PROGRAM_ID=<id> KEYPAIR=<funded wallet> L1_RPC=https://api.devnet.solana.com \
//     node setup_er_market.mjs
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
const {
  Connection, Keypair, PublicKey, Transaction, TransactionInstruction,
  SystemProgram, sendAndConfirmTransaction,
} = await import("@solana/web3.js");

const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const PID = new PublicKey(process.env.PIN_PROGRAM_ID);
const TOKEN = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const SYS = SystemProgram.programId;
const signer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(
  readFileSync(process.env.KEYPAIR || `${homedir()}/.config/solana/id.json`, "utf8"))));
const c = new Connection(L1_RPC, "confirmed");

const seedBuf = (x) => (Buffer.isBuffer(x) ? x : typeof x === "string" ? Buffer.from(x) : x.toBuffer());
const pda = (seeds, p = PID) => PublicKey.findProgramAddressSync(seeds.map(seedBuf), p)[0];
const ix = (tag, data, keys) => new TransactionInstruction({ programId: PID, keys, data: Buffer.concat([Buffer.from([tag]), data]) });
const meta = (pk, s, w) => ({ pubkey: pk, isSigner: s, isWritable: w });
const le = (n, b) => { const x = Buffer.alloc(b); x.writeBigUInt64LE(BigInt(n)); return x; };
const u32 = (n) => { const x = Buffer.alloc(4); x.writeUInt32LE(n >>> 0); return x; };
const i32 = (n) => { const x = Buffer.alloc(4); x.writeInt32LE(n | 0); return x; };
const send = (instrs, signers) => sendAndConfirmTransaction(c, new Transaction().add(...instrs), signers, { commitment: "confirmed", skipPreflight: true });

async function createMint(decimals) {
  const mint = Keypair.generate();
  const rent = await c.getMinimumBalanceForRentExemption(82);
  const create = SystemProgram.createAccount({ fromPubkey: signer.publicKey, newAccountPubkey: mint.publicKey, lamports: rent, space: 82, programId: TOKEN });
  // InitializeMint2 (tag 20): [decimals u8][mint_auth 32][freeze_opt u8=0]
  const data = Buffer.concat([Buffer.from([20, decimals]), signer.publicKey.toBuffer(), Buffer.from([0])]);
  const init = new TransactionInstruction({ programId: TOKEN, keys: [meta(mint.publicKey, false, true)], data });
  await send([create, init], [signer, mint]);
  return mint.publicKey;
}

const insurance = pda(["insurance_fund"]);
const base = await createMint(9);
const quote = await createMint(6);
const market = pda(["market", base, quote]);
const book = pda(["market_book", market]);

// initialize_market (tag 11): tick u64, mark u64, taker_fee u32, maker_rebate i32, min_base u64, max_oi u64, mmr u32
const mdata = Buffer.concat([le(1, 8), le(100000, 8), u32(10), i32(2), le(1, 8), le(1000000000, 8), u32(500)]);
await send([ix(11, mdata, [
  meta(signer.publicKey, true, true), meta(market, false, true),
  meta(base, false, false), meta(quote, false, false),
  meta(insurance, false, false), meta(SYS, false, false),
])], [signer]);

// init_market_book (tag 81)
await send([ix(81, Buffer.alloc(0), [
  meta(signer.publicKey, true, true), meta(market, false, false),
  meta(base, false, false), meta(quote, false, false),
  meta(book, false, true), meta(SYS, false, false),
])], [signer]);

// update_oracle (tag 15): mark u64
await send([ix(15, le(100000, 8), [meta(signer.publicKey, true, false), meta(market, false, true)])], [signer]);

console.log("ER market ready:");
console.log(`MARKET=${market.toBase58()}`);
console.log(`BASE_MINT=${base.toBase58()}`);
console.log(`QUOTE_MINT=${quote.toBase58()}`);
