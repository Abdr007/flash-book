#!/usr/bin/env node
// K-1 live-verification gate — the PASS predicate for the single-key rug finding.
//
// Asserts, against the LIVE chain, that every authority that could drain or
// rug Flash Book has been migrated off the single deploy wallet onto the
// governance multisig, and that the sequencer hot key holds NO authority role:
//
//   1. program upgrade authority        == <multisig>   (or None = immutable)
//   2. every market.authority           == <multisig>   (or default = burned)
//   3. every market.sequencer           != <multisig> and != any authority key
//   4. insurance_fund.authority         == <multisig>
//
// Exit 0 = K-1 RESOLVED (PASS); exit 1 = still FAIL. This script is the ONLY
// thing that flips K-1 to resolved — it is deliberately code-independent so the
// certificate cannot claim K-1 fixed from source alone.
//
// Usage:
//   node scripts/verify_k1_authority.mjs \
//     --program 5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq \
//     --multisig <SQUADS_VAULT_PDA> \
//     --markets <MARKET_PDA>[,<MARKET_PDA>...] \
//     [--insurance <INSURANCE_FUND_PDA>] [--url <RPC>] [--immutable]
//
// Requires the `solana` CLI on PATH and idl/flash_book.json for field offsets.

import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const args = Object.fromEntries(
  process.argv.slice(2).flatMap((a, i, arr) =>
    a.startsWith("--") ? [[a.slice(2), arr[i + 1]?.startsWith("--") || arr[i + 1] === undefined ? true : arr[i + 1]]] : []
  )
);
const PROGRAM = args.program;
const MULTISIG = args.multisig;
const MARKETS = (args.markets ?? "").split(",").map((s) => s.trim()).filter(Boolean);
const INSURANCE = args.insurance;
const URL = args.url ?? "https://api.mainnet-beta.solana.com";
const IMMUTABLE = !!args.immutable;

if (!PROGRAM || (!MULTISIG && !IMMUTABLE)) {
  console.error("usage: verify_k1_authority.mjs --program <PID> --multisig <PDA> --markets <PDA,...> [--insurance <PDA>] [--url <RPC>] [--immutable]");
  process.exit(2);
}

const here = dirname(fileURLToPath(import.meta.url));
const idl = JSON.parse(readFileSync(join(here, "..", "idl", "flash_book.json"), "utf8"));

// Fixed-size byte widths for the primitive types we need to skip.
const WIDTH = { pubkey: 32, publicKey: 32, u128: 16, i128: 16, u64: 8, i64: 8, u32: 4, i32: 4, u16: 2, i16: 2, u8: 1, i8: 1, bool: 1 };
// Anchor 0.30+ IDL: account field layouts live in `idl.types` keyed by name;
// `idl.accounts` only carries {name, discriminator}.
function structFields(name) {
  const def = idl.types.find((t) => t.name === name);
  if (!def || def.type?.kind !== "struct") throw new Error(`no struct type ${name} in IDL`);
  return def.type.fields;
}
function sizeOf(type) {
  if (typeof type === "string") {
    if (WIDTH[type] !== undefined) return WIDTH[type];
    return structFields(type).reduce((n, f) => n + sizeOf(f.type), 0);
  }
  if (type.array) return sizeOf(type.array[0]) * type.array[1];
  if (type.defined) return sizeOf(typeof type.defined === "string" ? type.defined : type.defined.name);
  throw new Error(`unsized type ${JSON.stringify(type)}`);
}
// Byte offset of `field` inside an Anchor account (8-byte disc + Borsh fields).
function offsetOf(accountName, field) {
  let off = 8; // discriminator
  for (const f of structFields(accountName)) {
    if (f.name === field) return off;
    off += sizeOf(f.type);
  }
  throw new Error(`no field ${field} in ${accountName}`);
}

function sh(cmd, argv) {
  return execFileSync(cmd, argv, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}
function accountPubkeyField(pubkey, accountName, field) {
  const out = sh("solana", ["account", pubkey, "--url", URL, "--output", "json"]);
  const j = JSON.parse(out);
  const b64 = j.account.data[0];
  const buf = Buffer.from(b64, "base64");
  const off = offsetOf(accountName, field);
  const bs58 = toBase58(buf.subarray(off, off + 32));
  return bs58;
}

// Minimal base58 (bitcoin alphabet) encoder for 32-byte pubkeys.
const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
function toBase58(bytes) {
  let x = 0n;
  for (const b of bytes) x = (x << 8n) | BigInt(b);
  let s = "";
  while (x > 0n) { s = B58[Number(x % 58n)] + s; x /= 58n; }
  for (const b of bytes) { if (b === 0) s = "1" + s; else break; }
  return s;
}

const fails = [];
const ok = (m) => console.log(`  ✓ ${m}`);
const bad = (m) => { console.error(`  ✗ ${m}`); fails.push(m); };

console.log(`K-1 authority verification for ${PROGRAM} @ ${URL}\n`);

// 1. Upgrade authority.
try {
  const show = JSON.parse(sh("solana", ["program", "show", PROGRAM, "--url", URL, "--output", "json"]));
  const upgradeAuth = show.authority ?? null;
  if (IMMUTABLE) {
    upgradeAuth === null ? ok("upgrade authority is None (immutable)") : bad(`upgrade authority is ${upgradeAuth}, expected immutable (None)`);
  } else {
    upgradeAuth === MULTISIG ? ok(`upgrade authority == multisig`) : bad(`upgrade authority is ${upgradeAuth}, expected ${MULTISIG}`);
  }
} catch (e) { bad(`could not read program upgrade authority: ${e.message}`); }

// 2 + 3. Per-market authority & sequencer.
const DEFAULT_PK = "11111111111111111111111111111111";
const authorityKeys = new Set([MULTISIG]);
for (const m of MARKETS) {
  try {
    const auth = accountPubkeyField(m, "MarketAccount", "authority");
    const seq = accountPubkeyField(m, "MarketAccount", "sequencer");
    (auth === MULTISIG || auth === DEFAULT_PK) ? ok(`market ${m.slice(0, 8)} authority ${auth === DEFAULT_PK ? "burned" : "== multisig"}`) : bad(`market ${m} authority is ${auth}, expected multisig or burned`);
    (seq !== MULTISIG && !authorityKeys.has(seq)) ? ok(`market ${m.slice(0, 8)} sequencer is a non-authority hot key`) : bad(`market ${m} sequencer ${seq} holds an authority role`);
  } catch (e) { bad(`market ${m}: ${e.message}`); }
}
if (MARKETS.length === 0) bad("no markets supplied — pass every live market PDA via --markets");

// 4. Insurance fund authority.
if (INSURANCE) {
  try {
    const auth = accountPubkeyField(INSURANCE, "InsuranceFundAccount", "authority");
    auth === MULTISIG ? ok(`insurance_fund authority == multisig`) : bad(`insurance_fund authority is ${auth}, expected multisig`);
  } catch (e) { bad(`insurance_fund ${INSURANCE}: ${e.message}`); }
} else {
  bad("no insurance fund supplied — pass the InsuranceFundAccount PDA via --insurance");
}

console.log();
if (fails.length === 0) {
  console.log("K-1: RESOLVED ✅ — every authority is on the multisig / immutable; sequencer holds no authority role.");
  process.exit(0);
} else {
  console.error(`K-1: FAIL ❌ — ${fails.length} check(s) failed. The single-key rug vector is NOT closed.`);
  process.exit(1);
}
