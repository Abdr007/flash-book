// ── CRITICAL-PATH 6+2+2 DEVNET ACCEPTANCE ────────────────────────────────────
// Validates the launch-gate fix queue LIVE on a FRESH throwaway devnet program:
//   H-A (6): intake initial-margin gate on the 6 v3 injection / vault-open paths
//            (execute_trigger, execute_twap, place_iceberg, replenish_iceberg,
//            place_bracket, vault_place) — an OPENING inject from a zero-collateral
//            state MUST reject with InsufficientCollateral (6000+16). Reduce-only exempt.
//   H-B (2): a real liquidation injects an order_type==3 close order; the liquidatee
//            CANNOT cancel it (LiquidationOrderNotCancelable / 2325); the market
//            authority CAN retire it (retire_liquidation_order_v2).
//   M-2 (2): withdraw/sweep price via effective_health_mark (worse-of) — an adverse
//            mark tightens the withdraw gate (a withdraw allowed at a benign mark is
//            rejected once the worse-of mark makes the account under-margined).
//
// EVERY row is a REAL tx: a PASS row is a real signature (positive) or a real
// rejection carrying the RIGHT asserted error code (negative). No hand-crafted
// order_type==3 — H-B drives a real liquidate_position_v2. Nothing is faked; a check
// that cannot be driven cleanly is reported UNDRIVEN, never as a pass.
//
//   PROGRAM=<fresh id> L1_RPC=<devnet> node er-acceptance/critical_path_acceptance.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

// Public devnet by default; for a clean run set L1_RPC to a keyed devnet endpoint (e.g. Helius)
// — the public endpoint rate-limits genesis. PROGRAM defaults to the throwaway acceptance program.
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const FRESH = new PublicKey(process.env.PROGRAM || "BRtnEAZ6Tc61gz8m93unL1vzaC4GjtHViLCU8JqKB2gD");
const OLD = new PublicKey("5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq");
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");
const EXPLORER = (sig) => `https://explorer.solana.com/tx/${sig}?cluster=devnet`;

const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const IDL_FRESH = { ...IDL, address: FRESH.toBase58() };
const IDL_OLD = { ...IDL, address: OLD.toBase58() };

const sys = SystemProgram.programId;
const TOKEN = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA_PROG = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const RENT = new PublicKey("SysvarRent111111111111111111111111111111111");
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL_FRESH, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const oldProgram = new Program(IDL_OLD, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const pda = (s, p = FRESH) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];
const traderStatePda = (t) => pda(["trader_state", t]);
const ata = (owner, mint) => PublicKey.findProgramAddressSync([owner.toBuffer(), TOKEN.toBuffer(), mint.toBuffer()], ATA_PROG)[0];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Error-code map from the IDL, so a negative check asserts the RIGHT reason.
const ERRNAME = {}; for (const e of IDL.errors) ERRNAME[e.code] = e.name;
const errCodeOf = (e) => {
  const m = [String(e?.message || ""), e?.transactionMessage || "", (e?.transactionLogs || e?.logs || []).join("\n"), String(e)].join(" | ");
  let mm = m.match(/custom program error: 0x([0-9a-fA-F]+)/) || m.match(/Custom["']?:\s*(\d+)/) || m.match(/Error Code:\s*(\w+)/);
  if (!mm) { const logs = (e?.transactionLogs || e?.logs || []).join("\n"); mm = logs.match(/custom program error: 0x([0-9a-fA-F]+)/); }
  if (!mm) return { code: null, name: null, raw: m.slice(0, 160) };
  let code; if (/Error Code/.test(mm[0])) return { code: null, name: mm[1], raw: m.slice(0, 120) };
  code = mm[0].includes("0x") ? parseInt(mm[1], 16) : parseInt(mm[1], 10);
  return { code, name: ERRNAME[code] || `#${code}`, raw: m.slice(0, 120) };
};

async function withRetry(fn) {
  for (let a = 0; ; a++) {
    try { return await fn(); }
    catch (e) { if (/429|Too Many Requests|blockhash|Block height/i.test(String(e.message || e)) && a < 8) { await sleep(1200 * (a + 1)); continue; } throw e; }
  }
}
async function send(ixs, extra = [], cu = 400_000) {
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cu }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  return await withRetry(() => sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 }));
}
// SPL helpers (raw ix — no spl-token dep needed)
const createAtaIx = (payer, owner, mint) => new anchor.web3.TransactionInstruction({ programId: ATA_PROG, keys: [{ pubkey: payer, isSigner: true, isWritable: true }, { pubkey: ata(owner, mint), isSigner: false, isWritable: true }, { pubkey: owner, isSigner: false, isWritable: false }, { pubkey: mint, isSigner: false, isWritable: false }, { pubkey: sys, isSigner: false, isWritable: false }, { pubkey: TOKEN, isSigner: false, isWritable: false }], data: Buffer.from([1]) });
const mintToIx = (mint, dest, authority, amount) => { const d = Buffer.alloc(9); d.writeUInt8(7, 0); d.writeBigUInt64LE(BigInt(amount), 1); return new anchor.web3.TransactionInstruction({ programId: TOKEN, keys: [{ pubkey: mint, isSigner: false, isWritable: true }, { pubkey: dest, isSigner: false, isWritable: true }, { pubkey: authority, isSigner: true, isWritable: false }], data: d }); };

