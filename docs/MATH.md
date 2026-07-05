# Math

Formal specification of the load-bearing computations. The settlement and
margin arithmetic have their own detailed documents
([SETTLEMENT.md](SETTLEMENT.md), [MARGIN_MATH.md](MARGIN_MATH.md),
[HAIRCUT_MATH.md](HAIRCUT_MATH.md)); this file covers matching, the FLP
quoter, funding, and the fee/insurance/solvency arithmetic.

## Notation

- `S` — base asset size in lots
- `P` — price in ticks
- `N` — notional in quote-lots, `N = S · P · tick_size`
- `q` — signed pool inventory; positive = long
- `σ` — realized volatility (rolling per-window return stdev)
- `u` — pool gross utilization (gross exposure / pool capital)

All USD/quote values use 6 decimals (`USD_UNIT`). Token decimals are
separate and per-mint.

---

## 1. Continuous price-time matching

Matching is a continuous central limit order book, not a batch auction.
A taker order walks the opposite side of the hypertree best-first (highest
bid / lowest ask), producing fills against resting makers in strict
price-then-FIFO priority until the taker is exhausted, its limit is
crossed, or the walk limit is hit. A residual either cancels (IOC) or
rests as a new limit order (subject to the anti-stuffing band). Each fill
is `(size, price, maker)`; the price is the resting maker's limit.

Order-type priority at the same price tier promotes liquidation and ADL
orders ahead of regular limits (the `order_type` byte → matcher priority
mapping). The walk is bounded by the per-market batch cap so a single
taker's compute stays within budget ([SETTLEMENT.md](SETTLEMENT.md) §5).

---

## 2. FLP quoter spread function

The FLP pool quotes two-sided passive liquidity on the book. Per quote
level `i ∈ {1..N_levels}` with cumulative size `Q_i`:

```
s(Q_i) = s₀ + α·vpin_bps + β·u + γ·|oi_imb| + κ·(Q_i / D_floor) + δ·σ
```

where `s₀` is the base spread floor and `α..δ` are the per-market spread
coefficients (`flp_spread_*_bps`). The `vpin_bps` toxicity term is held at
**zero** — the VPIN accumulator is retired — so the spread is driven by
utilization, OI imbalance, depth amortization, and realized volatility.

### Inventory-aware mid

```
skew       = − skew_magnitude · (q / pool_capital)
fair_value = oracle · (1 + skew)
```

When the pool is net-short (`q < 0`), `skew > 0` lifts `fair_value` above
the oracle, so the pool's bid is more attractive and it buys back its
short exposure first — inventory-aware market making.

### Quote ladder and growth cap

```
P_bid_i = round_to_tick(fair_value · (1 − s(Q_i)))
P_ask_i = round_to_tick(fair_value · (1 + s(Q_i)))
```

Each level emits one bid and one ask of `per_level_size`. The pool cannot
grow its position by more than `flp_max_growth_per_batch_bps` of capital
per refresh, and a hard inventory cap bounds gross exposure. Prices are
additionally validated within a band of the fresh oracle at settlement
(`FLP_MAX_FILL_DEVIATION_BPS`). Implementation: `matcher/flp_quoter.rs`.

---

## 3. Funding (cumulative index)

Funding is charged from a per-market cumulative index in Q64.64
fixed-point. A position stores the index at entry; on settlement it is
charged the delta:

```
funding_owed = sign(side_long) · notional · (cum_funding_index − I_at_entry)
```

Settlement moves `collateral ← collateral − funding_owed` and resets
`I_at_entry ← cum_funding_index`, equal-and-opposite against the Residual
so the change is conservative (a bounded ≤1 quote-lot transfer from
truncation, never a mint). Long pays when the index rose since entry,
short pays when it fell.

The index itself is economically **inert on-chain**: no instruction
advances it (no rate driver is wired), so `cum_funding_index` stays at its
initial value and every position settles `funding_owed = 0`. The
settlement-side charge math above is live and covered so that wiring a rate
driver later cannot change settlement semantics.
Implementation: `matcher/funding.rs::funding_owed`.

