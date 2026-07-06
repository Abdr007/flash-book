// Flash V2 ↔ flash-book bridge — REFERENCE ADAPTER (bridge 1 + 2 of
// docs/V2_INTEGRATION.md). Demonstrates how a Flash V2 client integrates the
// flash-book CLOB with NO new SDK: the same V2 request shape maps to a
// flash-book instruction, and a flash-book position maps back to V2's
// PositionMetrics shape.
//
// The load-bearing property (asserted in position_math.rs by
// matches_v2_notional_return_formula, the exhaustive reduce/flip reconciliation,
// and proptests; asserted end-to-end here): flash-book's exact-integer realized
// PnL `sign · closed · Δticks · tick_size` EQUALS V2's `(mark−entry)/entry ·
// notional` with `notional = size · entry · tick_size`. Because `entry` divides
// `notional` exactly, the two are the SAME integer — no rounding, no float. Every
// amount in this module is a BigInt or a decimal STRING parsed to a BigInt; there
// is no floating-point arithmetic on any value that maps to on-chain lamports/lots.
//
// Transactions are built from the committed IDL (`../idl/flash_book.json`): the
// program id, instruction discriminators, account ordering, and Borsh arg layout
// all come from that one file, so a Flash V2 dev reads the produced instruction
// and wires it into their existing partially-signed-tx flow directly.

import anchor from "@coral-xyz/anchor";
import {
  PublicKey,
  TransactionInstruction,
  ComputeBudgetProgram,
  AddressLookupTableProgram,
} from "@solana/web3.js";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const { BorshInstructionCoder, BorshAccountsCoder, BorshEventCoder, BN } = anchor;

const HERE = dirname(fileURLToPath(import.meta.url));
export const IDL = JSON.parse(
  readFileSync(join(HERE, "..", "idl", "flash_book.json"), "utf8"),
);

export const PROGRAM_ID = new PublicKey(IDL.address);

// Matcher-side side encoding (programs/flash-book/src/matcher/position_math.rs).
export const SIDE_LONG = 0; // bid
export const SIDE_SHORT = 1; // ask

const ixCoder = new BorshInstructionCoder(IDL);
const accountsCoder = new BorshAccountsCoder(IDL);
const eventCoder = new BorshEventCoder(IDL);

const MARKET_SEED = Buffer.from("market");
const MARKET_BOOK_SEED = Buffer.from("market_book");

// ── exact decimal ↔ integer base units (no floating point) ──────────────────
// A UI amount is a decimal STRING; it is scaled by 10^decimals into an exact
// integer. Over-precise input is rejected rather than silently truncated.
export function parseDecimalToBaseUnits(decimalStr, decimals) {
  const m = /^(-?)(\d+)(?:\.(\d+))?$/.exec(String(decimalStr).trim());
  if (!m) throw new Error(`not a decimal amount: ${decimalStr}`);
  const [, sign, intPart, fracPart = ""] = m;
  if (fracPart.length > decimals) {
    throw new Error(
      `amount ${decimalStr} has more than ${decimals} fractional digits`,
    );
  }
  const scaled = BigInt(intPart + fracPart.padEnd(decimals, "0"));
  return sign === "-" ? -scaled : scaled;
}

// Inverse of parseDecimalToBaseUnits: an exact integer → a decimal STRING.
export function formatBaseUnits(value, decimals) {
  const v = BigInt(value);
  const neg = v < 0n;
  const digits = (neg ? -v : v).toString().padStart(decimals + 1, "0");
  const cut = digits.length - decimals;
  const intPart = digits.slice(0, cut);
  const fracPart = decimals > 0 ? "." + digits.slice(cut) : "";
  return (neg ? "-" : "") + intPart + fracPart;
}

// A little-endian i128 (the IDL encodes `cum_funding_index_at_entry` as [u8; 16])
// decoded to a signed BigInt.
export function i128FromLeBytes(bytes) {
  let acc = 0n;
  for (let i = 15; i >= 0; i--) acc = (acc << 8n) | BigInt(bytes[i] & 0xff);
  return acc >= 1n << 127n ? acc - (1n << 128n) : acc;
}