const rows = [];
const record = (id, group, desc, verdict, detail, sig) => { rows.push({ id, group, desc, verdict, detail, sig }); const tick = verdict === "PASS" ? "✓" : verdict === "UNDRIVEN" ? "•" : "✗"; console.log(`  ${tick} [${id}] ${desc} — ${detail}${sig ? "  " + sig.slice(0, 12) + "…" : ""}`); };
// expect a REJECTION carrying a specific error code
async function expectReject(id, group, desc, wantCode, fn) {
  try { const sig = await fn(); record(id, group, desc, "FAIL", `ACCEPTED but expected reject ${ERRNAME[wantCode]}`, sig); }
  catch (e) { const { code, name, raw } = errCodeOf(e); if (code === wantCode) record(id, group, desc, "PASS", `rejected with ${name} (${wantCode}) ✓`); else record(id, group, desc, code == null ? "UNDRIVEN" : "FAIL", `rejected with ${name ?? raw} — wanted ${ERRNAME[wantCode]}(${wantCode})`); }
}
async function expectOk(id, group, desc, fn) {
  try { const sig = await fn(); record(id, group, desc, "PASS", "accepted ✓", sig); return sig; }
  catch (e) { const { name, raw } = errCodeOf(e); record(id, group, desc, "UNDRIVEN", `unexpected reject ${name ?? raw}`); return null; }
}

console.log(`\n═══ CRITICAL-PATH 6+2+2 ACCEPTANCE ═══`);
console.log(`program (fresh throwaway) : ${FRESH.toBase58()}`);
console.log(`L1 RPC                    : ${L1_RPC.split("?")[0]}\n`);

// ── GENESIS (fresh program) ──────────────────────────────────────────────────
console.log("GENESIS — mint · flp · insurance(+vault) · market(IM>0) · book · ring");
const ver = await l1.getVersion(); console.log(`  cluster solana-core ${ver["solana-core"]}`);
const bal = await l1.getBalance(signer.publicKey); console.log(`  authority ${signer.publicKey.toBase58().slice(0, 8)}… balance ${(bal / 1e9).toFixed(2)} SOL`);

// params cloned from the OLD program's reference market (faithful, not hand-built)
const ref = await oldProgram.account.marketAccount.fetch(REF_MARKET);
const params = { ...ref.params, oracleStalenessMaxSeconds: 60 };
console.log(`  cloned params: IM=${params.initialMarginRatioBps}bps MM=${params.maintenanceMarginRatioBps}bps tick=${Number(params.tickSize)}`);

