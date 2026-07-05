// Withdraw-anytime acceptance (Tier-2 — see ER_TRUST_BOUNDARY.md §1.2).
//
// Proves the reserved-margin model live, in two phases:
//
//   A (ER round-trip): deposit → delegate → rest orders on the LIVE ER →
//     sequencer attests the reservation on L1 → withdraw FREE balance on L1
//     mid-session (strict fail-closed, xdomain negative + positive) → taker
//     sweeps on the ER → commit/undelegate → ring + outbox survived.
//   B (L1-resident market): the same reservation gates around REAL fill
//     settlement — rest orders, attest, over-withdraw rejected, free-balance
//     partial withdraw succeeds, taker sweeps, apply_fill ×4 settles the
//     ring-authenticated fills into positions, attestation clears, and the
//     plain partial-withdraw path re-opens.
//
// No arm step, no lock, at any point. (The two phases exist because a
// delegated MarketAccount has no undelegate path today — the documented
// architectural residual — so L1 settlement is proven on an L1-resident
// market.)
//
// GATED: runs only when ER_RPC is set; skips cleanly (exit 0) otherwise.
//
//   L1_RPC=https://api.devnet.solana.com ER_RPC=https://devnet-as.magicblock.app \
//     node er-acceptance/withdraw_anytime_acceptance.mjs
//
// Requires: a funded keypair at ~/.config/solana/id.json that is the market
// authority (and doubles as the margin attestor here), and the program
// deployed (5VqBgu…) on the target cluster.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";

const { AnchorProvider, Program, Wallet, BN } = anchor;

// ── gate ──────────────────────────────────────────────────────────────────────
const ER_RPC = process.env.ER_RPC;
if (!ER_RPC) {
  console.log("SKIP withdraw-anytime acceptance: set ER_RPC=<MagicBlock ER endpoint> to run.");
  process.exit(0);
}
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";

// ── setup (mirrors er_acceptance.mjs) ─────────────────────────────────────────
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const DELEG = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const ER_VALIDATOR = new PublicKey(process.env.ER_VALIDATOR || "MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");
const MAGIC_PROGRAM = new PublicKey("Magic11111111111111111111111111111111111111");
const MAGIC_CONTEXT = new PublicKey("MagicContext1111111111111111111111111111111");
const sys = SystemProgram.programId;
const TOKEN_PROGRAM = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA_PROGRAM = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const ata = (owner, mint) =>
  PublicKey.findProgramAddressSync([owner.toBuffer(), TOKEN_PROGRAM.toBuffer(), mint.toBuffer()], ATA_PROGRAM)[0];
const createAtaIx = (payer, owner, mint) => new anchor.web3.TransactionInstruction({
  programId: ATA_PROGRAM,
  keys: [
    { pubkey: payer, isSigner: true, isWritable: true },
    { pubkey: ata(owner, mint), isSigner: false, isWritable: true },
    { pubkey: owner, isSigner: false, isWritable: false },
    { pubkey: mint, isSigner: false, isWritable: false },
    { pubkey: sys, isSigner: false, isWritable: false },
    { pubkey: TOKEN_PROGRAM, isSigner: false, isWritable: false },
  ],
  data: Buffer.from([1]),
});
const transferIx = (from, to, authority, amount) => {
  const d = Buffer.alloc(9); d.writeUInt8(3, 0); d.writeBigUInt64LE(BigInt(amount), 1);
  return new anchor.web3.TransactionInstruction({
    programId: TOKEN_PROGRAM,
    keys: [
      { pubkey: from, isSigner: false, isWritable: true },
      { pubkey: to, isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: true, isWritable: false },
    ],
    data: d,
  });
};
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
const traderStatePda = (trader) => pda(["trader_state", trader]);

// Public-RPC hygiene: web3.js's confirm loop can surface 429s as UNHANDLED
// rejections (socket callbacks outside the awaited promise) — don't let those
// kill the run; the awaited path still reports the stage result.
process.on("unhandledRejection", (e) => console.log(`  (background rpc noise: ${String(e?.message || e).slice(0, 120)})`));