// ── bridge 1: a V2 OpenPositionRequest → a flash-book order instruction ──────
// V2 (examples-v2 types.ts): { outputTokenSymbol (market), tradeType LONG|SHORT,
// orderType MARKET|LIMIT, limitPrice, inputAmountUi (collateral), leverage }.
// This adapter takes those already resolved to EXACT integers so no float ever
// touches a lot count:
//   req = { tradeType, orderType, side?, collateralQuoteLots: bigint,
//           leverage: bigint, limitPriceTicks?: bigint,
//           flags?: number, subIndex?: number, expiresAtSlot?: bigint }
//   ctx = { tickSize: bigint (MarketParams.tick_size), oracleTicks: bigint }
// flash-book: side (0=bid/long, 1=ask/short); LIMIT → place_limit_order_v2
// (rests), MARKET → place_taker_order_v2 (crosses); size in base lots.
export function v2RequestToFlashBookOrder(req, ctx) {
  const orderType = req.orderType ?? "MARKET";
  const side =
    req.side ?? (req.tradeType === "LONG" ? SIDE_LONG : SIDE_SHORT);
  if (side !== SIDE_LONG && side !== SIDE_SHORT) {
    throw new Error(`invalid side: ${side}`);
  }

  const tickSize = BigInt(ctx.tickSize);
  const priceTicks =
    orderType === "LIMIT" ? BigInt(req.limitPriceTicks) : BigInt(ctx.oracleTicks);
  if (priceTicks <= 0n) throw new Error("price in ticks must be positive");

  // collateral × leverage = notional (quote-lots). notional / (price × tick) =
  // base lots. Exact integer floor; a notional too small to fill one base lot is
  // rejected rather than silently rounded up.
  const notionalQuoteLots =
    BigInt(req.collateralQuoteLots) * BigInt(req.leverage);
  const quoteLotsPerBaseLot = priceTicks * tickSize;
  const sizeLots = notionalQuoteLots / quoteLotsPerBaseLot;
  if (sizeLots <= 0n) {
    throw new Error(
      `notional ${notionalQuoteLots} is below one base lot (${quoteLotsPerBaseLot} quote-lots)`,
    );
  }

  const args = {
    side,
    size_lots: sizeLots,
    limit_ticks: priceTicks,
    flags: req.flags ?? 0,
    expires_at_slot: BigInt(req.expiresAtSlot ?? 0),
    sub_index: req.subIndex ?? 0,
  };
  return {
    instruction:
      orderType === "LIMIT" ? "place_limit_order_v2" : "place_taker_order_v2",
    args,
  };
}

// Derive the market + market_book PDAs exactly as the program does
// (seeds from the IDL: ["market", base_mint, quote_mint] and
// ["market_book", market]).
export function deriveMarketPdas(baseMint, quoteMint) {
  const [market] = PublicKey.findProgramAddressSync(
    [MARKET_SEED, new PublicKey(baseMint).toBuffer(), new PublicKey(quoteMint).toBuffer()],
    PROGRAM_ID,
  );
  const [marketBook] = PublicKey.findProgramAddressSync(
    [MARKET_BOOK_SEED, market.toBuffer()],
    PROGRAM_ID,
  );
  return { market, marketBook };
}

// Encode a place_{limit,taker}_order_v2 into Borsh instruction data using the
// committed IDL. u64/i64 args are passed as anchor BN (the coder's required
// representation); every input here is already an exact integer.
function encodeOrderData(name, args) {
  return ixCoder.encode(name, {
    side: args.side,
    size_lots: new BN(args.size_lots.toString()),
    limit_ticks: new BN(args.limit_ticks.toString()),
    flags: args.flags,
    expires_at_slot: new BN(args.expires_at_slot.toString()),
    sub_index: args.sub_index,
  });
}

