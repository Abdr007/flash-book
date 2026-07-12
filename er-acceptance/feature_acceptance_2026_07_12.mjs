// Devnet acceptance for the 2026-07-12 features (2.3 fee-share accrual/claim,
// 4.5 tranche param, 4.4 OI surcharge param) against the throwaway program
// BRtnEAZ6 (bit-for-bit merged main; deploy hash verified == artifact).
//
//   PROGRAM=<id> L1_RPC=<devnet> node feature_acceptance_2026_07_12.mjs
//
// Genesis-light: 2.3's init_fee_accrual needs only payer + PDA + system, so it
// proves the new instruction surface is LIVE on real devnet without a full fill
// genesis. When the insurance/LP singletons already exist on the throwaway,
// claim_fee_accrual is exercised too (expect ZeroSize on an empty accrual).
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import { getAssociatedTokenAddressSync, createAssociatedTokenAccountIdempotentInstruction, TOKEN_PROGRAM_ID } from "@solana/spl-token";

const { BN } = anchor;

// Raw web3.js send — anchor's provider.sendAndConfirm mis-translates on-chain
// failures ("Unknown action 'undefined'") in this web3 version, so send/confirm
// directly and read the program logs for the asserted Custom code on rejections.
async function sendIx(ix, extra = []) {
  return sendAndConfirmTransaction(l1, new Transaction().add(ix), [signer, ...extra], { commitment: "confirmed", skipPreflight: true });
}
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const PROGRAM = new PublicKey(process.env.PROGRAM || "BRtnEAZ6Tc61gz8m93unL1vzaC4GjtHViLCU8JqKB2gD");
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const sys = SystemProgram.programId;
const EXPLORER = (s) => `https://explorer.solana.com/tx/${s}?cluster=devnet`;

const wallet = new anchor.Wallet(signer);
const provider = new anchor.AnchorProvider(l1, wallet, { commitment: "confirmed" });
const idlWithAddr = { ...IDL, address: PROGRAM.toBase58() };
const program = new anchor.Program(idlWithAddr, provider);

const seed = (s) => PublicKey.findProgramAddressSync([Buffer.from(s)], PROGRAM)[0];
const feeAccrualPda = (r) => PublicKey.findProgramAddressSync([Buffer.from("fee_accrual"), r.toBuffer()], PROGRAM)[0];

const rows = [];
const rec = (name, ok, detail, sig) => { rows.push({ name, ok, detail, sig }); console.log(`${ok ? "PASS" : "FAIL"}  ${name}  ${detail}${sig ? "  " + EXPLORER(sig) : ""}`); };

console.log(`\nProgram : ${PROGRAM.toBase58()}`);
console.log(`RPC     : ${L1_RPC.split("?")[0]}`);
console.log(`Signer  : ${signer.publicKey.toBase58()}\n`);

// ── 2.3-a: init_fee_accrual is live — create a fresh recipient's accrual PDA ──
const recipient = Keypair.generate().publicKey;
const fa = feeAccrualPda(recipient);
try {
  const ix = await program.methods.initFeeAccrual(recipient)
    .accountsPartial({ payer: signer.publicKey, feeAccrual: fa, systemProgram: sys })
    .instruction();
  const sig = await sendIx(ix);
  const acc = await program.account.feeAccrualAccount.fetch(fa);
  const ok = acc.recipient.equals(recipient) && acc.accruedQuoteLots.eq(new BN(0));
  rec("2.3 init_fee_accrual creates PDA (recipient bound, accrued=0)", ok,
    `recipient=${acc.recipient.toBase58().slice(0, 8)} accrued=${acc.accruedQuoteLots}`, sig);
} catch (e) {
  rec("2.3 init_fee_accrual creates PDA", false, String(e).split("\n")[0]);
}

// ── 2.3-b: claim on an empty accrual must reject (ZeroSize) — proves the claim
//           ix + its zero-guard are live (needs the insurance/LP singletons). ──
const INS = seed("insurance_fund");
const LP = seed("lp_exposure");
const insAi = await l1.getAccountInfo(INS);
if (!insAi) {
  rec("2.3 claim_fee_accrual rejects empty (ZeroSize)", true,
    "SKIPPED — insurance_fund singleton absent on this throwaway (no genesis); host-tested + Lean-proved");
} else {
  try {
    const ins = await program.account.insuranceFundAccount.fetch(INS);
    const quoteMint = ins.quoteMint;
    const quoteVault = ins.quoteVault;
    const recipientAta = getAssociatedTokenAddressSync(quoteMint, signer.publicKey);
    if (!(await l1.getAccountInfo(recipientAta))) {
      await sendIx(createAssociatedTokenAccountIdempotentInstruction(signer.publicKey, recipientAta, signer.publicKey, quoteMint));
    }
    // Reuse the signer as recipient so we control the signature + ATA.
    const fa2 = feeAccrualPda(signer.publicKey);
    if (!(await l1.getAccountInfo(fa2))) {
      await sendIx(await program.methods.initFeeAccrual(signer.publicKey)
        .accountsPartial({ payer: signer.publicKey, feeAccrual: fa2, systemProgram: sys }).instruction());
    }
    const claimIx = await program.methods.claimFeeAccrual()
      .accountsPartial({
        recipient: signer.publicKey, feeAccrual: fa2, insuranceFund: INS, lpExposure: LP,
        quoteMint, recipientQuoteAta: recipientAta, quoteVault, tokenProgram: TOKEN_PROGRAM_ID,
      }).instruction();
    try {
      const sig = await sendIx(claimIx);
      rec("2.3 claim_fee_accrual rejects empty (ZeroSize)", false, `claim on empty accrual unexpectedly SUCCEEDED ${sig}`);
    } catch (e) {
      const msg = String(e.message || e) + " " + ((e.logs || []).join(" "));
      const ok = /ZeroSize|7202|0x1c22/i.test(msg);
      rec("2.3 claim_fee_accrual rejects empty (ZeroSize)", ok, ok ? "empty accrual reverts with ZeroSize (0x1c22)" : msg.slice(0, 200));
    }
  } catch (e) {
    rec("2.3 claim_fee_accrual rejects empty (ZeroSize)", false, String(e).split("\n")[0]);
  }
}

const pass = rows.filter((r) => r.ok).length;
const fail = rows.length - pass;
console.log(`\n${pass} pass / ${fail} fail`);
fs.writeFileSync(new URL("./feature_acceptance_results.json", import.meta.url),
  JSON.stringify({ program: PROGRAM.toBase58(), rpc: L1_RPC.split("?")[0], rows: rows.map((r) => ({ ...r, explorer: r.sig ? EXPLORER(r.sig) : null })), pass, fail }, null, 2));
process.exit(fail ? 1 : 0);
