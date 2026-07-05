// HLP increment 2 LIVE acceptance: the pool auto-quotes.
//   1. fresh armed market (LOW oracle so the ~8M global FLP capital yields
//      non-zero per-level size) + book + ring
//   2. flp_refresh_quotes -> the pool posts a deterministic two-sided ladder
//   3. a taker crosses the pool's best ask -> ring records an FLP-maker fill
//   4. flp_refresh_quotes again -> cancels stale + reposts (idempotent)
// Verifies the quoter->book->cross pipeline runs on the real chain.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const sys = SystemProgram.programId;
const pda = (s, p = PID) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];
const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");
const FLP = pda(["flp_exposure"]);
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");

const send = async (ix, extra = []) => {
  const { blockhash } = await l1.getLatestBlockhash("confirmed");
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: signer.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true });
};
let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

console.log(`HLP increment-2 (auto-quoter) live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);
if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60; // ref market predates the init-time staleness bound
const ORACLE = 1000; // low so the ~8M global FLP capital yields non-zero levels
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);

console.log("setup: armed market (oracle=1000) + book + ring");
await send(await program.methods.initializeMarket(ref.params, new BN(ORACLE)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
await send(await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await send(await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
console.log(`  market ${M.toBase58()}\n`);

const ring = async () => { const d = (await l1.getAccountInfo(FC)).data; return { produced: Number(d.readBigUInt64LE(8)), settled: Number(d.readBigUInt64LE(16)) }; };

console.log("1) pool AUTO-QUOTES (flp_refresh_quotes) — deterministic ladder from oracle + inventory");
const sig1 = await send(await program.methods.flpRefreshQuotes().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, flpExposure: FLP }).instruction());
ok(!!sig1, `pool posted its on-book quote ladder LIVE — ${sig1.slice(0, 16)}…`);

console.log("2) taker crosses the pool's best ask (bid 10× oracle to guarantee a cross)");
await send(await program.methods.placeTakerOrderV2(0, new BN(1), new BN(ORACLE * 10), 0, new BN(0), 0).accountsPartial({ trader: signer.publicKey, market: M, marketBook: BOOK, traderState: pda(["trader_state", signer.publicKey]), position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction());
const r = await ring();
ok(r.produced >= 1, `a taker crossed an AUTO-QUOTED FLP level → ring-committed FLP-maker fill (produced=${r.produced})`);

console.log("3) re-quote (flp_refresh_quotes again) — cancel stale + repost");
// Quotes still resting from stage 1 are rate-limited for FLP_REFRESH_MIN_SLOTS
// (50 slots ≈ 20s); wait out the window so this exercises the STALE re-quote.
await new Promise((r) => setTimeout(r, 25_000));
const sig3 = await send(await program.methods.flpRefreshQuotes().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, flpExposure: FLP }).instruction());
ok(!!sig3, `pool re-quoted (cancel stale + repost) LIVE — ${sig3.slice(0, 16)}…`);

console.log(`\n${fail === 0 ? "✅ HLP AUTO-QUOTER LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
