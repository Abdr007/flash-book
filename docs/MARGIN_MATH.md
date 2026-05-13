# Margin Math

Formal specification of the margin model — cross vs isolated buckets,
healthy-trader invariant, stress lattice, and the liquidation/funding
routing that depends on those invariants. Companion to `MATH.md` (which
covers FBA clearing, FLP quoter, funding, etc.).

Target audience: auditors and contributors who need to verify that the
implementation in `programs/flash-book/src/matcher/risk.rs` +
`programs/flash-book/src/lib.rs` matches the model below.

## 0. Conventions

- All collateral and notional values are in **quote-lots** (Flash Book
  uses USDC-style 6-decimal quote at the lot level; convention shared
  with `MATH.md` and the rest of the program).
- `i128` arithmetic is used for intermediate signed sums to avoid
  overflow; final outputs clamp to `i64` / `u64` with explicit
  saturation flags (`ArithmeticOverflow` / `ArithmeticUnderflow`).
- Indices and notation:
  - `T` — trader (Pubkey)
  - `m` — market (Pubkey)
  - `P_m` — trader `T`'s position on market `m`
  - `Side ∈ {Long, Short}`
  - `size_m` — `P_m.size_lots` (always non-negative; side is separate)
  - `entry_m` — `P_m.entry_price_ticks`
  - `mark_m` — `MarketAccount.mark_price_ticks` (EMA-blended; see
    `MATH.md §2`)
  - `tick_m` — `MarketAccount.params.tick_size`
  - `mmr_m(s)` — effective maintenance margin in bps for a position of
    size `s` on `m` (see §2)
  - `C_T` — `TraderStateAccount.collateral_quote_lots` (the pooled
    "cross" bucket)
  - `c_m` — `P_m.collateral_quote_lots` (the per-position isolated
    bucket; `0` ⇔ position is cross-margined)
  - `BPS = 10_000`

---

## 1. Equity

### 1.1 Per-position unrealized PnL

For position `P_m` at price `p`:

```
notional(p) = size_m · p · tick_m                      (in quote-lots)
upnl(P_m, p) = sign · size_m · (p − entry_m) · tick_m
   where sign = +1 if Long, −1 if Short
```

Implemented in `risk.rs::unrealized_pnl_quote_lots`.

### 1.2 Per-position funding owed

```
owed_m = funding_owed(side, notional(mark_m), idx_now_m, idx_at_entry_m)
```

`funding_owed` is the standard signed integral of the funding rate
between `idx_at_entry_m` and `idx_now_m`; positive means trader pays.
Implemented in `matcher::funding::funding_owed`.

### 1.3 Equity (signed, in quote-lots)

For a set of positions `𝒫` evaluated against a bucket of collateral
`B`:

```
equity(𝒫, B) = B  +  Σ_m∈𝒫 upnl(P_m, mark_m)  −  Σ_m∈𝒫 owed_m
```

The two summations are computed in a single loop over `𝒫` in
`assess_margin` (lines 247–273) before the scenario loop.

---

## 2. Maintenance margin

Implementation: `MarketSnapshot::effective_mmr_bps` +
`risk::tiered_mmr_bps`.

### 2.1 Base + concentration tier

```
mmr_concentration(s, m) = mmr_m  +  (extra_m  if  s ≥ thresh_m  else  0)
```

`mmr_m = market.params.maintenance_margin_ratio_bps`
`extra_m = market.params.concentration_extra_mmr_bps`
`thresh_m = market.params.concentration_threshold_lots`

`thresh_m == 0` disables the tier (legacy single-MMR behaviour).

### 2.2 Tiered MMR (Hyperliquid-style)

If `MarketLeverageTiersAccount` is configured, a position's effective
MMR is the highest tier whose `min_notional_quote_lots` does not
exceed `notional(mark_m)`. Tiers are stored sorted ascending by
`min_notional` (enforced at write-time in
`lib.rs::init_market_leverage_tiers`). Falls back to the base
`mmr_concentration` if no tier matches.

Pure function `risk::tiered_mmr_bps` covers this. Audit invariant:
the schedule is monotone (MMR cannot decrease as notional rises).

