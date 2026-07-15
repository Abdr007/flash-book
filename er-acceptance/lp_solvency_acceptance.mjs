// LP permissionless-solvency live acceptance: LP posts, a taker crosses, and
// any keeper settles the ring-authenticated fill with the exact next sequence.
// The market uses a zero taker fee, so the taker needs no collateral.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction, createTransferInstruction, getAssociatedTokenAddressSync } from "@solana/spl-token";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
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

console.log(`LP permissionless-solvency live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);
if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60; // ref market predates the init-time staleness bound
const params = ref.params;
params.takerFeeBps = 0;   // no fee → taker needs no collateral
params.makerRebateBps = 0; // keep maker_rebate ≤ taker_fee
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);

console.log("setup: ARMED zero-fee market + book + ring; LP posts an ask; a fresh taker");
await sendAs(signer, await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), []);
await sendAs(signer, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.lpPostMakerOrder(1, new BN(1), new BN(100000), new BN(0)).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, lpExposure: LP }).instruction());
const taker = Keypair.generate();
await sendAs(signer, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 60_000_000 }));
const TS = pda(["trader_state", taker.publicKey]);
await sendAs(taker, await program.methods.openTraderState().accountsPartial({ trader: taker.publicKey, traderState: TS, systemProgram: sys }).instruction());
// The intake gate requires initial margin even on a zero-fee market: fund the
// taker's ATA from the signer and deposit collateral before the cross.
const takerAta = getAssociatedTokenAddressSync(QUOTE, taker.publicKey);
const signerAta = getAssociatedTokenAddressSync(QUOTE, signer.publicKey);
const TOKEN_PROGRAM = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const DEPOSIT = 1_000_000_000;
await sendAs(signer, createAssociatedTokenAccountIdempotentInstruction(signer.publicKey, takerAta, taker.publicKey, QUOTE));
await sendAs(signer, createTransferInstruction(signerAta, takerAta, signer.publicKey, DEPOSIT));
await sendAs(taker, await program.methods.depositCollateral(new BN(DEPOSIT)).accountsPartial({ trader: taker.publicKey, traderState: TS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: takerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction());
const TPOS = pda(["position", M, TS]);
console.log(`  market ${M.toBase58()}\n`);

console.log("1) taker crosses the LP ask → a ring commitment is pushed (maker = LP PDA)");
await sendAs(taker, await program.methods.placeTakerOrder(0, new BN(1), new BN(100000), 0, new BN(0), 0).accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK, traderState: TS, position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction());

console.log("2) a KEEPER settles via the ring path with fill_seq = 1 → must SETTLE");
const lpFillIx = await program.methods.applyLpFill(new BN(1), new BN(100000), 0, 0, new BN(1), false)
  .accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TS, takerPosition: TPOS, lpExposure: LP, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, systemProgram: sys })
  .remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }])
  .instruction();
let settled = false, detail = "";
try { await sendAs(signer, lpFillIx); settled = true; } catch (e) { detail = String(e.message || e).slice(0, 80); }
ok(settled, `ring fill with fill_seq=1 settled${settled?"":" — got: "+detail}`);

console.log("3) verify the settlement nonce advanced to 1");
const mkt = await program.account.marketAccount.fetch(M);
ok(Number(mkt.lastSettlementSeq) === 1, `last_settlement_seq = ${mkt.lastSettlementSeq} (== 1)`);

console.log(`\n${fail === 0 ? "✅ LP permissionless-solvency LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
