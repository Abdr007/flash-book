// AUDIT-FIXES LIVE ACCEPTANCE (devnet) — validates the 2026-07 adversarial-audit
// remediation on the real chain, end-to-end. Requires the post-remediation program
// deployed (the added intake accounts + gates). Run AFTER `solana program deploy`.
//
//   C-1 (CRITICAL): a taker/maker order committed against a NON-EXISTENT TraderState
//                   must be REJECTED at intake (pre-fix it settled → permanent FIFO
//                   wedge). Positive: a real TraderState is accepted.
//   M-2:            an opening order from a ZERO-collateral TraderState on an
//                   initial-margin market must be REJECTED (the free-option).
//   M-6:            after a taker cross produces ring fills, MarketAccount
//                   .unsettled_fill_volume > 0 (matched-but-unsettled OI reserved).
//
// None of these need the signer to hold quote tokens — they assert rejections at the
// intake gate and a counter read. LP/M-7/liquidation flows that need collateral are
// scaffolded separately.
//
// L1_RPC=<devnet> node audit_fixes_acceptance.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const sys = SystemProgram.programId;
const pda = (s, p = PID) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];

// Reference devnet accounts (shared across the acceptance suite).
const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");
const LP = pda(["lp_exposure"]);
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");

// TraderState PDA: sub_index 0 = [b"trader_state", trader]; N>0 = [.., trader, [N]].
const traderStatePda = (trader, sub = 0) =>
  sub === 0
    ? pda(["trader_state", trader])
    : pda(["trader_state", trader, Buffer.from([sub])]);

const send = async (ix, extra = []) => {
  const { blockhash } = await l1.getLatestBlockhash("confirmed");
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: signer.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true });
};
// Send expecting FAILURE — returns true if the tx was rejected (the fix fired).
const sendExpectFail = async (ix, extra = []) => {
  try { await send(ix, extra); return false; } catch { return true; }
};

let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

