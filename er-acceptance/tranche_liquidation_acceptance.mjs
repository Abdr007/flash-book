// Devnet acceptance for 4.5 (tranched liquidation): a liquidatable position
// larger than max_liq_tranche_lots injects a close order of only ONE tranche.
//
//   PROGRAM=BRtnEAZ6... L1_RPC=<keyed devnet> node tranche_liquidation_acceptance.mjs
//
// Flow: armed market (max_liq_tranche_lots=2) -> maker rests an ask -> taker
// BUYS 3 (long @ 100000, healthy) -> set_envelope_config -> update_oracle drops
// the oracle (first envelope observation seeds+skips the cap) so the long goes
// underwater -> liquidate_position_v2 -> assert LiquidationInjectedV2Event's
// size_lots == 2 (the tranche cap), not 3.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const FRESH = new PublicKey(process.env.PROGRAM || "BRtnEAZ6Tc61gz8m93unL1vzaC4GjtHViLCU8JqKB2gD");
const OLD = new PublicKey("5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq");
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
async function send(ixs, extra = [], cu = 700_000) {
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
async function eventsOf(sig) {
  const t = await l1.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
  const out = [];
  const parser = new anchor.EventParser(FRESH, program.coder);
  for (const ev of parser.parseLogs(t?.meta?.logMessages || [])) out.push(ev);
  return out;
}

console.log(`\nProgram : ${FRESH.toBase58()}\nRPC     : ${L1_RPC.split("?")[0]}\nSigner  : ${signer.publicKey.toBase58()}\n`);

const TRANCHE = 2, SIZE = 3;
const INS = pda(["insurance_fund"]);
const ins = await program.account.insuranceFundAccount.fetch(INS);
const QUOTE = ins.quoteMint, VAULT = ins.quoteVault;
const LP = pda(["lp_exposure"]);
const ref = await oldProgram.account.marketAccount.fetch(REF_MARKET);
const params = { ...ref.params, oracleStalenessMaxSeconds: 600, minNotionalQuoteLots: new BN(0), maxLiqTrancheLots: new BN(TRANCHE), liquidationCooldownSlots: 0 };
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);
const FO = pda(["fill_outbox", M]);
const ENV = pda(["envelope", M]);
const dummy = Keypair.generate().publicKey;
await send(await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: dummy, quoteVault: VAULT, oracleAccount: dummy, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).instruction(), [base]);
await send(await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).instruction());
await send(await program.methods.initFillCommitment(105).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).instruction());
await send(await program.methods.initFillOutbox().accountsPartial({ authority: signer.publicKey, market: M, fillOutbox: FO, fillCommitment: FC, systemProgram: sys }).instruction());
console.log(`genesis: armed market ${M.toBase58().slice(0, 8)}… (maxLiqTrancheLots=${TRANCHE})\n`);

// traders: maker (rests ask), taker (buys long — thin margin), liquidator
const maker = Keypair.generate(), taker = Keypair.generate(), liq = Keypair.generate();
for (const kp of [maker, taker, liq]) await send(SystemProgram.transfer({ fromPubkey: signer.publicKey, toPubkey: kp.publicKey, lamports: 40_000_000 }));
const MTS = pda(["trader_state", maker.publicKey]), TTS = pda(["trader_state", taker.publicKey]), LTS = pda(["trader_state", liq.publicKey]);
const deposits = { [MTS.toBase58()]: 5_000_000_000, [TTS.toBase58()]: Number(process.env.TAKER_DEP || 1_500_000), [LTS.toBase58()]: 5_000_000_000 };
for (const [kp, ts] of [[maker, MTS], [taker, TTS], [liq, LTS]]) {
  await send(await program.methods.openTraderState().accountsPartial({ trader: kp.publicKey, traderState: ts, systemProgram: sys }).instruction(), [kp]);
  const a = ata(kp.publicKey, QUOTE);
  await send([createAtaIx(signer.publicKey, kp.publicKey, QUOTE), mintToIx(QUOTE, a, signer.publicKey, 9_000_000_000)]);
  await send(await program.methods.depositCollateral(new BN(deposits[ts.toBase58()])).accountsPartial({ trader: kp.publicKey, traderState: ts, insuranceFund: INS, quoteMint: QUOTE, traderQuoteAta: a, quoteVault: VAULT, tokenProgram: TOKEN }).instruction(), [kp]);
}
console.log(`maker ${maker.publicKey.toBase58().slice(0, 8)}… taker ${taker.publicKey.toBase58().slice(0, 8)} (dep ${deposits[TTS.toBase58()]})… liq ${liq.publicKey.toBase58().slice(0, 8)}…\n`);

