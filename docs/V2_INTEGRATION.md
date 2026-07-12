# Flash Book ↔ Flash V2 — integration design

> Goal: the **best orderbook**, designed so **Flash V2 integrates it with minimal work**.
> Grounded in the real V2 surface (`github.com/flash-trade/examples-v2`,
> `flashapi.trade/v2`, the FLASH6 `perpetuals` IDL).

## The key insight — they're already aligned

flash-book and Flash V2 are the **same runtime shape**, so this is an *integration*, not a
re-platform:

| | Flash V2 (`FLASH6…` perpetuals) | flash-book (this repo) |
|---|---|---|
| Execution | MagicBlock Ephemeral Rollup, ~30–50 ms | MagicBlock Ephemeral Rollup, ~sub-50 ms |
| Settlement | commits to Solana L1 | commits to Solana L1 |
| Client surface | **hosted REST API** (`flashapi.trade/v2`), partially-signed txs, session keys, live WS | (add the same) |
| Pricing today | oracle + FLP **pool** (LP-vs-trader) | **CLOB** (price-time book) + FLP backstop |
| Market structure | LIMIT orders execute **against the pool** at limit price | LIMIT orders **rest and match** in a real book |

V2 already has `open_position_er`, `place_limit_order_er`, `execute_limit_order_er`, `Position`,
`Order`, `Custody`, `Pool` — and it already runs on the ER. flash-book supplies the piece V2
doesn't have: **a real continuous price-time order book with pool-backed liquidity.**

## The integration model — three bridges, zero new client SDK

V2's public surface is **the API** (no public SDK). So "easy integration" means: **expose
flash-book through the exact same API contract**, so an existing V2 client trades the CLOB with
*no new SDK and the same UX* (symbols, session keys, partially-signed txs, live WS).

### Bridge 1 — API parity (the client sees no difference)
Add CLOB endpoints to `flashapi.trade/v2` that mirror V2's shape:
- `openPosition` with `orderType: "LIMIT"` → **rests on the book** (today it hits the pool).
- new: `book(marketSymbol)` (depth), `cancelLimitOrder`/`editLimitOrder` (already exist), `fills` (WS).
- **Same request types** (`OpenPositionRequest`: `inputTokenSymbol`, `outputTokenSymbol`,
  `leverage`, `tradeType`, `limitPrice`, `owner`, `signer`/`sessionToken`), **same
  partially-signed-tx flow** (API returns base64 partial-signed → client signs owner slot →
  submit to ER RPC). A V2 client calls it exactly like a pool trade.

### Bridge 2 — one position model (fills land in V2's `Position`)
A CLOB fill must update the **same `Position`/`Custody`** a pool trade would, so a trader's
book fills and pool fills are **one unified position + margin**. flash-book's settlement
(`apply_fill`, already **permissionless-keeper** + ring-authenticated) writes the position
delta; the adapter maps flash-book's position account ↔ V2's `Position`/`Custody`. Net: a trader
never sees "two products" — one account, one margin, one PnL, filled by *whichever* is best.

### Bridge 3 — FLP as the on-book market maker (pool-backed CLOB)
V2's `Pool` (FLP) becomes flash-book's **on-book passive MM**: it rests two-sided quotes at
`oracle ± dynamic spread` (the existing `matcher::flp_quoter`, Avellaneda-Stoikov inventory
skew). Takers cross the **better of** the pool quote or an external MM's tighter quote.
- Guarantees liquidity from day one (**solves the CLOB cold-start problem**) using capital V2
  LPs already provide.
- LPs earn market-making PnL + fees (like Hyperliquid's HLP), protected by dynamic spread +
  inventory caps.
- Because the FLP quote is an **on-book maker**, its fill is a **book fill** → ring-committed →
  settled by the **permissionless keeper** (closes the FLP-permissionless gap for free).

## What Flash actually does to adopt it

1. **Point the V2 API's `orderType:"LIMIT"` path at flash-book's book** (+ add `book`/`fills`).
   Existing clients get real limit orders, zero client change.
2. **Map flash-book fills → V2 `Position`/`Custody`** (the adapter — margin/PnL unified).
3. **Have the FLP pool quote on the book** (wire `generate_quotes()` → resting FLP orders).

That's it. The hard parts — the CLOB engine, sub-50ms ER matching, fill authenticity,
the permissionless keeper, the FLP quoter math — are already built, machine-proven, and
validated on the live devnet ER. Integration is API wiring + the position adapter +
turning the pool into an on-book MM.

## Why this is "the best orderbook" for Flash specifically

- **Continuous price-time CLOB, no batch auction** — pro market structure (tight spreads, real limit
  orders, time priority) that the pool model can't give.
- **Pool-backed (HLP)** — deep guaranteed liquidity from V2's existing FLP, plus competitive
  MM improvement. Best of both; solves cold-start.
- **Same rails** — MagicBlock ER, sub-50ms, partially-signed API, session keys. A V2 user
  won't feel a seam.
- **Trust-minimized cheaply** — authenticated fills + permissionless keeper (no expensive
  validator network).
- **One account** — book fills and pool fills share position, margin, PnL.

## Sequenced plan

1. **Position adapter** (flash-book fill → V2 `Position`/`Custody`) — the load-bearing bridge.
2. **FLP-as-on-book-MM** (rest `flp_quoter` output) — pool-backed liquidity + closes FLP gap.
3. **API endpoints** on `flashapi.trade/v2` (`orderType:"LIMIT"` → book, `book`, `fills`).
4. **External MM onboarding** — tighter quotes on top of the pool floor.

Each is a real, scoped increment. #1 and #2 are on-chain (fund-critical, done deliberately);
#3 is API/service work; #4 is BD. None needs a validator network or a new chain.