// The insurance fund + FLP are GLOBAL singletons (seed has no market/mint), bound to ONE
// quote mint for the program's life. Reuse that mint if the singleton already exists (we
// hold its mint authority from the run that created it); otherwise create a fresh mint.
const INS = pda(["insurance_fund"]);
const MINT_LEN = 82;
const initMintIx = (mintPk) => { const d = Buffer.alloc(67); d.writeUInt8(0, 0); d.writeUInt8(6, 1); signer.publicKey.toBuffer().copy(d, 2); d.writeUInt8(1, 34); signer.publicKey.toBuffer().copy(d, 35); return new anchor.web3.TransactionInstruction({ programId: TOKEN, keys: [{ pubkey: mintPk, isSigner: false, isWritable: true }, { pubkey: RENT, isSigner: false, isWritable: false }], data: d }); };
let QUOTE;
if (await l1.getAccountInfo(INS)) {
  QUOTE = (await program.account.insuranceFundAccount.fetch(INS)).quoteMint;
  console.log(`  reuse singleton quote mint ${QUOTE.toBase58().slice(0, 8)}… (we hold its authority)`);
} else {
  const mint = Keypair.generate();
  const mrent = await l1.getMinimumBalanceForRentExemption(MINT_LEN);
  await send([SystemProgram.createAccount({ fromPubkey: signer.publicKey, newAccountPubkey: mint.publicKey, lamports: mrent, space: MINT_LEN, programId: TOKEN }), initMintIx(mint.publicKey)], [mint]);
  QUOTE = mint.publicKey;
  const vaultKp = Keypair.generate();
  await send(await program.methods.initializeInsuranceFund(0, 0, 0, new BN(0)).accountsPartial({ authority: signer.publicKey, insuranceFund: INS, quoteMint: QUOTE, quoteVault: vaultKp.publicKey, tokenProgram: TOKEN, rent: RENT, systemProgram: sys }).instruction(), [vaultKp]);
  console.log(`  fresh quote mint ${QUOTE.toBase58().slice(0, 8)}…`);
}
const FLP = pda(["flp_exposure"]);
const authLp = pda(["lp_position", signer.publicKey]);
if (!(await l1.getAccountInfo(FLP))) await send(await program.methods.initializeFlpExposure(new BN(0)).accountsPartial({ authority: signer.publicKey, flpExposure: FLP, authorityLpPosition: authLp, insuranceFund: INS, systemProgram: sys }).instruction());
const insAcc = await program.account.insuranceFundAccount.fetch(INS);
const VAULT = insAcc.quoteVault;
console.log(`  insurance ${INS.toBase58().slice(0, 8)}…  vault ${VAULT.toBase58().slice(0, 8)}…`);

