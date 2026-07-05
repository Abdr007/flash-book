// ER reserved-margin attestation cranker — the sequencer-side half of the
// withdraw-anytime model (ER_TRUST_BOUNDARY.md §1.2).
//
// Collateral is authoritative on L1 while resting orders live on the ER, so
// the margin a live ER order reserves reaches L1 only through the
// sequencer-signed `attest_er_reserved_margin`. This service closes that loop
// continuously: it reads the delegated book, computes each trader's total
// resting-order initial margin, and attests any change on L1 — bounding the
// documented attestation-lag window to roughly one poll interval.
//
// Reservation policy (v2): a trader's reservation is the sum of
//     im = ceil(size_lots × price_ticks × tick_size × initial_margin_ratio_bps / 10_000)
// over (a) every RESTING order on the book, and (b) every UNSETTLED fill —
// outbox rows at absolute indices [ring.settled, ring.produced), reserved for
// BOTH the taker and the maker (an FLP virtual-quote fill, maker ==
// Pubkey::default(), reserves only the taker side). A fill therefore keeps its
// margin reserved seamlessly from the moment the order rests until apply_fill
// settles it into a position the filled-position gate covers — the
// order-fills-then-withdraw-before-settlement window is closed.
//
// Book, ring, and outbox are snapshotted in ONE getMultipleAccountsInfo call
// per market, so an in-flight fill cannot fall between two reads for more
// than a single poll interval.
//
// Usage:
//   L1_RPC=<l1> ER_RPC=<er> MARKETS=<mkt1,mkt2,…> node attestation_cranker.mjs
//   optional: KEYPAIR=~/.config/solana/id.json  INTERVAL_MS=2000  ONCE=1
//
// The keypair must be the pinned attestor of every trader's
// ErMarginAttestation it maintains (init_er_margin_attestation pins it).
import fs from "fs";
import os from "os";
import anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, Transaction, ComputeBudgetProgram, sendAndConfirmTransaction } from "@solana/web3.js";

const { AnchorProvider, Program, Wallet, BN } = anchor;

// ── config ────────────────────────────────────────────────────────────────────
const L1_RPC = process.env.L1_RPC || "https://api.devnet.solana.com";
const ER_RPC = process.env.ER_RPC; // optional: without it, books are read from L1 only
const MARKETS = (process.env.MARKETS || "").split(",").map((s) => s.trim()).filter(Boolean);
const INTERVAL_MS = Number(process.env.INTERVAL_MS || 2000);
const ONCE = !!process.env.ONCE; // single pass (for tests / cron-style runs)
if (MARKETS.length === 0) {
  console.error("MARKETS=<comma-separated market pubkeys> is required");
  process.exit(1);
}

const IDL = JSON.parse(fs.readFileSync(new URL("../idl/flash_book.json", import.meta.url)));
const PID = new PublicKey(IDL.address);
const keypairPath = process.env.KEYPAIR || `${os.homedir()}/.config/solana/id.json`;
const attestor = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(keypairPath))));
const l1 = new Connection(L1_RPC, "confirmed");
const er = ER_RPC ? new Connection(ER_RPC, "confirmed") : null;
const program = new Program(IDL, new AnchorProvider(l1, new Wallet(attestor), { commitment: "confirmed" }));
const pda = (seeds) =>
  PublicKey.findProgramAddressSync(
    seeds.map((x) => (Buffer.isBuffer(x) ? x : typeof x === "string" ? Buffer.from(x) : x.toBuffer())),
    PID,
  )[0];
const traderStatePda = (trader, subIndex) =>
  subIndex === 0
    ? pda(["trader_state", trader])
    : pda(["trader_state", trader, Buffer.from([subIndex])]);
const log = (...a) => console.log(new Date().toISOString(), ...a);

// ── hypertree book decoding ───────────────────────────────────────────────────
// Account layout: [8B disc][256B header][N × 96B RBNode slab].
// Header (offsets within the 256B header): bids_root u32 @104, asks_root @112.
// RBNode (offsets within a 96B node): left u32 @0, right u32 @4, then the
// 80B RestingOrderV2 payload @16: price_ticks u64 @32-16, size_lots u64 @40-16,
// trader Pubkey @56-16, side u8 @92-16, sub_index u8 @95-16 (node-relative:
// price @32, size @40, trader @56, side @92, sub_index @95).
const MARKET_BOOK_DISC = Buffer.from([0xfb, 0xba, 0x00, 0x4b, 0x4d, 0x4b, 0x42, 0x01]);
const HEADER_OFF = 8;
const SLAB_OFF = 8 + 256;
const NODE_BYTES = 96;

// Walk one RBT (indices are BYTE offsets into the slab; anything that is not a
// node-aligned in-bounds offset — including the NIL sentinel — ends a branch).
function walkTree(data, rootIndex, visit) {
  const slabLen = data.length - SLAB_OFF;
  const valid = (idx) => idx % NODE_BYTES === 0 && idx + NODE_BYTES <= slabLen;
  if (!valid(rootIndex)) return;
  const stack = [rootIndex];
  const seen = new Set();
  while (stack.length) {
    const idx = stack.pop();
    if (seen.has(idx)) continue; // fail-safe against a corrupt/cyclic commit
    seen.add(idx);
    const off = SLAB_OFF + idx;
    visit(off);
    for (const child of [data.readUInt32LE(off), data.readUInt32LE(off + 4)]) {
      if (valid(child)) stack.push(child);
    }
  }
}

