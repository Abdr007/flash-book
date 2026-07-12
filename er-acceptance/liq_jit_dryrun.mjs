// LIQUIDATION JIT-AUCTION dry-run LIVE on devnet. Proves the JIT liquidation
// auction (fixed: discriminator double-strip) works end-to-end on the deployed
// program: open an underwater position, pre-commit an in-band JIT offer, then
// liquidate — the offer must be SELECTED and its remaining size CONSUMED (which
// was impossible before the fix, when every offer deserialized to garbage), and
// an out-of-band offer must be left untouched.
//
//   L1_RPC=<devnet> node liq_jit_dryrun.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, createAssociatedTokenAccountInstruction, createMintToInstruction, TOKEN_PROGRAM_ID } from "@solana/spl-token";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const signer = Keypair.fromSecretKey(
  new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))),
);
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const sys = SystemProgram.programId;
const pda = (s, p = PID) =>
  PublicKey.findProgramAddressSync(
    s.map((x) => (Buffer.isBuffer(x) ? x : typeof x === "string" ? Buffer.from(x) : x.toBuffer())),
    p,
  )[0];
const u32le = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");
const LP = pda(["lp_exposure"]);
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");

const sendAs = async (kp, ix, extra = []) => {
  const { blockhash } = await l1.getLatestBlockhash("confirmed");
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: kp.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [kp, ...extra], { commitment: "confirmed", skipPreflight: true });
};
let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };
const nowUnix = () => Math.floor(Date.now() / 1000);

console.log(`Liquidation JIT-auction dry-run — L1=${L1_RPC}\n`);

// Fresh market: zero fee (taker needs no collateral) + a trusted oracle
// (oracle_staleness > 0) so a dropped oracle drives the worse-of health.
const ref = await program.account.marketAccount.fetch(REF_MARKET);
const params = ref.params;
params.takerFeeBps = 0;
params.makerRebateBps = 0;
params.oracleStalenessMaxSeconds = 3600;

const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);
const ENV = pda(["envelope", M]);

console.log("setup: armed zero-fee market + book + ring + WIDE envelope");
await sendAs(signer, await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), [base]);
await sendAs(signer, await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.setEnvelopeConfig(400, new BN(1), new BN(0), 5000, 50, new BN(0), new BN(100)).accountsPartial({ authority: signer.publicKey, market: M, envelopeConfig: ENV, systemProgram: sys }).instruction());
console.log(`  market ${M.toBase58()}\n`);

// ── Open a LONG for a thin-collateral (near-max-leverage) taker. ────────────
console.log("1) LP posts ask 1@100_000; a thin-collateral taker crosses; keeper settles via the ring");
await sendAs(signer, await program.methods.lpPostMakerOrder(1, new BN(1), new BN(100000), new BN(0)).accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, lpExposure: LP }).instruction());
const taker = Keypair.generate();
await sendAs(signer, SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: taker.publicKey, lamports: 60_000_000 }));
const TS = pda(["trader_state", taker.publicKey]);
await sendAs(taker, await program.methods.openTraderState().accountsPartial({ trader: taker.publicKey, traderState: TS, systemProgram: sys }).instruction());
// Fund the taker with THIN collateral (2_600, just over the 2.5% intake margin
// on a 100_000 notional) so it opens near max leverage and a small oracle drop
// tips it underwater.
const takerAta = getAssociatedTokenAddressSync(QUOTE, taker.publicKey);
await sendAs(signer, createAssociatedTokenAccountInstruction(signer.publicKey, takerAta, taker.publicKey, QUOTE));
await sendAs(signer, createMintToInstruction(QUOTE, takerAta, signer.publicKey, 5000n));
await sendAs(taker, await program.methods.depositCollateral(new BN(2600)).accountsPartial({ trader: taker.publicKey, traderState: TS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: takerAta, quoteVault: VAULT, tokenProgram: TOKEN_PROGRAM_ID }).instruction());
const TPOS = pda(["position", M, TS]);
await sendAs(taker, await program.methods.placeTakerOrderV2(0, new BN(1), new BN(100000), 0, new BN(0), 0).accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK, traderState: TS, position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction());
await sendAs(signer, await program.methods.applyLpFill(new BN(1), new BN(100000), 0, 0, new BN(1), false).accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TS, takerPosition: TPOS, lpExposure: LP, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, systemProgram: sys }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction());
const pos0 = await program.account.positionAccount.fetch(TPOS);
ok(Number(pos0.sizeLots) === 1 && pos0.side === 0, `taker holds LONG ${pos0.sizeLots} @ ${pos0.entryPriceTicks} (thin collateral)`);