---

## 3. Stress scenario lattice

`risk::default_scenarios(markets)` produces:

1. Flat (no shocks)
2. For each `m`, per-market shocks `±{2%, 5%, 10%, 20%}` (8 scenarios)
3. All-down 10%, all-up 10%
4. Black-swan ±30%

A `Scenario` is a `Vec<StressShock { market, shock_bps: i32 }>`. A
position whose `market` is not listed in a scenario is implicitly
unshocked at 0 bps. Shock is applied to `mark_m` to produce the
stressed price `mark_m · (1 + shock/BPS)`.

The lattice is **finite** (≤ `1 + 8·|markets| + 4` scenarios) so
on-chain assessment is bounded compute.

---

## 4. Cross-margin assessment (`assess_margin`)

Given `𝒫`, `B`, scenarios `𝒮`:

```
equity = equity(𝒫, B)                              (signed i128)

required = max_{σ ∈ 𝒮}  Σ_m∈𝒫  loss_m(σ) + mm_m(σ)
   where:
     stressed_m(σ)   = mark_m · (1 + shock_σ_m / BPS)
     loss_m(σ)       = − upnl(P_m, stressed_m(σ))     (≥ 0 when adverse)
     mm_m(σ)         = size_m · stressed_m(σ) · tick_m
                       · mmr_concentration(size_m, m) / BPS

is_healthy = equity ≥ required
```

Saturating arithmetic at each step: `required` clamps to `u64::MAX`
on overflow; `equity` clamps to `i128` boundaries.

The function returns `MarginAssessment { required, equity, is_healthy,
worst_scenario_idx }`. `worst_scenario_idx` is the index of `σ` that
produced the maximal `required` — surfaces to keepers for "which
scenario tipped this position" diagnostics.

Implementation: `risk.rs::assess_margin` lines 241–336.

---

## 5. Isolated-margin assessment (`assess_margin_split`)

### 5.1 Buckets

Partition `𝒫` into:

- `𝒫_cross` — positions with `c_m == 0`
- `𝒫_iso = { P_m : c_m > 0 }` — each position is its own singleton
  bucket, evaluated against `c_m` (not `C_T`)

### 5.2 Healthy invariant

```
is_healthy ⟺
   assess_margin(𝒫_cross, C_T, 𝒮).is_healthy
   AND
   ∀ m ∈ 𝒫_iso : assess_margin({P_m}, c_m, 𝒮).is_healthy
```

Equivalently: every bucket must independently pass the cross-margin
healthy test. A failure of any isolated bucket is sufficient to mark
the trader unhealthy.

### 5.3 Aggregate outputs

The returned `MarginAssessment` summarises across buckets:

```
required = Σ_buckets  bucket.required        (saturating)
equity   = Σ_buckets  bucket.equity_signed   (saturating)
worst_scenario_idx = scenario from the tightest bucket
   (tightness ≡ bucket.required − bucket.equity_signed; larger = closer
    to liquidation)
```

UIs use `required` for "total locked margin" and `equity` for "total
headroom".

Implementation: `risk.rs::assess_margin_split` lines 364–429.

### 5.4 Dispatch (`assess_margin_unified`)

Every handler call site routes through this wrapper. It checks
`p.collateral_quote_lots` on each snapshot:

```
if ∀ p ∈ 𝒫 : p.collateral_quote_lots == 0
   → assess_margin(𝒫, C_T, 𝒮)             (byte-identical to pre-Phase-2)
else
   → assess_margin_split(𝒫, C_T, 𝒮, derived_iso_map)
   where derived_iso_map = { (p.market, p.collateral_quote_lots) :
                              p.collateral_quote_lots > 0 }
```

Implementation: `risk.rs::assess_margin_unified` lines 432–470.

### 5.5 Why isolation is *strict*

The split assessment evaluates each isolated position **alone** against
its own collateral. The cross pool is invisible to the isolated
bucket; a fat cross pool cannot rescue an under-collateralised
isolated position. Conversely, an isolated failure cannot bleed back
into the cross set — the cross bucket evaluation does not include
the isolated position at all. Test
`isolated_margin_tests::isolated_unhealthy_when_underfunded_even_if_cross_pool_huge`
locks in the first direction; `cross_set_protected_when_isolated_fails`
locks in the second.

