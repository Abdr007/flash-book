# Math

Formal specification of every load-bearing computation.

## Notation

- `t` — time, measured in ER blocks (typically ~10 ms each)
- `b` — batch number; one batch = N blocks (default 5 → 50 ms)
- `S` — base asset size (e.g. SOL units)
- `P` — price (USD per base unit)
- `N` — notional, `N = S · P`
- `q` — signed inventory; positive = long
- `σ` — realized volatility (per-batch return stdev)
- `u` — utilization (pool gross exposure / pool capital)

All USD values use 6 decimals (Flash V2 convention; see `USD_DECIMALS`).

---

## 1. FBA Walrasian clearing

For a batch of orders, partition into buys `B` (long-side) and sells `S`
(short-side). Each order has limit price `ℓ_i` and size `s_i`. Define:

```
D(p) = Σ { s_i : i ∈ B, ℓ_i ≥ p }      (demand at price p)
S(p) = Σ { s_i : i ∈ S, ℓ_i ≤ p }      (supply at price p)
V(p) = min(D(p), S(p))                  (matchable volume at p)
```

The clearing price set is:

```
P* = arg max_p V(p)
```

Volume `V*` = `V(p*)`. If `|P*| = 1`, the clearing price `p*` is unique.
If `|P*| > 1`, tie-break in this order:

1. The price closest to the prior mark.
2. The midpoint of the indifference interval `[min P*, max P*]` if it
   contains the prior mark.

**Property (MEV-neutral within batch):** For any permutation of order
arrival within a batch, `D(p)` and `S(p)` are unchanged — they sum over
identical sets. Therefore `p*` and `V*` are invariant to arrival order.
No participant can profit from observing another's order in the same
batch.

**Tested in:** `tests/matcher.test.ts` — `MEV-neutrality` case verifies
clearing price equals across permutations.

---

## 2. Virtual FLP Quoter spread function

Per quote level `i ∈ {1..N_levels}`, with cumulative size
`Q_i = i · per_level_size`:

```
s(Q_i, t) = s₀ + α·VPIN(t) + β·u(t) + γ·|oi_imb(t)| + κ·(Q_i / D_floor) + δ·σ(t)
```

where:
- `s₀` — base spread floor (governance, default 5 bps)
- `α` — VPIN coefficient (default 0.5)
- `β` — utilization coefficient (default 0.3)
- `γ` — OI imbalance coefficient (default 0.2)
- `κ` — depth amortization (default 0.05)
- `δ` — realized-vol coefficient (default 2.0)

### Inventory-aware mid (Avellaneda-Stoikov)

```
skew_magnitude = λ + γ_risk · σ²
skew           = − skew_magnitude · (q / pool_capital)
fair_value     = oracle · (1 + skew)
```

When pool is net-short (`q < 0`), `skew > 0` → `fair_value > oracle` →
pool's bid is more attractive → pool buys back its short exposure first.
This is the standard inventory-aware market-making formulation; see
Avellaneda & Stoikov 2008 §3 for derivation.

### Quote ladder

```
P_bid_i = round_to_tick(fair_value · (1 − s(Q_i, t)))
P_ask_i = round_to_tick(fair_value · (1 + s(Q_i, t)))
```

Each level emits one bid and one ask order with size `per_level_size`.

### Per-batch growth cap

```
USD_cap = pool_capital · max_growth_pct
per_level_size_USD = USD_cap / N_levels
per_level_size = per_level_size_USD / oracle
```

The pool cannot grow its position by more than `max_growth_pct` of capital
per batch — a hard safety bound. Default `max_growth_pct = 0.005` (0.5%).

---

## 3. VPIN — toxicity signal

Volume buckets close when `B_buy + B_sell ≥ V_bucket`. For each closed
bucket:

```
imbalance = |B_buy − B_sell| / V_bucket    ∈ [0, 1]
VPIN     ← VPIN · (1 − α_ema) + imbalance · α_ema
α_ema    = 2 / (W_ema + 1)
```

Default `V_bucket = 100`, `W_ema = 50`.

VPIN ≈ 0 when buy/sell volumes are balanced (uninformed flow).
VPIN ≈ 1 when one side dominates persistently (informed/toxic flow).

Reference: Easley, López de Prado, O'Hara (2012),
*Flow Toxicity and Liquidity in a High-Frequency World*.

---

## 4. Continuous funding (cumulative index)

Per block of duration `Δt`:

