// REDUCE-ONLY / CLOSE-ONLY LIVE ACCEPTANCE (devnet) — validates Item A on the real
// chain end-to-end against the deployed program. Requires the Item-A .so deployed.
//
//   NEG-1: a reduce-only TAKER with no opposing position is rejected fail-closed
//          with ReduceOnlyNoPosition (NOT the old blanket OutOfRange) — proving the
//          flag is honored and routed to the clamp, never a silent open.
//   POS-1: a reduce-only LIMIT is ACCEPTED and rests (pre-Item-A it was rejected
//          OutOfRange) — the flag is honored on the limit path too.
//   NEG-2: after the authority moves the market to CloseOnly, a PLAIN opener
//          (flags=0) is forced reduce-only and rejected fail-closed.
//   POS-2: in the same CloseOnly market a reduce-only limit still rests (a reducing
//          order is admitted) — wind-down closes positions, never blocks closing.
//
// The negatives trip at intake and need no crossing liquidity or settlement.
//   L1_RPC=https://api.devnet.solana.com node reduce_only_acceptance.mjs
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/clober.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const signer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/id.json`))));
const l1 = new Connection(L1_RPC, "confirmed");
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(signer), { commitment: "confirmed" }));
const sys = SystemProgram.programId;
const pda = (s, p = PID) => PublicKey.findProgramAddressSync(s.map((x) => (Buffer.isBuffer(x) ? x : (typeof x === "string" ? Buffer.from(x) : x.toBuffer()))), p)[0];

const FLAG_REDUCE_ONLY = 2;
const STATUS_CLOSE_ONLY = 5;

// Reference devnet accounts (shared across the acceptance suite).
const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");
const LP = pda(["lp_exposure"]);
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");

const traderStatePda = (trader, sub = 0) =>
  sub === 0 ? pda(["trader_state", trader]) : pda(["trader_state", trader, Buffer.from([sub])]);

let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

// Run a method builder to completion; return { sig } on success or { code, num }
// (the decoded Anchor error name + number) on a program rejection.
const run = async (builder) => {
  try {
    const sig = await builder.rpc({ commitment: "confirmed" });
    return { sig };
  } catch (e) {
    if (e instanceof anchor.AnchorError) {
      return { code: e.error.errorCode.code, num: e.error.errorCode.number };
    }
    // Fall back to scraping the message/logs for the custom code.
    const s = `${e}\n${(e.logs || []).join("\n")}`;
    const m = s.match(/custom program error: 0x([0-9a-fA-F]+)/) || s.match(/Custom\((\d+)\)/);
    return { code: "unknown", num: m ? parseInt(m[1], m[0].includes("0x") ? 16 : 10) : -1, raw: s.slice(0, 300) };
  }
};

console.log(`REDUCE-ONLY / CLOSE-ONLY live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);

// Build a fresh armed market with IM=0 so reduce-only is isolated from the margin
// gate; open the signer's main TraderState.
const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const BOOK = pda(["market_book", M]);
const FC = pda(["fill_commit", M]);
const params = { ...ref.params, initialMarginRatioBps: 0, oracleStalenessMaxSeconds: new BN(60) };
console.log("setup: fresh market + signer TraderState");
await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).rpc();
await program.methods.initMarketBook().accountsPartial({ authority: signer.publicKey, market: M, marketBook: BOOK, systemProgram: sys }).rpc();
await program.methods.initFillCommitment(256).accountsPartial({ authority: signer.publicKey, market: M, fillCommitment: FC, systemProgram: sys }).rpc();
console.log(`  market ${M.toBase58()}`);
const TS0 = traderStatePda(signer.publicKey, 0);
if (!(await l1.getAccountInfo(TS0))) {
  await program.methods.openTraderState().accountsPartial({ trader: signer.publicKey, traderState: TS0, systemProgram: sys }).rpc();
}
ok(!!(await l1.getAccountInfo(TS0)), `TraderState(sub 0) exists ${TS0.toBase58().slice(0, 8)}…`);

const takerBuilder = (flags, { position = null } = {}) =>
  program.methods.placeTakerOrder(0, new BN(1), new BN(100000), flags, new BN(0), 0)
    .accountsPartial({ trader: signer.publicKey, market: M, marketBook: BOOK, traderState: TS0, position })
    .remainingAccounts([{ pubkey: FC, isWritable: true, isSigner: false }]);
const limitBuilder = (flags) =>
  program.methods.placeLimitOrder(0, new BN(1), new BN(100000), flags, new BN(0), 0)
    .accountsPartial({ trader: signer.publicKey, market: M, marketBook: BOOK, traderState: TS0, position: null });

// ── NEG-1: reduce-only taker, no position → ReduceOnlyNoPosition ───────────────
console.log("\nNEG-1: reduce-only taker with no opposing position");
const n1 = await run(takerBuilder(FLAG_REDUCE_ONLY));
ok(n1.code === "ReduceOnlyNoPosition" || n1.num === 8324,
  `reduce-only taker fail-closed as ReduceOnlyNoPosition (got ${n1.code}/${n1.num}) — NOT the old OutOfRange(7003)`);

// ── POS-1: reduce-only LIMIT is accepted (rests) — flag honored on limit path ──
console.log("\nPOS-1: reduce-only limit is accepted (rests)");
const p1 = await run(limitBuilder(FLAG_REDUCE_ONLY));
ok(!!p1.sig, `reduce-only limit ACCEPTED — ${p1.sig ? p1.sig.slice(0, 12) + "…" : `REJECTED ${p1.code}/${p1.num}`}`);

// ── move the market to CloseOnly (authority) ───────────────────────────────────
console.log("\nsetup: authority moves the market to CloseOnly(5)");
const c = await run(program.methods.setMarketStatus(STATUS_CLOSE_ONLY).accountsPartial({ authority: signer.publicKey, market: M, guardianAccount: null }));
ok(!!c.sig, `market set to CloseOnly — ${c.sig ? c.sig.slice(0, 12) + "…" : `FAILED ${c.code}/${c.num}`}`);
ok((await program.account.marketAccount.fetch(M)).status === STATUS_CLOSE_ONLY, "  market.status == CloseOnly(5)");

// ── NEG-2: plain opener in CloseOnly is forced reduce-only and rejected ─────────
console.log("\nNEG-2: plain opener (flags=0) in a CloseOnly market");
const n2 = await run(takerBuilder(0));
ok(n2.code === "ReduceOnlyNoPosition" || n2.num === 8324,
  `CloseOnly forces reduce-only → opener rejected (got ${n2.code}/${n2.num})`);

// ── POS-2: a reducing order still rests in CloseOnly (wind-down admits closing) ─
console.log("\nPOS-2: a limit still rests in CloseOnly (forced reduce-only, admitted)");
const p2 = await run(limitBuilder(0));
ok(!!p2.sig, `limit rests in CloseOnly — ${p2.sig ? p2.sig.slice(0, 12) + "…" : `REJECTED ${p2.code}/${p2.num}`}`);

console.log(`\n${fail === 0 ? "✅ REDUCE-ONLY / CLOSE-ONLY LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
