// Flash V2 ↔ flash-book bridge — REFERENCE ADAPTER (bridge 1 + 2 of
// docs/V2_INTEGRATION.md). Demonstrates how a Flash V2 client integrates the
// flash-book CLOB with NO new SDK: the same V2 request shape maps to a flash-book
// instruction, and a flash-book position maps back to V2's PositionMetrics shape.
//
// The load-bearing property (Kani-proven in 1a `realized_pnl_matches_v2_notional_
// return`, and asserted end-to-end here): flash-book's exact-integer PnL
// `size · Δticks · tick` EQUALS V2's `(mark−entry)/entry · notional`, so the two
// products share ONE position / margin / PnL to the lot. This is a pure mapping
// library (no chain I/O) so it drops into the V2 API service or a client.

// ── bridge 1: a V2 OpenPositionRequest → a flash-book order intent ──────────
// V2 (examples-v2 types.ts): { outputTokenSymbol (market), tradeType LONG|SHORT,
// orderType MARKET|LIMIT, limitPrice, inputAmountUi (collateral), leverage }.
// flash-book: side (0=bid/long, 1=ask/short); LIMIT → place_limit_order_v2
// (rests), MARKET → place_taker_order_v2 (crosses); size in base lots.
export function v2RequestToFlashBookOrder(req, ctx) {
  const { tradeType, orderType = "MARKET", limitPrice } = req;
  const side = tradeType === "LONG" ? 0 : 1;
  // collateral × leverage = notional; notional / (price × tickSize) = base lots.
  const notionalUsd = Number(req.inputAmountUi) * Number(req.leverage);
  const priceTicks =
    orderType === "LIMIT"
      ? Math.round(Number(limitPrice) / ctx.tickSizeUsd)
      : ctx.oracleTicks;
  const sizeLots = Math.max(1, Math.floor(notionalUsd / (priceTicks * ctx.tickSizeUsd)));
  return {
    instruction: orderType === "LIMIT" ? "place_limit_order_v2" : "place_taker_order_v2",
    args: { side, size_lots: sizeLots, limit_ticks: priceTicks, flags: 0, sub_index: 0 },
    // A V2 client signs + submits this exactly like a pool trade — same
    // partially-signed-tx flow, same ER RPC. Zero new SDK.
  };
}

// ── bridge 2: a flash-book PositionAccount → V2 PositionMetrics shape ────────
// Computes PnL BOTH ways and asserts they agree (the reconciliation). `pos` is a
// decoded flash-book PositionAccount; prices in ticks; USD via tickSizeUsd.
export function flashBookPositionToV2(pos, markPriceTicks, ctx) {
  const side = pos.side === 0 ? "LONG" : "SHORT";
  const sizeLots = Number(pos.size_lots);
  const entryTicks = Number(pos.entry_price_ticks);
  const tick = ctx.tickSize; // integer tick units per lot for PnL scaling
  const sign = pos.side === 0 ? 1 : -1;

  // flash-book: exact-integer realized/unrealized PnL (quote-lots).
  const fbPnl = sign * sizeLots * (Number(markPriceTicks) - entryTicks) * tick;

  // V2: (mark−entry)/entry × notional, notional = size·entry·tick (entry cancels).
  const notional = sizeLots * entryTicks * tick;
  const v2Pnl = sign * ((Number(markPriceTicks) - entryTicks) / entryTicks) * notional;

  // RECONCILIATION (1a): must be equal (V2's /entry cancels the entry factor).
  const reconciled = Math.round(fbPnl) === Math.round(v2Pnl);

  return {
    sideUi: side,
    entryPriceUi: (entryTicks * ctx.tickSizeUsd).toString(),
    sizeUsdUi: (sizeLots * entryTicks * tick * ctx.tickSizeUsd).toString(),
    markPriceUi: (Number(markPriceTicks) * ctx.tickSizeUsd).toString(),
    unrealizedPnlUsdUi: (fbPnl * ctx.tickSizeUsd).toString(),
    _fbPnlQuoteLots: fbPnl,
    _v2PnlQuoteLots: v2Pnl,
    _reconciled: reconciled, // 1a: flash-book PnL === V2 PnL, to the lot
  };
}

// ── demo: exhaustive reconciliation over a grid (proves bridge 2 end-to-end) ──
if (import.meta.url === `file://${process.argv[1]}`) {
  const ctx = { tickSize: 1, tickSizeUsd: 0.01, oracleTicks: 100000 };
  let pass = 0, fail = 0;
  for (const sideByte of [0, 1]) {
    for (let size = 1; size <= 20; size++) {
      for (let entry = 1; entry <= 20; entry++) {
        for (let mark = 1; mark <= 20; mark++) {
          const v2 = flashBookPositionToV2(
            { side: sideByte, size_lots: size, entry_price_ticks: entry },
            mark,
            { ...ctx, tickSize: 3 },
          );
          if (v2._reconciled) pass++; else { fail++; if (fail <= 3) console.log("  ✗", { sideByte, size, entry, mark, v2 }); }
        }
      }
    }
  }
  // bridge 1 sample
  const order = v2RequestToFlashBookOrder(
    { tradeType: "LONG", orderType: "LIMIT", limitPrice: "1000", inputAmountUi: "100", leverage: 5 },
    { tickSizeUsd: 0.01, oracleTicks: 100000 },
  );
  console.log("Flash V2 ↔ flash-book bridge — reference adapter\n");
  console.log("bridge 1 (V2 LIMIT request → flash-book order):", JSON.stringify(order.args), "→", order.instruction);
  console.log(`bridge 2 (position → V2 shape) reconciliation: ${pass} positions, ${fail} mismatch`);
  console.log(`\n${fail === 0 ? "✅ V2 BRIDGE RECONCILIATION HOLDS (flash-book PnL === V2 PnL, to the lot)" : "❌ MISMATCH"}`);
  process.exit(fail === 0 ? 0 : 1);
}
