// FUNDING-CRANK LIVE ACCEPTANCE (devnet) — validates the permissionless funding
// crank on the deployed program. Proves the instruction runs on real infra: a
// non-authority caller cranks; the first tick seeds the crank clock (no accrual);
// a second tick with a real wall-clock Δt is a safe no-op on a premium-free market
// (mark == oracle at init ⇒ zero rate). The exact rate·Δt accrual, the rate/Δt
// clamps, and the long/short zero-sum are covered by the Kani proof, the 4000-case
// proptest, and the funding_index_delta unit tests; a live premium needs a Pyth/
// Lazer feed and is out of scope here.
//
//   L1_RPC=https://api.devnet.solana.com node funding_crank_acceptance.mjs
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
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const QUOTE = new PublicKey("5NL1XQZ4ZdiLR6a6VwCZWQ6DMCLdafCvbDFjeVRzcama");
const INS = new PublicKey("B9MgERuAheDM3pzh3Z4VwYMZxSGpMmYATfjpuutpgAVJ");
const VAULT = new PublicKey("2FNwaiQ1u5aJLbHviSch2p3pBVmnyMJK54v1cVtMuPVd");
const OBV = new PublicKey("Cbf3TwLKvHsh1mH72PjNt7z7dpmbtxdYZNTWxybyde22");
const OOR = new PublicKey("GebX5o8WUFLoJrMMGK1LjSBSCiSD3LZeRa248arggvDD");
const LP = pda(["lp_exposure"]);
const REF_MARKET = new PublicKey("DRTiohFdhTbyCHkc8huNMSgrgV3oDryayJHEavB5vztZ");

let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; console.log("  ✓", m); } else { fail++; console.log("  ✗ FAIL:", m); } };

console.log(`FUNDING-CRANK live acceptance — L1=${L1_RPC}\n`);
const ref = await program.account.marketAccount.fetch(REF_MARKET);

const base = Keypair.generate();
const M = pda(["market", base.publicKey, QUOTE]);
const params = { ...ref.params, oracleStalenessMaxSeconds: new BN(60) };
console.log("setup: fresh market");
await program.methods.initializeMarket(params, new BN(100000)).accountsPartial({ authority: signer.publicKey, baseMint: base.publicKey, quoteMint: QUOTE, baseVault: OBV, quoteVault: VAULT, oracleAccount: OOR, market: M, insuranceFund: INS, lpExposure: LP, systemProgram: sys }).rpc();
console.log(`  market ${M.toBase58()}`);

// A NON-authority signer cranks — permissionless.
const cranker = Keypair.generate();
await program.methods.crankFunding().accountsPartial({ caller: cranker.publicKey, market: M }).signers([cranker]).rpc();
let m = await program.account.marketAccount.fetch(M);
ok(m.cumFundingIndex.eq(new BN(0)), "first crank: index still 0 (seed-only tick)");
ok(!m.lastFundingCrankUnix.eq(new BN(0)), `first crank: clock seeded (last_crank=${m.lastFundingCrankUnix.toString()})`);
const seededAt = m.lastFundingCrankUnix.toString();

// Real wall-clock Δt, then crank again (still no premium: mark == oracle).
await sleep(3000);
const sig2 = await program.methods.crankFunding().accountsPartial({ caller: cranker.publicKey, market: M }).signers([cranker]).rpc();
m = await program.account.marketAccount.fetch(M);
ok(!!sig2, `second crank by non-authority ACCEPTED — ${sig2.slice(0, 12)}… (permissionless)`);
ok(m.lastFundingCrankUnix.gt(new BN(seededAt)), `crank clock advanced with real Δt (${seededAt} → ${m.lastFundingCrankUnix.toString()})`);
ok(m.cumFundingIndex.eq(new BN(0)), "premium-free market (mark==oracle) accrues nothing — the fail-safe holds live");
console.log(`  mark=${m.markPriceTicks} oracle=${m.oraclePriceTicks} rate=${m.lastFundingRateBpsPerSec}`);

console.log(`\n${fail === 0 ? "✅ FUNDING-CRANK LIVE ACCEPTANCE PASSED" : "❌ FAILED"} — ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
