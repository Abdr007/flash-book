// ── Track B — MagicBlock ER fill-latency benchmark (devnet, reproduced) ──────────
//
// Measures the CLIENT-OBSERVED round-trip of a REAL taker fill on the MagicBlock ER:
// build placeTakerOrderV2 → sendRawTransaction to the ER validator → confirm. Reuses
// the proven er_acceptance genesis (init market + book/ring/outbox, delegate to the
// ER, fund maker+taker, ER margin attestations), then loops rest-bid → timed-taker.
//
// METHODOLOGY (disclosed — read before quoting a number):
//   • Clock: performance.now() around send→confirm("confirmed") on the ER connection.
//     This includes CLIENT↔ER network RTT (this client is NOT co-located with the ER
//     validator MAS1Dt9… / devnet-as.magicblock.app), so it is an UPPER BOUND on the
//     ER-side execution latency, not pure execution time. A co-located client (or the
//     ER's own slot cadence, ~sub-50ms) is the lower bound.
//   • Warm: WARMUP iterations discarded; steady-state only.
//   • Per-fill: size-1 taker fully matches one resting size-1 bid (book returns empty;
//     positions form only at post-undelegate settlement, so no ER-side margin accrual).
//   • Captured per tx: signature, wall-clock ms, compute units (from getTransaction).
//   • Reported: N, p50/p90/p95/p99, min/max, mean, + raw rows. No best-case dressing.
//
//   L1_RPC=https://api.devnet.solana.com ER_RPC=https://devnet-as.magicblock.app \
//     SAMPLES=40 node er-acceptance/latency_benchmark.mjs
import fs from "fs";
import os from "os";
import { performance } from "perf_hooks";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";
const { AnchorProvider, Program, Wallet, BN } = anchor;

const ER_RPC = process.env.ER_RPC;
if (!ER_RPC) { console.log("SKIP: set ER_RPC=<MagicBlock ER endpoint> (e.g. https://devnet-as.magicblock.app)"); process.exit(0); }
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const SAMPLES = parseInt(process.env.SAMPLES || "40", 10);
const WARMUP = parseInt(process.env.WARMUP || "5", 10);

const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const DELEG = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
const ER_VALIDATOR = new PublicKey(process.env.ER_VALIDATOR || "MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");
const sys = SystemProgram.programId;
const TOKEN_PROGRAM = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA_PROGRAM = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const ata = (owner, mint) => PublicKey.findProgramAddressSync([owner.toBuffer(), TOKEN_PROGRAM.toBuffer(), mint.toBuffer()], ATA_PROGRAM)[0];
const createAtaIx = (payer, owner, mint) => new anchor.web3.TransactionInstruction({ programId: ATA_PROGRAM, keys: [ { pubkey: payer, isSigner: true, isWritable: true }, { pubkey: ata(owner, mint), isSigner: false, isWritable: true }, { pubkey: owner, isSigner: false, isWritable: false }, { pubkey: mint, isSigner: false, isWritable: false }, { pubkey: sys, isSigner: false, isWritable: false }, { pubkey: TOKEN_PROGRAM, isSigner: false, isWritable: false } ], data: Buffer.from([1]) });
const transferIx = (from, to, authority, amount) => { const d = Buffer.alloc(9); d.writeUInt8(3, 0); d.writeBigUInt64LE(BigInt(amount), 1); return new anchor.web3.TransactionInstruction({ programId: TOKEN_PROGRAM, keys: [ { pubkey: from, isSigner: false, isWritable: true }, { pubkey: to, isSigner: false, isWritable: true }, { pubkey: authority, isSigner: true, isWritable: false } ], data: d }); };
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
const traderStatePda = (t) => pda(["trader_state", t]);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sleep0 = (ms) => new Promise((r) => setTimeout(r, ms));
async function withRetry(fn, label = "") {
  for (let attempt = 0; ; attempt++) {
    try { return await fn(); }
    catch (e) {
      const msg = String(e.message || e);
      if (/429|Too Many Requests/i.test(msg) && attempt < 8) { await sleep0(1000 * (attempt + 1)); continue; }
      throw e;
    }
  }
}
async function send(conn, ixs, signers, cuLimit = 400_000) {
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cuLimit }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  return await withRetry(() => sendAndConfirmTransaction(conn, tx, [signer, ...signers], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 }), "send");
}
const decodeOutbox = (d) => ({ produced: Number(d.readBigUInt64LE(8)), settled: Number(d.readBigUInt64LE(16)), cap: d.readUInt32LE(24) });
const pct = (a, p) => { const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))]; };

