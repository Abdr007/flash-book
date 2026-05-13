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

### 8.1 Realized PnL materialisation — RESOLVED (Phase 2g)

**Status: fixed.** The post-Phase-2g `apply_fill` and `apply_flp_fill`
handlers now materialise the realized-PnL delta into the right
collateral bucket on every fill, closing the gap this section
previously documented.

#### Mechanism

`apply_fill_to_position` (the pure-math helper at lib.rs:11314+) still
accumulates the realized-PnL delta onto `pos.realized_pnl_quote_lots`
as it did before — that field remains the per-position lifetime
realized-PnL tally for indexers.

The new piece is at the `apply_fill` and `apply_flp_fill` call sites
(lib.rs around line 3226 and 5295). Each handler now:

1. Snapshots `pos.realized_pnl_quote_lots` and
   `pos.collateral_quote_lots > 0` BEFORE the
   `apply_fill_to_position` call.
2. Reads the post-state and computes
   `delta = post_realized − pre_realized`.
3. Routes the delta to the correct bucket via
   `apply_realized_pnl_delta(delta, isolated, &mut position, &mut trader_state)`.

#### Routing rule (`compute_realized_pnl_routing`)

```
gain (delta > 0):
    isolated → position.collateral_quote_lots += delta   (checked_add)
    cross    → trader_state.collateral_quote_lots += delta (checked_add)
    Overflow → ArithmeticOverflow

loss (delta < 0):
    isolated → position.collateral_quote_lots = saturating_sub
               (the unpaid remainder is absorbed; the next health
                check will trip)
    cross    → trader_state.collateral_quote_lots -= |delta|
               (checked_sub; surfaces InsufficientCollateral rather
                than going negative)
```

The pure-math helper `compute_realized_pnl_routing(delta, isolated,
iso_collateral, cross_collateral)` returns `(new_iso, new_cross)` and
is covered by 11 unit tests in `mod realized_pnl_routing_tests`.

#### Why isolated losses saturate (same as `settle_funding`)

If a loss exceeds the per-position isolated bucket, we deliberately
saturate at 0 rather than bleed into the cross pool. The unpaid
shortfall is recovered through the standard liquidation flow:
`liquidate_position_v2` reads `position.collateral_quote_lots` (now
0), the stress lattice marks the position unhealthy, and the
synthetic close + insurance fund + ADL waterfall absorbs the
remainder. This keeps the I-3 "cross-pool insulation" invariant (§9)
intact even in the loss-realisation path — an isolated position's
losses cannot bleed back into the trader's cross collateral via the
fill path.

#### Why cross losses error instead of saturating

The cross-collateral path uses `checked_sub`. In principle the
pre-fill margin check at `place_limit_order_v2` /
`place_taker_order_v2` should have prevented the trader from taking a
fill they couldn't afford. If we ever reach a cross loss > pooled
collateral here, something else has gone wrong (a stale-mark
exploit, a bug in the margin gate); failing the fill is safer than
silently going negative or letting the protocol absorb the
shortfall.

#### What still doesn't materialise

The FLP-pool side of `apply_flp_fill` doesn't accumulate on
`pos.realized_pnl_quote_lots` (the FLP's PnL flows through the
`FlpMarketExposure` per-market entry and is captured in NAV walks);
no settlement is needed there.

`auto_deleverage` still writes the bankruptcy-price loss directly to
`underwater_trader_state.collateral_quote_lots` (lib.rs:5839+). For
isolated underwater positions this currently bypasses the
per-position bucket — the same gap MARGIN_MATH §8.3 describes for
ADL. Phase 2g fixed the normal-fill path; ADL routing for isolated
positions remains §8.3 follow-up work.

### 8.2 `apply_fill` fee routing

Taker fees and maker fees mutate `taker_trader_state.collateral_quote_lots`
and `maker_trader_state.collateral_quote_lots` regardless of whether
the underlying position is isolated. For full isolation the fee
debit/credit should land on `position.collateral_quote_lots` when the
position is isolated.

`ApplyFill` Accounts does not currently carry the position account
(only the trader-state accounts), so adding fee routing wants a ctx
expansion in a separate audited commit.

