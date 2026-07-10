# GOTCHAS.md — non-obvious footguns for Flash Book clients

Read this before writing a client or an agent. Everything here is behavior that
will surprise you if you assume "it works like a typical CLOB." Grounded in the
on-chain program; when in doubt the IDL (`idl/flash_book.json`) and the account
init constraints are the source of truth.

## PDA seeds

Derive these; do not hardcode addresses (they are per-market/per-trader).

| PDA | Seeds |
|---|---|
| `MarketAccount` | `[b"market", base_mint, quote_mint]` |
| `TraderStateAccount` | `[b"trader_state", trader]` (per wallet) |
| `PositionAccount` | `[b"position", market, trader_state]` (one per market per trader) |
| `InsuranceFundAccount` | `[b"insurance_fund"]` (singleton) |
| `FeeTiersAccount` | `[b"fee_tiers"]` |
| `oracle_config` | `[b"oracle_config", market]` |

Because a position PDA is seeded `[b"position", market, trader_state]`, **a trader
holds at most one position per market.** This is load-bearing for the margin-walk
completeness proof (below) — the set of a trader's open positions is a set of
distinct *markets*.

## Sequencer / `apply_fill` trust model

`place_taker_order_v2` does **not** settle the fill itself — it emits a
`FillBatchEvent`, and settlement is applied by `apply_fill`. Who may call `apply_fill`
depends on whether the market is *armed*:

- **Armed market** (the default, `fill_commitment_required`): `apply_fill` is
  **permissionless**. It recomputes `keccak(fill_preimage)` binding market + both
  trader identities + side/size/price/sub-indices + JIT flag, and pops it FIFO from
  the on-chain commitment ring. A caller can only settle the *exact* committed
  fills, to the *exact* parties, in order — no fabrication, redirection, or reorder.
  This is the censorship-resistant keeper model.
- **Unarmed (legacy) market**: `apply_fill` is gated to the single
  `market.sequencer`. A market whose sequencer is still the zero pubkey **fails
  closed** — settlement halts until `set_market_sequencer` runs.

Do not build a client that calls `apply_fill` on an unarmed market you don't
sequence. Do not assume your taker order is "filled" until `apply_fill` lands.

## Margin-walk completeness (withdraw / risk checks)

`partial_withdraw_collateral` (and other risk-checked paths) require you to supply
**every** open position as `(market, position)` account pairs. The on-chain gate is
exact-count (`remaining.len() == open_positions * 2`) + PDA-binding + market-dedupe
+ live-only (`size_lots > 0`). Omitting a position to understate the requirement is
**impossible** — the gate rejects an incomplete or padded walk (proven:
`formal_verification/lean/AuthCompleteness.lean`). If your client under-supplies,
you get a clean rejection, not a loss — but you must supply the full set to succeed.

## Withdraw-anytime reserve margin

There is no "arm/lock" step. Withdrawable is computed live on every release path:

```
withdrawable = collateral − max(IM, floor) − er_reserved
```

`er_reserved` is the initial margin reserved by your live orders resting on the
MagicBlock Ephemeral Rollup, surfaced to L1 by the sequencer-signed attestation
(`sequencer/attestation_cranker.mjs`). There is a documented attestation-lag window
(~one poll interval). Sessions last 7 days.

## Funding is two instructions, both may be permissionless

- `crank_funding()` advances `market.cum_funding_index`. **Permissionless.** The
  *first-ever* call only seeds the clock (accrues nothing); a same-second call is a
  no-op; `dt` is clamped to one funding period, so a long-dormant market takes a
  single bounded jump on resume, never an unbounded spike. Rate is clamped to the
  market's cap.
- `settle_funding()` realizes a position's accrued funding into collateral through
  the Kani-proven `route_funding` path (Δcollateral == −Δresidual, so funding moves
  value but never mints it).

An agent that wants its PnL current should crank + settle before reading equity.

## Health price is worse-of, with staleness gates

Liquidation and withdraw prices use `worse-of(mark, oracle)` for your side, via
`effective_health_mark` — never a single naive source. When the mark is stale the
oracle becomes the sole price and **must** be fresh + configured, else the position
is left open (fail-safe) rather than liquidated off a bad price. Mirror this in your
own risk model: do not price your liquidation off one source.

## Amounts, units, sides, flags

- Integers only. `quote_lots` (quote lot), `ticks` (price; `price = ticks ×
  tick_size`), `base_lots` (size). No floats on chain.
- `side`: `0 = long`, `1 = short`.
- `sub_index`: selects a sub-account (`0` = primary; `1..=255` for isolated
  sub-accounts). The margin/ADL engine buckets isolated positions separately.
- `place_*_v2` `flags`: `bit0 post_only`, `bit1 reduce_only`, `bit2 ioc`, `bit3 jit`,
  `bits4-5 stp_mode` (self-trade prevention mode).
- `expires_at_slot`: `0` means no expiry.

## Reading the book

The `MarketBookAccount` is a hypertree (red-black-tree slab), **not** a flat array.
Each `RestingOrderV2` carries the trader pubkey inline. Walk the slab (links are
byte offsets; `NIL = u32::MAX`) to reconstruct bids/asks. Snapshot book + fill-ring
+ outbox in **one** `getMultipleAccountsInfo` so an in-flight fill can't fall
between reads. `sequencer/attestation_cranker.mjs` has a validated raw decoder.

## Errors

114 typed errors (see the IDL `errors` array). A custom on-chain error code =
its `errors.rs` discriminant + 6000 (Anchor convention). Common fail-closed ones:
`OracleTooStale`, `MarkTooStale`, `ProtocolInsolvent`, `HaircutResidualUnbacked`,
`ErMarginNotReady`. These are guards firing, not bugs — handle them, don't retry
blindly.
