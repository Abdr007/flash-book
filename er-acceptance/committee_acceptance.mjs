// Phase 2 (M-14 endgame) live acceptance — committee threshold-attested batch.
//
// Proves the decentralized-sequencer PRIMITIVE end-to-end against the deployed
// program: an M-of-N validator committee threshold-signs a batch (state transition
// over the CONTINUOUS CLOB's fills — NOT an auction), and `commit_batch` accepts
// it iff >= threshold DISTINCT members validly Ed25519-signed the canonical
// message, the epoch matches, batch_seq strictly increases, and prev_state_root
// chains. Adversarial cases (sub-quorum, forged sig, replay, broken chain) reject.
//
// Run: L1_RPC=<devnet rpc> node committee_acceptance.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram,
  Ed25519Program, SYSVAR_INSTRUCTIONS_PUBKEY, sendAndConfirmTransaction,
} from "@solana/web3.js";
import nacl from "tweetnacl";
import sha3 from "js-sha3";
const { keccak256 } = sha3;
const { Program, AnchorProvider, Wallet, BN } = anchor;
// Validators sign the keccak DIGEST of the 152-byte canonical message (matches
// the on-chain `keccak::hash(batch_attestation_message(..))`).
const digestOf = (market, h) => Buffer.from(keccak256.arrayBuffer(batchMsg(market, h)));

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const sys = SystemProgram.programId;
const pda = (s, p = PID) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];

// Shared devnet accounts (from the ER harness) to build a fresh market.
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");
const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");
const LP = pda(["lp_exposure"]);

const send = (ixs, extra = []) => {
  const tx = new Transaction();
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  return sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
};

const results = [];
const ok = (name, detail = "") => { results.push({ name, ok: true }); console.log(`  ✓ ${name}${detail ? " — " + detail : ""}`); };
const bad = (name, detail = "") => { results.push({ name, ok: false }); console.log(`  ✗ ${name}${detail ? " — " + detail : ""}`); };

// EXACT match of `batch_attestation_message` in lib.rs (152 bytes LE).
function batchMsg(market, h) {
  const b = Buffer.alloc(152);
  market.toBuffer().copy(b, 0);
  b.writeBigUInt64LE(BigInt(h.epoch), 32);
  b.writeBigUInt64LE(BigInt(h.batchSeq), 40);
  Buffer.from(h.prevRoot).copy(b, 48);
  Buffer.from(h.fillsRoot).copy(b, 80);
  Buffer.from(h.newRoot).copy(b, 112);
  b.writeBigUInt64LE(BigInt(h.mark), 144);
  return b;
}

let M, COMM, ATT, validators, epoch;

// Build + send a commit_batch tx: k Ed25519 precompile ixs (indices 0..k-1) then
// commit_batch. `signVals` are the validators that sign; `corrupt` flips a sig byte.
async function commit(seq, prevRoot, newRoot, signVals, { corrupt = false } = {}) {
  const h = { epoch, batchSeq: seq, prevRoot, fillsRoot: new Uint8Array(32).fill(7), newRoot, mark: 100000 };
  const digest = digestOf(M, h);
  const ed = [];
  const attestors = [];
  signVals.forEach((val, i) => {
    const slot = validators.findIndex((v) => v.publicKey.equals(val.publicKey));
    let sig = nacl.sign.detached(digest, val.secretKey);
    if (corrupt && i === 0) { sig = Uint8Array.from(sig); sig[0] ^= 0xff; }
    ed.push(Ed25519Program.createInstructionWithPublicKey({ publicKey: val.publicKey.toBytes(), message: digest, signature: sig }));
    attestors.push({ validatorSlot: slot, ed25519IxIndex: i });
  });
  const header = {
    epoch: new BN(epoch), batchSeq: new BN(seq),
    prevStateRoot: Array.from(prevRoot), fillsMerkleRoot: Array.from(h.fillsRoot),
    newStateRoot: Array.from(newRoot), markTicks: new BN(100000),
  };
  const ci = await program.methods.commitBatch(header, attestors).accountsPartial({
    payer: signer.publicKey, market: M, committee: COMM, batchAttestation: ATT,
    instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY, systemProgram: sys,
  }).instruction();
  return send([...ed, ci]);
}
const toHeader = (h) => ({ epoch: new BN(h.epoch), batchSeq: new BN(h.batchSeq), prevStateRoot: Array.from(h.prevRoot), fillsMerkleRoot: Array.from(h.fillsRoot), newStateRoot: Array.from(h.newRoot), markTicks: new BN(h.mark) });
const edIx = (val, digest) => Ed25519Program.createInstructionWithPublicKey({ publicKey: val.publicKey.toBytes(), message: digest, signature: nacl.sign.detached(digest, val.secretKey) });