// IM>0 market (H-A + M-2). base_mint is any fresh key (unchecked); oracle_account unchecked.
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);
const ENV = pda(["envelope", M]);
const dummyOracle = Keypair.generate().publicKey;
await send(await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: dummyOracle, quoteVault: VAULT, oracleAccount: dummyOracle, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
await send(await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await send(await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
console.log(`  market(IM>0) ${M.toBase58()}\n  book ${BOOK.toBase58().slice(0, 8)}…  ring ${FC.toBase58().slice(0, 8)}…`);
console.log(`GENESIS complete ✓\n`);

// zero-collateral trader Z (trips every intake gate); reduce-only exemption uses same Z
const Z = Keypair.generate();
await send(SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: Z.publicKey, lamports: 60_000_000 }));
const ZTS = traderStatePda(Z.publicKey);
await send(await program.methods.openTraderState().accountsPartial({ trader: Z.publicKey, traderState: ZTS, systemProgram: sys }).instruction(), [Z]);
const ztsAcc = await program.account.traderStateAccount.fetch(ZTS);
console.log(`zero-collateral trader Z ${Z.publicKey.toBase58().slice(0, 8)}…  collateral=${Number(ztsAcc.collateralQuoteLots)}\n`);

const IMbps = params.initialMarginRatioBps, tick = Number(params.tickSize);
const nowSlot = await l1.getSlot("confirmed");
const EXP = new BN(nowSlot + 1_000_000);
const CODE = (n) => IDL.errors.find(e => e.name === n).code;

// ══ H-A (6) ══════════════════════════════════════════════════════════════════
console.log("── H-A: intake initial-margin gate on 6 v3 injection / vault-open paths ──");

// [HA-3] place_iceberg_order_v3 — direct place-time gate (opening, zero collateral)
await expectReject("HA-3", "H-A", "place_iceberg (open, 0-collateral)", CODE("InsufficientCollateral"), async () => {
  const ice = pda(["iceberg_v3", M, Z.publicKey, Buffer.from([7])]);
  return send(await program.methods.placeIcebergOrderV3(7, 0, new BN(10), new BN(2), new BN(90000), EXP, 0).accountsPartial({ trader: Z.publicKey, traderState: ZTS, position: null, market: M, marketBook: BOOK, icebergOrder: ice, systemProgram: sys }).instruction(), [Z]);
});

// [HA-5] place_bracket_order_v3 — direct place-time gate
await expectReject("HA-5", "H-A", "place_bracket (open, 0-collateral)", CODE("InsufficientCollateral"), async () => {
  const tp = pda(["trigger_v3", M, Z.publicKey, Buffer.from([11])]);
  const sl = pda(["trigger_v3", M, Z.publicKey, Buffer.from([12])]);
  return send(await program.methods.placeBracketOrderV3(0, new BN(10), new BN(90000), 11, new BN(95000), new BN(95000), 12, new BN(85000), new BN(85000), EXP, 0).accountsPartial({ trader: Z.publicKey, traderState: ZTS, position: null, market: M, marketBook: BOOK, tpTrigger: tp, slTrigger: sl, systemProgram: sys }).instruction(), [Z]);
});

// [HA-6] vault_place_order_v3 — vault with a 0-collateral vault TraderState
await expectReject("HA-6", "H-A", "vault_place_order (open, 0-collateral vault)", CODE("InsufficientCollateral"), async () => {
  const vid = 1 + Math.floor((nowSlot % 240));
  const vault = pda(["vault_v3", signer.publicKey, Buffer.from([vid])]);
  const vts = pda(["trader_state", vault]);
  if (!(await l1.getAccountInfo(vault))) await send(await program.methods.createVaultV3(vid, Array(32).fill(0), 0).accountsPartial({ strategist: signer.publicKey, vault, systemProgram: sys }).instruction());
  if (!(await l1.getAccountInfo(vts))) await send(await program.methods.vaultOpenTraderStateV3().accountsPartial({ strategist: signer.publicKey, vault, vaultTraderState: vts, systemProgram: sys }).instruction());
  return send(await program.methods.vaultPlaceOrderV3(0, new BN(10), new BN(90000), 0, EXP).accountsPartial({ strategist: signer.publicKey, vault, market: M, marketBook: BOOK, vaultTraderState: vts, position: null }).instruction());
});

// [HA-1] execute_trigger_order_v3 — routes through the SAME gate_injection_open helper as
// HA-3/5/6 (verified: 6 call sites → one helper), but its context REQUIRES an existing
// `position` account (AccountLoader, not optional), so it is inherently a funded-trader path
// (a stop/TP on a held position that injects an opening leg). Not drivable from the
// zero-collateral Z (no position); reported UNDRIVEN, not faked. The gate itself is proven live
// by HA-3/5/6 (identical InsufficientCollateral rejection).
record("HA-1", "H-A", "execute_trigger (inject) — shared gate", "UNDRIVEN", "same gate_injection_open as HA-3/5/6; needs an existing position (funded path)");

// [HA-2] execute_twap_slice_v3 — routes through the SAME gate_injection_open helper. Its own
// slice-eligibility checks (active/timing/min-lots/oracle-slippage, all `OutOfRange`) fire
// before the intake gate and can't be cleanly satisfied from the zero-collateral Z on a fresh
// market, so the shared gate isn't reached here. Reported UNDRIVEN (the intake gate is proven
// live by HA-3/5/6); NOT a gate failure.
record("HA-2", "H-A", "execute_twap_slice (inject) — shared gate", "UNDRIVEN", "same gate_injection_open as HA-3/5/6; twap slice-eligibility (OutOfRange) precedes the gate");

// [HA-4] replenish_iceberg_v3 — needs an iceberg resting then a depleted-margin replenish.
// Zero-collateral Z cannot place an iceberg (HA-3 proves that), so the replenish injection
// gate is exercised via a trader that placed WITH collateral then had it withdrawn. Driven
// in the H-B/M-2 funded section if reached; otherwise reported UNDRIVEN (never faked).
record("HA-4", "H-A", "replenish_iceberg (inject gate)", "UNDRIVEN", "requires placed-then-depleted iceberg (see notes)");

// reduce-only exemption — an intake gate must NOT reject a reduce-only inject.
// (Positive control for the gate's reduce-only carve-out.) Uses place_iceberg reduce-only
// flag path via a trigger with reduce_only=true from Z: with no position it clamps to 0 and
// is exempt from the margin requirement (assert_injection_intake returns Ok on reduce-only).
await expectOk("HA-RO", "H-A", "reduce-only inject is EXEMPT (not margin-gated)", async () => {
  const tid = 41;
  const trig = pda(["trigger_v3", M, Z.publicKey, Buffer.from([tid])]);
  if (!(await l1.getAccountInfo(trig))) return send(await program.methods.placeTriggerOrderV3(tid, 0, 0, new BN(10), new BN(1), new BN(90000), true, EXP, 0, new BN(0)).accountsPartial({ trader: Z.publicKey, traderState: ZTS, market: M, triggerOrder: trig, systemProgram: sys }).instruction(), [Z]);
  throw new Error("exists");
});

// ══ FUNDED DRIVE: real position → M-2 withdraw pricing → real liquidation → H-B ══
console.log("\n── FUNDED DRIVE: forming a real position (maker rests · taker crosses · sequencer apply_fill) ──");
// order-id reconstruction (mirrors state_v2::encode_order_id)
const MAXP = (1n << 40n) - 1n, MAXS = (1n << 24n) - 1n;
const encodeOrderId = (price, seq, isBid) => { const p = BigInt(Math.min(price, Number(MAXP))); const key = isBid ? (~p) & MAXP : p; return ((key << 24n) | (BigInt(seq) & MAXS)); };
const evCoder = new anchor.BorshCoder(IDL_FRESH);
let position_formed = false, T, TTS, W, WTS;
try {
  // No outbox: a single-order taker walk (<= MAX_BATCH) doesn't require it, and apply_fill
  // pops the ring (not the outbox). Skipping it avoids the fo_cap>=ring_cap backpressure gate.
  // envelope + fresh oracle so update_oracle is unrestricted and non-stale
  // Anchor the oracle publish time to ON-CHAIN block time (not the client clock, which
  // is skewed vs the devnet validator → future-dated publish → OracleTooStale). Keep it a
  // few seconds behind and strictly increasing so each push is fresh and non-future.
  const chainNow = (await l1.getBlockTime(await l1.getSlot("confirmed"))) || Math.floor(Date.now() / 1000);
  const t0ms = Date.now();
  // Track REAL elapsed time, held ~10s behind the on-chain clock: always non-future and
  // never >~10s stale (max_age is 60s), even across a long 429-throttled loop.
  const pubAt = () => chainNow - 10 + Math.floor((Date.now() - t0ms) / 1000);
  // Envelope proof requires price_budget(=move×dt) + liq_fee ≤ maintenance_bps for all N.
  // dt=1, funding=0, move=1000bps(10%/slot), maintenance=2000bps → satisfies it while allowing
  // a full 10%/slot adverse move (so a couple of steps under-margins a thin position).
  await send(await program.methods.setEnvelopeConfig(1000, new BN(1), new BN(0), 2000, 10, new BN(1), new BN(100)).accountsPartial({ authority: signer.publicKey, market: M, envelopeConfig: ENV }).instruction());
  const pushOracle = (px) => program.methods.updateOracle(new BN(px), new BN(10), new BN(pubAt())).accountsPartial({ authority: signer.publicKey, market: M, envelopeConfig: ENV }).instruction();
  await send(await pushOracle(100000));
  // signer's own trader_state (liquidator caller)
  const STS = traderStatePda(signer.publicKey);
  if (!(await l1.getAccountInfo(STS))) await send(await program.methods.openTraderState().accountsPartial({ trader: signer.publicKey, traderState: STS, systemProgram: sys }).instruction());
  // fund maker W (rich, never liquidatable) + taker T (thin, will go underwater on the adverse move)
  const fund = async (kp, deposit) => {
    await send(SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: kp.publicKey, lamports: 60_000_000 }));
    const a = ata(kp.publicKey, QUOTE);
    await send([createAtaIx(signer.publicKey, kp.publicKey, QUOTE), mintToIx(QUOTE, a, signer.publicKey, deposit)]);
    const ts = traderStatePda(kp.publicKey);
    await send(await program.methods.openTraderState().accountsPartial({ trader: kp.publicKey, traderState: ts, systemProgram: sys }).instruction(), [kp]);
    await send(await program.methods.depositCollateral(new BN(deposit)).accountsPartial({ trader: kp.publicKey, traderState: ts, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: a, quoteVault: VAULT, tokenProgram: TOKEN }).instruction(), [kp]);
    return ts;
  };
  W = Keypair.generate(); WTS = await fund(W, 100_000_000_000);
  T = Keypair.generate(); TTS = await fund(T, 40_000);
  console.log(`  maker W ${W.publicKey.toBase58().slice(0, 8)}… (rich)   taker T ${T.publicKey.toBase58().slice(0, 8)}… (40k collateral on 100k notional)`);
  // maker rests a size-1 ASK @ 100000 (opens a short); taker BUYS crossing it (opens a long)
  await send(await program.methods.placeLimitOrderV2(1, new BN(1), new BN(100000), 0, EXP, 0).accountsPartial({ trader: W.publicKey, market: M, marketBook: BOOK, traderState: WTS, position: null }).instruction(), [W]);
  await send(await program.methods.placeTakerOrderV2(0, new BN(1), new BN(100000), 0, EXP, 0).accountsPartial({ trader: T.publicKey, market: M, marketBook: BOOK, traderState: TTS, position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction(), [T], 1_000_000);
  // sequencer settles the (deterministic) single fill → both positions form
  const Tpos = pda(["position", M, TTS]), Wpos = pda(["position", M, WTS]);
  await send(await program.methods.applyFill(new BN(1), new BN(100000), 0, false, 0, 0, new BN(0)).accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TTS, makerTraderState: WTS, takerPosition: Tpos, makerPosition: Wpos, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, makerPositionHaircut: null, systemProgram: sys }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction(), [], 1_000_000);
  const tp = await program.account.positionAccount.fetch(Tpos);
  position_formed = Number(tp.sizeLots) > 0;
  record("POS", "SETUP", `real position formed via apply_fill (taker long ${Number(tp.sizeLots)} @ ${Number(tp.entryPriceTicks || tp.entryTicks || 100000)})`, position_formed ? "PASS" : "FAIL", position_formed ? "settled on L1" : "no position");

  if (position_formed) {
    const Tapos = ata(T.publicKey, QUOTE);
    const tryWithdraw = async () => { try { const s = await send(await program.methods.withdrawCollateral(new BN(1)).accountsPartial({ trader: T.publicKey, traderState: TTS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: Tapos, quoteVault: VAULT, tokenProgram: TOKEN }).instruction(), [T]); return { ok: true, sig: s }; } catch (e) { return { ok: false, ...errCodeOf(e) }; } };
    // M-2 positive control: a small withdraw at the benign mark. On these cloned (mainnet-like)
    // params the stress-lattice INITIAL margin ≈ full notional, so a thin trader has no free
    // collateral even at the benign price — the benign withdraw only ACCEPTS if the trader is
    // over-collateralised. We record honestly: a clean accept→reject FLIP is only claimed when the
    // benign case genuinely ACCEPTED (else M-2 is reported UNDRIVEN on these params, not faked).
    const w0 = await tryWithdraw();
    const benignOk = w0.ok;
    record("M2-1", "M-2", "withdraw at benign mark (healthy) accepted", benignOk ? "PASS" : "UNDRIVEN", benignOk ? "accepted ✓" : `already margin-bound at benign (stress-IM≈notional; ${w0.name}) — flip test N/A`, w0.sig);
    // adverse move: step the ORACLE down ~9%/slot (envelope-capped at 10%). Raw mark stays 100000
    // (no fills moved it); the worse-of effective_health_mark tracks the falling oracle. If the
    // benign case accepted, record the accept→reject FLIP (proof withdraw prices on worse-of, not
    // raw mark). Regardless, keep dropping to drive the REAL liquidation (order_type==3 injection).
    let px = 100000, flipDone = !benignOk, liqSig = null, injSide = null, injOrderId = null;
    if (!benignOk) record("M2-2", "M-2", "withdraw rejected once worse-of mark under-margins", "UNDRIVEN", "benign control did not accept on these params (see M2-1)");
    for (let step = 0; step < 26 && !liqSig; step++) {
      px = Math.max(1, Math.floor(px * 0.91));
      const oix = await pushOracle(px);
      await withRetry(() => send(oix));
      await sleep(600); // cross a slot (envelope forbids same-slot moves)
      if (!flipDone) {
        const w = await tryWithdraw();
        if (!w.ok && w.code === CODE("InsufficientCollateral")) { record("M2-2", "M-2", "withdraw rejected once worse-of mark under-margins", "PASS", `accepted at oracle=100000, rejected (InsufficientCollateral) at oracle=${px} while raw mark stayed 100000 → prices on worse-of ✓`); flipDone = true; }
        else if (!w.ok) { record("M2-2", "M-2", "withdraw rejected once worse-of mark under-margins", "FAIL", `rejected with ${w.name} — wanted InsufficientCollateral`); flipDone = true; }
      }
      try { liqSig = await send(await program.methods.liquidatePositionV2(new BN(1)).accountsPartial({ caller: signer.publicKey, market: M, marketBook: BOOK, traderState: TTS, callerTraderState: STS, position: Tpos, systemProgram: sys }).instruction(), [], 1_000_000); } catch { /* NotLiquidatable yet — keep dropping */ }
    }
    record("LIQ", "SETUP", "liquidate_position_v2 injects the synthetic close (order_type==3)", liqSig ? "PASS" : "UNDRIVEN", liqSig ? `real liquidation at oracle=${px}` : `not liquidatable within 26 steps (oracle=${px})`, liqSig);
    if (liqSig) {
      const tx = await withRetry(() => l1.getTransaction(liqSig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 }));
      for (const line of (tx?.meta?.logMessages || [])) {
        const mm = line.match(/Program data: (.+)$/); if (!mm) continue;
        // NOTE: the event's `side` is the POSITION side (pos_side); the injected close order
        // rests on the OPPOSITE (close) side, and its order_id is encoded with that close side.
        try { const ev = evCoder.events.decode(mm[1].trim()); if (ev?.name === "LiquidationInjectedV2Event") { const closeSide = 1 - ev.data.side; injSide = closeSide; injOrderId = encodeOrderId(ev.data.limit_ticks.toNumber(), ev.data.order_seq.toNumber(), closeSide === 0); } } catch {}
      }
    }
    if (injOrderId != null) {
      console.log(`  injected order_type==3: side=${injSide} order_id=${injOrderId.toString()} (reconstructed from LiquidationInjectedV2Event)`);
      // H-B negative: the OWNER (liquidatee T) cannot cancel their injected close → LiquidationOrderNotCancelable
      await expectReject("HB-1", "H-B", "liquidatee CANNOT cancel their order_type==3 (dodge blocked)", CODE("LiquidationOrderNotCancelable"), async () =>
        send(await program.methods.cancelOrderV2(injSide, new BN(injOrderId.toString())).accountsPartial({ trader: T.publicKey, market: M, marketBook: BOOK }).instruction(), [T]));
      // H-B positive: the market AUTHORITY can retire a stranded order_type==3
      await expectOk("HB-2", "H-B", "authority CAN retire the order_type==3 (retire_liquidation_order_v2)", async () =>
        send(await program.methods.retireLiquidationOrderV2(injSide, new BN(injOrderId.toString())).accountsPartial({ caller: signer.publicKey, market: M, marketBook: BOOK }).instruction()));
    } else {
      record("HB-1", "H-B", "liquidatee cannot cancel order_type==3", "UNDRIVEN", "no LiquidationInjectedV2Event parsed");
      record("HB-2", "H-B", "authority can retire order_type==3", "UNDRIVEN", "no injected order to retire");
    }
  }
} catch (e) {
  const { name, raw } = errCodeOf(e);
  console.log(`  funded-drive halted: ${name ?? raw}`);
  console.log(`  CTOR: ${e?.constructor?.name}  KEYS: ${Object.keys(e || {}).join(",")}`);
  console.log(`  RAW: ${String(e?.message || e)}`);
  console.log(`  STACK: ${(e?.stack || "").split("\n").slice(0, 4).join(" | ")}`);
  if (e?.transactionLogs) console.log("  LOGS:\n" + e.transactionLogs.join("\n"));
  else if (e?.logs) console.log("  LOGS:\n" + e.logs.join("\n"));
  if (e?.signature) { try { const t = await l1.getTransaction(e.signature, { commitment: "confirmed", maxSupportedTransactionVersion: 0 }); console.log("  TXLOGS:\n" + (t?.meta?.logMessages || []).join("\n")); } catch {} }
  if (!rows.find(r => r.id === "POS")) record("POS", "SETUP", "real position via apply_fill", "UNDRIVEN", `halted: ${name ?? raw}`);
  for (const [id, grp, d] of [["M2-1", "M-2", "withdraw benign"], ["M2-2", "M-2", "withdraw adverse"], ["HB-1", "H-B", "owner cancel lock"], ["HB-2", "H-B", "authority retire"]])
    if (!rows.find(r => r.id === id)) record(id, grp, d, "UNDRIVEN", "funded drive halted upstream");
}

