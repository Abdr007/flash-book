// Devnet acceptance for 4.4 (OI-crowding maintenance surcharge). A/B proof:
// the SAME zero-PnL position (health = collateral, well above the base MM) is
// NOT liquidatable on a slope=0 market but IS liquidatable on an otherwise
// identical slope>0 market — so the surcharge alone flips the decision.
//
//   PROGRAM=BRtnEAZ6... L1_RPC=<keyed devnet> node oi_surcharge_acceptance.mjs
//
// No oracle move needed: the taker opens a long at oracle=entry (0 PnL), so its
// health equals its collateral. Deposit is set well above the base maintenance
// requirement but below the surcharged one; only the OI surcharge (a huge slope
// so the position's own OI hits the cap) can push MM past the health.
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
const CODE = (n) => IDL.errors.find((e) => e.name === n).code;
async function withRetry(fn) { for (let a = 0; ; a++) { try { return await fn(); } catch (e) { if (/429|Too Many Requests|blockhash|Block height/i.test(String(e.message || e)) && a < 8) { await sleep(1200 * (a + 1)); continue; } throw e; } } }
async function send(ixs, extra = [], cu = 700_000) {
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: cu }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  for (const i of (Array.isArray(ixs) ? ixs : [ixs])) tx.add(i);
  return withRetry(() => sendAndConfirmTransaction(l1, tx, [signer, ...extra], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 }));
}
function errCodeOf(e) {
  const logs = (e?.transactionLogs || e?.logs || []).join("\n");
  const m = [String(e?.message || ""), e?.transactionMessage || "", logs, String(e)].join(" | ");
  const mm = m.match(/custom program error: 0x([0-9a-fA-F]+)/) || m.match(/Custom["']?:\s*(\d+)/) || m.match(/Error Number:\s*(\d+)/);
  if (!mm) return null;
  return mm[0].includes("0x") ? parseInt(mm[1], 16) : parseInt(mm[1], 10);
}
const createAtaIx = (payer, owner, mint) => new anchor.web3.TransactionInstruction({ programId: ATA_PROG, keys: [{ pubkey: payer, isSigner: true, isWritable: true }, { pubkey: ata(owner, mint), isSigner: false, isWritable: true }, { pubkey: owner, isSigner: false, isWritable: false }, { pubkey: mint, isSigner: false, isWritable: false }, { pubkey: sys, isSigner: false, isWritable: false }, { pubkey: TOKEN, isSigner: false, isWritable: false }], data: Buffer.from([1]) });
const mintToIx = (mint, dest, authority, amount) => { const d = Buffer.alloc(9); d.writeUInt8(7, 0); d.writeBigUInt64LE(BigInt(amount), 1); return new anchor.web3.TransactionInstruction({ programId: TOKEN, keys: [{ pubkey: mint, isSigner: false, isWritable: true }, { pubkey: dest, isSigner: false, isWritable: true }, { pubkey: authority, isSigner: true, isWritable: false }], data: d }); };
async function injectedInTx(sig) {
  const t = await l1.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
  const parser = new anchor.EventParser(FRESH, program.coder);
  for (const ev of parser.parseLogs(t?.meta?.logMessages || [])) if (/liquidationInjectedEvent/i.test(ev.name)) return true;
  return false;
}
const rows = [];
const rec = (n, ok, d, sig) => { rows.push({ n, ok, d, sig }); console.log(`${ok ? "PASS" : "FAIL"}  ${n}  ${d}${sig ? "  " + EXPLORER(sig) : ""}`); };

console.log(`\nProgram : ${FRESH.toBase58()}\nRPC     : ${L1_RPC.split("?")[0]}\nSigner  : ${signer.publicKey.toBase58()}\n`);

const INS = pda(["insurance_fund"]);
const ins = await program.account.insuranceFundAccount.fetch(INS);
const QUOTE = ins.quoteMint, VAULT = ins.quoteVault;
const LP = pda(["lp_exposure"]);
const ref = await oldProgram.account.marketAccount.fetch(REF_MARKET);
const SIZE = 10;
const DEP = Number(process.env.DEP || 1_500_000); // health = collateral; >> base MM, << surcharged MM
const SLOPE = Number(process.env.SLOPE || 4_294_967_295); // 10-lot OI × slope/1e6 → hits the cap
const MAXX = Number(process.env.MAXX || 40_000); // +50% MM surcharge

const liq = Keypair.generate();
await send(SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: liq.publicKey, lamports: 60_000_000 }));
const LTS = pda(["trader_state", liq.publicKey]);
await send(await program.methods.openTraderState().accountsPartial({ trader: liq.publicKey, traderState: LTS, systemProgram: sys }).instruction(), [liq]);
{ const a = ata(liq.publicKey, QUOTE); await send([createAtaIx(signer.publicKey, liq.publicKey, QUOTE), mintToIx(QUOTE, a, signer.publicKey, 9_000_000_000)]); await send(await program.methods.depositCollateral(new BN(5_000_000_000)).accountsPartial({ trader: liq.publicKey, traderState: LTS, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: a, quoteVault: VAULT, tokenProgram: TOKEN }).instruction(), [liq]); }

