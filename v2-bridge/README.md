# Flash V2 ↔ flash-book bridge (reference)

Reference adapter for integrating the **flash-book CLOB** into **Flash V2**'s
API-first stack — the concrete first step of `docs/V2_INTEGRATION.md`. Flash V2's
public surface is a hosted REST API (`flashapi.trade/v2`, no public SDK) over the
MagicBlock ER; flash-book runs on the *same* ER. So integration is a **mapping**,
not a re-platform.

## What this is

A mapping + transaction-construction library (`flash-book-v2-adapter.mjs`) that
drops into the V2 API service or a client. Every amount is a **BigInt** or a
decimal **string** parsed to a BigInt — no floating-point arithmetic touches any
value that maps to on-chain lots/lamports. Instruction discriminators, account
ordering, and the Borsh arg layout all come from the committed IDL
(`../idl/flash_book.json`) via anchor's coders, so the produced instruction is
byte-for-byte what the deployed program expects.

- **Bridge 1 — API parity + real tx.** `v2RequestToFlashBookOrder()` maps a V2
  `OpenPositionRequest` to a typed flash-book order (`LIMIT` →
  `place_limit_order_v2`, rests; `MARKET` → `place_taker_order_v2`, crosses).
  `buildOrderInstruction()` turns that into a real `TransactionInstruction`:
  program id from the IDL, the market / market_book PDAs derived from the IDL
  seeds, accounts ordered and signer/writable-flagged exactly as the IDL
  declares, all six args (`side`, `size_lots`, `limit_ticks`, `flags`,
  `expires_at_slot`, `sub_index`) Borsh-encoded, plus the compute-budget
  instructions. A V2 client submits it exactly like a pool trade — same
  partially-signed-tx flow, same ER RPC, **zero new SDK**.
- **Bridge 2 — one position/PnL model.** `decodePositionAccount()` decodes a raw
  `PositionAccount` into typed, named BigInt fields; `flashBookPositionToV2()`
  maps it to V2's `PositionMetrics` shape, computing PnL both ways in exact
  integers and asserting they are the *same* integer.
- **Events + ALTs.** `decodeEvent()` turns a `Program data:` log line into a
  typed `{ name, data }`; `buildOrderLookupTable()` builds the Address-Lookup-Table
  create/extend instructions for the many-account settlement / portfolio-walk path.

## The load-bearing property

flash-book's exact-integer realized PnL `sign · closed · Δticks · tick_size`
(programs/flash-book/src/matcher/position_math.rs) **equals** V2's
`(mark − entry)/entry · notional` with `notional = size · entry · tick_size`.
Because `entry` divides `notional` exactly, the two are the **same integer** —
computed here in BigInt, they compare `===` with no rounding. This is verified at
three layers:

- **In the matcher core** (`position_math.rs`): `matches_v2_notional_return_formula`
  and the exhaustive `reduce_flip_pnl_and_v2_reconciliation_exhaustive_small`
  (1,700+ cases, both sides, profit and loss, several tick sizes) plus 4,000-case
  proptests assert `pnl = sign·closed·Δ·tick` and the V2 reconciliation
  `fb·entry == (mark−entry)·notional`. The neighbouring open / no-PnL-without-
  reduction invariants are additionally Kani-proven.
- **End-to-end here** — the demo reconciles **16,000 positions across both sides /
  sizes / entries / marks with 0 mismatch**, and the SDK test suite checks the
  identity exactly (including values beyond 2^53 that float would lose) and
  round-trips every instruction/account through the committed IDL.

That's why a V2 client and flash-book share **one position, one margin, one PnL,
to the lot** — a trader never sees "two products."

```
cd v2-bridge && npm install
npm run demo   # → 16000 positions, 0 mismatch — reconciliation holds
npm test       # → SDK tests: exact identity, IDL round-trips, PDA/keys, decode
```

## What remains (needs the V2 side, not flash-book)

- The V2 **API service** (`flashapi.trade/v2`) wiring `orderType:"LIMIT"` to the
  book + adding `book`/`fills` endpoints — an off-chain service change.
- The **position adapter** landing flash-book fills in V2's `Position`/`Custody`
  — cross-program/account-model work on the V2 (`FLASH6…`) side.

flash-book already exposes what these need: ring-authenticated permissionless
settlement, the auto-quoted pool ladder, and the V2-reconciled PnL above.
