// HLP (1b) LIVE acceptance on devnet: the pool-backed CLOB loop.
//   1. init a fresh armed market + book + fill-commitment ring
//   2. LP posts a resting maker ASK (lp_post_maker_order) — verifies the BPF
//      stack fix works on the real chain (this instruction crashed pre-fix)
//   3. a taker crosses it (place_taker_order_v2) -> the matcher pushes a STANDARD
//      commitment with maker = the lp_exposure PDA
//   4. assert the ring recorded exactly one produced commitment -> the LP order
//      rested, was crossed, and is ring-committed (ready for permissionless
//      apply_lp_fill, which is CI-verified end-to-end).
// L1_RPC=<devnet> node hlp_acceptance.mjs
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

const send = async (ix, extra = []) => {
  const { blockhash } = await l1.getLatestBlockhash("confirmed");
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: signer.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true });
};

let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

console.log(`HLP live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);
if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60; // ref market predates the init-time staleness bound
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);

console.log("setup: init armed market + book + ring");
await send(await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), [base]);
await send(await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await send(await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
console.log(`  market ${M.toBase58()}\n`);

console.log("1) LP posts a resting maker ASK 1 @ 100_000 (lp_post_maker_order)");
const postSig = await send(await program.methods.lpPostMakerOrder(1, new BN(1), new BN(100000), new BN(0)).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, lpExposure: LP }).instruction());
ok(!!postSig, `LP maker order posted LIVE (BPF stack fix holds) — ${postSig.slice(0, 16)}…`);

console.log("2) taker crosses: bid 1 @ 100_000 (place_taker_order_v2)");
await send(await program.methods.placeTakerOrderV2(0, new BN(1), new BN(100000), 0, new BN(0), 0).accountsPartial({ trader: signer.publicKey, market: M, marketBook: BOOK, traderState: pda(["trader_state", signer.publicKey]), position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction());

console.log("3) verify the ring recorded the LP-maker fill");
const fcData = (await l1.getAccountInfo(FC)).data;
const produced = Number(fcData.readBigUInt64LE(8));
const settled = Number(fcData.readBigUInt64LE(16));
ok(produced === 1, `matcher pushed exactly one commitment for the crossed LP quote (produced=${produced})`);
ok(settled === 0, `commitment pending settlement (settled=${settled}) — ready for permissionless apply_lp_fill`);

console.log(`\n${fail === 0 ? "✅ HLP LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
