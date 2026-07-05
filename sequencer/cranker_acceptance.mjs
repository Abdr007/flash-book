// Attestation-cranker live acceptance — proves the PRODUCTION withdraw-anytime
// loop end-to-end on the real MagicBlock devnet ER with NO manual attestation:
// the cranker alone observes the delegated book and drives L1 attestations.
//
//   rest orders on ER → cranker attests the exact computed reservation →
//   strict withdraw fails closed → over-withdraw rejected → free balance
//   withdrawable mid-session → taker consumes the orders → the reservation
//   PERSISTS as unsettled-fill margin (maker AND taker) and the last lots stay
//   locked → on an L1-resident market the same fills SETTLE via apply_fill and
//   the cranker releases exactly the settled market's reservation.
//
// GATED: runs only when ER_RPC is set; skips cleanly (exit 0) otherwise.
//
//   L1_RPC=<devnet> ER_RPC=https://devnet-as.magicblock.app node cranker_acceptance.mjs
import fs from "fs";
import os from "os";
import { spawn } from "child_process";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";

const { AnchorProvider, Program, Wallet, BN } = anchor;

const ER_RPC = process.env.ER_RPC;
if (!ER_RPC) {
  console.log("SKIP cranker acceptance: set ER_RPC=<MagicBlock ER endpoint> to run.");
  process.exit(0);
}
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";

const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const DELEG = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const ER_VALIDATOR = new PublicKey(process.env.ER_VALIDATOR || "MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");
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

process.on("unhandledRejection", (e) => console.log(`  (background rpc noise: ${String(e?.message || e).slice(0, 120)})`));

async function send(conn, ixs, signers, cuLimit = 400_000) {
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cuLimit }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  for (let attempt = 0; ; attempt++) {
    try {
      const sig = await sendAndConfirmTransaction(conn, tx, [signer, ...signers], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
      await sleep(400);
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
async function expectErr(conn, ixs, signers, errCode, what) {
  try {
    await send(conn, ixs, signers);
  } catch (e) {
    const m = String(e.message || e);
    if (m.includes(`"Custom":${errCode}`) || m.includes(`Custom(${errCode})`)) return `rejected with ${errCode} as required`;
    throw new Error(`${what}: failed but with the WRONG error: ${m.slice(0, 200)}`);
  }
  throw new Error(`${what}: must be rejected, but succeeded`);
}
const stages = [];
async function stage(name, fn) {
  try { const r = await fn(); stages.push({ name, ok: true }); console.log(`  ✓ ${name}${typeof r === "string" ? " — " + r : ""}`); return r; }
  catch (e) { stages.push({ name, ok: false, err: String(e.message || e).slice(0, 200) }); console.log(`  ✗ ${name}: ${String(e.message || e).slice(0, 200)}`); throw e; }
}
// Poll until fn() is truthy or timeout.
async function until(what, fn, timeoutMs = 45_000, everyMs = 1500) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t0 > timeoutMs) throw new Error(`timeout waiting for ${what}`);
    await sleep(everyMs);
  }
}

const ERR_USE_XDOMAIN = 8302;
const ERR_ER_MARGIN_RESERVED = 8301;
const DEPOSIT = 5_000_000_000;

console.log(`cranker acceptance — L1=${L1_RPC} ER=${ER_RPC}`);
const maker = Keypair.generate();
const taker = Keypair.generate();
const makerTS = pda(["trader_state", maker.publicKey]);
const takerTS = pda(["trader_state", taker.publicKey]);
const makerAta = ata(maker.publicKey, QUOTE);
const takerAta = ata(taker.publicKey, QUOTE);
const makerEM = pda(["er_margin", makerTS]);
const takerEM = pda(["er_margin", takerTS]);