---

## 6. Liquidation routing (`liquidate_position_v2`)

### 6.1 Health gate

Calls `assess_margin_unified` against `[P_target]` with
`C_T = trader_state.collateral_quote_lots`. The unified dispatcher
routes:

- Cross position (`c_target == 0`) → assess against `C_T` as before.
- Isolated position (`c_target > 0`) → assess against `c_target`
  alone; cross bucket is empty (vacuously healthy).

The trader must be **unhealthy** on this assessment for the call to
proceed (`require!(!is_healthy)`).

### 6.2 Dual-source price

For both cross and isolated paths, the health gate uses
`health_price = worse-of(mark, oracle)` for the position's direction:

```
Long  : health_price = min(mark, oracle)   (lower = worse for trader)
Short : health_price = max(mark, oracle)   (higher = worse for trader)
```

Implementation: lines 5222–5244. Stale oracle gate refuses to
liquidate (line 5202–5206).

### 6.3 Liquidator reward routing

```
notional        = close_size · oracle · tick_m
reward_bps_eff  = liquidator_reward_bps · (elapsed / auction_duration)
                  (Dutch auction, clamped to ≥0 and ≤1)
reward          = notional · reward_bps_eff / BPS    (capped at u64::MAX)
```

The reward **debits** the trader and **credits**
`caller_trader_state.collateral_quote_lots`:

```
if c_target > 0:                              (isolated)
    paid = min(reward, c_target)
    position.collateral_quote_lots -= paid
else:                                          (cross)
    paid = min(reward, C_T)
    trader_state.collateral_quote_lots -= paid
caller_trader_state.collateral_quote_lots += paid
```

The cross pool is **never** touched on the isolated path —
invariant.

Implementation: lines 5495–5547.

### 6.4 ADL (`auto_deleverage`)

Same health-gate dispatch as §6.1. The actual PnL settlement at the
bankruptcy price still debits `underwater_trader_state.collateral_quote_lots`
(lines 5839–5854) — **not** the per-position bucket. This is a known
gap and is the same gap as §8 below. For Phase 2 the ADL path uses
the unified health gate but the cash flow remains on the cross pool;
isolating a position does NOT yet redirect ADL settlement.

---

## 7. Funding routing (`settle_funding`)

After computing `owed_i64` (signed, positive = trader pays):

```
is_isolated = position.collateral_quote_lots > 0

if owed > 0:
    if is_isolated:
        pay = min(owed, c_m); position.collateral_quote_lots -= pay
    else:
        pay = min(owed, C_T); trader_state.collateral_quote_lots -= pay
elif owed < 0:
    recv = |owed|
    if is_isolated:
        position.collateral_quote_lots += recv  (checked, overflow → err)
    else:
        trader_state.collateral_quote_lots += recv  (checked)

position.funding_paid_quote_lots += owed
position.cum_funding_index_at_entry = market.cum_funding_index
```

### 7.1 Truncation semantics

If `c_m < owed` on the isolated path, the unpaid remainder is
**absorbed**: `cum_funding_index_at_entry` advances unconditionally
and the shortfall is forgotten. This matches the cross-path
behaviour (`min(owed, C_T)` truncation already in place) — the
protocol intentionally trades collection precision for liveness. The
position becomes liquidatable on the next health check, and the
liquidator reward + JIT auction + insurance fund cover the actual
shortfall.

### 7.2 Event semantics

`FundingSettledEvent.new_collateral` reports
`trader_state.collateral_quote_lots` regardless of whether the cash
flow hit the per-position bucket. Off-chain consumers must read
`position.collateral_quote_lots` separately to track the isolated
balance. Intentional: changing the event shape would break existing
indexers; the per-position state is already independently observable.

Implementation: lines 2577–2615.

---

## 8. Known gaps (Phase 2b targets)

### 8.1 Realized PnL materialisation

`apply_fill_to_position` (lines 11035–11115) updates
`pos.realized_pnl_quote_lots` on close. It never reads from or writes
to `trader_state.collateral_quote_lots` or `pos.collateral_quote_lots`.

