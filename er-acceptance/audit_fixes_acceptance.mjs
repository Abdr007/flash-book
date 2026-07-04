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
// intake gate and a counter read. FLP/M-7/liquidation flows that need collateral are
// scaffolded separately.
//
// L1_RPC=<devnet> node audit_fixes_acceptance.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
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
const FLP = pda(["flp_exposure"]);
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
  await send(await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, flpExposure: FLP, systemProgram: sys }).instruction(), [base]);
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
  program.methods.placeTakerOrderV2(side, new BN(size), new BN(price), 0, new BN(0), sub)
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
//    cross reserves unsettled OI. First the FLP posts a maker so the taker crosses.
console.log("\nC-1 positive + M-6: real TraderState accepted; cross reserves OI");
await send(await program.methods.flpPostMakerOrder(1, new BN(1), new BN(100000), new BN(0)).accountsPartial({ authority: signer.publicKey, market: im0.M, marketBook: im0.BOOK, flpExposure: FLP }).instruction());
const c1okSig = await send(await placeTaker(im0, 0, TS0)); // crosses the FLP ask
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

// ── F-1 (CRITICAL): vault_place_order_v3 must REQUIRE the vault's TraderState ────
// Pre-fix a vault could rest a maker order under (vault_pk,0) with NO TraderState;
// a taker crossing it committed a fill that could NEVER settle (apply_fill hard-
// loads the maker TraderState) → permanent FIFO wedge, brickable by any user for
// rent (create_vault_v3 is permissionless). Post-fix vault_trader_state is a
// required, PDA-checked account on VaultPlaceOrderV3, so a vault order is
// structurally unable to rest against a non-existent TraderState.
console.log("\nF-1: vault order requires the vault's TraderState (anti-wedge, CRITICAL)");
const vaultId = 1 + Math.floor(Math.random() * 250);
const vaultPda = pda(["vault_v3", signer.publicKey, Buffer.from([vaultId])]);
try { await send(await program.methods.createVaultV3(vaultId, Array(32).fill(0), 0).accountsPartial({ strategist: signer.publicKey, vault: vaultPda, systemProgram: sys }).instruction()); } catch (e) {}
ok(!!(await l1.getAccountInfo(vaultPda)), `  vault created ${vaultPda.toBase58().slice(0, 8)}…`);
const vaultTs = pda(["trader_state", vaultPda]);
ok(!(await l1.getAccountInfo(vaultTs)), "  precondition: vault TraderState does NOT exist yet");
const vaultPlace = () => program.methods.vaultPlaceOrderV3(1, new BN(1), new BN(100000), 0, new BN(0))
  .accountsPartial({ strategist: signer.publicKey, vault: vaultPda, market: im0.M, marketBook: im0.BOOK, vaultTraderState: vaultTs }).instruction();
const f1neg = await sendExpectFail(await vaultPlace());
ok(f1neg, "F-1: vault order with a NON-EXISTENT vault TraderState is REJECTED (wedge closed)");
await send(await program.methods.vaultOpenTraderStateV3().accountsPartial({ strategist: signer.publicKey, vault: vaultPda, vaultTraderState: vaultTs, systemProgram: sys }).instruction());
const f1sig = await send(await vaultPlace());
ok(!!f1sig, `F-1: vault order WITH a real vault TraderState is ACCEPTED — ${String(f1sig).slice(0, 12)}… (no false-reject)`);

console.log(`\n${fail === 0 ? "✅ AUDIT-FIXES LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