---

## 4. Mark price — fill EMA, oracle-banded

```
mark_ema ← blend(mark_ema, last_fill_price)               (fill EMA)
mark     = clamp(mark_ema, oracle · (1 − ε), oracle · (1 + ε))  (band ε)
```

The mark is an EMA over recent fill prices, clamped to a band of the
fresh oracle (`ε` = the effective oracle band, ≤ `MAX_ORACLE_BAND_BPS`,
enforced by `apply_fill` regardless of per-market config). Moving the
mark requires actually clearing volume at the target price, and the
oracle band caps how far a manipulated fill stream can push it — the
manipulation-resistance backstop for the worse-of(mark, oracle)
liquidation price. Per-slot move gates further bound the rate of change.

---

## 5. Stress-lattice maintenance margin

Required margin is the sum over markets of each market's worst-case
scenario loss, and health compares **available collateral** (not equity
with mark PnL) against it. The full derivation — the per-market
aggregation that closes cross-market offset, and the
available-collateral gate that avoids double-counting unrealized PnL — is
in [MARGIN_MATH.md](MARGIN_MATH.md) §4.

Scenario set Σ (default):
- `flat`
- per-market `±{2, 5, 10, 20}%` (8 scenarios per market)
- `all_down_10pct`, `all_up_10pct`
- `black_swan_down (-30% all)`, `black_swan_up (+30% all)`

Total: `1 + 8·M + 4` scenarios for M markets, bounded by
`MAX_STRESS_SCENARIOS`. O(N · |Σ|) cost.

**Hedge property:** for `P_long + P_short` on the same market, the
directional loss terms cancel under a shared shock in every scenario;
only the maintenance margin on stressed notional remains, so required
margin collapses.

---

## 6. Liquidation — bankruptcy waterfall

For a liquidated position at fill price `p_fill`:

```
realized_pnl = sign · S · (p_fill − entry)
penalty      = S · p_fill · liq_penalty_bps / 1e4
remaining    = collateral + realized_pnl − penalty

if remaining ≥ 0:
    collateral_recovered = remaining
    shortfall = 0
else:
    collateral_recovered = 0
    shortfall = −remaining
```

Waterfall:

1. `covered = min(insurance.balance, shortfall); insurance.balance −= covered`
2. `remaining_shortfall = shortfall − covered`
3. If `remaining_shortfall > 0`: ADL counter-positions ranked by
   `profit_ratio · leverage`, each closed at the bankruptcy price until the shortfall
   covered.

Insurance fund contributions per fill are configured in bps on the
`InsuranceFundAccount`: a share of the taker fee and a share of the
liquidation penalty. Recommended fund target:
`fund ≥ 0.01 · Σ_markets (OI_long + OI_short) · mark`.

---

## 7. Solvency invariant

At every settlement:

```
Σ trader_collateral
  + FLP_capital
  + insurance_fund.balance
  ≤ quote_vault_balance  (± accrued fees)
```

The protocol-owned subset (`vault ≥ insurance + FLP`) is machine-checked
directly (`matcher/insurance::assess_solvency`, Kani-proven). The
whole-program form including trader collateral is the Certora target
(`certora/PROPERTIES.md`) and is checked one-sidedly on-chain by
`partial_collateral_proves_insolvent`.

---

## 8. Numerical safety

All on-chain arithmetic is integer with checked overflow: money-moving
paths use `checked_*` and reject on overflow (`ArithmeticOverflow`), and
rounding is always floored in the direction that cannot mint value (see
the rounding table in the settlement and margin docs). There are no
floating-point values on any path.

The matcher uses `1e-12` as the comparison epsilon for "zero size remaining"
to avoid floating-point round-off creating perpetual partial fills.

All matching arithmetic is in integer lot/tick space — no floating-point
in the matcher path.
