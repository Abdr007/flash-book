// FLP hardening H-1 LIVE (devnet): on the ring-authenticated path the caller-
// supplied fill_seq is IGNORED (auto-incremented), so a permissionless keeper
// CANNOT wedge settlement. Full loop: FLP posts → taker crosses → a keeper settles
// via the ring path with fill_seq = u64::MAX → the fill SETTLES and
// market.last_settlement_seq becomes 1 (NOT u64::MAX). Market uses a zero taker
// fee so the taker needs no collateral. L1_RPC=<devnet> node hlp_h1_acceptance.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
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

const sendAs = async (kp, ix, extra = []) => {
  const { blockhash } = await l1.getLatestBlockhash("confirmed");
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: kp.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [kp, ...extra], { commitment: "confirmed", skipPreflight: true });
};
let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

console.log(`FLP H-1 live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);
const params = ref.params;
params.takerFeeBps = 0;   // no fee → taker needs no collateral
params.makerRebateBps = 0; // keep maker_rebate ≤ taker_fee
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);

console.log("setup: ARMED zero-fee market + book + ring; FLP posts an ask; a fresh taker");
await sendAs(signer, await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
await sendAs(signer, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.flpPostMakerOrder(1, new BN(1), new BN(100000), new BN(0)).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, flpExposure: FLP }).instruction());
const taker = Keypair.generate();
await sendAs(signer, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 60_000_000 }));
const TS = pda(["trader_state", taker.publicKey]);
await sendAs(taker, await program.methods.openTraderState().accountsPartial({ trader: taker.publicKey, traderState: TS, systemProgram: sys }).instruction());
const TPOS = pda(["position", M, TS]);
console.log(`  market ${M.toBase58()}\n`);

console.log("1) taker crosses the FLP ask → a ring commitment is pushed (maker = FLP PDA)");
await sendAs(taker, await program.methods.placeTakerOrderV2(0, new BN(1), new BN(100000), 0, new BN(0), 0).accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction());

console.log("2) a KEEPER settles via the ring path with fill_seq = u64::MAX → must SETTLE (seq ignored)");
const U64MAX = new BN("18446744073709551615");
const flpFillIx = await program.methods.applyFlpFill(new BN(1), new BN(100000), 0, 0, U64MAX, false)
  .accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TS, takerPosition: TPOS, flpExposure: FLP, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, systemProgram: sys })
  .remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }])
  .instruction();
let settled = false, detail = "";
try { await sendAs(signer, flpFillIx); settled = true; } catch (e) { detail = String(e.message || e).slice(0, 80); }
ok(settled, `ring fill with fill_seq=u64::MAX SETTLED — the caller's nonce was ignored${settled?"":" — got: "+detail}`);

console.log("3) verify the nonce AUTO-INCREMENTED to 1 (not wedged at u64::MAX)");
const mkt = await program.account.marketAccount.fetch(M);
ok(Number(mkt.lastSettlementSeq) === 1, `last_settlement_seq = ${mkt.lastSettlementSeq} (== 1, not u64::MAX) → market not bricked; DoS closed`);

console.log(`\n${fail === 0 ? "✅ FLP H-1 LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
