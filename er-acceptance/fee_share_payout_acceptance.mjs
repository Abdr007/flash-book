// Devnet HAPPY-PATH acceptance for 2.3: a REAL matched fill accrues a referrer
// fee share on-chain, then the referrer claims it. Closes the value-moving gap
// the instruction-surface harness (feature_acceptance_2026_07_12.mjs) left open.
//
//   PROGRAM=BRtnEAZ6... L1_RPC=<keyed devnet> node fee_share_payout_acceptance.mjs
//
// Markets are armed by default (C-1), so this drives the full matcher flow:
// init market+book+ring+outbox → maker rests a bid → taker sweeps (pushes a
// keccak commitment) → apply_fill settles it WITH the ring AND the referrer's
// FeeAccrual appended to remaining_accounts (find_fee_accrual discovers it by
// PDA and accrues min(share, available_surplus)) → claim_fee_accrual pays out.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const FRESH = new PublicKey(process.env.PROGRAM || "BRtnEAZ6Tc61gz8m93unL1vzaC4GjtHViLCU8JqKB2gD");
const OLD = new PublicKey("8Vdd5n4zbmxqwqY8Xv8JbEcvbih3JsEZzJBtfkoeGp2z");
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");
const EXPLORER = (s) => `https://explorer.solana.com/tx/${s}?cluster=devnet`;
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
const sys = SystemProgram.programId;
const TOKEN = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA_PROG = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program({ ...IDL, address: FRESH.toBase58() }, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const oldProgram = new Program({ ...IDL, address: OLD.toBase58() }, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const pda = (s, p = FRESH) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];
const ata = (owner, mint) => PublicKey.findProgramAddressSync([owner.toBuffer(), TOKEN.toBuffer(), mint.toBuffer()], ATA_PROG)[0];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function withRetry(fn) { for (let a = 0; ; a++) { try { return await fn(); } catch (e) { if (/429|Too Many Requests|blockhash|Block height/i.test(String(e.message || e)) && a < 8) { await sleep(1200 * (a + 1)); continue; } throw e; } } }
async function send(ixs, extra = [], cu = 600_000) {
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cu }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  return withRetry(() => sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 }));
}
const createAtaIx = (payer, owner, mint) => new anchor.web3.TransactionInstruction({ programId: ATA_PROG, keys: [{ pubkey: payer, isSigner: true, isWritable: true }, { pubkey: ata(owner, mint), isSigner: false, isWritable: true }, { pubkey: owner, isSigner: false, isWritable: false }, { pubkey: mint, isSigner: false, isWritable: false }, { pubkey: sys, isSigner: false, isWritable: false }, { pubkey: TOKEN, isSigner: false, isWritable: false }], data: Buffer.from([1]) });
const mintToIx = (mint, dest, authority, amount) => { const d = Buffer.alloc(9); d.writeUInt8(7, 0); d.writeBigUInt64LE(BigInt(amount), 1); return new anchor.web3.TransactionInstruction({ programId: TOKEN, keys: [{ pubkey: mint, isSigner: false, isWritable: true }, { pubkey: dest, isSigner: false, isWritable: true }, { pubkey: authority, isSigner: true, isWritable: false }], data: d }); };
const rows = [];
const rec = (n, ok, d, sig) => { rows.push({ n, ok, d, sig }); console.log(`${ok ? "PASS" : "FAIL"}  ${n}  ${d}${sig ? "  " + EXPLORER(sig) : ""}`); };

console.log(`\nProgram : ${FRESH.toBase58()}\nRPC     : ${L1_RPC.split("?")[0]}\nSigner  : ${signer.publicKey.toBase58()}\n`);

// ── genesis: reuse INS/LP; fresh ARMED market with a referrer share ──────────
const INS = pda(["insurance_fund"]);
const ins = await program.account.insuranceFundAccount.fetch(INS);
const QUOTE = ins.quoteMint, VAULT = ins.quoteVault;
const LP = pda(["lp_exposure"]);
const ref = await oldProgram.account.marketAccount.fetch(REF_MARKET);
const params = { ...ref.params, oracleStalenessMaxSeconds: 600, referrerShareBps: 2000, takerFeeBps: 50, makerRebateBps: 0, minNotionalQuoteLots: new BN(0) };
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);
const FO = pda(["fill_outbox", M]);
const dummy = Keypair.generate().publicKey;
await send(await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: dummy, quoteVault: VAULT, oracleAccount: dummy, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), [base]);
await send(await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await send(await program.methods.initFillCommitment(105).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
await send(await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, fillCommitment: FC, systemProgram: sys }).instruction());
console.log(`genesis: armed market ${M.toBase58().slice(0, 8)}… (referrerShareBps=${params.referrerShareBps}, takerFeeBps=${params.takerFeeBps})\n`);

