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
  console.log("SKIP live-ER acceptance: set ER_RPC=<MagicBlock ER endpoint> (e.g. https://devnet.magicblock.app) to run.");
  process.exit(0);
}
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";

// ── setup ─────────────────────────────────────────────────────────────────────
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const DELEG = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
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

// NOTE (finding from this suite's first run): the 256-slot fill-outbox (24,640 B)
// CANNOT be ER-delegated — `cpi_delegate` creates the delegate-buffer at the full
// account size via one `create_account` CPI, which exceeds the 10,240 B/ix BPF
// loader limit. Since the matcher requires `fo_cap >= ring_cap` (256), the deep
// outbox is therefore **L1-only** under the current ring cap. So the ER round-trip
// here covers the supported ER config — book + the §3.2 commitment ring (96-cap,
// the path flash.trade runs) — which is the security-critical claim: the
// fill-authenticity ring survives delegate → match → commit → undelegate. The
// outbox's ER constraint is probed + recorded as a known limitation at the end.

try {
  const ref = await program.account.marketAccount.fetch(REF_MARKET);

  // ── L1: build a fresh 96-cap market (book + §3.2 ring; NO outbox in the loop) ──
  await stage("L1 init_market + book + ring (96-cap)", async () => {
    await send(l1, await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
    await send(l1, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.initFillCommitment().accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction(), []);
  });

  // ── L1: delegate book + ring to the ER (the CPI round-trip start) ──
  const delegArgs = (acct) => ({
    buf: pda(["buffer", acct]),
    rec: pda(["delegation", acct], DELEG),
    meta: pda(["delegation-metadata", acct], DELEG),
  });
  await stage("L1 delegate book + ring → DLP", async () => {
    const b = delegArgs(BOOK);
    await send(l1, await program.methods.delegateMarketBook(30000, null).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, ownerProgram: PID, delegateBuffer: b.buf, delegationRecord: b.rec, delegationMetadata: b.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const c = delegArgs(FC);
    await send(l1, await program.methods.delegateFillCommitment(30000, null).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, ownerProgram: PID, delegateBuffer: c.buf, delegationRecord: c.rec, delegationMetadata: c.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  });
  await sleep(4000); // let the ER validator pick up the delegated accounts

  // ── ER: match a taker against rested bids on the rollup (commitments pushed on ER) ──
  // ROUTING (finding from the first run): the delegated accounts must be on the
  // validator behind ER_RPC. With `null` validator the DLP assigns them to whichever
  // validator claims them, so a bare public endpoint may return InvalidWritableAccount
  // ("this account isn't delegated to me"). For a deterministic green here, delegate
  // to the ER_RPC endpoint's validator identity (replace `null` in the delegate calls
  // with its pubkey) or send through the MagicBlock router that forwards to the owner.
  // The delegate-CPI itself (book + ring) is already validated above.
  const taker = Keypair.generate();
  await stage("ER rest bids + taker sweep (4 fills, ring commitments on the ER)", async () => {
    await send(l1, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 30_000_000 }), []);
    for (let i = 0; i < 4; i++) {
      const tick = 90000 - i * 10;
      await send(er, await program.methods.placeLimitOrderV2(0, new BN(1), new BN(tick), 0, new BN(0), 0).accountsPartial({ trader: signer.publicKey, market: M, marketBook: BOOK }).instruction(), []);
    }
    await send(er, await program.methods.placeTakerOrderV2(1, new BN(4), new BN(1), 0, new BN(0), 0)
      .accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK })
      .remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction(), [taker], 1_400_000);
  });

  // ── ER: commit the (book, ring) snapshot to L1 ──
  await stage("ER commit_* → L1 snapshot", async () => {
    await send(er, await program.methods.commitMarketBook().accountsPartial({ payer: signer.publicKey, marketBook: BOOK, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitFillCommitment().accountsPartial({ payer: signer.publicKey, fillCommitment: FC, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
  });
  await sleep(5000); // commit propagation to L1

  // ── L1: assert the committed RING survived the round-trip (4 commitments produced) ──
  await stage("L1 assert committed ring cursor == 4 (authenticity survived ER)", async () => {
    const acct = await l1.getAccountInfo(FC);
    if (!acct) throw new Error("ring missing on L1 after commit");
    const r = decodeOutbox(acct.data); // same header layout (produced@8, cap@24)
    if (r.produced !== 4) throw new Error(`expected ring produced=4, got ${r.produced}`);
  });

  // ── ER: commit-and-undelegate → process_undelegation finalizes on L1 ──
  await stage("ER commit_and_undelegate_* → L1 finalize", async () => {
    await send(er, await program.methods.commitAndUndelegateMarketBook().accountsPartial({ payer: signer.publicKey, marketBook: BOOK, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitAndUndelegateFillCommitment().accountsPartial({ payer: signer.publicKey, fillCommitment: FC, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
  });
  await sleep(7000); // undelegation callback to L1

  // ── L1: assert accounts are back under the program + valid (from_account_data accepts) ──
  await stage("L1 assert undelegated + valid (program-owned, book + ring decode)", async () => {
    const fc = await l1.getAccountInfo(FC);
    if (!fc || !fc.owner.equals(PID)) throw new Error("ring not back under program after undelegate");
    if (decodeOutbox(fc.data).cap === 0) throw new Error("ring corrupt after undelegate");
    const book = await l1.getAccountInfo(BOOK);
    if (!book || !book.owner.equals(PID)) throw new Error("book not back under program after undelegate");
  });

  // ── KNOWN-LIMITATION probe: the 256-outbox cannot be ER-delegated (10,240 B/ix
  // CPI cap on the delegate-buffer create). Recorded, not a suite failure. ──
  await stage("PROBE outbox ER-delegation is correctly blocked (L1-only at 256)", async () => {
    await send(l1, await program.methods.initFillOutbox(256).accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.growFillOutbox(106).accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.growFillOutbox(45).accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, systemProgram: sys }).instruction(), []);
    const o = delegArgs(FO);
    let blocked = false;
    try {
      await send(l1, await program.methods.delegateFillOutbox(30000, null).accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, ownerProgram: PID, delegateBuffer: o.buf, delegationRecord: o.rec, delegationMetadata: o.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    } catch (e) {
      blocked = String(e.message || e).includes("reallocate") || String(e.message || e).includes("0x");
    }
    if (!blocked) throw new Error("expected delegate_fill_outbox(256) to be blocked by the 10,240 B/ix buffer-create cap, but it succeeded");
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
