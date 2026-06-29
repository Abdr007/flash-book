// Live-ER acceptance suite (Tier-2 — see ER_TRUST_BOUNDARY.md §3).
//
// Exercises the ONE thing solana-program-test structurally cannot: the real
// MagicBlock CPI round-trip — delegate (book + ring + outbox) → match on the ER →
// commit to L1 → commit-and-undelegate → process_undelegation — and asserts the L1
// state is consistent and settleable after the round-trip.
//
// GATED: runs only when ER_RPC is set (the MagicBlock devnet ER endpoint, e.g.
// https://devnet.magicblock.app). Without it the suite SKIPS cleanly (exit 0),
// exactly like the SBF benches skip without BPF_OUT_DIR — so it never breaks CI and
// is reproducible on demand before a mainnet cut.
//
//   L1_RPC=https://api.devnet.solana.com ER_RPC=https://devnet.magicblock.app \
//     node er-acceptance/er_acceptance.mjs
//
// Requires: a funded keypair at ~/.config/solana/id.json that is the market
// authority, and the program deployed (5VqBgu…) on the target cluster.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";

const { AnchorProvider, Program, Wallet, BN } = anchor;

// ── gate ──────────────────────────────────────────────────────────────────────
const ER_RPC = process.env.ER_RPC;
if (!ER_RPC) {
  console.log("SKIP live-ER acceptance: set ER_RPC=<MagicBlock ER endpoint> (the ER_VALIDATOR's endpoint, e.g. https://devnet-as.magicblock.app, or the Magic Router https://devnet-rpc.magicblock.app) to run.");
  process.exit(0);
}
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";

// ── setup ─────────────────────────────────────────────────────────────────────
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const DELEG = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
// Pin the ER validator that will own the delegated accounts, so the ER match stage
// transacts against the validator that actually holds them (deterministic — no
// `null`-validator routing ambiguity). Default = the MagicBlock devnet ER validator
// (identity MAS1Dt9…, FQDN https://devnet-as.magicblock.app); override via ER_VALIDATOR.
// Set ER_RPC to THAT validator's endpoint (or the Magic Router devnet-rpc.magicblock.app).
const ER_VALIDATOR = new PublicKey(process.env.ER_VALIDATOR || "MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");
const MAGIC_PROGRAM = new PublicKey("Magic11111111111111111111111111111111111111");
const MAGIC_CONTEXT = new PublicKey("MagicContext1111111111111111111111111111111");
const sys = SystemProgram.programId;
// devnet market reference (params + vault/oracle to clone), per the replay infra.
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");
const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");

const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const er = new Connection(ER_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const FLP = PublicKey.findProgramAddressSync([Buffer.from("flp_exposure")], PID)[0];
const pda = (s, p = PID) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function send(conn, ixs, signers, cuLimit = 400_000) {
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cuLimit }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  return await sendAndConfirmTransaction(conn, tx, [signer, ...signers], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
}

const stages = [];
async function stage(name, fn) {
  try { const r = await fn(); stages.push({ name, ok: true }); console.log(`  ✓ ${name}`); return r; }
  catch (e) { stages.push({ name, ok: false, err: String(e.message || e).slice(0, 160) }); console.log(`  ✗ ${name}: ${String(e.message || e).slice(0, 160)}`); throw e; }
}

// outbox account decode (raw layout — see SEQUENCER_OUTBOX_CUTOVER.md §2b)
function decodeOutbox(data) {
  return { produced: Number(data.readBigUInt64LE(8)), settled: Number(data.readBigUInt64LE(16)), cap: data.readUInt32LE(24) };
}

console.log(`live-ER acceptance — L1=${L1_RPC} ER=${ER_RPC}`);
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);
const FO = pda(["fill_outbox", M]);
console.log("market", M.toBase58());

// VERSATILE config: a per-market cap of 105 keeps BOTH the ring (3,424 B) and the
// FULL outbox (10,144 B) one-CPI delegate-safe (< 10,240 B), so the entire off-log
// pipeline — book + §3.2 ring + fill-outbox — delegates to the ER. (At cap 256 the
// outbox can't be ER-delegated; that's the L1 deep-sweep config. See
// FILL_OUTBOX_DESIGN.md §10.) This round-trip therefore covers the FULL ER pipeline.
const CAP = 105;

