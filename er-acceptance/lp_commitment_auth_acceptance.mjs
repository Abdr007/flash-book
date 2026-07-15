// LP hardening commitment-authentication LIVE (devnet): on an ARMED market, apply_lp_fill via the
// SEQUENCER path with NO fill-commitment must be REJECTED (Unauthorized) — the
// ring is mandatory. This is the closed sequencer-LP-fabrication channel, on the
// real chain. (The ring path itself is exercised by liquidity_pool_acceptance.mjs.)
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const sys = SystemProgram.programId;
const pda = (s, p = PID) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];
const QUOTE = new PublicKey("5NL1XQZ4ZdiLR6a6VwCZWQ6DMCLdafCvbDFjeVRzcama");
const INS = new PublicKey("B9MgERuAheDM3pzh3Z4VwYMZxSGpMmYATfjpuutpgAVJ");
const VAULT = new PublicKey("2FNwaiQ1u5aJLbHviSch2p3pBVmnyMJK54v1cVtMuPVd");
const OBV = new PublicKey("Cbf3TwLKvHsh1mH72PjNt7z7dpmbtxdYZNTWxybyde22");
const OOR = new PublicKey("GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD");
const LP = pda(["lp_exposure"]);
const REF_MARKET = new PublicKey("DRTiohFdhTbyCHkc8huNMSgrgV3oDryayJHEavB5vztZ");

const sendAs = async (kp, ix, extra = []) => {
  const { blockhash } = await l1.getLatestBlockhash("confirmed");
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: kp.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [kp, ...extra], { commitment: "confirmed", skipPreflight: true });
};
let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

console.log(`LP commitment-authentication live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);
if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60; // ref market predates the init-time staleness bound
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);

console.log("setup: ARMED market + book + ring; a fresh taker trader-state");
await sendAs(signer, await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), []);
await sendAs(signer, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction()); // ARMS the market
const taker = Keypair.generate();
await sendAs(signer, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 60_000_000 }));
const TS = pda(["trader_state", taker.publicKey]);
await sendAs(taker, await program.methods.openTraderState().accountsPartial({ trader: taker.publicKey, traderState: TS, systemProgram: sys }).instruction());
const TPOS = pda(["position", M, TS]);
console.log(`  armed market ${M.toBase58()}  taker_state ${TS.toBase58().slice(0,8)}…\n`);

console.log("1) sequencer settles an LP fill on the ARMED market WITH NO ring → must be REJECTED");
let rejected = false, detail = "";
const lpFillIx = await program.methods.applyLpFill(new BN(1), new BN(100000), 0, 0, new BN(1), false)
  .accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TS, takerPosition: TPOS, lpExposure: LP, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, systemProgram: sys })
  .instruction();
try {
  await sendAs(signer, lpFillIx);
} catch (e) {
  const s = [e.message, JSON.stringify(e.logs || [])].join(" ");
  // 7100 = Unauthorized = 0x1bbc
  rejected = /0x1bbc|Unauthorized|Error Number: 7100/i.test(s);
  detail = String(e.message || e).slice(0, 60);
  if (!rejected) { const m = String(e.message||e).match(/Transaction\s+([1-9A-HJ-NP-Za-km-z]{40,})/); if (m) { try { const t = await l1.getTransaction(m[1], {maxSupportedTransactionVersion:0, commitment:"confirmed"}); rejected = /0x1bbc|Unauthorized|7100/i.test((t?.meta?.logMessages||[]).join(" ")); } catch {} } }
}
ok(rejected, `armed market REJECTS the sequencer LP path without a ring (commitment-authentication: fabrication channel closed)${rejected?"":" — got: "+detail}`);

// position must NOT have been created
const posAcct = await l1.getAccountInfo(TPOS);
ok(posAcct === null, "no taker position created — the fabricated fill was rejected before settlement");

console.log(`\n${fail === 0 ? "✅ LP commitment-authentication LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
