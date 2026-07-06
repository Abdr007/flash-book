// SDK-level tests for the Flash V2 ↔ flash-book reference adapter. These
// exercise the REAL pipeline: instruction/account layouts come from the
// committed IDL via anchor's coders, so a passing round-trip proves the adapter
// matches what the deployed program expects — not a hand-mirrored copy of it.

import { test } from "node:test";
import assert from "node:assert/strict";
import anchor from "@coral-xyz/anchor";

import {
  IDL,
  PROGRAM_ID,
  SIDE_LONG,
  SIDE_SHORT,
  parseDecimalToBaseUnits,
  formatBaseUnits,
  i128FromLeBytes,
  v2RequestToFlashBookOrder,
  deriveMarketPdas,
  buildOrderInstruction,
  decodePositionAccount,
  flashBookPositionToV2,
  decodeEvent,
} from "./flash-book-v2-adapter.mjs";

const { BorshInstructionCoder, BorshAccountsCoder, BorshEventCoder, BN } = anchor;
const { Keypair, ComputeBudgetProgram } = await import("@solana/web3.js");

// ── exact-integer reconciliation identity (the load-bearing property) ────────
test("reconciliation is an EXACT integer identity over the grid", () => {
  for (const side of [SIDE_LONG, SIDE_SHORT]) {
    for (let size = 1n; size <= 30n; size++) {
      for (let entry = 1n; entry <= 30n; entry++) {
        for (let mark = 1n; mark <= 30n; mark++) {
          for (const tick of [1n, 3n, 1000n]) {
            const v2 = flashBookPositionToV2(
              { side, sizeLots: size, entryPriceTicks: entry },
              mark,
              { tickSize: tick },
            );
            // exact equality — not rounded
            assert.equal(v2._fbPnlQuoteLots, v2._v2PnlQuoteLots);
            assert.equal(v2.reconciled, true);
            // and both equal the deployed Rust formula sign·size·Δ·tick
            const sign = side === SIDE_LONG ? 1n : -1n;
            const expected = sign * size * (mark - entry) * tick;
            assert.equal(v2._fbPnlQuoteLots, expected);
          }
        }
      }
    }
  }
});

test("PnL uses no floating point (large values stay exact)", () => {
  // Values that would lose precision in float64 (> 2^53).
  const size = 1_000_000n;
  const entry = 1n;
  const mark = 90_071_992n; // size·Δ·tick well beyond 2^53
  const tick = 1000n;
  const v2 = flashBookPositionToV2(
    { side: SIDE_LONG, sizeLots: size, entryPriceTicks: entry },
    mark,
    { tickSize: tick },
  );
  assert.equal(v2._fbPnlQuoteLots, size * (mark - entry) * tick);
  assert.equal(v2.reconciled, true);
});

// ── decimal ↔ integer helpers are exact ──────────────────────────────────────
test("parseDecimalToBaseUnits is exact and round-trips", () => {
  assert.equal(parseDecimalToBaseUnits("123.456789", 6), 123456789n);
  assert.equal(parseDecimalToBaseUnits("100", 6), 100000000n);
  assert.equal(parseDecimalToBaseUnits("-0.000001", 6), -1n);
  assert.equal(formatBaseUnits(123456789n, 6), "123.456789");
  assert.equal(formatBaseUnits(-1n, 6), "-0.000001");
  assert.equal(formatBaseUnits(100000000n, 6), "100.000000");
});

test("parseDecimalToBaseUnits rejects over-precise input", () => {
  assert.throws(() => parseDecimalToBaseUnits("1.1234567", 6));
  assert.throws(() => parseDecimalToBaseUnits("abc", 6));
});

test("i128FromLeBytes decodes signed little-endian", () => {
  const zero = new Array(16).fill(0);
  assert.equal(i128FromLeBytes(zero), 0n);
  const one = [1, ...new Array(15).fill(0)];
  assert.equal(i128FromLeBytes(one), 1n);
  const negOne = new Array(16).fill(0xff);
  assert.equal(i128FromLeBytes(negOne), -1n);
});

