# Flash V2 ↔ flash-book bridge (reference)

Reference adapter for integrating the **flash-book CLOB** into **Flash V2**'s
API-first stack — the concrete first step of `docs/V2_INTEGRATION.md`. Flash V2's
public surface is a hosted REST API (`flashapi.trade/v2`, no public SDK) over the
MagicBlock ER; flash-book runs on the *same* ER. So integration is a **mapping**,
not a re-platform.

## What this is

A **pure mapping library** (`flash-book-v2-adapter.mjs`, no chain I/O) that drops
into the V2 API service or a client:

- **Bridge 1 — API parity.** `v2RequestToFlashBookOrder()` maps a V2
  `OpenPositionRequest` (`tradeType`, `orderType`, `limitPrice`, `leverage`) to a
  flash-book order intent: `LIMIT` → `place_limit_order_v2` (rests on the book),
  `MARKET` → `place_taker_order_v2` (crosses). A V2 client submits it exactly like
  a pool trade — same partially-signed-tx flow, same ER RPC, **zero new SDK**.
- **Bridge 2 — one position/PnL model.** `flashBookPositionToV2()` maps a
  flash-book `PositionAccount` to V2's `PositionMetrics` shape.

## The load-bearing property

flash-book's exact-integer PnL `size · Δticks · tick` **equals** V2's
`(mark − entry)/entry · notional` — the `/entry` cancels the entry factor in
`notional`, so they are the *same number* (flash-book just carries no float
division). This is:

- **Kani-proven** in 1a (`realized_pnl_matches_v2_notional_return`), and
- **asserted end-to-end here** — the demo reconciles **16,000 positions across
  both sides / sizes / entries / marks with 0 mismatch.**

That's why a V2 client and flash-book share **one position, one margin, one PnL,
to the lot** — a trader never sees "two products."

```
node v2-bridge/flash-book-v2-adapter.mjs
# → 16000 positions, 0 mismatch — reconciliation holds
```

## What remains (needs the V2 side, not flash-book)

- The V2 **API service** (`flashapi.trade/v2`) wiring `orderType:"LIMIT"` to the
  book + adding `book`/`fills` endpoints — an off-chain service change.
- The **position adapter** landing flash-book fills in V2's `Position`/`Custody`
  — cross-program/account-model work on the V2 (`FLASH6…`) side.

flash-book already exposes what these need: ring-authenticated permissionless
settlement, the auto-quoted pool ladder, and the V2-reconciled PnL above.