// ── traders: maker (rests) + taker (sweeps, has referrer) ─────────────────────
const maker = Keypair.generate(), taker = Keypair.generate(), referrer = Keypair.generate();
for (const kp of [maker, taker]) await send(SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: kp.publicKey, lamports: 40_000_000 }));
const MTS = pda(["trader_state", maker.publicKey]), TTS = pda(["trader_state", taker.publicKey]);
for (const [kp, ts] of [[maker, MTS], [taker, TTS]]) {
  await send(await program.methods.openTraderState().accountsPartial({ trader: kp.publicKey, traderState: ts, systemProgram: sys }).instruction(), [kp]);
  const a = ata(kp.publicKey, QUOTE);
  await send([createAtaIx(signer.publicKey, kp.publicKey, QUOTE), mintToIx(QUOTE, a, signer.publicKey, 9_000_000_000)]);
  await send(await program.methods.depositCollateral(new BN(5_000_000_000)).accountsPartial({ trader: kp.publicKey, traderState: ts, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: a, quoteVault: VAULT, tokenProgram: TOKEN }).instruction(), [kp]);
}
await send(await program.methods.setTraderReferrer(referrer.publicKey).accountsPartial({ trader: taker.publicKey, traderState: TTS }).instruction(), [taker]);
const FA = pda(["fee_accrual", referrer.publicKey]);
await send(await program.methods.initFeeAccrual(referrer.publicKey).accountsPartial({ payer: signer.publicKey, feeAccrual: FA, systemProgram: sys }).instruction());
console.log(`maker ${maker.publicKey.toBase58().slice(0, 8)}… taker ${taker.publicKey.toBase58().slice(0, 8)}… referrer ${referrer.publicKey.toBase58().slice(0, 8)}…\n`);

// ── real fill: maker rests a bid, taker sweeps (commits), apply_fill settles ──
const MPOS = pda(["position", M, MTS]), TPOS = pda(["position", M, TTS]);
let accrued = new BN(0), fillSig;
try {
  // maker rests a bid: buy 1 @ 90000
  await send(await program.methods.placeLimitOrder(0, new BN(10), new BN(90000), 0, new BN(0), 0).accountsPartial({ trader: maker.publicKey, market: M, marketBook: BOOK, traderState: MTS, position: null }).instruction(), [maker]);
  // taker sweeps: sell 1 @ 1 (aggressive → crosses the bid; pushes a commitment)
  await send(await program.methods.placeTakerOrder(1, new BN(10), new BN(1), 0, new BN(0), 0).accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK, traderState: TTS, position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }, { pubkey: FO, isWritable: true, isSigner: false }]).instruction(), [taker], 1_400_000);
  // apply_fill settles the committed fill WITH the ring + the referrer accrual
  fillSig = await send(await program.methods.applyFill(new BN(10), new BN(90000), 1, false, 0, 0, new BN(1))
    .accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TTS, makerTraderState: MTS, takerPosition: TPOS, makerPosition: MPOS, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, makerPositionHaircut: null, systemProgram: sys })
    .remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }, { pubkey: FA, isWritable: true, isSigner: false }]).instruction(), [], 1_000_000);
  const fa = await program.account.feeAccrualAccount.fetch(FA);
  accrued = fa.accruedQuoteLots;
  rec("2.3 apply_fill accrues referrer share on-chain (real matched fill)", accrued.gtn(0), `accrued=${accrued} quote-lots`, fillSig);
} catch (e) {
  const sig = String(e).match(/[1-9A-HJ-NP-Za-km-z]{60,}/)?.[0];
  let logs = "";
  if (sig) { try { const t = await l1.getTransaction(sig, { maxSupportedTransactionVersion: 0 }); logs = (t?.meta?.logMessages || []).slice(-4).join(" | "); } catch {} }
  rec("2.3 apply_fill accrues referrer share on-chain (real matched fill)", false, (String(e.message || e).slice(0, 120) + " " + logs).slice(0, 260));
}

// ── claim: referrer drains the accrual to their ATA ───────────────────────────
if (accrued.gtn(0)) {
  await send(SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: referrer.publicKey, lamports: 20_000_000 }));
  const rAta = ata(referrer.publicKey, QUOTE);
  await send([createAtaIx(signer.publicKey, referrer.publicKey, QUOTE)]);
  const claimSig = await send(await program.methods.claimFeeAccrual().accountsPartial({ recipient: referrer.publicKey, feeAccrual: FA, insuranceFund: INS, lpExposure: LP, quoteMint: QUOTE, recipientQuoteAta: rAta, quoteVault: VAULT, tokenProgram: TOKEN }).instruction(), [referrer]);
  const bal = BigInt((await l1.getTokenAccountBalance(rAta)).value.amount);
  const faAfter = await program.account.feeAccrualAccount.fetch(FA);
  rec("2.3 claim_fee_accrual pays out to referrer ATA + zeroes accrual", bal === BigInt(accrued.toString()) && faAfter.accruedQuoteLots.eqn(0), `ATA balance=${bal} accrual now=${faAfter.accruedQuoteLots}`, claimSig);
}

const pass = rows.filter((r) => r.ok).length, fail = rows.length - pass;
console.log(`\n${pass} pass / ${fail} fail`);
fs.writeFileSync(new URL("./fee_share_payout_results.json", import.meta.url), JSON.stringify({ program: FRESH.toBase58(), rpc: L1_RPC.split("?")[0], market: M.toBase58(), rows: rows.map((r) => ({ ...r, explorer: r.sig ? EXPLORER(r.sig) : null })), pass, fail }, null, 2));
process.exit(fail ? 1 : 0);
