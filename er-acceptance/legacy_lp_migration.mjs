// One-time devnet migration for the legacy LP-seed wedge (see
// close_legacy_lp_accounts in the program): the de-brand seed rename moved
// the pool singleton (`flp_exposure` -> `lp_exposure`) but the treasury
// LpPositionAccount seed stayed canonical, so the pre-rename treasury blocks
// `initialize_liquidity_pool` forever. This script, run by the insurance-fund
// authority AFTER upgrading the program:
//   1. asserts the canonical `lp_exposure` singleton does not exist
//   2. runs close_legacy_lp_accounts (closes the stranded treasury + drains
//      the orphaned pre-rename singleton; rent -> authority)
//   3. runs initialize_liquidity_pool(0) and asserts the pool is live
// Idempotent: if the pool already exists it exits 0 without doing anything.
// L1_RPC=<devnet> node legacy_lp_migration.mjs
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

const LP = pda(["lp_exposure"]);
const INS = pda(["insurance_fund"]);
const TREASURY = pda(["lp_position", signer.publicKey]);
const LEGACY_FLP = pda(["flp_exposure"]);

console.log(`program            ${PID.toBase58()}`);
console.log(`authority          ${signer.publicKey.toBase58()}`);
console.log(`lp_exposure        ${LP.toBase58()}`);
console.log(`legacy flp_expo    ${LEGACY_FLP.toBase58()}`);
console.log(`legacy treasury    ${TREASURY.toBase58()}`);

if (await l1.getAccountInfo(LP)) {
  console.log("pool singleton already initialized — nothing to migrate ✅");
  process.exit(0);
}

const treasury = await l1.getAccountInfo(TREASURY);
const legacyFlp = await l1.getAccountInfo(LEGACY_FLP);
if (!treasury || !legacyFlp) {
  console.error("expected legacy wreckage not found (treasury:", !!treasury, "flp_exposure:", !!legacyFlp, ") — refusing to guess; initialize the pool directly.");
  process.exit(1);
}

const balBefore = await l1.getBalance(signer.publicKey);
const migrateSig = await program.methods
  .closeLegacyLpAccounts()
  .accountsPartial({
    authority: signer.publicKey,
    insuranceFund: INS,
    lpExposure: LP,
    legacyTreasuryPosition: TREASURY,
    legacyFlpExposure: LEGACY_FLP,
  })
  .rpc();
console.log(`close_legacy_lp_accounts ✅ ${migrateSig}`);

if (await l1.getAccountInfo(TREASURY)) throw new Error("legacy treasury still exists");
if (await l1.getAccountInfo(LEGACY_FLP)) throw new Error("legacy flp_exposure still exists");
const balAfter = await l1.getBalance(signer.publicKey);
console.log(`legacy accounts reaped, rent reclaimed ~${(balAfter - balBefore) / 1e9} SOL (net of fees)`);

const initSig = await program.methods
  .initializeLiquidityPool(new BN(0))
  .accountsPartial({
    authority: signer.publicKey,
    lpExposure: LP,
    authorityLpPosition: TREASURY,
    insuranceFund: INS,
    systemProgram: sys,
  })
  .rpc();
console.log(`initialize_liquidity_pool ✅ ${initSig}`);

const pool = await program.account.liquidityPoolAccount.fetch(LP);
if (pool.authority.toBase58() !== signer.publicKey.toBase58()) throw new Error("pool authority mismatch");
console.log(`pool live: authority=${pool.authority.toBase58()} shares=${pool.lpSharesOutstanding.toString()}`);
console.log("MIGRATION COMPLETE ✅ — market creation is unblocked");