```
premium(t) = (mark(t) − oracle(t)) / oracle(t)
rate(t)    = clamp(K · premium(t), ±r_max)
ΔI         = rate(t) · Δt
cum_funding_index ← cum_funding_index + ΔI
```

Default `K = 1/3600` so a steady 1% premium yields ~1% funding per hour.
`r_max = 1e-6` per second (≈ 3.6% per hour cap).

For position with stored `I_at_entry`:

```
funding_owed = sign(side_long) · notional · (cum_funding_index − I_at_entry)
```

Long pays when `ΔI > 0` (mark > oracle), short pays when `ΔI < 0`.
Settlement is `collateral ← collateral − funding_owed`, then
`I_at_entry ← cum_funding_index`.

This is the same cumulative-index pattern Compound / Aave use; the novelty
is per-block resolution (~10 ms) which eliminates the funding-tick sniping
game.

---

## 5. Mark price — TWAP, oracle-banded

```
TWAP_w = (1/w) · Σ_{i = b−w+1}^{b} clearing_price_i        (window w)
mark   = clamp(TWAP_w, oracle · (1 − ε), oracle · (1 + ε))  (band ε)
```

Default `w = 5` (last 5 batches), `ε = 100 bps`.

**Manipulation resistance:** moving the mark requires actually clearing
volume at the target price. A would-be manipulator pays for every basis
point of mark movement. The oracle band caps catastrophic divergence.

---

## 6. Stress-lattice maintenance margin

For trader portfolio `{P_i}` and scenario set `Σ`:

```
loss_i(s) = max(0, −sign_i · S_i · (mark_i · (1 + shock_i(s)) − entry_i))
            + S_i · mark_i · (1 + shock_i(s)) · m_maint_i

required(s) = Σ_i loss_i(s)

M_maint = max_{s ∈ Σ} required(s)
equity  = collateral + Σ_i unrealized_pnl_i − Σ_i funding_owed_i

healthy ⇔ equity ≥ M_maint
```

Scenario set Σ (default):
- `flat`
- per-market `±{2, 5, 10, 20}%` (8 scenarios per market)
- `all_down_10pct`, `all_up_10pct`
- `black_swan_down (-30% all)`, `black_swan_up (+30% all)`

Total: `1 + 8·M + 4` scenarios for M markets. For M = 5, that's 45
scenarios — evaluated once per portfolio per batch. O(N · |Σ|) cost.

**Hedge property:** for `P_long + P_short` same market, `loss_long(s) +
loss_short(s)` cancels the directional term in every scenario; only the
maintenance margin on stressed notional remains. Required margin
collapses by orders of magnitude.

---

## 7. Liquidation — bankruptcy waterfall

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
   `profit_ratio · leverage`, each closed at batch mark until shortfall
   covered.

Insurance fund contributions per fill:
```
fee_contribution    = taker_fee · 0.10
toxtax_contribution = toxicity_tax · 0.50
penalty_contribution = liq_penalty · 0.50
```

Recommended fund target: `fund ≥ 0.01 · Σ_markets (OI_long + OI_short) · mark`.

---

## 8. Toxicity tax (taker)

Per fill:

```
toxicity_tax = notional · tax_max_bps · min(1, VPIN(t)) / 1e4
```

Tax max default 5 bps. When VPIN ≈ 1 (highly toxic flow), full 5 bps tax
applies; when VPIN ≈ 0, no tax. Tax flows 50% to insurance fund, 50% to
maker rebate pool (rebate distribution is a future enhancement; currently
the contributing portion is held by the protocol).

---

## 9. Solvency invariant

At every batch boundary:

```
Σ trader_collateral
  + FLP_capital
  + insurance_fund.balance
  ≡ Σ initial_endowments
    + Σ realized_proceeds
    − Σ realized_payouts
```

Implementation: `engine.checkInvariants()` verifies finiteness of all
balances and non-negative insurance fund. Strong form (book-balance
equality) is asserted in long-running fuzz tests via the
`invariants hold across many random batches` test case.

---

## 10. Numerical safety

All financial computation uses:

- `safeNumber(x, fallback)` — replaces `NaN` / `Infinity` with fallback.
- `clamp(x, lo, hi)` — bounds enforcement.
- `Number.isFinite()` guards before any consumed value.

The matcher uses `1e-12` as the comparison epsilon for "zero size remaining"
to avoid floating-point round-off creating perpetual partial fills.

For production (Rust), all matching arithmetic moves to integer lot/tick
space — no floating-point in the matcher path. The simulator's float math
is a faithful reference but not the production target.