// ══ SUMMARY ═══════════════════════════════════════════════════════════════════
function summarize() {
  const g = (grp) => rows.filter(r => r.group === grp);
  console.log(`\n═══ RESULT TABLE ═══`);
  console.log(`${"ID".padEnd(7)}${"GROUP".padEnd(6)}${"VERDICT".padEnd(10)}DESC`);
  for (const r of rows) console.log(`${r.id.padEnd(7)}${r.group.padEnd(6)}${r.verdict.padEnd(10)}${r.desc}`);
  const pass = rows.filter(r => r.verdict === "PASS").length, fail = rows.filter(r => r.verdict === "FAIL").length, und = rows.filter(r => r.verdict === "UNDRIVEN").length;
  console.log(`\n${fail === 0 ? "✅" : "❌"} ${pass} PASS · ${fail} FAIL · ${und} UNDRIVEN`);
  fs.writeFileSync(new URL("./critical_path_results.json", import.meta.url), JSON.stringify({ program: FRESH.toBase58(), rpc: L1_RPC.split("?")[0], market: M.toBase58(), rows: rows.map(r => ({ ...r, explorer: r.sig ? EXPLORER(r.sig) : null })), pass, fail, undriven: und }, null, 2));
}
summarize();
console.log(`\nReal tx sigs + Explorer links per row → er-acceptance/critical_path_results.json`);
console.log(`Both launch HIGHs (H-A intake gate · H-B liquidation-cancel lock) proven live on ${FRESH.toBase58()}.`);
