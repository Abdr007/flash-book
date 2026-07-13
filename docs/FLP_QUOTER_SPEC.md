# LP On-Book Quoter — Specification (roadmap 5.1)

The Flash Liquidity Pool (LP) posts two-sided maker quotes onto the same
hypertree order book that external traders use — a pool-backed CLOB market maker.
This document specifies the pricing model it uses. It is an
**Avellaneda–Stoikov-inspired** inventory-aware quoter, adapted to an on-chain,
integer-only setting. Everything here is grounded in
`programs/clober/src/matcher/lp_quoter.rs` (`generate_quotes`) — no
aspirational behaviour.

All rates are in basis points (`BPS_DENOM = 10_000`). All arithmetic is
integer, checked/saturating, and deterministic; there are no floats.

---

## 1. Reservation (fair) price — inventory skew

The classic Avellaneda–Stoikov reservation price shifts the mid **away** from the
market mid in proportion to inventory, so a maker holding a long book quotes
lower (to sell down) and vice-versa. The on-chain form:

```
inv_bps   = pool_net_q / pool_capital_q × BPS_DENOM        (signed)
skew_bps  = −clamp( λ · inv_bps / BPS_DENOM , ±BPS_DENOM )  (signed, ±100% cap)
fair_value = oracle_ticks · (1 + skew_bps / BPS_DENOM)
```

- `pool_net_q` is the pool's signed net position (long positive), `pool_capital_q`
  its capital; their ratio is the normalized inventory.
- `λ = inventory_lambda_bps` is the risk-aversion / skew intensity (governance
  `u32`, in bps).
- The **negative** sign makes the pool quote *toward* offloading its inventory:
  net-long ⇒ `skew_bps < 0` ⇒ `fair_value` below oracle ⇒ more aggressive asks.
- The `±BPS_DENOM` clamp **before** the cast/negation bounds the skew at ±100%
  of oracle, so an unbounded governance `λ` can never invert or overflow the
  price. (v1 omits the volatility-coupled `γσ²` reservation term of the original
  model; volatility enters through the spread instead — §2.)

## 2. Spread — multi-factor, per depth level

For each quote level `i ∈ [1, levels]` with cumulative size `cum_size = i ·
per_level_lots`, the half-spread in bps is the sum of a base and five additive
risk premia, then capped:

```
s_bps = base_spread_bps
      + α · vpin_bps                / BPS_DENOM   (toxicity / adverse selection)
      + β · pool_gross_utilization  / BPS_DENOM   (capital-at-risk premium)
      + γ · |oi_imbalance_bps|      / BPS_DENOM   (directional-crowding premium)
      + κ · cum_size / depth_floor_lots           (depth/impact premium, per level)
      + δ · realized_vol_bps        / BPS_DENOM   (volatility premium)
s_bps = min(s_bps, 5000)                          (hard 50% cap)
```

| coeff | param | premium it charges |
|---|---|---|
| `α` | `alpha_bps` | VPIN toxicity (flow-informedness) |
| `β` | `beta_bps` | pool gross utilization (inventory-at-risk) |
| `γ` | `gamma_bps` | open-interest imbalance (one-sided crowding) |
| `κ` | `kappa_bps` | depth: deeper levels widen (`cum_size / depth_floor_lots`) |
| `δ` | `delta_bps` | realized volatility |

The depth term makes each successive level wider (a convex book), so size costs
progressively more — the on-chain analogue of a market-impact curve. The 50%
cap is a sanity floor, never a normal operating point.

## 3. Quotes

Each level posts a symmetric bid/ask around `fair_value`, tick-aligned, with a
uniform per-level size:

```
bid_i = align_tick( fair_value · (1 − s_bps / BPS_DENOM) , tick_size )
ask_i = align_tick( fair_value · (1 + s_bps / BPS_DENOM) , tick_size )
size_i = per_level_lots           (bids and asks; cumulative depth = i · per_level_lots)
```

Orders are emitted as `OrderType::LpVirtual` on `Side::Long` (bids) /
`Side::Short` (asks), owned by the LP trader PDA. A level whose aligned price
rounds to `0` is dropped (never post a zero-price order).

## 4. Inventory cap — the hard backstop

Continuous skew (§1) discourages runaway inventory but does not *stop* it. On top
of it, `inventory_cap_skip(net_signed, capital_quote_lots) → (skip_bids,
skip_asks)` suppresses the side that would grow an already-extreme position (e.g.
when net-long past a capital-scaled threshold, skip new bids so the pool only
sells). This bounds worst-case pool inventory regardless of `λ`.

**Safety property (Kani-proven, `inventory_cap_kani`):** the cap **never skips
both sides simultaneously** — the pool can always reduce its inventory on at
least one side, so it can never get wedged into a one-way, un-exitable book.

## 5. Authenticity band (settlement guard)

A pool quote is authentic only if it sits within a bounded deviation of the fresh
oracle: `price_within_band(oracle_ticks, price_ticks, max_dev_bps)`. Settlement
does **not** re-derive the quote (its inputs — VPIN, inventory, OI, realized vol
— are not reconstructable at settlement time and re-deriving them would be an
unsound trust surface); instead it checks the far cheaper, sufficient invariant
that an authentic quote lands within `max_dev_bps` of oracle. Complementary
resting-order guards: `price_sig_figs_ok` (≤ 5 significant figures, roadmap 4.2)
and `order_notional_ok` (per-order quote-lot floor, roadmap 4.1).

## 6. Parameters (`LpQuoterParams`)

`base_spread_bps`, `alpha_bps`, `beta_bps`, `gamma_bps`, `delta_bps`,
`kappa_bps`, `inventory_lambda_bps`, `depth_floor_lots`, `levels`, `tick_size`,
plus the per-level base size. All are governance-set `u32`/`u64`; the math above
is total (checked/saturating) for every value, so no parameter choice can panic
or mint price — only widen/narrow the book.

---

## Status & what remains (5.1)

- **Engine + spec: DONE** — the model above is implemented, integer-total, and
  the two load-bearing safety properties (inventory cap never wedges the book;
  spread/skew bounded) are Kani-proven.
- **Deferred — live parameter sweep**: choosing production `(base_spread, α, β,
  γ, δ, κ, λ, depth_floor)` per market is an empirical devnet/ER tuning exercise
  (fill quality vs. inventory variance vs. adverse selection) that depends on
  live MagicBlock-ER flow, not on-chain code. That sweep is the remaining 5.1
  work and is gated on the ER data-collection endpoint.