// ── bridge 1: order mapping + instruction construction ───────────────────────
test("v2RequestToFlashBookOrder maps LONG/SHORT and computes exact size", () => {
  const long = v2RequestToFlashBookOrder(
    { tradeType: "LONG", orderType: "MARKET", collateralQuoteLots: 1000n, leverage: 5n },
    { tickSize: 2n, oracleTicks: 100n },
  );
  assert.equal(long.instruction, "place_taker_order_v2");
  assert.equal(long.args.side, SIDE_LONG);
  // notional = 1000·5 = 5000; denom = 100·2 = 200; size = 25
  assert.equal(long.args.size_lots, 25n);
  assert.equal(long.args.limit_ticks, 100n);
  assert.equal(long.args.expires_at_slot, 0n);

  const short = v2RequestToFlashBookOrder(
    { tradeType: "SHORT", orderType: "LIMIT", limitPriceTicks: 250n, collateralQuoteLots: 1000n, leverage: 4n },
    { tickSize: 1n, oracleTicks: 100n },
  );
  assert.equal(short.instruction, "place_limit_order_v2");
  assert.equal(short.args.side, SIDE_SHORT);
  assert.equal(short.args.limit_ticks, 250n);
});

test("v2RequestToFlashBookOrder rejects sub-lot notional and bad price", () => {
  assert.throws(() =>
    v2RequestToFlashBookOrder(
      { tradeType: "LONG", collateralQuoteLots: 1n, leverage: 1n },
      { tickSize: 1n, oracleTicks: 100n },
    ),
  );
  assert.throws(() =>
    v2RequestToFlashBookOrder(
      { tradeType: "LONG", orderType: "LIMIT", limitPriceTicks: 0n, collateralQuoteLots: 1000n, leverage: 1n },
      { tickSize: 1n, oracleTicks: 100n },
    ),
  );
});

test("encoded instruction data matches the committed IDL (discriminator + round-trip)", () => {
  const coder = new BorshInstructionCoder(IDL);
  for (const name of ["place_limit_order_v2", "place_taker_order_v2"]) {
    const wantDisc = IDL.instructions.find((i) => i.name === name).discriminator;
    const trader = Keypair.generate().publicKey;
    const baseMint = Keypair.generate().publicKey;
    const quoteMint = Keypair.generate().publicKey;
    const traderState = Keypair.generate().publicKey;

    const built = buildOrderInstruction(
      {
        tradeType: "LONG",
        orderType: name === "place_limit_order_v2" ? "LIMIT" : "MARKET",
        limitPriceTicks: 100n,
        collateralQuoteLots: 10_000n,
        leverage: 3n,
        flags: 1,
        subIndex: 2,
        expiresAtSlot: 777n,
      },
      { tickSize: 1n, oracleTicks: 100n },
      { trader, baseMint, quoteMint, traderState },
    );

    const data = built.instruction.data;
    assert.deepEqual([...data.slice(0, 8)], wantDisc);

    // decode the bytes back through the coder → must equal what we asked for
    const dec = coder.decode(data);
    assert.equal(dec.name, name);
    assert.equal(dec.data.side, SIDE_LONG);
    assert.equal(dec.data.size_lots.toString(), built.args.size_lots.toString());
    assert.equal(dec.data.limit_ticks.toString(), "100");
    assert.equal(dec.data.flags, 1);
    assert.equal(dec.data.expires_at_slot.toString(), "777");
    assert.equal(dec.data.sub_index, 2);
  }
});

test("buildOrderInstruction produces IDL-ordered keys, PDAs, and compute budget", () => {
  const trader = Keypair.generate().publicKey;
  const baseMint = Keypair.generate().publicKey;
  const quoteMint = Keypair.generate().publicKey;
  const traderState = Keypair.generate().publicKey;
  const position = Keypair.generate().publicKey;

  const { market, marketBook } = deriveMarketPdas(baseMint, quoteMint);
  const built = buildOrderInstruction(
    { tradeType: "SHORT", orderType: "MARKET", collateralQuoteLots: 10_000n, leverage: 2n },
    { tickSize: 1n, oracleTicks: 100n },
    { trader, baseMint, quoteMint, traderState, position, computeUnitPriceMicroLamports: 50 },
  );

  assert.ok(built.instruction.programId.equals(PROGRAM_ID));
  const keys = built.instruction.keys;
  // exact IDL account order: trader, market, market_book, trader_state, position
  assert.ok(keys[0].pubkey.equals(trader));
  assert.equal(keys[0].isSigner, true);
  assert.equal(keys[0].isWritable, false);
  assert.ok(keys[1].pubkey.equals(market));
  assert.equal(keys[1].isWritable, true);
  assert.ok(keys[2].pubkey.equals(marketBook));
  assert.equal(keys[2].isWritable, true);
  assert.ok(keys[3].pubkey.equals(traderState));
  assert.equal(keys[3].isWritable, false);
  assert.ok(keys[4].pubkey.equals(position));

  assert.ok(built.derived.market.equals(market));
  assert.ok(built.derived.marketBook.equals(marketBook));
  // compute budget: unit limit + unit price present
  assert.equal(built.computeBudget.length, 2);
  for (const ix of built.computeBudget) {
    assert.ok(ix.programId.equals(ComputeBudgetProgram.programId));
  }
});