// ── Drop the oracle to 97_000 → worse-of health ⇒ underwater. ───────────────
console.log("\n2) drop the oracle 100_000 → 97_000 (worse-of health turns the position underwater)");
await sendAs(signer, await program.methods.updateOracle(new BN(97000), new BN(10), new BN(nowUnix())).accountsPartial({ authority: signer.publicKey, market: M, envelopeConfig: ENV }).instruction());
const mk = await program.account.marketAccount.fetch(M);
ok(Number(mk.oraclePriceTicks) === 97000, `oracle now ${mk.oraclePriceTicks}`);

// ── Pre-commit two JIT offers. close_side for a long is Short ⇒ the band is
// (synthetic ≈ 96_515, health 97_000]: 97_000 is in-band, 97_500 is out-of-band.
console.log("\n3) pre-commit an IN-BAND offer @97_000 and an OUT-OF-BAND offer @97_500");
const IN_NONCE = 1, OOB_NONCE = 2;
const IN_OFFER = pda(["jit_liq_offer", M, signer.publicKey, u32le(IN_NONCE)]);
const OOB_OFFER = pda(["jit_liq_offer", M, signer.publicKey, u32le(OOB_NONCE)]);
await sendAs(signer, await program.methods.placeJitLiquidationOffer(IN_NONCE, PublicKey.default, 0, new BN(97000), new BN(1), new BN(0), 0).accountsPartial({ maker: signer.publicKey, market: M, jitOffer: IN_OFFER, systemProgram: sys }).instruction());
await sendAs(signer, await program.methods.placeJitLiquidationOffer(OOB_NONCE, PublicKey.default, 0, new BN(97500), new BN(1), new BN(0), 0).accountsPartial({ maker: signer.publicKey, market: M, jitOffer: OOB_OFFER, systemProgram: sys }).instruction());
ok(true, "both offers placed");

// ── Liquidate (caller = signer ≠ taker) passing BOTH offers. ────────────────
console.log("\n4) liquidate the underwater position, passing both JIT offers");
const CALLER_TS = pda(["trader_state", signer.publicKey]);
try { await sendAs(signer, await program.methods.openTraderState().accountsPartial({ trader: signer.publicKey, traderState: CALLER_TS, systemProgram: sys }).instruction()); } catch { /* already exists */ }
let liqSig = "", liqErr = "";
try {
  liqSig = await sendAs(signer, await program.methods.liquidatePositionV2(new BN(0)).accountsPartial({ caller: signer.publicKey, market: M, marketBook: BOOK, traderState: TS, callerTraderState: CALLER_TS, position: TPOS, systemProgram: sys }).remainingAccounts([{ pubkey: IN_OFFER, isWritable: true, isSigner: false }, { pubkey: OOB_OFFER, isWritable: true, isSigner: false }]).instruction());
} catch (e) { liqErr = String(e.message || e).slice(0, 160); }
ok(liqSig !== "", `liquidate_position_v2 SETTLED${liqSig ? " — " + liqSig : " — got: " + liqErr}`);

// ── Verify: in-band offer CONSUMED, out-of-band offer UNTOUCHED. ─────────────
console.log("\n5) verify the JIT auction selected the in-band offer and rejected the out-of-band one");
const inAfter = await program.account.jitLiquidationOfferAccount.fetch(IN_OFFER);
const oobAfter = await program.account.jitLiquidationOfferAccount.fetch(OOB_OFFER);
ok(Number(inAfter.remainingSizeLots) === 0, `in-band offer CONSUMED (remaining ${inAfter.remainingSizeLots} == 0) — auction deserialized + selected it on-chain (impossible pre-fix)`);
ok(Number(oobAfter.remainingSizeLots) === 1, `out-of-band offer REJECTED (remaining ${oobAfter.remainingSizeLots} == 1) — H-1 bound held`);

console.log(`\n${fail === 0 ? "✅ LIQUIDATION JIT DRY-RUN PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