async function scenario(label, slope) {
  const params = { ...ref.params, oracleStalenessMaxSeconds: 600, minNotionalQuoteLots: new BN(0), liquidationCooldownSlots: 0, oiMmrSlopeBpsPerMillionLots: slope, oiMmrMaxExtraBps: MAXX, maxLiqTrancheLots: new BN(0) };
  const base = Keypair.generate();
  const M = pda(["market", base.publicKey, QUOTE]);
  const BOOK = pda(["market_book", M]), FC = pda(["fill_commit", M]), FO = pda(["fill_outbox", M]);
  const dummy = Keypair.generate().publicKey;
  await send(await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: dummy, quoteVault: VAULT, oracleAccount: dummy, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), [base]);
  await send(await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
  await send(await program.methods.initFillCommitment(105).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
  await send(await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, fillCommitment: FC, systemProgram: sys }).instruction());

  const maker = Keypair.generate(), taker = Keypair.generate();
  for (const kp of [maker, taker]) await send(SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: kp.publicKey, lamports: 40_000_000 }));
  const MTS = pda(["trader_state", maker.publicKey]), TTS = pda(["trader_state", taker.publicKey]);
  for (const [kp, ts, dep] of [[maker, MTS, 5_000_000_000], [taker, TTS, DEP]]) {
    await send(await program.methods.openTraderState().accountsPartial({ trader: kp.publicKey, traderState: ts, systemProgram: sys }).instruction(), [kp]);
    const a = ata(kp.publicKey, QUOTE);
    await send([createAtaIx(signer.publicKey, kp.publicKey, QUOTE), mintToIx(QUOTE, a, signer.publicKey, 9_000_000_000)]);
    await send(await program.methods.depositCollateral(new BN(dep)).accountsPartial({ trader: kp.publicKey, traderState: ts, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: a, quoteVault: VAULT, tokenProgram: TOKEN }).instruction(), [kp]);
  }
  const MPOS = pda(["position", M, MTS]), TPOS = pda(["position", M, TTS]);
  // maker asks 10 @ 100000; taker buys 10 -> long 10 @ 100000 (0 PnL, health = DEP)
  await send(await program.methods.placeLimitOrder(1, new BN(SIZE), new BN(100000), 0, new BN(0), 0).accountsPartial({ trader: maker.publicKey, market: M, marketBook: BOOK, traderState: MTS, position: null }).instruction(), [maker]);
  await send(await program.methods.placeTakerOrder(0, new BN(SIZE), new BN(200000), 0, new BN(0), 0).accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK, traderState: TTS, position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }, { pubkey: FO, isWritable: true, isSigner: false }]).instruction(), [taker], 1_400_000);
  await send(await program.methods.applyFill(new BN(SIZE), new BN(100000), 0, false, 0, 0, new BN(1)).accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TTS, makerTraderState: MTS, takerPosition: TPOS, makerPosition: MPOS, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, makerPositionHaircut: null, systemProgram: sys }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction(), [], 1_000_000);
  // attempt liquidation (no adverse move) — outcome depends ONLY on the surcharge.
  // A healthy position's liquidate_position SUCCEEDS but injects nothing; a
  // liquidatable one injects a close order (LiquidationInjectedEvent). So key
  // off the event, not tx success.
  try {
    const sig = await send(await program.methods.liquidatePosition(new BN(0)).accountsPartial({ caller: liq.publicKey, market: M, marketBook: BOOK, traderState: TTS, callerTraderState: LTS, position: TPOS, systemProgram: sys }).instruction(), [liq], 1_000_000);
    const injected = await injectedInTx(sig);
    return { liquidatable: injected, sig };
  } catch (e) {
    return { liquidatable: false, code: errCodeOf(e) };
  }
}

const A = await scenario("A slope=0", 0);
console.log(`A (slope=0):   liquidatable=${A.liquidatable}${A.code ? ` code=${A.code}(${A.code === CODE("NotLiquidatable") ? "NotLiquidatable" : A.code})` : ""}`);
const B = await scenario("B slope>0", SLOPE);
console.log(`B (slope=${SLOPE}): liquidatable=${B.liquidatable}${B.code ? ` code=${B.code}` : ""}\n`);

rec("4.4 zero-PnL position is HEALTHY with slope=0 (surcharge off)", A.liquidatable === false && A.code === CODE("NotLiquidatable"), `slope=0 → NotLiquidatable (health ${DEP} > base MM)`);
rec("4.4 SAME position is LIQUIDATABLE with slope>0 (surcharge on)", B.liquidatable === true, `slope=${SLOPE},maxExtra=${MAXX} → liquidation injected`, B.sig);
rec("4.4 the OI surcharge ALONE flips liquidatability", A.liquidatable === false && B.liquidatable === true, "healthy@slope0, underwater@slope>0 — identical otherwise");

const pass = rows.filter((r) => r.ok).length, fail = rows.length - pass;
console.log(`\n${pass} pass / ${fail} fail`);
fs.writeFileSync(new URL("./oi_surcharge_results.json", import.meta.url), JSON.stringify({ program: FRESH.toBase58(), rpc: L1_RPC.split("?")[0], dep: DEP, slope: SLOPE, maxExtra: MAXX, rows: rows.map((r) => ({ ...r, explorer: r.sig ? EXPLORER(r.sig) : null })), pass, fail }, null, 2));
process.exit(fail ? 1 : 0);