### 8.3 ADL settlement — RESOLVED (Phase 2h)

**Status: fixed.** `auto_deleverage` now routes the bankruptcy-price
loss and the counter-trader gain to the right collateral buckets:

```
underwater isolated → position.collateral_quote_lots is debited
                      (saturating_sub; the unpaid remainder is absorbed
                      by the insurance fund + ADL waterfall as before;
                      the cross pool is NEVER touched, preserving I-3)
underwater cross    → trader_state.collateral_quote_lots is debited
                      (saturating_sub, same as legacy ADL behaviour)

counter isolated    → position.collateral_quote_lots is credited
                      (checked_add; overflow → ArithmeticOverflow)
counter cross       → trader_state.collateral_quote_lots is credited
                      (checked_add; overflow → ArithmeticOverflow)
```

The pure-math helpers `route_adl_loss(isolated, loss, pos, cross)`
and `route_adl_gain(isolated, gain, pos, cross) -> Result<(u64, u64)>`
encapsulate the routing decision. Both are covered by 11 unit tests
in `mod adl_routing_tests`, including the critical
`isolated_adl_leaves_both_cross_pools_untouched` case that locks in
I-3 end-to-end for the ADL path.

#### Bankruptcy-price sourcing

The bankruptcy-price `bp` calculation is

```
long  : bp = entry - C / (size · tick)
short : bp = entry + C / (size · tick)
```

where `C` is the collateral backing the underwater position. Phase 2h
selects `C` from the per-position bucket if the position is isolated,
else from the cross pool:

```
C := if underwater.collateral_quote_lots > 0
     { underwater.collateral_quote_lots }
     else
     { underwater_trader_state.collateral_quote_lots }
```

This matters because using the cross pool's full balance to compute
`bp` for an isolated position would over-estimate the backstop —
the resulting `bp` would be too favourable to the underwater trader,
the counter-trader would be force-closed at a worse price than they
should be, and the loss attributed to the position would exceed
the isolated bucket. Sourcing `C` from the actual backing bucket is
the I-3-preserving math.

#### Realized-PnL bookkeeping

`underwater_trader_state.realized_pnl_quote_lots -= loss` and
`counter_trader_state.realized_pnl_quote_lots += gain` are still
written regardless of which bucket actually moved. These are
informational fields — indexers and UIs use them to display
lifetime PnL summaries; they don't represent spendable collateral.

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

This document tracks the on-chain risk model through Phase 2h (ADL
routing for isolated positions). With §8.1 and §8.3 resolved, the
remaining §8 entry is §8.2 (`apply_fill` fee routing — already
resolved in Phase 2b but kept as a section for the historical
chain). Future invariant changes should update the corresponding
numbered sections and the invariant table in §9.

## 11. Phase 2c — Position PDA migration

The Phase 2c follow-up commit migrated Position PDAs from being keyed
on the trader's wallet to being keyed on the trader_state PDA:

```
Pre-2c   : [POS_SEED, market.key(), wallet.key()]
Post-2c  : [POS_SEED, market.key(), trader_state.key()]
```

This is the foundation for sub-account trading (Phase 2d): each
TraderStateAccount — main or sub — now has its own distinct
PositionAccount per market. Pre-2c, main and sub would have aliased
onto the same position, defeating risk isolation.

Migration is provided as a one-shot per (wallet, market) ix:
`migrate_position_to_trader_state_key`. It reads the legacy position,
init's a new position at the trader_state-keyed address with the same
on-chain state (size, side, entry, funding indices, realized PnL,
isolated collateral, timing fields), closes the legacy position, and
refunds rent to the trader. The new account is `init` (not
`init_if_needed`) so a second migration attempt against an existing
new position fails — protects against accidental double-migration.

`docs/SUB_ACCOUNT_TRADING.md` covers the architectural rationale and
the remaining Phase 2d/2e work (RestingOrderV2 schema + matcher fill
routing) required for sub-accounts to PLACE orders rather than just
hold collateral.

The PDA change does NOT alter any margin-math invariants in §1–§9 of
this document. Position seeds are an account-derivation concern; the
risk model operates on PositionSnapshots which carry the same data
fields regardless of where the on-chain position lives.