The only path that materialises realized PnL into a collateral bucket
is `auto_deleverage` (lines 5839–5854), and it materialises into
`trader_state.collateral_quote_lots` directly (separate from any
isolation).

**Consequence:** closing a profitable position via the normal fill
path does NOT credit the trader's spendable collateral. The trader
sees `realized_pnl_quote_lots` rise on the position, but
`trader_state.collateral_quote_lots` is unchanged. They cannot
`withdraw_collateral` against the realized gain because the withdraw
guards stress against `trader_state.collateral_quote_lots` alone.

This is a **pre-existing** protocol gap, NOT a Phase 2 regression.
Resolving it is a focused commit: define a `settle_realized_pnl(P_m)`
ix that drains `pos.realized_pnl_quote_lots` into the correct bucket
(`c_m` if isolated, else `C_T`), then zero the position field.
Alternative: fold the materialisation directly into
`apply_fill_to_position`, but that requires passing the trader_state
account into every call site.

### 8.2 `apply_fill` fee routing

Taker fees and maker fees mutate `taker_trader_state.collateral_quote_lots`
and `maker_trader_state.collateral_quote_lots` regardless of whether
the underlying position is isolated. For full isolation the fee
debit/credit should land on `position.collateral_quote_lots` when the
position is isolated.

`ApplyFill` Accounts does not currently carry the position account
(only the trader-state accounts), so adding fee routing wants a ctx
expansion in a separate audited commit.

### 8.3 ADL settlement

`auto_deleverage` debits the underwater trader's cross pool
(`underwater_trader_state.collateral_quote_lots`) directly. For an
isolated underwater position, the bankruptcy-price loss should come
from `position.collateral_quote_lots` first, with insurance-fund
shortfall coverage and counter-trader gain semantics otherwise
unchanged. Punt to Phase 2b.

---

## 9. Invariants summary

For any trader `T`:

| # | Invariant | Enforced by |
|---|-----------|-------------|
| I-1 | Cross health: `assess_margin(𝒫_cross, C_T, 𝒮).is_healthy` | All trade-path call sites via `assess_margin_unified` |
| I-2 | Isolated independence: each `P_m ∈ 𝒫_iso` is healthy against `c_m` alone | `assess_margin_split` (5.2) |
| I-3 | Cross pool insulation: liquidation of an isolated position never debits `C_T` | `liquidate_position_v2` (6.3) |
| I-4 | Isolated bucket insulation: cross-path liquidation never debits any `c_m` | `liquidate_position_v2` (6.3) — cross branch never references `c_m` |
| I-5 | Funding insulation: funding owed/received on an isolated position never touches `C_T` | `settle_funding` (7) |
| I-6 | Phase 2 single-isolated cap: at most one position per trader has `c_m > 0` | `set_position_isolated` rejects when a sibling already has `c_m > 0` |
| I-7 | Cash conservation on transition: `set_position_isolated(amount)` decreases `C_T` by `amount` and increases `c_m` by `amount` (atomic, no intermediate observable state) | `set_position_isolated` handler |
| I-8 | Reverse transition: `set_position_cross()` increases `C_T` by `c_m` and zeroes `c_m`, then runs cross health check | `set_position_cross` handler |

Tests covering these invariants:

- `risk::isolated_margin_tests::*` — unit tests for I-1, I-2, the
  unified dispatch, and the `assess_margin_split` contract.
- `tests/proptest_*.rs` — randomised stress over `assess_margin` /
  liquidation flow (Phase 2: extend to isolated paths — see #3 in
  the Phase 2 punch list).
- Integration tests in `tests/integration.rs` cover the on-chain
  side of `deposit_collateral`, `withdraw_collateral`,
  `liquidate_position_v2`, and now exercise the unified dispatch
  via the existing happy paths.

## 10. Versioning

This document tracks the on-chain risk model as of commit `550624e`
("feat: isolated-margin Phase 2 — split risk + per-bucket reward/
funding routing"). Future invariant changes — particularly resolving
§8 gaps — should update the corresponding numbered sections and the
invariant table in §9.