// Decode every live resting order on both sides of a book account.
function decodeRestingOrders(data) {
  if (!data.subarray(0, 8).equals(MARKET_BOOK_DISC)) throw new Error("not a MarketBookAccount");
  const orders = [];
  const visit = (off) => {
    orders.push({
      priceTicks: data.readBigUInt64LE(off + 32),
      sizeLots: data.readBigUInt64LE(off + 40),
      trader: new PublicKey(data.subarray(off + 56, off + 88)),
      side: data.readUInt8(off + 92),
      subIndex: data.readUInt8(off + 95),
    });
  };
  walkTree(data, data.readUInt32LE(HEADER_OFF + 104), visit); // bids
  walkTree(data, data.readUInt32LE(HEADER_OFF + 112), visit); // asks
  return orders;
}

// ── unsettled-fill decoding (fill outbox addressed by the ring's cursors) ────
// Ring header: produced u64 @8, settled u64 @16 — `settled` is authoritative
// (apply_fill advances it). Outbox: 64B header (cap u32 @24) + 96B slots:
// taker @0, maker @32, size u64 @64, price u64 @72, taker_side @88,
// taker_sub @89, maker_sub @90. Slot for absolute fill index i = i % cap.
const DEFAULT_PUBKEY = Buffer.alloc(32);
function decodeUnsettledFills(ringData, outboxData) {
  const produced = ringData.readBigUInt64LE(8);
  const settled = ringData.readBigUInt64LE(16);
  const cap = BigInt(outboxData.readUInt32LE(24));
  if (cap === 0n || produced < settled || produced - settled > cap) {
    throw new Error(`inconsistent ring/outbox cursors (produced=${produced} settled=${settled} cap=${cap})`);
  }
  const fills = [];
  for (let i = settled; i < produced; i++) {
    const off = 64 + Number(i % cap) * 96;
    if (off + 96 > outboxData.length) throw new Error("outbox slot out of bounds");
    fills.push({
      taker: outboxData.subarray(off, off + 32),
      maker: outboxData.subarray(off + 32, off + 64),
      sizeLots: outboxData.readBigUInt64LE(off + 64),
      priceTicks: outboxData.readBigUInt64LE(off + 72),
      takerSub: outboxData.readUInt8(off + 89),
      makerSub: outboxData.readUInt8(off + 90),
    });
  }
  return fills;
}

// ── reservation computation ───────────────────────────────────────────────────
const BPS = 10_000n;
function imFor(sizeLots, priceTicks, tickSize, imBps) {
  return (sizeLots * priceTicks * tickSize * imBps + BPS - 1n) / BPS; // ceil — rounds toward the protocol
}
function computeReservations(orders, fills, tickSize, imBps) {
  const perState = new Map(); // trader_state base58 → { pubkey, reserved: bigint }
  const add = (traderBytes, subIndex, im) => {
    const ts = traderStatePda(new PublicKey(traderBytes), subIndex);
    const key = ts.toBase58();
    const cur = perState.get(key) || { pubkey: ts, reserved: 0n };
    cur.reserved += im;
    perState.set(key, cur);
  };
  for (const o of orders) {
    if (o.sizeLots === 0n) continue;
    add(o.trader.toBuffer(), o.subIndex, imFor(o.sizeLots, o.priceTicks, tickSize, imBps));
  }
  for (const f of fills) {
    if (f.sizeLots === 0n) continue;
    const im = imFor(f.sizeLots, f.priceTicks, tickSize, imBps);
    add(f.taker, f.takerSub, im);
    if (!f.maker.equals(DEFAULT_PUBKEY)) add(f.maker, f.makerSub, im); // FLP fills reserve pool margin, not a trader's
  }
  return perState;
}

// ── attestation ───────────────────────────────────────────────────────────────
async function attest(traderState, reserved, epoch) {
  const erMargin = pda(["er_margin", traderState]);
  const ix = await program.methods
    .attestErReservedMargin(new BN(reserved.toString()), new BN(epoch.toString()))
    .accountsPartial({ attestor: attestor.publicKey, erMargin, traderState })
    .instruction();
  const tx = new Transaction();
  tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: 60_000 }));
  tx.add(ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 }));
  tx.add(ix);
  return await sendAndConfirmTransaction(l1, tx, [attestor], { commitment: "confirmed", skipPreflight: true, maxRetries: 5 });
}

// Read the (book, ring, outbox) trio in ONE call — a same-slot snapshot on
// whichever domain currently holds them (ER when delegated, else L1).
async function fetchMarketAccounts(meta) {
  const keys = [meta.book, meta.ring, meta.outbox];
  if (er) {
    try {
      const a = await er.getMultipleAccountsInfo(keys);
      if (a?.[0]?.data?.length) return a.map((x) => x?.data ?? null);
    } catch {
      /* fall through to L1 */
    }
  }
  const a = await l1.getMultipleAccountsInfo(keys);
  return a.map((x) => x?.data ?? null);
}