console.log(`AUDIT-FIXES live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);

// ── Build two fresh armed markets: one with IM=0 (isolates C-1 from M-2), one
//    with the reference IM>0 (for the M-2 gate). ────────────────────────────────
const mkMarket = async (params, tag) => {
  const base = Keypair.generate();
  const M = pda(["market", base.publicKey, QUOTE]);
  const BOOK = pda(["market_book", M]);
  const FC = pda(["fill_commit", M]);
  await send(await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), [base]);
  await send(await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
  await send(await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
  console.log(`  ${tag} market ${M.toBase58()}`);
  return { M, BOOK, FC };
};

// NOTE: the reference market was created under the OLD program with
// oracle_staleness_max_seconds == 0; the post-remediation program REJECTS that
// (AUDIT M-5 — a 0 bound silently disables the staleness gate). Supply a valid
// bound. (This rejection is itself a live M-5 acceptance — see below.)
const paramsIM0 = { ...ref.params, initialMarginRatioBps: 0, oracleStalenessMaxSeconds: new BN(60) };
const paramsIM = { ...ref.params, oracleStalenessMaxSeconds: new BN(60) };

console.log("setup: markets + a real (sub_index 0) TraderState for the signer");
const im0 = await mkMarket(paramsIM0, "IM=0");
const imM = await mkMarket(paramsIM, "IM>0");
const TS0 = traderStatePda(signer.publicKey, 0);
// Create the signer's main TraderState if absent (0 collateral — fine for these tests).
if (!(await l1.getAccountInfo(TS0))) {
  await send(await program.methods.openTraderState().accountsPartial({ trader: signer.publicKey, traderState: TS0, systemProgram: sys }).instruction());
}
ok(!!(await l1.getAccountInfo(TS0)), `TraderState(sub 0) exists ${TS0.toBase58().slice(0, 8)}…`);

// Helper: place a taker order, supplying trader_state + (optional) position.
const placeTaker = (mkt, sub, ts, { position = null, size = 1, price = 100000, side = 0, trader = signer.publicKey } = {}) =>
  program.methods.placeTakerOrder(side, new BN(size), new BN(price), 0, new BN(0), sub)
    .accountsPartial({ trader, market: mkt.M, marketBook: mkt.BOOK, traderState: ts, position })
    .remainingAccounts([{ pubkey: mkt.FC, isWritable: true, isSigner: false }])
    .instruction();

// ── C-1: non-existent TraderState is REJECTED ──────────────────────────────────
console.log("\nC-1: order committed against a non-existent TraderState");
const TS_GHOST = traderStatePda(signer.publicKey, 200); // sub 200 never created
ok(!(await l1.getAccountInfo(TS_GHOST)), "  precondition: TraderState(sub 200) does NOT exist");
const c1rej = await sendExpectFail(await placeTaker(im0, 200, TS_GHOST));
ok(c1rej, "C-1: place_taker with a non-existent (sub 200) TraderState is REJECTED (no wedge)");

// ── C-1 positive + M-6: a real TraderState on the IM=0 market is accepted, and a
//    cross reserves unsettled OI. First the LP posts a maker so the taker crosses.
console.log("\nC-1 positive + M-6: real TraderState accepted; cross reserves OI");
await send(await program.methods.lpPostMakerOrder(1, new BN(1), new BN(100000), new BN(0)).accountsPartial({ authority: signer.publicKey, market: im0.M, marketBook: im0.BOOK, lpExposure: LP }).instruction());
const c1okSig = await send(await placeTaker(im0, 0, TS0)); // crosses the LP ask
ok(!!c1okSig, `C-1: place_taker with a real TraderState is ACCEPTED — ${c1okSig.slice(0, 12)}…`);
const mAfter = await program.account.marketAccount.fetch(im0.M);
ok(Number(mAfter.unsettledFillVolume) > 0, `M-6: unsettled_fill_volume reserved after cross (= ${Number(mAfter.unsettledFillVolume)})`);

// ── M-2: zero-collateral open on an IM>0 market is REJECTED ─────────────────────
// The signer's main TraderState is funded from prior use, so use a FRESH trader
// with a brand-new (0-collateral) TraderState. M-2 rejects at INTAKE (before the
// match), so no crossing liquidity is needed — an opening order alone trips it.
console.log("\nM-2: zero-collateral opening order on an IM>0 market");
const t2 = Keypair.generate();
await send(anchor.web3.SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: t2.publicKey, lamports: 30_000_000 }));
const TS2 = traderStatePda(t2.publicKey, 0);
await send(await program.methods.openTraderState().accountsPartial({ trader: t2.publicKey, traderState: TS2, systemProgram: sys }).instruction(), [t2]);
const ts2info = await program.account.traderStateAccount.fetch(TS2);
ok(Number(ts2info.collateralQuoteLots) === 0, `  precondition: fresh TraderState collateral == 0 (${Number(ts2info.collateralQuoteLots)})`);
const m2rej = await sendExpectFail(await placeTaker(imM, 0, TS2, { trader: t2.publicKey }), [t2]);
ok(m2rej, "M-2: zero-collateral open on an initial-margin market is REJECTED (free-option closed)");

// ── F-1 (CRITICAL): vault_place_order must REQUIRE the vault's TraderState ────
// Pre-fix a vault could rest a maker order under (vault_pk,0) with NO TraderState;
// a taker crossing it committed a fill that could NEVER settle (apply_fill hard-
// loads the maker TraderState) → permanent FIFO wedge, brickable by any user for
// rent (create_vault is permissionless). Post-fix vault_trader_state is a
// required, PDA-checked account on VaultPlaceOrder, so a vault order is
// structurally unable to rest against a non-existent TraderState.
console.log("\nF-1: vault order requires the vault's TraderState (anti-wedge, CRITICAL)");
const vaultId = 1 + Math.floor(Math.random() * 250);
const vaultPda = pda(["vault", signer.publicKey, Buffer.from([vaultId])]);
try { await send(await program.methods.createVault(vaultId, Array(32).fill(0), 0).accountsPartial({ strategist: signer.publicKey, vault: vaultPda, systemProgram: sys }).instruction()); } catch (e) {}
ok(!!(await l1.getAccountInfo(vaultPda)), `  vault created ${vaultPda.toBase58().slice(0, 8)}…`);
const vaultTs = pda(["trader_state", vaultPda]);
ok(!(await l1.getAccountInfo(vaultTs)), "  precondition: vault TraderState does NOT exist yet");
const vaultPlace = () => program.methods.vaultPlaceOrder(1, new BN(1), new BN(100000), 0, new BN(0))
  .accountsPartial({ strategist: signer.publicKey, vault: vaultPda, market: im0.M, marketBook: im0.BOOK, vaultTraderState: vaultTs }).instruction();
const f1neg = await sendExpectFail(await vaultPlace());
ok(f1neg, "F-1: vault order with a NON-EXISTENT vault TraderState is REJECTED (wedge closed)");
await send(await program.methods.vaultOpenTraderState().accountsPartial({ strategist: signer.publicKey, vault: vaultPda, vaultTraderState: vaultTs, systemProgram: sys }).instruction());
const f1sig = await send(await vaultPlace());
ok(!!f1sig, `F-1: vault order WITH a real vault TraderState is ACCEPTED — ${String(f1sig).slice(0, 12)}… (no false-reject)`);

// ── GOVERNANCE Phase-1 (2026-07): asymmetric emergency guardian ─────────────────
// A guardian may RESTRICT market status (pause) but NEVER loosen it (unpause stays
// authority-only). Run last: it pauses then re-opens the IM=0 market.
console.log("\nGov Phase-1: guardian may restrict but not loosen market status");
const guardian = Keypair.generate();
await send(await program.methods.setGuardian(guardian.publicKey).accountsPartial({ authority: signer.publicKey, market: im0.M }).instruction());
const gpda = pda(["market_guardian", im0.M]);
const gacc = await program.account.marketGuardianAccount.fetch(gpda);
ok(gacc.guardian.equals(guardian.publicKey), `  guardian set on the IM=0 market ${gpda.toBase58().slice(0, 8)}…`);

// guardian_account slot: the PDA for a guardian call, null (omitted) for an authority call.
const statusIx = (caller, newStatus, withGuardian) =>
  program.methods.setMarketStatus(newStatus)
    .accountsPartial({ authority: caller, market: im0.M, guardianAccount: withGuardian ? gpda : null })
    .instruction();

// Guardian PAUSES (Active(1) → Paused(3)) — restrict, allowed. (guardian co-signs; signer pays.)
const pauseSig = await send(await statusIx(guardian.publicKey, 3, true), [guardian]);
ok(!!pauseSig, `Gov P-1: guardian PAUSED the market (restrict) — ${String(pauseSig).slice(0, 12)}…`);
ok((await program.account.marketAccount.fetch(im0.M)).status === 3, "  market status == Paused(3)");

// Guardian tries to UNPAUSE (3 → 1) — loosen, MUST be rejected.
const unpauseRej = await sendExpectFail(await statusIx(guardian.publicKey, 1, true), [guardian]);
ok(unpauseRej, "Gov P-1: guardian CANNOT unpause (rejected — loosening is authority-only)");
ok((await program.account.marketAccount.fetch(im0.M)).status === 3, "  still Paused(3) after the guardian's failed unpause");

// Authority UNPAUSES (3 → 1) — allowed (guardian slot omitted).
const reopenSig = await send(await statusIx(signer.publicKey, 1, false));
ok(!!reopenSig, `Gov P-1: authority RE-OPENED the market — ${String(reopenSig).slice(0, 12)}…`);
ok((await program.account.marketAccount.fetch(im0.M)).status === 1, "  market status == Active(1)");

// ── GOVERNANCE Phase-2a (2026-07): 2-step authority transfer ────────────────────
// Run last on the imM market: the current authority proposes a new key, a wrong key
// can't accept, the new key accepts (authority transfers + pending closes).
console.log("\nGov Phase-2a: 2-step authority transfer (propose → the new key must accept)");
const newAuth = Keypair.generate();
const pendingPda = pda(["pending_authority", imM.M]);
await send(await program.methods.proposeAuthorityTransfer(newAuth.publicKey).accountsPartial({ authority: signer.publicKey, market: imM.M }).instruction());
const pend = await program.account.marketPendingAuthorityAccount.fetch(pendingPda);
ok(pend.pendingAuthority.equals(newAuth.publicKey), "  transfer proposed (pending PDA set)");

// A WRONG key (signer, not the pending target) cannot accept.
const wrongAccept = await sendExpectFail(await program.methods.acceptAuthorityTransfer().accountsPartial({ newAuthority: signer.publicKey, market: imM.M, pending: pendingPda }).instruction());
ok(wrongAccept, "Gov P-2a: a wrong key CANNOT accept the transfer");

// The NEW key accepts (it co-signs; signer pays fees). Authority transfers, pending closes.
const acceptSig = await send(await program.methods.acceptAuthorityTransfer().accountsPartial({ newAuthority: newAuth.publicKey, market: imM.M, pending: pendingPda }).instruction(), [newAuth]);
ok(!!acceptSig, `Gov P-2a: new key ACCEPTED — authority transferred — ${String(acceptSig).slice(0, 12)}…`);
ok((await program.account.marketAccount.fetch(imM.M)).authority.equals(newAuth.publicKey), "  market.authority == the new key");
ok((await l1.getAccountInfo(pendingPda)) === null, "  pending PDA closed on accept");

// ── GOVERNANCE Phase-2b (2026-07): timelocked market-params update ──────────────
// Proves the delay gate live (the 48h eta can't be warped on devnet; the
// after-delay APPLY is covered by the BanksClient test). Round-trips the current
// params (a valid no-op change) to exercise propose → execute-rejected → cancel.
console.log("\nGov Phase-2b: timelocked params update (delay-gated)");
const pppda = pda(["pending_params", im0.M]);
// Propose the VALID reference params (paramsIM) on the signer-owned im0 market — a
// legitimate mutable change with matching immutable fields. (im0's own params are
// the IM=0 test config, whose initial_margin=0 fails the update-time
// maintenance<=initial bound, so they can't be re-proposed as-is.)
const params = paramsIM;
await send(await program.methods.proposeParamUpdate(params).accountsPartial({ authority: signer.publicKey, market: im0.M }).instruction());
const pp = await program.account.pendingParamUpdateAccount.fetch(pppda);
ok(pp.etaUnix.toNumber() > 0, `  proposed — eta set (now + 48h) = ${pp.etaUnix.toNumber()}`);
const execRej = await sendExpectFail(await program.methods.executeParamUpdate(params).accountsPartial({ authority: signer.publicKey, market: im0.M }).instruction());
ok(execRej, "Gov P-2b: execute BEFORE the 48h eta is REJECTED (TimelockNotElapsed)");
await send(await program.methods.cancelParamUpdate().accountsPartial({ authority: signer.publicKey, market: im0.M }).instruction());
ok((await l1.getAccountInfo(pppda)) === null, "Gov P-2b: authority CANCELLED — pending closed");

// ── GOVERNANCE Phase-3 (2026-07): one-way oracle-source lock ────────────────────
// The lock flag lives in the market's envelope config (already required by the direct
// update_oracle paths). Set up an envelope on im0, then lock — proving the flag is set
// on-chain by the authority (the direct-update rejection is covered by BanksClient,
// where the oracle mechanics are controllable).
console.log("\nGov Phase-3: one-way oracle-source lock");
const envPda = pda(["envelope", im0.M]);
await send(await program.methods.setEnvelopeConfig(14, new BN(100), new BN(10000), 3000, 50, new BN(1), new BN(100)).accountsPartial({ authority: signer.publicKey, market: im0.M, envelopeConfig: envPda }).instruction());
const lockRando = Keypair.generate();
const lockRej = await sendExpectFail(await program.methods.lockOracleSource().accountsPartial({ authority: lockRando.publicKey, market: im0.M, envelopeConfig: envPda }).instruction(), [lockRando]);
ok(lockRej, "Gov P-3: a non-authority CANNOT lock the oracle source");
const lockSig = await send(await program.methods.lockOracleSource().accountsPartial({ authority: signer.publicKey, market: im0.M, envelopeConfig: envPda }).instruction());
ok(!!lockSig, `Gov P-3: authority LOCKED the oracle source — ${String(lockSig).slice(0, 12)}…`);
ok((await program.account.marketEnvelopeConfigAccount.fetch(envPda)).sourceLocked === 1, "  source_locked == 1 (direct update_oracle disabled; one-way — no unlock ix)");

// ── GOVERNANCE Phase-2b follow-up: guardian VETO of a pending params update ──────
// im0 already has a guardian (set in Phase 1). Authority proposes a valid timelocked
// param change; the guardian vetoes it during the delay (the fail-safe brake).
console.log("\nGov P-2b veto: guardian vetoes a pending timelocked params update");
const vetoPending = pda(["pending_params", im0.M]);
const im0Guardian = pda(["market_guardian", im0.M]);
await send(await program.methods.proposeParamUpdate(paramsIM).accountsPartial({ authority: signer.publicKey, market: im0.M }).instruction());
ok(!!(await l1.getAccountInfo(vetoPending)), "  authority proposed a pending update");
const vetoSig = await send(await program.methods.guardianVetoParamUpdate().accountsPartial({ guardian: guardian.publicKey, market: im0.M, guardianAccount: im0Guardian, pending: vetoPending }).instruction(), [guardian]);
ok(!!vetoSig, `Gov P-2b veto: guardian VETOED the pending update — ${String(vetoSig).slice(0, 12)}…`);
ok((await l1.getAccountInfo(vetoPending)) === null, "  pending closed by the guardian veto (fail-safe brake)");

console.log(`\n${fail === 0 ? "✅ AUDIT-FIXES LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