try {
  const ref = await program.account.marketAccount.fetch(REF_MARKET);

  // ── L1: build a fresh cap-105 market (book + ring + FULL outbox, no grow) ──
  await stage(`L1 init_market + book + ring + outbox (cap ${CAP}, ER-capable)`, async () => {
    await send(l1, await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
    await send(l1, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.initFillCommitment(CAP).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, fillCommitment: FC, systemProgram: sys }).instruction(), []);
    const fo = await l1.getAccountInfo(FO);
    if (decodeOutbox(fo.data).cap !== CAP) throw new Error(`outbox not full ${CAP} in one ix`);
  });

  // ── L1: delegate book + ring + OUTBOX to the ER (all one-CPI-safe at cap 105) ──
  const delegArgs = (acct) => ({
    buf: pda(["buffer", acct]),
    rec: pda(["delegation", acct], DELEG),
    meta: pda(["delegation-metadata", acct], DELEG),
  });
  await stage("L1 delegate market + book + ring + OUTBOX → DLP (full writable set)", async () => {
    // book/ring/outbox first (they read the market for auth while it's still
    // program-owned), then the MARKET last (place_limit writes its OI, so the ER
    // needs every writable account delegated — else a delegated/undelegated mix
    // is rejected `InvalidWritableAccount`). All pinned to ER_VALIDATOR.
    const b = delegArgs(BOOK);
    await send(l1, await program.methods.delegateMarketBook(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, ownerProgram: PID, delegateBuffer: b.buf, delegationRecord: b.rec, delegationMetadata: b.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const c = delegArgs(FC);
    await send(l1, await program.methods.delegateFillCommitment(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, ownerProgram: PID, delegateBuffer: c.buf, delegationRecord: c.rec, delegationMetadata: c.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const o = delegArgs(FO);
    await send(l1, await program.methods.delegateFillOutbox(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, ownerProgram: PID, delegateBuffer: o.buf, delegationRecord: o.rec, delegationMetadata: o.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const m = delegArgs(M);
    await send(l1, await program.methods.delegateMarket(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, ownerProgram: PID, delegateBuffer: m.buf, delegationRecord: m.rec, delegationMetadata: m.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  });
  await sleep(4000); // let the ER validator pick up the delegated accounts

  // ── ER: match a taker on the rollup (commitments pushed + outbox written ON the ER) ──
  // ROUTING: with `null` validator the DLP assigns the accounts to whichever validator
  // claims them; the ER match must transact against THAT validator (or the MagicBlock
  // router), else it returns InvalidWritableAccount. Pin the validator identity in the
  // delegate calls, or route through the MagicBlock router, for a deterministic green.
  const taker = Keypair.generate();
  await stage("ER rest bids + taker sweep (4 fills; commitments + outbox on the ER)", async () => {
    await send(l1, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 30_000_000 }), []);
    for (let i = 0; i < 4; i++) {
      const tick = 90000 - i * 10;
      await send(er, await program.methods.placeLimitOrderV2(0, new BN(1), new BN(tick), 0, new BN(0), 0).accountsPartial({ trader: signer.publicKey, market: M, marketBook: BOOK }).instruction(), []);
    }
    await send(er, await program.methods.placeTakerOrderV2(1, new BN(4), new BN(1), 0, new BN(0), 0)
      .accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK })
      .remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }, { pubkey: FO, isWritable: true, isSigner: false }]).instruction(), [taker], 1_400_000);
  });

  // ── ER: commit the (book, ring, outbox) snapshot to L1 ──
  await stage("ER commit_* → L1 snapshot", async () => {
    await send(er, await program.methods.commitMarketBook().accountsPartial({ payer: signer.publicKey, marketBook: BOOK, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitFillCommitment().accountsPartial({ payer: signer.publicKey, fillCommitment: FC, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitFillOutbox().accountsPartial({ payer: signer.publicKey, fillOutbox: FO, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
  });
  await sleep(5000); // commit propagation to L1

  // ── L1: assert ring + outbox both survived the round-trip (4 fills each) ──
  await stage("L1 assert committed ring + outbox cursor == 4 (authenticity + data survived ER)", async () => {
    const r = decodeOutbox((await l1.getAccountInfo(FC)).data);
    if (r.produced !== 4) throw new Error(`ring produced=${r.produced}, expected 4`);
    const o = decodeOutbox((await l1.getAccountInfo(FO)).data);
    if (o.produced !== 4) throw new Error(`outbox produced=${o.produced}, expected 4`);
  });

  // ── ER: commit-and-undelegate → process_undelegation finalizes on L1 ──
  await stage("ER commit_and_undelegate_* → L1 finalize", async () => {
    await send(er, await program.methods.commitAndUndelegateMarketBook().accountsPartial({ payer: signer.publicKey, marketBook: BOOK, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitAndUndelegateFillCommitment().accountsPartial({ payer: signer.publicKey, fillCommitment: FC, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitAndUndelegateFillOutbox().accountsPartial({ payer: signer.publicKey, fillOutbox: FO, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
  });
  await sleep(7000); // undelegation callback to L1

  // ── L1: assert all three back under the program + valid (from_account_data accepts) ──
  await stage("L1 assert undelegated + valid (book + ring + outbox program-owned, decode)", async () => {
    for (const [name, k] of [["ring", FC], ["outbox", FO], ["book", BOOK]]) {
      const a = await l1.getAccountInfo(k);
      if (!a || !a.owner.equals(PID)) throw new Error(`${name} not back under program after undelegate`);
    }
    if (decodeOutbox((await l1.getAccountInfo(FO)).data).cap !== CAP) throw new Error("outbox corrupt after undelegate");
  });
} catch (e) {
  // a stage already logged the failure; fall through to the report
}

const passed = stages.filter((s) => s.ok).length;
console.log(`\n========== LIVE-ER ACCEPTANCE: ${passed}/${stages.length} stages ==========`);
for (const s of stages) console.log(`  ${s.ok ? "PASS" : "FAIL"}  ${s.name}${s.ok ? "" : "  — " + s.err}`);
const allOk = stages.length > 0 && stages.every((s) => s.ok);
console.log(allOk ? "\nLIVE-ER ROUND-TRIP PASS ✅ (delegate → match → commit → undelegate → L1 consistent)" : "\nLIVE-ER ROUND-TRIP INCOMPLETE ❌ (see first FAIL above)");
process.exit(allOk ? 0 : 1);
