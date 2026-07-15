// liquidity pool permissionless auto-quoter + rate-limit LIVE acceptance (devnet).
//   1. a NON-authority keeper refreshes the pool's quotes -> PERMISSIONLESS
//   2. the same keeper immediately re-quotes -> REJECTED (RefreshTooSoon): the
//      pool's quotes are still fresh, so the book can't be churned
//   3. after LP_REFRESH_MIN_SLOTS (~10 slots) elapse -> re-quote ALLOWED
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
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
const LP = pda(["lp_exposure"]);
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");

const sendAs = async (kp, ix, commitment = "confirmed") => {
  const { blockhash } = await l1.getLatestBlockhash(commitment);
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: kp.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [kp], { commitment, skipPreflight: true });
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

console.log(`liquidity pool permissionless + rate-limit live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);
if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60; // ref market predates the init-time staleness bound
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);

console.log("setup: armed market (oracle=1000) + book + ring, then fund a NON-authority keeper");
await sendAs(signer, await program.methods.initializeMarket(ref.params, new BN(1000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
const keeper = Keypair.generate();
await sendAs(signer, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: keeper.publicKey, lamports: 200_000_000 }));
console.log(`  market ${M.toBase58()}  keeper ${keeper.publicKey.toBase58()} (NOT the authority ${signer.publicKey.toBase58().slice(0, 8)}…)\n`);

const refreshIx = (who) => program.methods.lpRefreshQuotes().accountsPartial({ authority: who, market: M, marketBook: BOOK, lpExposure: LP }).instruction();

// RefreshTooSoon = error 2315 = 0x90b. A confirmation failure surfaces only the
// tx signature; fetch its on-chain logs to confirm the SPECIFIC program error.
const isRateLimited = async (e) => {
  const s = [e.message, JSON.stringify(e.logs || [])].join(" ");
  if (/0x207b|0x90b|RefreshTooSoon|Error Number: 8315|Error Number: 2315/i.test(s)) return true;
  const m = String(e.message || e).match(/Transaction\s+([1-9A-HJ-NP-Za-km-z]{40,})/);
  if (m) {
    try {
      const t = await l1.getTransaction(m[1], { maxSupportedTransactionVersion: 0, commitment: "confirmed" });
      const logs = (t?.meta?.logMessages || []).join(" ");
      if (/0x207b|0x90b|RefreshTooSoon|Error Number: 8315|Error Number: 2315/i.test(logs)) return true;
    } catch {}
  }
  return false;
};

console.log("1) NON-authority keeper refreshes → PERMISSIONLESS (processed commitment)");
const sig1 = await sendAs(keeper, await refreshIx(keeper.publicKey), "processed");
ok(!!sig1, `a non-authority keeper posted the pool's ladder — permissionless — ${sig1.slice(0, 16)}…`);

console.log("2) same keeper re-quotes with minimal delay → rate-limited (RefreshTooSoon)");
let rejected = false, rejErr = "";
try { await sendAs(keeper, await refreshIx(keeper.publicKey), "processed"); } catch (e) { rejected = await isRateLimited(e); rejErr = String(e.message || e).slice(0, 70); }
ok(rejected, `re-quote REJECTED while quotes fresh — the book can't be churned${rejected ? "" : " (got: " + rejErr + ")"}`);

console.log("3) wait ~24s (> LP_REFRESH_MIN_SLOTS=50 ≈ 20s) then re-quote → ALLOWED");
await sleep(24000);
let sig3 = null;
try { sig3 = await sendAs(keeper, await refreshIx(keeper.publicKey)); } catch (e) { console.log("   (err) ", String(e.message || e).slice(0, 70)); }
ok(!!sig3, `re-quote allowed once the rate-limit window elapsed — ${sig3 ? sig3.slice(0, 16) + "…" : "n/a"}`);

console.log(`\n${fail === 0 ? "✅ PERMISSIONLESS + RATE-LIMIT LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