// open: maker rests ask @100000 size 3; taker buys 3 -> long 3 @100000
const MPOS = pda(["position", M, MTS]), TPOS = pda(["position", M, TTS]);
await send(await program.methods.placeLimitOrderV2(1, new BN(SIZE), new BN(100000), 0, new BN(0), 0).accountsPartial({ trader: maker.publicKey, market: M, marketBook: BOOK, traderState: MTS, position: null }).instruction(), [maker]);
await send(await program.methods.placeTakerOrderV2(0, new BN(SIZE), new BN(200000), 0, new BN(0), 0).accountsPartial({ trader: taker.publicKey, market: M, marketBook: BOOK, traderState: TTS, position: null }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }, { pubkey: FO, isWritable: true, isSigner: false }]).instruction(), [taker], 1_400_000);
await send(await program.methods.applyFill(new BN(SIZE), new BN(100000), 0, false, 0, 0, new BN(1)).accountsPartial({ sequencer: signer.publicKey, market: M, insuranceFund: INS, takerTraderState: TTS, makerTraderState: MTS, takerPosition: TPOS, makerPosition: MPOS, feeTiers: null, marketHaircut: null, takerPositionHaircut: null, makerPositionHaircut: null, systemProgram: sys }).remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]).instruction(), [], 1_000_000);
const tp0 = await program.account.positionAccount.fetch(TPOS);
console.log(`opened: taker position size=${tp0.sizeLots} side=${tp0.side} (0=long)\n`);

// envelope + oracle drop (first observation seeds+skips the cap)
await send(await program.methods.setEnvelopeConfig(400, new BN(1), new BN(0), 5000, 50, new BN(0), new BN(100)).accountsPartial({ authority: signer.publicKey, market: M, envelopeConfig: ENV, systemProgram: sys }).instruction());
const now = Math.floor(Date.now() / 1000) - 5;
const DROP = Number(process.env.DROP || 50000);
await send(await program.methods.updateOracle(new BN(DROP), new BN(0), new BN(now)).accountsPartial({ authority: signer.publicKey, market: M, envelopeConfig: ENV }).instruction());
console.log(`oracle dropped 100000 -> ${DROP}; taker long now underwater\n`);

// liquidate — expect an injected close order of exactly TRANCHE lots
try {
  const sig = await send(await program.methods.liquidatePositionV2(new BN(0)).accountsPartial({ caller: liq.publicKey, market: M, marketBook: BOOK, traderState: TTS, callerTraderState: LTS, position: TPOS, systemProgram: sys }).instruction(), [liq], 1_000_000);
  const evs = await eventsOf(sig);
  const inj = evs.find((e) => e.name === "liquidationInjectedV2Event" || e.name === "LiquidationInjectedV2Event");
  const injected = inj ? Number(inj.data.sizeLots) : null;
  rec("4.5 liquidation injects exactly one tranche", injected === TRANCHE, `injected size_lots=${injected} (position was ${SIZE}, tranche=${TRANCHE})`, sig);
} catch (e) {
  const sig = String(e).match(/[1-9A-HJ-NP-Za-km-z]{60,}/)?.[0];
  let logs = "";
  if (sig) { try { const t = await l1.getTransaction(sig, { maxSupportedTransactionVersion: 0 }); logs = (t?.meta?.logMessages || []).slice(-5).join(" | "); } catch {} }
  rec("4.5 liquidation injects exactly one tranche", false, (String(e.message || e).slice(0, 100) + " " + logs).slice(0, 300));
}

const pass = rows.filter((r) => r.ok).length, fail = rows.length - pass;
console.log(`\n${pass} pass / ${fail} fail`);
fs.writeFileSync(new URL("./tranche_liquidation_results.json", import.meta.url), JSON.stringify({ program: FRESH.toBase58(), rpc: L1_RPC.split("?")[0], market: M.toBase58(), rows: rows.map((r) => ({ ...r, explorer: r.sig ? EXPLORER(r.sig) : null })), pass, fail }, null, 2));
process.exit(fail ? 1 : 0);