// slash_equivocation: validator `slot` signs TWO conflicting batches at the same
// (epoch, seq) but different new_state_root → provable equivocation → jail.
async function slash(slot, seq, newA, newB) {
  const val = validators[slot];
  const base = { epoch, batchSeq: seq, prevRoot: new Uint8Array(32), fillsRoot: new Uint8Array(32).fill(7), mark: 100000 };
  const hA = { ...base, newRoot: newA }, hB = { ...base, newRoot: newB };
  const ed = [edIx(val, digestOf(M, hA)), edIx(val, digestOf(M, hB))];
  const si = await program.methods.slashEquivocation(slot, toHeader(hA), 0, toHeader(hB), 1).accountsPartial({ reporter: signer.publicKey, market: M, committee: COMM, instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY }).instruction();
  return send([...ed, si]);
}
const expectFail = async (name, fn) => { try { await fn(); bad(name, "was ACCEPTED (should reject)"); } catch { ok(name); } };

console.log(`committee (Phase 2) acceptance — L1=${L1_RPC}`);
try {
  const ref = await program.account.marketAccount.fetch(REF_MARKET);
  if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60; // ref market predates the init-time staleness bound
  const base = Keypair.generate();
  M = pda(["market", base.publicKey, QUOTE]);
  await send(await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), [base]);
  ok("fresh market created", M.toBase58());

  validators = [Keypair.generate(), Keypair.generate(), Keypair.generate(), Keypair.generate()];
  COMM = pda(["seq_committee", M]);
  ATT = pda(["batch_attest", M]);
  await send(await program.methods.setSequencerCommittee(validators.map((v) => v.publicKey), 3).accountsPartial({ authority: signer.publicKey, market: M, committee: COMM, systemProgram: sys }).instruction());
  const comm = await program.account.sequencerCommittee.fetch(COMM);
  epoch = Number(comm.epoch);
  (comm.validatorCount === 4 && comm.threshold === 3) ? ok("committee set (N=4, threshold=3, BFT f=1)", `epoch=${epoch}`) : bad("committee set");

  const R0 = new Uint8Array(32);              // genesis chaining root
  const R1 = new Uint8Array(32).fill(0x11);
  const R2 = new Uint8Array(32).fill(0x22);

  // 1. quorum of 3 (of 4) valid sigs, chained from genesis → ACCEPT
  await commit(1, R0, R1, [validators[0], validators[1], validators[2]]);
  let a = await program.account.batchAttestation.fetch(ATT);
  (Number(a.lastBatchSeq) === 1 && Buffer.from(a.lastStateRoot).equals(Buffer.from(R1))) ? ok("threshold quorum (3/4) ACCEPTED + state root recorded") : bad("quorum accept", `seq=${a.lastBatchSeq}`);

  // 2. sub-quorum (only 2 sigs) → REJECT
  await expectFail("sub-quorum (2/4 < threshold) REJECTED", () => commit(2, R1, R2, [validators[0], validators[1]]));

  // 3. forged signature (corrupt one) → REJECT (native precompile fails)
  await expectFail("forged Ed25519 signature REJECTED", () => commit(2, R1, R2, [validators[0], validators[1], validators[2]], { corrupt: true }));

  // 4. non-member signer → REJECT
  const outsider = Keypair.generate();
  await expectFail("non-member attestor REJECTED", async () => {
    // sign with outsider but claim slot 3 (validators[3]) — sig won't verify for that pubkey
    const h = { epoch, batchSeq: 2, prevRoot: R1, fillsRoot: new Uint8Array(32).fill(7), newRoot: R2, mark: 100000 };
    const digest = digestOf(M, h);
    const ed = [validators[0], validators[1]].map((v) => Ed25519Program.createInstructionWithPublicKey({ publicKey: v.publicKey.toBytes(), message: digest, signature: nacl.sign.detached(digest, v.secretKey) }));
    ed.push(Ed25519Program.createInstructionWithPublicKey({ publicKey: outsider.publicKey.toBytes(), message: digest, signature: nacl.sign.detached(digest, outsider.secretKey) }));
    const header = { epoch: new BN(epoch), batchSeq: new BN(2), prevStateRoot: Array.from(R1), fillsMerkleRoot: Array.from(h.fillsRoot), newStateRoot: Array.from(R2), markTicks: new BN(100000) };
    const attestors = [{ validatorSlot: 0, ed25519IxIndex: 0 }, { validatorSlot: 1, ed25519IxIndex: 1 }, { validatorSlot: 3, ed25519IxIndex: 2 }];
    const ci = await program.methods.commitBatch(header, attestors).accountsPartial({ payer: signer.publicKey, market: M, committee: COMM, batchAttestation: ATT, instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY, systemProgram: sys }).instruction();
    await send([...ed, ci]);
  });

  // 5. valid next batch chaining onto R1 → ACCEPT (seq advances 1 → 2)
  await commit(2, R1, R2, [validators[1], validators[2], validators[3]]);
  a = await program.account.batchAttestation.fetch(ATT);
  (Number(a.lastBatchSeq) === 2 && Buffer.from(a.lastStateRoot).equals(Buffer.from(R2))) ? ok("chained batch #2 (different quorum) ACCEPTED") : bad("chain advance", `seq=${a.lastBatchSeq}`);

  // 6. replay batch #2 (seq not strictly greater) → REJECT
  await expectFail("replay (seq <= last) REJECTED", () => commit(2, R2, R1, [validators[0], validators[1], validators[2]]));

  // 7. broken chain (wrong prev_state_root) → REJECT
  await expectFail("broken chain (prev != last_state_root) REJECTED", () => commit(3, R1, R0, [validators[0], validators[1], validators[2]]));

  // 8. EQUIVOCATION: validator 0 double-signs conflicting batches at (epoch, seq 9) → JAIL
  await slash(0, 9, new Uint8Array(32).fill(0xaa), new Uint8Array(32).fill(0xbb));
  let cc = await program.account.sequencerCommittee.fetch(COMM);
  ((Number(cc.jailedMask) & 1) === 1) ? ok("equivocator (validator 0) JAILED via permissionless fraud proof") : bad("equivocation slash", `mask=${cc.jailedMask}`);

  // 9. non-equivocation (identical batch signed twice) → slash REJECTED
  await expectFail("non-equivocation (same digest) slash REJECTED", async () => {
    const h = { epoch, batchSeq: 9, prevRoot: new Uint8Array(32), fillsRoot: new Uint8Array(32).fill(7), newRoot: new Uint8Array(32).fill(0xcc), mark: 100000 };
    const d = digestOf(M, h);
    const ed = edIx(validators[1], d);
    const si = await program.methods.slashEquivocation(1, toHeader(h), 0, toHeader(h), 1).accountsPartial({ reporter: signer.publicKey, market: M, committee: COMM, instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY }).instruction();
    await send([ed, ed, si]);
  });

  // 10. a JAILED validator's attestation is void → a batch including slot 0 REJECTED
  await expectFail("jailed validator's attestation REJECTED", () => commit(6, R2, R1, [validators[0], validators[1], validators[2]]));

  // 11. quorum from the 3 NON-jailed validators → ACCEPT (BFT liveness holds after slashing f=1)
  await commit(6, R2, R1, [validators[1], validators[2], validators[3]]);
  cc = await program.account.batchAttestation.fetch(ATT);
  (Number(cc.lastBatchSeq) === 6) ? ok("post-jail quorum (3 non-jailed) ACCEPTED — liveness holds") : bad("post-jail quorum", `seq=${cc.lastBatchSeq}`);
} catch (e) {
  bad("unexpected setup error", String(e.message || e).slice(0, 200));
}

const passed = results.filter((r) => r.ok).length;
console.log(`\n========== COMMITTEE (PHASE 2) ACCEPTANCE: ${passed}/${results.length} ==========`);
const allOk = results.length >= 7 && results.every((r) => r.ok);
console.log(allOk ? "PHASE 2 LIVE PASS ✅ (M-of-N threshold attestation + chaining + adversarial rejects)" : "PHASE 2 INCOMPLETE ❌");
process.exit(allOk ? 0 : 1);