let cranker = null;
try {
  const ref = await program.account.marketAccount.fetch(REF_MARKET);
  if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60;
  const tickSize = BigInt(ref.params.tickSize.toString());
  const imBps = BigInt(ref.params.initialMarginRatioBps);

  const base = Keypair.generate();
  const M = pda(["market", base.publicKey, QUOTE]);
  const BOOK = pda(["market_book", M]);
  const FC = pda(["fill_commit", M]);
  const FO = pda(["fill_outbox", M]);
  // Market B (L1-resident, created mid-run — the cranker watches it from spawn
  // and picks it up lazily) proves release-at-settlement.
  const baseB = Keypair.generate();
  const MB = pda(["market", baseB.publicKey, QUOTE]);
  const BOOKB = pda(["market_book", MB]);
  const FCB = pda(["fill_commit", MB]);
  const FOB = pda(["fill_outbox", MB]);
  console.log("market A", M.toBase58(), "| market B", MB.toBase58());

  await stage("L1 init market + book + ring + outbox", async () => {
    await send(l1, await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
    await send(l1, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.initFillCommitment(105).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, fillCommitment: FC, systemProgram: sys }).instruction(), []);
  });

  await stage("L1 fund maker + taker + attestation accounts", async () => {
    const signerAta = ata(signer.publicKey, QUOTE);
    await send(l1, [
      SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: maker.publicKey, lamports: 80_000_000 }),
      SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 80_000_000 }),
    ], []);
    await send(l1, [createAtaIx(signer.publicKey, maker.publicKey, QUOTE), transferIx(signerAta, makerAta, signer.publicKey, DEPOSIT)], []);
    await send(l1, [createAtaIx(signer.publicKey, taker.publicKey, QUOTE), transferIx(signerAta, takerAta, signer.publicKey, DEPOSIT)], []);
    await send(l1, await program.methods.openTraderState().accountsPartial({ trader: maker.publicKey, traderState: makerTS, systemProgram: sys }).instruction(), [maker]);
    await send(l1, await program.methods.openTraderState().accountsPartial({ trader: taker.publicKey, traderState: takerTS, systemProgram: sys }).instruction(), [taker]);
    const dep = (trader, traderState, tAta) => program.methods.depositCollateral(new BN(DEPOSIT)).accountsPartial({ trader, traderState, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: tAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();
    await send(l1, await dep(maker.publicKey, makerTS, makerAta), [maker]);
    await send(l1, await dep(taker.publicKey, takerTS, takerAta), [taker]);
    const initAttest = (traderState, erMargin) => program.methods.initErMarginAttestation(signer.publicKey).accountsPartial({ authority: signer.publicKey, insuranceFund: INS, traderState, erMargin, systemProgram: sys }).instruction();
    await send(l1, await initAttest(makerTS, makerEM), []);
    await send(l1, await initAttest(takerTS, takerEM), []);
  });

  const delegArgs = (acct) => ({
    buf: pda(["buffer", acct]),
    rec: pda(["delegation", acct], DELEG),
    meta: pda(["delegation-metadata", acct], DELEG),
  });
  await stage("L1 delegate market + book + ring + outbox → DLP", async () => {
    const MAGIC_ARGS = [30000, ER_VALIDATOR];
    const b = delegArgs(BOOK);
    await send(l1, await program.methods.delegateMarketBook(...MAGIC_ARGS).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, ownerProgram: PID, delegateBuffer: b.buf, delegationRecord: b.rec, delegationMetadata: b.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const c = delegArgs(FC);
    await send(l1, await program.methods.delegateFillCommitment(...MAGIC_ARGS).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, ownerProgram: PID, delegateBuffer: c.buf, delegationRecord: c.rec, delegationMetadata: c.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const o = delegArgs(FO);
    await send(l1, await program.methods.delegateFillOutbox(...MAGIC_ARGS).accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, ownerProgram: PID, delegateBuffer: o.buf, delegationRecord: o.rec, delegationMetadata: o.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
    const m = delegArgs(M);
    await send(l1, await program.methods.delegateMarket(...MAGIC_ARGS).accountsPartial({ authority: signer.publicKey, market: M, ownerProgram: PID, delegateBuffer: m.buf, delegationRecord: m.rec, delegationMetadata: m.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  });
  await sleep(4000);

  await stage("spawn the attestation cranker (no manual attests from here on)", async () => {
    cranker = spawn(process.execPath, [new URL("./attestation_cranker.mjs", import.meta.url).pathname], {
      env: { ...process.env, L1_RPC, ER_RPC, MARKETS: `${M.toBase58()},${MB.toBase58()}`, INTERVAL_MS: "1500" },
      stdio: ["ignore", "pipe", "pipe"],
    });
    cranker.stdout.on("data", (d) => process.stdout.write(`    [cranker] ${d}`));
    cranker.stderr.on("data", (d) => process.stdout.write(`    [cranker!] ${d}`));
    await sleep(3000);
    if (cranker.exitCode !== null) throw new Error(`cranker exited early (${cranker.exitCode})`);
  });

  // Expected reservation for 4 × 1-lot bids at ticks 90000, 89990, 89980, 89970
  // under the cranker's policy: ceil(size×price×tick_size×im_bps/10000) each.
  const BPS = 10_000n;
  let expected = 0n;
  for (let i = 0; i < 4; i++) {
    const notional = 1n * BigInt(90000 - i * 10) * tickSize;
    expected += (notional * imBps + BPS - 1n) / BPS;
  }

  await stage("ER maker rests 4 bids on the delegated book", async () => {
    for (let i = 0; i < 4; i++)
      await send(er, await program.methods.placeLimitOrderV2(0, new BN(1), new BN(90000 - i * 10), 0, new BN(0), 0).accountsPartial({ trader: maker.publicKey, market: M, marketBook: BOOK, traderState: makerTS, position: null }).instruction(), [maker]);
  });

  await stage(`cranker converges the attestation to ${expected} (no manual attest)`, async () => {
    // The cranker attests continuously, so a snapshot taken while orders are
    // still landing yields an intermediate value; wait for convergence to the
    // final book, not merely the first nonzero attestation.
    const att = await until(`cranker attestation == ${expected}`, async () => {
      const a = await program.account.erMarginAttestation.fetch(makerEM);
      return BigInt(a.reservedMarginQuoteLots.toString()) === expected ? a : null;
    });
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    if (ts.erActive !== 1) throw new Error("er_active must be 1 after the cranker attests");
    return `reserved ${expected} (epoch ${att.epoch})`;
  });

  const strictWithdrawIx = (amount) =>
    program.methods.withdrawCollateral(new BN(amount)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();
  const xdomainWithdrawIx = (amount) =>
    program.methods.withdrawCollateralXdomain(new BN(amount)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, erMargin: makerEM, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();

  await stage("L1 strict withdraw while cranker-attested → rejected (UseXDomainWithdraw)", async () =>
    expectErr(l1, await strictWithdrawIx(1_000), [maker], ERR_USE_XDOMAIN, "strict withdraw"));

  await stage("L1 xdomain over-withdraw → rejected (ErMarginReserved)", async () => {
    const free = BigInt(DEPOSIT) - expected;
    return expectErr(l1, await xdomainWithdrawIx(new BN((free + 1_000n).toString())), [maker], ERR_ER_MARGIN_RESERVED, "over-withdraw");
  });

  await stage("L1 withdraw the FREE balance mid-session → succeeds, reservation stays", async () => {
    const free = BigInt(DEPOSIT) - expected;
    const sig = await send(l1, await xdomainWithdrawIx(new BN(free.toString())), [maker]);
    const ts = await program.account.traderStateAccount.fetch(makerTS);
    if (BigInt(ts.collateralQuoteLots.toString()) !== expected) throw new Error(`post-withdraw collateral ${ts.collateralQuoteLots}, expected ${expected}`);
    return `withdrew ${free}, kept exactly the reservation; ${sig.slice(0, 20)}…`;
  });

  await stage("ER taker sweeps all 4 bids", async () => {
    await send(er, await program.methods.placeTakerOrderV2(1, new BN(4), new BN(1), 0, new BN(0), 0)
      .accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK, traderState: takerTS, position: null })
      .remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }, { pubkey: FO, isWritable: true, isSigner: false }]).instruction(), [taker], 1_400_000);
  });

  await stage("cranker carries the reservation through the UNSETTLED-FILL window (maker + taker)", async () => {
    // The orders left the book, but their fills sit in the outbox awaiting
    // settlement: the maker's reservation must persist and the taker must gain
    // an equal one — no gap between order margin and fill margin.
    await until(`taker attestation == ${expected}`, async () => {
      const a = await program.account.erMarginAttestation.fetch(takerEM);
      return BigInt(a.reservedMarginQuoteLots.toString()) === expected ? a : null;
    });
    const m = await program.account.erMarginAttestation.fetch(makerEM);
    if (BigInt(m.reservedMarginQuoteLots.toString()) !== expected)
      throw new Error(`maker reservation ${m.reservedMarginQuoteLots}, expected ${expected} (must persist through the fill window)`);
    return `maker ${expected} persists, taker ${expected} appears`;
  });

  await stage("maker's last lots NOT withdrawable while its fills are unsettled → rejected (ErMarginReserved)", async () =>
    expectErr(l1, await xdomainWithdrawIx(1_000), [maker], ERR_ER_MARGIN_RESERVED, "withdraw against unsettled fills"));

  // ════ Market B (L1-resident): the same window RELEASES at settlement ════
  await stage("B: L1 init market + book + ring + outbox (cranker picks it up lazily)", async () => {
    await send(l1, await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: baseB.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: MB, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [baseB]);
    await send(l1, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: MB, marketBook: BOOKB, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.initFillCommitment(105).accountsPartial({ authority: signer.publicKey, market: MB, fillCommitment: FCB, systemProgram: sys }).instruction(), []);
    await send(l1, await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: MB, fillOutbox: FOB, fillCommitment: FCB, systemProgram: sys }).instruction(), []);
  });

  await stage("B: maker re-deposits + rests 4 bids on the L1 book → reservation stacks across markets", async () => {
    await send(l1, await program.methods.depositCollateral(new BN(1_000_000_000)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction(), [maker]);
    for (let i = 0; i < 4; i++)
      await send(l1, await program.methods.placeLimitOrderV2(0, new BN(1), new BN(90000 - i * 10), 0, new BN(0), 0).accountsPartial({ trader: maker.publicKey, market: MB, marketBook: BOOKB, traderState: makerTS, position: null }).instruction(), [maker]);
    await until(`maker attestation == ${expected * 2n} (A fills + B orders)`, async () => {
      const a = await program.account.erMarginAttestation.fetch(makerEM);
      return BigInt(a.reservedMarginQuoteLots.toString()) === expected * 2n ? a : null;
    });
    return `maker reservation ${expected * 2n} = ${expected} unsettled A-fills + ${expected} resting B-orders`;
  });

  await stage("B: taker sweeps on L1 → both sides reserved for BOTH markets' unsettled fills", async () => {
    await send(l1, await program.methods.placeTakerOrderV2(1, new BN(4), new BN(1), 0, new BN(0), 0)
      .accountsPartial({ trader: taker.publicKey, market: MB, marketBook: BOOKB, traderState: takerTS, position: null })
      .remainingAccounts([{ pubkey: FCB, isWritable: true, isSigner: false }, { pubkey: FOB, isWritable: true, isSigner: false }]).instruction(), [taker], 1_400_000);
    await until(`taker attestation == ${expected * 2n}`, async () => {
      const a = await program.account.erMarginAttestation.fetch(takerEM);
      return BigInt(a.reservedMarginQuoteLots.toString()) === expected * 2n ? a : null;
    });
  });

  const makerPosB = pda(["position", MB, makerTS]);
  const takerPosB = pda(["position", MB, takerTS]);
  await stage("B: apply_fill × 4 settles → cranker releases EXACTLY the settled market's reservation", async () => {
    for (let i = 0; i < 4; i++) {
      const tick = 90000 - i * 10;
      await send(l1, await program.methods.applyFill(new BN(1), new BN(tick), 1, false, 0, 0, new BN(i + 1))
        .accountsPartial({ sequencer: signer.publicKey, market: MB, insuranceFund: INS, takerTraderState: takerTS, makerTraderState: makerTS, takerPosition: takerPosB, makerPosition: makerPosB, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, makerPositionHaircut: null, systemProgram: sys })
        .remainingAccounts([{ pubkey: FCB, isWritable: true, isSigner: false }]).instruction(), [], 600_000);
    }
    await until(`maker attestation back to ${expected} (B settled, A persists)`, async () => {
      const a = await program.account.erMarginAttestation.fetch(makerEM);
      return BigInt(a.reservedMarginQuoteLots.toString()) === expected ? a : null;
    });
    const t = await program.account.erMarginAttestation.fetch(takerEM);
    if (BigInt(t.reservedMarginQuoteLots.toString()) !== expected)
      throw new Error(`taker reservation ${t.reservedMarginQuoteLots}, expected ${expected}`);
    return `both sides released ${expected} at settlement; market A's unsettled ${expected} correctly persists`;
  });

  await stage("maker still withdraws free balance mid-everything (partial xdomain, position walk)", async () => {
    const sig = await send(l1, await program.methods.partialWithdrawCollateralXdomain(new BN(100_000)).accountsPartial({ trader: maker.publicKey, traderState: makerTS, erMargin: makerEM, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: makerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM })
      .remainingAccounts([{ pubkey: MB, isWritable: false, isSigner: false }, { pubkey: makerPosB, isWritable: false, isSigner: false }]).instruction(), [maker], 600_000);
    return sig.slice(0, 20) + "…";
  });
} catch (e) {
  // a stage already logged the failure; fall through to the report
} finally {
  if (cranker && cranker.exitCode === null) cranker.kill("SIGTERM");
}

const passed = stages.filter((s) => s.ok).length;
console.log(`\n========== CRANKER ACCEPTANCE: ${passed}/${stages.length} stages ==========`);
for (const s of stages) console.log(`  ${s.ok ? "PASS" : "FAIL"}  ${s.name}${s.ok ? "" : "  — " + s.err}`);
const allOk = stages.length > 0 && stages.every((s) => s.ok);
console.log(allOk ? "\nCRANKER PASS ✅ (orders AND unsettled fills stay reserved with zero manual steps; released exactly at settlement)" : "\nCRANKER INCOMPLETE ❌ (see first FAIL above)");
process.exit(allOk ? 0 : 1);