console.log(`ER fill-latency benchmark — L1=${L1_RPC} ER=${ER_RPC} samples=${SAMPLES} warmup=${WARMUP}`);
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);
const FO = pda(["fill_outbox", M]);
const CAP = 105;
// Fresh maker + taker keypairs (0 open positions) — the reused `signer` accrues
// positions across devnet runs and would hit the MAX_POSITIONS intake gate
// (TooManyOpenPositions/2323). `signer` stays the authority + fee payer.
const maker = Keypair.generate();
const taker = Keypair.generate();
const makerTS = traderStatePda(maker.publicKey);
const takerTS = traderStatePda(taker.publicKey);
const makerAta = ata(maker.publicKey, QUOTE);
const takerAta = ata(taker.publicKey, QUOTE);
const DEPOSIT = 5_000_000_000;

try {
  const ref = await program.account.marketAccount.fetch(REF_MARKET);
  if (!ref.params.oracleStalenessMaxSeconds) ref.params.oracleStalenessMaxSeconds = 60;
  console.log("genesis: init market + delegate to ER …", M.toBase58());
  await send(l1, await program.methods.initializeMarket(ref.params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
  await send(l1, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction(), []);
  await send(l1, await program.methods.initFillCommitment(CAP).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction(), []);
  await send(l1, await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, fillCommitment: FC, systemProgram: sys }).instruction(), []);
  const dgl = (a) => ({ buf: pda(["buffer", a]), rec: pda(["delegation", a], DELEG), meta: pda(["delegation-metadata", a], DELEG) });
  const b = dgl(BOOK), c = dgl(FC), o = dgl(FO), m = dgl(M);
  await send(l1, await program.methods.delegateMarketBook(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, ownerProgram: PID, delegateBuffer: b.buf, delegationRecord: b.rec, delegationMetadata: b.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  await send(l1, await program.methods.delegateFillCommitment(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, ownerProgram: PID, delegateBuffer: c.buf, delegationRecord: c.rec, delegationMetadata: c.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  await send(l1, await program.methods.delegateFillOutbox(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, ownerProgram: PID, delegateBuffer: o.buf, delegationRecord: o.rec, delegationMetadata: o.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  await send(l1, await program.methods.delegateMarket(30000, ER_VALIDATOR).accountsPartial({ authority: signer.publicKey, market: M, ownerProgram: PID, delegateBuffer: m.buf, delegationRecord: m.rec, delegationMetadata: m.meta, systemProgram: sys, delegationProgram: DELEG }).instruction(), []);
  await sleep(4000);
  console.log("fund fresh maker + taker, ER margin attestations …");
  const signerAta = ata(signer.publicKey, QUOTE);
  await send(l1, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: maker.publicKey, lamports: 50_000_000 }), []);
  await send(l1, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 50_000_000 }), []);
  await send(l1, [createAtaIx(signer.publicKey, maker.publicKey, QUOTE), transferIx(signerAta, makerAta, signer.publicKey, DEPOSIT)], []);
  await send(l1, [createAtaIx(signer.publicKey, taker.publicKey, QUOTE), transferIx(signerAta, takerAta, signer.publicKey, DEPOSIT)], []);
  await send(l1, await program.methods.openTraderState().accountsPartial({ trader: maker.publicKey, traderState: makerTS, systemProgram: sys }).instruction(), [maker]);
  await send(l1, await program.methods.openTraderState().accountsPartial({ trader: taker.publicKey, traderState: takerTS, systemProgram: sys }).instruction(), [taker]);
  const dep = (t, ts, a) => program.methods.depositCollateral(new BN(DEPOSIT)).accountsPartial({ trader: t, traderState: ts, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: a, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM }).instruction();
  await send(l1, await dep(maker.publicKey, makerTS, makerAta), [maker]);
  await send(l1, await dep(taker.publicKey, takerTS, takerAta), [taker]);
  const initAttest = (ts) => program.methods.initErMarginAttestation(signer.publicKey).accountsPartial({ authority: signer.publicKey, insuranceFund: INS, traderState: ts, erMargin: pda(["er_margin", ts]), systemProgram: sys }).instruction();
  await send(l1, await initAttest(makerTS), []);
  await send(l1, await initAttest(takerTS), []);
  await sleep(2000);

  // ── timed loop: rest a size-1 bid, then time the size-1 taker fill on the ER ──
  console.log(`\nmeasuring ${SAMPLES} taker fills on the ER (+${WARMUP} warmup) …`);
  const rows = [];
  let cuOnce = null;
  const total = SAMPLES + WARMUP;
  for (let i = 0; i < total; i++) {
    const tick = 90000 - (i % 80) * 10;
    await send(er, await program.methods.placeLimitOrderV2(0, new BN(1), new BN(tick), 0, new BN(0), 0).accountsPartial({ trader: maker.publicKey, market: M, marketBook: BOOK, traderState: makerTS, position: null }).instruction(), [maker]);
    // Time ONLY the successful send attempt (429 backoff happens between attempts,
    // outside the measured window), so the distribution reflects clean round-trips.
    const takerIx = await program.methods.placeTakerOrderV2(1, new BN(1), new BN(1), 0, new BN(0), 0).accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK, traderState: takerTS, position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }, { pubkey: FO, isWritable: true, isSigner: false }]).instruction();
    let sig, ms;
    for (let attempt = 0; ; attempt++) {
      try {
        const tx = new Transaction();
        tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }));
        tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
        tx.add(takerIx);
        const t0 = performance.now();
        sig = await sendAndConfirmTransaction(er, tx, [signer, taker], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
        ms = performance.now() - t0;
        break;
      } catch (e) {
        if (/429|Too Many Requests/i.test(String(e.message || e)) && attempt < 8) { await sleep(1500 * (attempt + 1)); continue; }
        throw e;
      }
    }
    // Fetch CU only ONCE (it's constant per identical fill) to avoid the public
    // endpoint's per-RPC-call 429 rate limit; pace iterations for the same reason.
    let cu = null;
    if (cuOnce === null) { try { const tx = await er.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 }); cuOnce = tx?.meta?.computeUnitsConsumed ?? null; } catch {} }
    cu = cuOnce;
    const warm = i >= WARMUP;
    rows.push({ i, warm, ms: +ms.toFixed(2), cu, sig });
    if (i % 5 === 0 || !warm) console.log(`  #${i}${warm ? "" : " (warmup)"}  ${ms.toFixed(1)} ms  cu=${cu ?? "?"}  ${sig.slice(0, 12)}…`);
    await sleep(400); // pacing to stay under the public ER endpoint rate limit
  }
  const m2 = rows.filter((r) => r.warm).map((r) => r.ms);
  const cus = rows.filter((r) => r.warm && r.cu != null).map((r) => r.cu);
  const mean = m2.reduce((a, x) => a + x, 0) / m2.length;
  const report = {
    endpoint: ER_RPC, validator: ER_VALIDATOR.toBase58(), market: M.toBase58(),
    method: "client performance.now() around placeTakerOrderV2 send->confirm('confirmed') on the ER; INCLUDES client<->ER network RTT (upper bound on ER execution)",
    samples: m2.length, warmup: WARMUP,
    latency_ms: { p50: +pct(m2, 50).toFixed(2), p90: +pct(m2, 90).toFixed(2), p95: +pct(m2, 95).toFixed(2), p99: +pct(m2, 99).toFixed(2), min: +Math.min(...m2).toFixed(2), max: +Math.max(...m2).toFixed(2), mean: +mean.toFixed(2) },
    compute_units: cus.length ? { min: Math.min(...cus), max: Math.max(...cus), median: pct(cus, 50) } : null,
    raw: rows,
  };
  fs.writeFileSync(new URL("./latency_results.json", import.meta.url), JSON.stringify(report, null, 2));
  console.log("\n========== ER FILL LATENCY (client-observed round-trip, incl. network) ==========");
  console.log(`  samples=${report.samples}  p50=${report.latency_ms.p50}ms  p90=${report.latency_ms.p90}ms  p95=${report.latency_ms.p95}ms  p99=${report.latency_ms.p99}ms`);
  console.log(`  min=${report.latency_ms.min}ms  max=${report.latency_ms.max}ms  mean=${report.latency_ms.mean}ms  CU median=${report.compute_units?.median ?? "?"}`);
  console.log(`  raw + methodology → er-acceptance/latency_results.json`);
  console.log(`  NOTE: this is CLIENT round-trip (incl. network to ${ER_RPC}); it is an UPPER BOUND on ER-side execution latency.`);
} catch (e) {
  console.log(`\nBENCH FAILED at some stage: ${String(e.message || e).slice(0, 300)}`);
  process.exit(1);
}