// ── main loop ─────────────────────────────────────────────────────────────────
const marketMeta = new Map(); // market base58 → { book, tickSize, imBps }
// Market params load lazily and retry every pass until the market exists, so
// the cranker can be pointed at a market that is created after it starts.
async function loadMarketMeta(marketPk) {
  try {
    const m = await program.account.marketAccount.fetch(marketPk);
    marketMeta.set(marketPk.toBase58(), {
      book: pda(["market_book", marketPk]),
      ring: pda(["fill_commit", marketPk]),
      outbox: pda(["fill_outbox", marketPk]),
      tickSize: BigInt(m.params.tickSize.toString()),
      imBps: BigInt(m.params.initialMarginRatioBps),
    });
    return true;
  } catch {
    return false;
  }
}

// Trader states whose last-known reservation is nonzero — the set that must be
// re-attested to zero once their orders disappear. Seeded from chain at
// startup so a cranker restart still zeroes stale reservations.
const liveReservations = new Map(); // trader_state base58 → bigint (last attested nonzero)

async function reconcileStartup() {
  const all = await program.account.erMarginAttestation.all();
  for (const { account } of all) {
    const reserved = BigInt(account.reservedMarginQuoteLots.toString());
    if (reserved > 0n) liveReservations.set(account.traderState.toBase58(), reserved);
  }
  log(`startup reconcile: ${liveReservations.size} trader_state(s) with a live on-chain reservation`);
}

async function pass() {
  // Aggregate reservations across ALL watched markets (an attestation is
  // per-trader_state, spanning that trader's orders on every market).
  const wanted = new Map(); // trader_state base58 → { pubkey, reserved }
  for (const mkt of MARKETS) {
    if (!marketMeta.has(mkt) && !(await loadMarketMeta(new PublicKey(mkt)))) continue;
    const meta = marketMeta.get(mkt);
    const [bookData, ringData, outboxData] = await fetchMarketAccounts(meta);
    if (!bookData) continue;
    let orders, fills;
    try {
      orders = decodeRestingOrders(bookData);
      // A market without a ring/outbox has no unsettled-fill surface to track.
      fills = ringData && outboxData ? decodeUnsettledFills(ringData, outboxData) : [];
    } catch (e) {
      log(`WARN market ${mkt}: ${e.message}`);
      continue;
    }
    for (const [key, v] of computeReservations(orders, fills, meta.tickSize, meta.imBps)) {
      const cur = wanted.get(key) || { pubkey: v.pubkey, reserved: 0n };
      cur.reserved += v.reserved;
      wanted.set(key, cur);
    }
  }
  // Anything previously live but no longer wanted must be attested to zero.
  for (const [key, last] of liveReservations) {
    if (!wanted.has(key) && last > 0n) wanted.set(key, { pubkey: new PublicKey(key), reserved: 0n });
  }
  for (const [key, { pubkey, reserved }] of wanted) {
    let acct;
    try {
      acct = await program.account.erMarginAttestation.fetch(pda(["er_margin", pubkey]));
    } catch {
      // No attestation account: this trader cannot have placed on a delegated
      // book (er_margin_ready gates placement), so only warn when a reserve
      // was actually computed for them.
      if (reserved > 0n) log(`WARN ${key}: resting ER orders but NO ErMarginAttestation account — cannot attest`);
      continue;
    }
    const onchain = BigInt(acct.reservedMarginQuoteLots.toString());
    if (onchain === reserved) {
      if (reserved > 0n) liveReservations.set(key, reserved);
      else liveReservations.delete(key);
      continue;
    }
    try {
      const epoch = BigInt(acct.epoch.toString()) + 1n;
      const sig = await attest(pubkey, reserved, epoch);
      log(`attested ${key}: ${onchain} → ${reserved} (epoch ${epoch}) ${sig}`);
      if (reserved > 0n) liveReservations.set(key, reserved);
      else liveReservations.delete(key);
    } catch (e) {
      log(`ERROR attesting ${key}: ${String(e.message || e).slice(0, 160)}`);
    }
  }
}

log(`attestation cranker — attestor ${attestor.publicKey.toBase58()}`);
log(`L1=${L1_RPC} ER=${ER_RPC || "(none — L1 books only)"} markets=${MARKETS.length} interval=${INTERVAL_MS}ms`);
for (const mkt of MARKETS) {
  if (!(await loadMarketMeta(new PublicKey(mkt)))) log(`market ${mkt} not found yet — will retry each pass`);
}
await reconcileStartup();
for (;;) {
  const t0 = Date.now();
  try {
    await pass();
  } catch (e) {
    log(`ERROR pass: ${String(e.message || e).slice(0, 160)}`);
  }
  if (ONCE) break;
  await new Promise((r) => setTimeout(r, Math.max(0, INTERVAL_MS - (Date.now() - t0))));
}