async function send(conn, ixs, signers, cuLimit = 400_000) {
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cuLimit }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  // Throttle + retry-on-429: the public devnet RPC rate-limits bursts.
  for (let attempt = 0; ; attempt++) {
    try {
      const sig = await sendAndConfirmTransaction(conn, tx, [signer, ...signers], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
      await sleep(700);
      return sig;
    } catch (e) {
      const m = String(e.message || e);
      if (attempt < 4 && (m.includes("429") || m.includes("Too Many Requests") || m.includes("blockhash"))) {
        await sleep(3000 * (attempt + 1));
        continue;
      }
      throw e;
    }
  }
}
// Send and report {sig, cu} from the confirmed tx meta.
async function sendCu(conn, ixs, signers, cuLimit = 400_000) {
  const sig = await send(conn, ixs, signers, cuLimit);
  const tx = await conn.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
  return { sig, cu: tx?.meta?.computeUnitsConsumed ?? -1 };
}
// Expect the instruction to FAIL with the on-chain custom error `errCode`.
async function expectErr(conn, ixs, signers, errCode, what) {
  try {
    await send(conn, ixs, signers);
  } catch (e) {
    const m = String(e.message || e);
    if (m.includes(`"Custom":${errCode}`) || m.includes(`Custom(${errCode})`) || m.includes(`0x${errCode.toString(16)}`))
      return `rejected with ${errCode} as required`;
    throw new Error(`${what}: failed but with the WRONG error: ${m.slice(0, 200)}`);
  }
  throw new Error(`${what}: must be rejected, but succeeded`);
}

const stages = [];
async function stage(name, fn) {
  try { const r = await fn(); stages.push({ name, ok: true }); console.log(`  ✓ ${name}${typeof r === "string" ? " — " + r : ""}`); return r; }
  catch (e) { stages.push({ name, ok: false, err: String(e.message || e).slice(0, 200) }); console.log(`  ✗ ${name}: ${String(e.message || e).slice(0, 200)}`); throw e; }
}
function decodeOutbox(data) {
  return { produced: Number(data.readBigUInt64LE(8)), settled: Number(data.readBigUInt64LE(16)), cap: data.readUInt32LE(24) };
}

console.log(`withdraw-anytime acceptance — L1=${L1_RPC} ER=${ER_RPC}`);
const CAP = 105;

// On-chain custom error codes = errors.rs discriminant + 6000.
const ERR_USE_XDOMAIN = 8302;      // UseXDomainWithdraw
const ERR_ER_MARGIN_RESERVED = 8301; // ErMarginReserved

const DEPOSIT = 5_000_000_000;  // 5,000 quote-lots each
const RESERVE = 2_000_000_000;  // sequencer-attested margin behind the resting bids

// ── shared trader setup: BOTH traders are fresh keypairs, so every run starts
// from a clean state (deterministic balances, epoch 0, no leftover positions) ──
const maker = Keypair.generate();
const taker = Keypair.generate();
const makerTS = traderStatePda(maker.publicKey);
const takerTS = traderStatePda(taker.publicKey);
const makerAta = ata(maker.publicKey, QUOTE);
const takerAta = ata(taker.publicKey, QUOTE);
const makerEM = pda(["er_margin", makerTS]);
const takerEM = pda(["er_margin", takerTS]);
let epoch = 0; // strictly-increasing across the whole run
const attestIx = (reserved) =>
  program.methods.attestErReservedMargin(new BN(reserved), new BN(++epoch)).accountsPartial({ attestor: signer.publicKey, erMargin: makerEM, traderState: makerTS }).instruction();

// A fresh cap-105 market: init book/ring/outbox, return the PDAs.
async function buildMarket(refParams) {
  const base = Keypair.generate();
  const M = pda(["market", base.publicKey, QUOTE]);
  const BOOK = pda(["market_book", M]);
  const FC = pda(["fill_commit", M]);
  const FO = pda(["fill_outbox", M]);
  await send(l1, await program.methods.initializeMarket(refParams, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
  await send(l1, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction(), []);
  await send(l1, await program.methods.initFillCommitment(CAP).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction(), []);
  await send(l1, await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, fillCommitment: FC, systemProgram: sys }).instruction(), []);
  return { M, BOOK, FC, FO };
}

const restBidIx = (mkt, tick) =>
  program.methods.placeLimitOrderV2(0, new BN(1), new BN(tick), 0, new BN(0), 0).accountsPartial({ trader: maker.publicKey, market: mkt.M, marketBook: mkt.BOOK, traderState: makerTS, position: null }).instruction();
const takerSweepIx = (mkt) =>
  program.methods.placeTakerOrderV2(1, new BN(4), new BN(1), 0, new BN(0), 0)
    .accountsPartial({ trader: taker.publicKey, market: mkt.M, marketBook: mkt.BOOK, traderState: takerTS, position: null })
    .remainingAccounts([{ pubkey: mkt.FC, isWritable: true, isSigner: false }, { pubkey: mkt.FO, isWritable: true, isSigner: false }]).instruction();