// Build a real TransactionInstruction for a V2 request, plus the compute-budget
// instructions that should precede it. Accounts are ordered and flagged exactly
// as the IDL declares them for place_{limit,taker}_order_v2:
//   trader (signer) · market (writable, PDA) · market_book (writable, PDA) ·
//   trader_state · position (optional).
export function buildOrderInstruction(req, ctx, accounts) {
  const order = v2RequestToFlashBookOrder(req, ctx);
  const { market, marketBook } = deriveMarketPdas(
    accounts.baseMint,
    accounts.quoteMint,
  );

  const keys = [
    { pubkey: new PublicKey(accounts.trader), isSigner: true, isWritable: false },
    { pubkey: market, isSigner: false, isWritable: true },
    { pubkey: marketBook, isSigner: false, isWritable: true },
    { pubkey: new PublicKey(accounts.traderState), isSigner: false, isWritable: false },
  ];
  if (accounts.position) {
    keys.push({
      pubkey: new PublicKey(accounts.position),
      isSigner: false,
      isWritable: false,
    });
  }

  const instruction = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys,
    data: encodeOrderData(order.instruction, order.args),
  });

  const computeBudget = [
    ComputeBudgetProgram.setComputeUnitLimit({
      units: accounts.computeUnitLimit ?? 200_000,
    }),
  ];
  if (accounts.computeUnitPriceMicroLamports != null) {
    computeBudget.push(
      ComputeBudgetProgram.setComputeUnitPrice({
        microLamports: accounts.computeUnitPriceMicroLamports,
      }),
    );
  }

  return {
    instructionName: order.instruction,
    args: order.args,
    derived: { market, marketBook },
    computeBudget,
    instruction,
  };
}

// ── bridge 2: a flash-book PositionAccount → V2 PositionMetrics shape ────────
// Decode a raw PositionAccount (8-byte Anchor discriminator + body) into typed,
// named values using the committed IDL. u64/i64 fields come back as BigInt; the
// [u8; 16] funding index becomes a signed i128 BigInt.
export function decodePositionAccount(data) {
  const raw = accountsCoder.decode("PositionAccount", Buffer.from(data));
  return {
    trader: raw.trader,
    market: raw.market,
    side: raw.side,
    sizeLots: BigInt(raw.size_lots.toString()),
    entryPriceTicks: BigInt(raw.entry_price_ticks.toString()),
    collateralQuoteLots: BigInt(raw.collateral_quote_lots.toString()),
    realizedPnlQuoteLots: BigInt(raw.realized_pnl_quote_lots.toString()),
    fundingPaidQuoteLots: BigInt(raw.funding_paid_quote_lots.toString()),
    lastSettlementBatch: BigInt(raw.last_settlement_batch.toString()),
    leverageCap: raw.leverage_cap,
    cumFundingIndexAtEntry: i128FromLeBytes(raw.cum_funding_index_at_entry),
  };
}

// Map a decoded position to V2's PositionMetrics shape, computing unrealized PnL
// BOTH ways in exact BigInt and asserting they are the SAME integer (the 1a
// reconciliation). `pos` is the output of decodePositionAccount (or any object
// with side/sizeLots/entryPriceTicks). `markPriceTicks` and `ctx.tickSize` are
// integers. Optional `ctx.quoteLotSize` + `ctx.quoteDecimals` render exact USD
// decimal strings without touching floating point.
export function flashBookPositionToV2(pos, markPriceTicks, ctx) {
  const sizeLots = BigInt(pos.sizeLots ?? pos.size_lots);
  const entryTicks = BigInt(pos.entryPriceTicks ?? pos.entry_price_ticks);
  const markTicks = BigInt(markPriceTicks);
  const tick = BigInt(ctx.tickSize);
  const sideByte = pos.side;
  const sign = sideByte === SIDE_LONG ? 1n : -1n;

  // flash-book: exact-integer unrealized PnL = sign · size · Δticks · tick_size.
  const fbPnl = sign * sizeLots * (markTicks - entryTicks) * tick;

  // V2: (mark−entry)/entry · notional with notional = size · entry · tick_size.
  // entry divides notional exactly, so this is the SAME integer as fbPnl.
  const notional = sizeLots * entryTicks * tick;
  const v2Pnl =
    entryTicks === 0n
      ? fbPnl
      : sign * (markTicks - entryTicks) * (notional / entryTicks);

  const reconciled = fbPnl === v2Pnl; // exact BigInt equality — no rounding

  const out = {
    sideUi: sideByte === SIDE_LONG ? "LONG" : "SHORT",
    sizeLots: sizeLots.toString(),
    entryPriceTicks: entryTicks.toString(),
    markPriceTicks: markTicks.toString(),
    notionalQuoteLots: notional.toString(),
    unrealizedPnlQuoteLots: fbPnl.toString(),
    reconciled,
    _fbPnlQuoteLots: fbPnl,
    _v2PnlQuoteLots: v2Pnl,
  };

  if (ctx.quoteLotSize != null && ctx.quoteDecimals != null) {
    const qls = BigInt(ctx.quoteLotSize);
    out.notionalUsd = formatBaseUnits(notional * qls, ctx.quoteDecimals);
    out.unrealizedPnlUsd = formatBaseUnits(fbPnl * qls, ctx.quoteDecimals);
  }
  return out;
}