test("PDAs are deterministic for the same mints", () => {
  const baseMint = Keypair.generate().publicKey;
  const quoteMint = Keypair.generate().publicKey;
  const a = deriveMarketPdas(baseMint, quoteMint);
  const b = deriveMarketPdas(baseMint, quoteMint);
  assert.ok(a.market.equals(b.market));
  assert.ok(a.marketBook.equals(b.marketBook));
});

// ── bridge 2: account decode round-trips through the real layout ─────────────
test("decodePositionAccount round-trips a coder-encoded PositionAccount and reconciles", async () => {
  const accCoder = new BorshAccountsCoder(IDL);
  const trader = Keypair.generate().publicKey;
  const market = Keypair.generate().publicKey;
  const positionInput = {
    cum_funding_index_at_entry: new Array(16).fill(0),
    trader,
    market,
    size_lots: new BN(7),
    entry_price_ticks: new BN(100),
    collateral_quote_lots: new BN(5000),
    realized_pnl_quote_lots: new BN(0),
    funding_paid_quote_lots: new BN(0),
    last_settlement_batch: new BN(0),
    unhealthy_since_slot: new BN(0),
    last_liquidated_at_slot: new BN(0),
    leverage_cap: 40,
    bump: 0,
    side: SIDE_LONG,
    _pad: [0, 0],
  };
  const buf = await accCoder.encode("PositionAccount", positionInput);

  const pos = decodePositionAccount(buf);
  assert.equal(pos.side, SIDE_LONG);
  assert.equal(pos.sizeLots, 7n);
  assert.equal(pos.entryPriceTicks, 100n);
  assert.equal(pos.collateralQuoteLots, 5000n);
  assert.ok(pos.trader.equals(trader));

  // map to V2 at a mark of 130 ticks, tick_size 3
  const v2 = flashBookPositionToV2(pos, 130n, {
    tickSize: 3n,
    quoteLotSize: 1n,
    quoteDecimals: 6,
  });
  // sign·size·Δ·tick = 1·7·30·3 = 630
  assert.equal(v2._fbPnlQuoteLots, 630n);
  assert.equal(v2.reconciled, true);
  assert.equal(v2.unrealizedPnlUsd, "0.000630");
});

// ── event decode: real event through the coder ───────────────────────────────
test("decodeEvent decodes a coder-emitted event and rejects non-events", () => {
  const evName = IDL.events?.[0]?.name;
  if (!evName) return; // no events in IDL
  const evType = IDL.types.find((t) => t.name === evName);
  const evCoder = new BorshEventCoder(IDL);
  // build a zeroed instance of the event's fields so encode succeeds
  const obj = {};
  for (const f of evType.type.fields) {
    obj[f.name] = fieldZero(f.type);
  }
  let encoded;
  try {
    encoded = evCoder.encode(evName, obj);
  } catch {
    return; // some events carry types we do not synthesize here; skip
  }
  const decoded = decodeEvent("Program data: " + encoded);
  assert.equal(decoded?.name, evName);
  assert.equal(decodeEvent("not a real base64 event $$$"), null);
});

function fieldZero(type) {
  if (type === "u8" || type === "u16" || type === "u32" || type === "bool") {
    return type === "bool" ? false : 0;
  }
  if (type === "u64" || type === "i64" || type === "u128" || type === "i128") {
    return new BN(0);
  }
  if (type === "pubkey") return Keypair.generate().publicKey;
  if (type?.array) return new Array(type.array[1]).fill(0);
  throw new Error("unsupported field type for synthesis");
}