const strictWithdrawIx = (amount) =>
  program.methods.withdrawCollateral(new BN(amount)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();
const xdomainWithdrawIx = (amount) =>
  program.methods.withdrawCollateralXdomain(new BN(amount)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, erMargin: makerEM, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();
const partialXdomainIx = (amount) =>
  program.methods.partialWithdrawCollateralXdomain(new BN(amount)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, erMargin: makerEM, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();

try {
  const ref = await program.account.marketAccount.fetch(REF_MARKET);
  if (!ref.params.oracleStalenessMaxSeconds || ref.params.oracleStalenessMaxSeconds === 0) {
    ref.params.oracleStalenessMaxSeconds = 60;
  }

  // ════ shared: traders + deposits + attestation accounts ════
  const signerAta = ata(signer.publicKey, QUOTE);
  await stage("L1 fund maker + taker (open + deposit + er-margin attestation accounts)", async () => {
    await send(l1, [
      SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: maker.publicKey, lamports: 80_000_000 }),
      SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 80_000_000 }),
    ], []);
    // Maker gets DEPOSIT (phase A) + 3e9 (phase B top-up) in its fresh ATA.
    await send(l1, [createAtaIx(signer.publicKey, maker.publicKey, QUOTE), transferIx(signerAta, makerAta, signer.publicKey, DEPOSIT + 3_000_000_000)], []);
    await send(l1, [createAtaIx(signer.publicKey, taker.publicKey, QUOTE), transferIx(signerAta, takerAta, signer.publicKey, DEPOSIT)], []);
    await send(l1, await program.methods.openTraderState().accountsPartial({ trader: maker.publicKey, traderState: makerTS, systemProgram: sys }).instruction(), [maker]);
    await send(l1, await program.methods.openTraderState().accountsPartial({ trader: taker.publicKey, traderState: takerTS, systemProgram: sys }).instruction(), [taker]);
    const dep = (trader, traderState, tAta) => program.methods.depositCollateral(new BN(DEPOSIT)).accountsPartial({ trader, traderState, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: tAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();
    await send(l1, await dep(maker.publicKey, makerTS, makerAta), [maker]);
    await send(l1, await dep(taker.publicKey, takerTS, takerAta), [taker]);
    // Order placement on a delegated book requires er_margin_ready: init the
    // attestation accounts (authority pins the attestor).
    const initAttest = (traderState, erMargin) => program.methods.initErMarginAttestation(signer.publicKey).accountsPartial({ authority: signer.publicKey, insuranceFund: INS, traderState, erMargin, systemProgram: sys }).instruction();
    await send(l1, await initAttest(makerTS, makerEM), []);
    await send(l1, await initAttest(takerTS, takerEM), []);
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    if (ts.erMarginReady !== 1) throw new Error("er_margin_ready must be set after init_er_margin_attestation");
  });

  // ════ Phase A — live-ER round trip: withdraw mid-session against ER orders ════
  const A = await stage(`A: L1 init market + book + ring + outbox (cap ${CAP})`, () => buildMarket(ref.params));
  console.log("  phase-A market", A.M.toBase58());

  const delegArgs = (acct) => ({
    buf: pda(["buffer", acct]),
    rec: pda(["delegation", acct], DELEG),
    meta: pda(["delegation-metadata", acct], DELEG),
  });
  await stage("A: L1 delegate market + book + ring + outbox → DLP", async () => {
    const b = delegArgs(A.BOOK);
    await send(l1, await program.methods.delegateMarketBook(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: A.M, marketBook: A.BOOK, ownerProgram: PID, delegateBuffer: b.buf, delegationRecord: b.rec, delegationMetadata: b.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const c = delegArgs(A.FC);
    await send(l1, await program.methods.delegateFillCommitment(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: A.M, fillCommitment: A.FC, ownerProgram: PID, delegateBuffer: c.buf, delegationRecord: c.rec, delegationMetadata: c.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const o = delegArgs(A.FO);
    await send(l1, await program.methods.delegateFillOutbox(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: A.M, fillOutbox: A.FO, ownerProgram: PID, delegateBuffer: o.buf, delegationRecord: o.rec, delegationMetadata: o.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const m = delegArgs(A.M);
    await send(l1, await program.methods.delegateMarket(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: A.M, ownerProgram: PID, delegateBuffer: m.buf, delegationRecord: m.rec, delegationMetadata: m.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  });
  await sleep(4000);

  await stage("A: ER maker rests 4 bids on the delegated book", async () => {
    for (let i = 0; i < 4; i++) await send(er, await restBidIx(A, 90000 - i * 10), [maker]);
  });

  await stage(`A: L1 sequencer attests reserved margin = ${RESERVE} → er_active`, async () => {
    await send(l1, await attestIx(RESERVE), []);
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    if (ts.erActive !== 1) throw new Error("er_active must be 1 after attesting a live reservation");
  });

  await stage("A: L1 strict withdraw while ER-active → rejected (UseXDomainWithdraw)", async () =>
    expectErr(l1, await strictWithdrawIx(1_000), [maker], ERR_USE_XDOMAIN, "strict withdraw while ER-active"));

  await stage("A: L1 xdomain over-withdraw (breaches reservation) → rejected (ErMarginReserved)", async () =>
    expectErr(l1, await xdomainWithdrawIx(DEPOSIT - RESERVE + 1_000), [maker], ERR_ER_MARGIN_RESERVED, "xdomain over-withdraw"));

  await stage("A: L1 withdraw FREE balance mid-session (ER orders live) → succeeds", async () => {
    const before = await program.account.traderStateAccount.fetch(makerTS);
    const ataBefore = BigInt((await l1.getTokenAccountBalance(makerAta)).value.amount);
    const part = 500_000_000;                                  // partial xdomain path
    const { sig: s1, cu: c1 } = await sendCu(l1, await partialXdomainIx(part), [maker]);
    const rest = before.collateralQuoteLots.toNumber() - RESERVE - part; // drain to EXACTLY the reservation
    const { sig: s2, cu: c2 } = await sendCu(l1, await xdomainWithdrawIx(rest), [maker]);
    const after = await program.account.traderStateAccount.fetch(makerTS);
    const ataAfter = BigInt((await l1.getTokenAccountBalance(makerAta)).value.amount);
    if (after.collateralQuoteLots.toNumber() !== RESERVE) throw new Error(`collateral after = ${after.collateralQuoteLots}, expected ${RESERVE}`);
    if (ataAfter - ataBefore !== BigInt(part + rest)) throw new Error(`ATA delta ${ataAfter - ataBefore}, expected ${part + rest}`);
    return `state ${before.collateralQuoteLots}→${after.collateralQuoteLots}, ATA +${ataAfter - ataBefore}; partial ${s1} (${c1} CU), strict ${s2} (${c2} CU)`;
  });

  await stage("A: ER taker sweep → 4 fills (ring + outbox on the ER)", async () => {
    await send(er, await takerSweepIx(A), [taker], 1_400_000);
  });

  await stage("A: ER commit_and_undelegate book/ring/outbox → L1 finalize", async () => {
    await send(er, await program.methods.commitAndUndelegateMarketBook().accountsPartial({ payer: signer.publicKey, marketBook: A.BOOK, market: A.M, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitAndUndelegateFillCommitment().accountsPartial({ payer: signer.publicKey, fillCommitment: A.FC, market: A.M, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
    await send(er, await program.methods.commitAndUndelegateFillOutbox().accountsPartial({ payer: signer.publicKey, fillOutbox: A.FO, market: A.M, magicContext: MAGIC_CONTEXT, magicProgram: MAGIC_PROGRAM }).instruction(), []);
  });
  await sleep(7000);

  await stage("A: L1 assert undelegated + ring/outbox produced == 4 (fills survived, settleable)", async () => {
    for (const [name, k] of [["ring", A.FC], ["outbox", A.FO], ["book", A.BOOK]]) {
      const a = await l1.getAccountInfo(k);
      if (!a || !a.owner.equals(PID)) throw new Error(`${name} not back under program after undelegate`);
    }
    const r = decodeOutbox((await l1.getAccountInfo(A.FC)).data);
    if (r.produced !== 4) throw new Error(`ring produced=${r.produced}, expected 4`);
    const o = decodeOutbox((await l1.getAccountInfo(A.FO)).data);
    if (o.produced !== 4) throw new Error(`outbox produced=${o.produced}, expected 4`);
  });

  await stage("A: L1 attest 0 (orders consumed) → er_active clears", async () => {
    await send(l1, await attestIx(0), []);
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    if (ts.erActive !== 0) throw new Error("er_active must clear when the reservation returns to 0");
  });

  // ════ Phase B — L1-resident market: the same gates around REAL settlement ════
  const B = await stage(`B: L1 init market + book + ring + outbox (L1-resident)`, () => buildMarket(ref.params));
  console.log("  phase-B market", B.M.toBase58());

  await stage("B: L1 maker re-deposits + rests 4 bids on the L1 book", async () => {
    await send(l1, await program.methods.depositCollateral(new BN(3_000_000_000)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction(), [maker]);
    for (let i = 0; i < 4; i++) await send(l1, await restBidIx(B, 90000 - i * 10), [maker]);
  });

  await stage(`B: L1 attest reserved margin = ${RESERVE} for the resting bids`, async () => {
    await send(l1, await attestIx(RESERVE), []);
  });

  await stage("B: L1 xdomain over-withdraw → rejected (ErMarginReserved)", async () => {
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    const free = ts.collateralQuoteLots.toNumber() - RESERVE;
    return expectErr(l1, await xdomainWithdrawIx(free + 1_000), [maker], ERR_ER_MARGIN_RESERVED, "xdomain over-withdraw");
  });

  await stage("B: L1 partial xdomain withdraw of free balance (orders resting) → succeeds", async () => {
    const before = await program.account.traderStateAccount.fetch(makerTS);
    const { sig, cu } = await sendCu(l1, await partialXdomainIx(500_000_000), [maker]);
    const after = await program.account.traderStateAccount.fetch(makerTS);
    if (before.collateralQuoteLots.toNumber() - after.collateralQuoteLots.toNumber() !== 500_000_000)
      throw new Error("collateral delta mismatch");
    return `${sig} (${cu} CU)`;
  });

  await stage("B: L1 taker sweep → 4 fills committed to the ring", async () => {
    await send(l1, await takerSweepIx(B), [taker], 1_400_000);
    const r = decodeOutbox((await l1.getAccountInfo(B.FC)).data);
    if (r.produced !== 4) throw new Error(`ring produced=${r.produced}, expected 4`);
  });

  const makerPos = pda(["position", B.M, makerTS]);
  const takerPos = pda(["position", B.M, takerTS]);
  await stage("B: L1 apply_fill × 4 (ring-authenticated) settles into positions", async () => {
    const cus = [];
    for (let i = 0; i < 4; i++) {
      const tick = 90000 - i * 10;
      const { cu } = await sendCu(l1, await program.methods.applyFill(new BN(1), new BN(tick), 1, false, 0, 0, new BN(i + 1))
        .accountsPartial({ sequencer: signer.publicKey, market: B.M, insuranceFund: INS, takerTraderState: takerTS, makerTraderState: makerTS, takerPosition: takerPos, makerPosition: makerPos, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, makerPositionHaircut: null, systemProgram: sys })
        .remainingAccounts([{ pubkey: B.FC, isWritable: true, isSigner: false }]).instruction(), [], 600_000);
      cus.push(cu);
    }
    const mp = await program.account.positionAccount.fetch(makerPos);
    const tp = await program.account.positionAccount.fetch(takerPos);
    if (mp.sizeLots.toNumber() !== 4 || mp.side !== 0) throw new Error(`maker position ${mp.sizeLots}/${mp.side}, expected 4 long`);
    if (tp.sizeLots.toNumber() !== 4 || tp.side !== 1) throw new Error(`taker position ${tp.sizeLots}/${tp.side}, expected 4 short`);
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    return `maker 4 long, taker 4 short; maker collateral ${ts.collateralQuoteLots} (reservation held through settlement); CU ${cus.join("/")}`;
  });

  await stage("B: L1 attest 0 → er_active clears → plain partial withdraw (position walk) works", async () => {
    await send(l1, await attestIx(0), []);
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    if (ts.erActive !== 0) throw new Error("er_active must clear when the reservation returns to 0");
    const { sig, cu } = await sendCu(l1, await program.methods.partialWithdrawCollateral(new BN(1_000_000)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM })
      .remainingAccounts([{ pubkey: B.M, isWritable: false, isSigner: false }, { pubkey: makerPos, isWritable: false, isSigner: false }]).instruction(), [maker], 600_000);
    return `${sig} (${cu} CU)`;
  });
} catch (e) {
  // a stage already logged the failure; fall through to the report
}

const passed = stages.filter((s) => s.ok).length;
console.log(`\n========== WITHDRAW-ANYTIME ACCEPTANCE: ${passed}/${stages.length} stages ==========`);
for (const s of stages) console.log(`  ${s.ok ? "PASS" : "FAIL"}  ${s.name}${s.ok ? "" : "  — " + s.err}`);
const allOk = stages.length > 0 && stages.every((s) => s.ok);
console.log(allOk ? "\nWITHDRAW-ANYTIME PASS ✅ (deposit → rest orders → withdraw free balance mid-session → fills settle → paths re-open, no lock anywhere)" : "\nWITHDRAW-ANYTIME INCOMPLETE ❌ (see first FAIL above)");
process.exit(allOk ? 0 : 1);