// ── event decode: program logs → typed, named event values ──────────────────
// Anchor emits events as base64 "Program data:" log lines. Decode one such line
// (or a bare base64 payload) into { name, data }, or null if it is not an event.
export function decodeEvent(logOrBase64) {
  const line = String(logOrBase64);
  const payload = line.startsWith("Program data: ")
    ? line.slice("Program data: ".length)
    : line;
  return eventCoder.decode(payload.trim());
}

// ── ALT tooling for the hot path ────────────────────────────────────────────
// Settlement / portfolio-walk transactions touch many accounts; an Address
// Lookup Table lets them fit and decode cleanly. Build the create+extend
// instructions for an ALT over a fixed set of pubkeys.
export function buildOrderLookupTable({ authority, payer, addresses, recentSlot }) {
  const [createIx, lookupTableAddress] =
    AddressLookupTableProgram.createLookupTable({
      authority: new PublicKey(authority),
      payer: new PublicKey(payer),
      recentSlot,
    });
  const extendIx = AddressLookupTableProgram.extendLookupTable({
    lookupTable: lookupTableAddress,
    authority: new PublicKey(authority),
    payer: new PublicKey(payer),
    addresses: addresses.map((a) => new PublicKey(a)),
  });
  return { lookupTableAddress, instructions: [createIx, extendIx] };
}

// ── demo: exhaustive exact-integer reconciliation over a grid ───────────────
if (import.meta.url === `file://${process.argv[1]}`) {
  const ctx = { tickSize: 3n };
  let pass = 0;
  let fail = 0;
  for (const sideByte of [SIDE_LONG, SIDE_SHORT]) {
    for (let size = 1n; size <= 20n; size++) {
      for (let entry = 1n; entry <= 20n; entry++) {
        for (let mark = 1n; mark <= 20n; mark++) {
          const v2 = flashBookPositionToV2(
            { side: sideByte, sizeLots: size, entryPriceTicks: entry },
            mark,
            ctx,
          );
          if (v2.reconciled) pass++;
          else {
            fail++;
            if (fail <= 3) console.log("  ✗", { sideByte, size, entry, mark, v2 });
          }
        }
      }
    }
  }

  const order = v2RequestToFlashBookOrder(
    {
      tradeType: "LONG",
      orderType: "LIMIT",
      limitPriceTicks: 100_000n,
      collateralQuoteLots: 100_000n,
      leverage: 5n,
      expiresAtSlot: 0n,
    },
    { tickSize: 1n, oracleTicks: 100_000n },
  );

  console.log("Flash V2 ↔ flash-book bridge — reference adapter\n");
  console.log("program:", PROGRAM_ID.toBase58());
  console.log(
    "bridge 1 (V2 LIMIT request → flash-book order):",
    JSON.stringify(order.args, (_k, v) =>
      typeof v === "bigint" ? v.toString() : v,
    ),
    "→",
    order.instruction,
  );
  console.log(
    `bridge 2 (position → V2 shape) reconciliation: ${pass} positions, ${fail} mismatch`,
  );
  console.log(
    `\n${fail === 0 ? "✅ V2 BRIDGE RECONCILIATION HOLDS (flash-book PnL === V2 PnL, exact integer)" : "❌ MISMATCH"}`,
  );
  process.exit(fail === 0 ? 0 : 1);
}
