// FLP mode-lock live acceptance (F-4). Run AFTER `solana program deploy`.
//   L1_RPC=<devnet> node flp_mode_lock_acceptance.mjs
// Proves on-chain that the singleton and per-market v3 FLP systems are mutually
// exclusive on minting LP shares: a singleton deposit claims MODE_SINGLETON,
// after which a v3 deposit fails closed with FlpSystemModeConflict.
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
const { Program, AnchorProvider, Wallet, BN } = anchor;

const L1_RPC = process.env.L1_RPC || "https://solana-devnet.api.onfinality.io/public";
const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
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

const QUOTE = new PublicKey("CJKxS7WBFaEoZkEBxd8kgWPtVShvTAfZswx4oFwGtQL3");
const INS = new PublicKey("6GwRAhhTJG5M6tLa4s7yWjCriStuD3NrF3eqaBCD74FF");
const VAULT = new PublicKey("Dqc79x21BmbdFNXXP9ZsPKpC6sUAm2cR2wovyQkroeYc");
const OBV = new PublicKey("5zJhoFomJRC3xoC7Kj33owGtVQ8t23wMAPLEjcgz8EhD");
const OOR = new PublicKey("8pRrwZ9knaCbbqDbPew28Tv965gxvfT2y9JKoUc3CnFH");
const REF_MARKET = new PublicKey("3UWaYaqCkEsyhx5mQ9XWKsrRcqXZ736dBK7KK9oeU66q");
const FLP = pda(["flp_exposure"]);
const FLP_MODE = pda(["flp_mode"]);
const TOKEN = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATOKEN = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const ata = (owner, mint) =>
  PublicKey.findProgramAddressSync([owner.toBuffer(), TOKEN.toBuffer(), mint.toBuffer()], ATOKEN)[0];
const SIGNER_ATA = ata(signer.publicKey, QUOTE);

const send = async (ix, extra = []) => {
  const { blockhash } = await l1.getLatestBlockhash("confirmed");
  const tx = new anchor.web3.Transaction({ recentBlockhash: blockhash, feePayer: signer.publicKey }).add(ix);
  return await anchor.web3.sendAndConfirmTransaction(l1, tx, [signer, ...extra], {
    commitment: "confirmed",
    skipPreflight: false,
  });
};
let pass = 0,
  fail = 0;
const ok = (c, m) => {
  if (c) {
    pass++;
    console.log("  ✓", m);
  } else {
    fail++;
    console.log("  ✗ FAIL:", m);
  }
};

console.log(`FLP mode-lock live acceptance — L1=${L1_RPC}\n`);

// ── Setup: the singleton deposit marks NAV across every LIVE FLP exposure
// slot, so each registered market must ride in remaining_accounts with a
// FRESH oracle. Leftover throwaway markets from prior acceptance runs may
// have a zero staleness bound (pre-bound era) or a stale/absent oracle —
// heal each: give it a real bound, ensure its envelope config exists, and
// refresh the oracle at the current mark (zero price move). ──────────────────
const exp = await program.account.flpExposureAccount.fetch(FLP);
const liveMarkets = exp.perMarket.filter((s) => s.side !== 255).map((s) => s.market);
console.log(`  (NAV walk spans ${liveMarkets.length} live exposure slot(s))`);
for (const mkt of liveMarkets) {
  const m = await program.account.marketAccount.fetch(mkt);
  if (!m.params.oracleStalenessMaxSeconds) {
    m.params.oracleStalenessMaxSeconds = 3600;
    await send(
      await program.methods.updateMarketParams(m.params).accountsPartial({ authority: signer.publicKey, market: mkt }).instruction(),
    );
  }
  const env = pda(["envelope", mkt]);
  if (!(await l1.getAccountInfo(env)))
    await send(
      await program.methods
        .setEnvelopeConfig(14, new BN(100), new BN(10_000), 3_000, 50, new BN(1), new BN(100))
        .accountsPartial({ authority: signer.publicKey, market: mkt, envelopeConfig: env, systemProgram: sys })
        .instruction(),
    );
  await send(
    await program.methods
      .updateOracle(new BN(m.oraclePriceTicks.toString()), new BN(0), new BN(Math.floor(Date.now() / 1000) - 2))
      .accountsPartial({ authority: signer.publicKey, market: mkt, envelopeConfig: env })
      .instruction(),
  );
}
const liveMetas = liveMarkets.map((m) => ({ pubkey: m, isSigner: false, isWritable: false }));

// ── Positive: a singleton deposit claims/matches MODE_SINGLETON ──────────────
const lpPos = pda(["lp_position", signer.publicKey]);
let sig;
try {
  sig = await send(
    await program.methods
      .depositFlpCapital(new BN(1_000_000))
      .accountsPartial({
        authority: signer.publicKey,
        flpExposure: FLP,
        lpPosition: lpPos,
        flpMode: FLP_MODE,
        insuranceFund: INS,
        quoteMint: QUOTE,
        authorityQuoteAta: SIGNER_ATA,
        quoteVault: VAULT,
        tokenProgram: TOKEN,
        systemProgram: sys,
      })
      .remainingAccounts(liveMetas)
      .instruction(),
  );
  ok(true, `singleton deposit_flp_capital ALLOWED (claims MODE_SINGLETON) — ${sig}`);
} catch (e) {
  ok(false, "singleton deposit failed: " + e);
}
const mode = await program.account.flpModeAccount.fetch(FLP_MODE);
ok(mode.mode === 1, `flp_mode.mode == 1 (singleton), got ${mode.mode}`);

// ── Negative: a v3 deposit is rejected by the lock ───────────────────────────
// Reuse the existing REF_MARKET; init its v3 pool if absent (tolerant of an
// already-initialized pool from a prior run).
const M = REF_MARKET;
const EXP = pda(["flp_per_market", M]);
try {
  await send(
    await program.methods
      .initFlpPerMarketV3()
      .accountsPartial({ authority: signer.publicKey, insuranceFund: INS, market: M, exposure: EXP, systemProgram: sys })
      .instruction(),
  );
} catch (e) {
  // already initialized — fine
}
const POS = pda(["flp_position_v3", EXP, signer.publicKey]);
let rejErr = "";
let rejected = false;
try {
  await send(
    await program.methods
      .flpDepositV3(new BN(1_000_000))
      .accountsPartial({
        lp: signer.publicKey,
        exposure: EXP,
        position: POS,
        flpMode: FLP_MODE,
        insuranceFund: INS,
        quoteMint: QUOTE,
        lpQuoteAta: SIGNER_ATA,
        quoteVault: VAULT,
        tokenProgram: TOKEN,
        systemProgram: sys,
      })
      .instruction(),
  );
} catch (e) {
  rejected = true;
  rejErr = String(e) + " " + JSON.stringify(e.logs || (e.transactionLogs ?? []));
}
ok(
  rejected && (rejErr.includes("FlpSystemModeConflict") || rejErr.includes("8321") || rejErr.includes("0x2081")),
  `v3 flp_deposit_v3 REJECTED with FlpSystemModeConflict while singleton active${rejected ? "" : " (NOT rejected!)"}`,
);
if (rejected && !(rejErr.includes("FlpSystemModeConflict") || rejErr.includes("8321") || rejErr.includes("0x2081")))
  console.log("    (rejected but with unexpected error:", rejErr.slice(0, 400), ")");

console.log(`\n${fail === 0 ? "ALL PASS" : "FAILURES"}: ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
