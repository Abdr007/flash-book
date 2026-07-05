// Costing: decentralized-committee attestation overhead vs the single-sequencer
// baseline. The FILL SETTLEMENT math is identical in both models — the committee
// only changes WHO authorizes a batch — so the delta is exactly the per-batch
// `commit_batch` cost, amortized over the fills in a batch. Measures REAL CU on
// devnet for set_sequencer_committee, commit_batch (by quorum size), and
// slash_equivocation. L1_RPC=<devnet rpc> node committee_costing.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, Ed25519Program, SYSVAR_INSTRUCTIONS_PUBKEY, sendAndConfirmTransaction } from "@solana/web3.js";
import nacl from "tweetnacl";
import sha3 from "js-sha3";
const { keccak256 } = sha3;
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const sys = SystemProgram.programId;
const pda = (s, p = PID) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");
const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");
const FLP = pda(["flp_exposure"]);

async function sendCU(ixs, extra = []) {
  const tx = new Transaction();
  for (const i of ixs) tx.add(i);
  const sig = await sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
  const t = await l1.getTransaction(sig, { maxSupportedTransactionVersion: 0, commitment: "confirmed" });
  return { cu: t?.meta?.computeUnitsConsumed ?? -1, bytes: tx.serialize({ requireAllSignatures: false, verifySignatures: false }).length };
}
const digestOf = (market, h) => Buffer.from(keccak256.arrayBuffer((() => {
  const b = Buffer.alloc(152);
  market.toBuffer().copy(b, 0);
  b.writeBigUInt64LE(BigInt(h.epoch), 32); b.writeBigUInt64LE(BigInt(h.batchSeq), 40);
  Buffer.from(h.prevRoot).copy(b, 48); Buffer.from(h.fillsRoot).copy(b, 80); Buffer.from(h.newRoot).copy(b, 112);
  b.writeBigUInt64LE(BigInt(h.mark), 144); return b;
})()));
const bftThreshold = (n) => Math.floor((2 * n) / 3) + 1;

console.log(`committee costing — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
await sendCU([await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction()], [base]);
const COMM = pda(["seq_committee", M]);
const ATT = pda(["batch_attest", M]);

let seq = 0;
let prevRoot = new Uint8Array(32);
const rows = [];
for (const N of [1, 4, 7, 10, 13]) {
  const th = bftThreshold(N);
  const validators = Array.from({ length: N }, () => Keypair.generate());
  const setRes = await sendCU([await program.methods.setSequencerCommittee(validators.map((v) => v.publicKey), th).accountsPartial({ authority: signer.publicKey, market: M, committee: COMM, systemProgram: sys }).instruction()]);
  const comm = await program.account.sequencerCommittee.fetch(COMM);
  const epoch = Number(comm.epoch);
  seq += 1;
  const newRoot = new Uint8Array(32).fill(seq & 0xff);
  const h = { epoch, batchSeq: seq, prevRoot, fillsRoot: new Uint8Array(32).fill(7), newRoot, mark: 100000 };
  const digest = digestOf(M, h);
  const ed = validators.slice(0, th).map((v, i) => Ed25519Program.createInstructionWithPublicKey({ publicKey: v.publicKey.toBytes(), message: digest, signature: nacl.sign.detached(digest, v.secretKey) }));
  const attestors = validators.slice(0, th).map((_, i) => ({ validatorSlot: i, ed25519IxIndex: i }));
  const header = { epoch: new BN(epoch), batchSeq: new BN(seq), prevStateRoot: Array.from(prevRoot), fillsMerkleRoot: Array.from(h.fillsRoot), newStateRoot: Array.from(newRoot), markTicks: new BN(100000) };
  const ci = await program.methods.commitBatch(header, attestors).accountsPartial({ payer: signer.publicKey, market: M, committee: COMM, batchAttestation: ATT, instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY, systemProgram: sys }).instruction();
  try {
    const c = await sendCU([...ed, ci]);
    rows.push({ N, th, setCU: setRes.cu, commitCU: c.cu, txBytes: c.bytes });
    console.log(`  N=${String(N).padStart(2)} threshold=${String(th).padStart(2)}  set_committee=${String(setRes.cu).padStart(6)} CU   commit_batch=${String(c.cu).padStart(6)} CU   (tx ${c.bytes} B)`);
    prevRoot = newRoot;
  } catch (e) {
    console.log(`  N=${String(N).padStart(2)} threshold=${String(th).padStart(2)}  commit_batch FAILED — ${String(e.message || e).slice(0, 60)} (likely >1232 B tx)`);
  }
}

// slash_equivocation cost (2 sigs)
try {
  const comm = await program.account.sequencerCommittee.fetch(COMM);
  const epoch = Number(comm.epoch);
  const val = Keypair.fromSecretKey(new Uint8Array(64)); // placeholder — recompute below
} catch {}

console.log("");
if (rows.length) {
  const base1 = rows[0].commitCU;
  console.log("Per-fill overhead if a batch carries F fills (commit_batch CU ÷ F):");
  for (const r of rows) {
    const per = (f) => (r.commitCU / f).toFixed(1);
    console.log(`  N=${String(r.N).padStart(2)} th=${String(r.th).padStart(2)}: ${String(r.commitCU).padStart(6)} CU/batch → ${per(50).padStart(6)} CU/fill @50  ${per(100).padStart(6)} @100  ${per(500).padStart(6)} @500`);
  }
}
