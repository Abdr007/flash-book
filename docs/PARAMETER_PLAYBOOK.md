# Parameter Playbook

Recommended parameter ranges for Flash Book markets, organized by
asset class. Use these as starting points; tune based on observed
volatility and flow.

## Reference asset classes

| Class | Examples | Daily σ (typical) | Liquidity profile |
|---|---|---|---|
| **Tier-1 major** | BTC, ETH | 2–4% | Deep, low fragmentation |
| **Tier-2 major** | SOL, BNB | 3–6% | Moderately deep |
| **Mid-cap** | DOGE, MATIC, AVAX | 5–10% | Variable |
| **Long-tail** | meme coins, low-cap alts | 10–50% | Thin, gappy |

## Recommended `MarketParams`

### Tier-1 major (BTC, ETH)

```rust
MarketParams {
    tick_size:                          1,                  // 1e-3 USD (3 decimal precision)
    base_lot_size:                      100_000,            // 0.0001 BTC
    quote_lot_size:                     1,
    min_base_lots:                      1,                  // ~$0.10 minimum order
    max_leverage:                       50,
    maintenance_margin_ratio_bps:       50,                 // 0.5%
    initial_margin_ratio_bps:           100,                // 1.0%
    max_oi_base_lots:                   1_000_000_000_000,  // ~$1B
    concentration_threshold_lots:       100_000_000_000,    // $100M positions = whale
    concentration_extra_mmr_bps:        100,                // +1% for whales
    oracle_staleness_max_seconds:       30,
    oracle_confidence_max_bps:          50,                 // 0.5% max conf
    oracle_band_bps:                    2_000,              // 20% — vol-adaptive
    funding_period_seconds:             3_600,              // hourly
    funding_per_period_max_bps:         50,                 // 0.5%/hour cap
    mark_ema_alpha_bps:                 2_000,              // 20% per fill
    mark_max_change_bps:                500,                // 5% clamp
    liquidation_cooldown_slots:         50,                 // ~20 seconds
    liquidation_penalty_bps:            150,                // 1.5%
    flp_max_growth_per_batch_bps:       100,                // 1%
}
```

### Tier-2 major (SOL, BNB)

Same shape, tighter caps and higher fees:

```diff
- maintenance_margin_ratio_bps:       50,
+ maintenance_margin_ratio_bps:       75,                  // 0.75%
- initial_margin_ratio_bps:           100,
+ initial_margin_ratio_bps:           150,                 // 1.5%
- max_leverage:                       50,
+ max_leverage:                       25,
- liquidation_penalty_bps:            150,
+ liquidation_penalty_bps:            200,                 // 2.0%
```

### Mid-cap

```diff
- maintenance_margin_ratio_bps:       50,
+ maintenance_margin_ratio_bps:       200,                 // 2.0%
- initial_margin_ratio_bps:           100,
+ initial_margin_ratio_bps:           500,                 // 5.0%
- max_leverage:                       50,
+ max_leverage:                       10,
- liquidation_penalty_bps:            150,
+ liquidation_penalty_bps:            300,                 // 3.0%
- oracle_confidence_max_bps:          50,
+ oracle_confidence_max_bps:          200,                 // 2.0%
```

### Long-tail

```diff
- maintenance_margin_ratio_bps:       50,
+ maintenance_margin_ratio_bps:       1_000,               // 10%
- initial_margin_ratio_bps:           100,
+ initial_margin_ratio_bps:           2_000,               // 20%
- max_leverage:                       50,
+ max_leverage:                       5,
- liquidation_penalty_bps:            150,
+ liquidation_penalty_bps:            500,                 // 5.0%
- oracle_confidence_max_bps:          50,
+ oracle_confidence_max_bps:          500,                 // 5.0%
- oracle_band_bps:                    2_000,
+ oracle_band_bps:                    5_000,               // 50%
```

## Envelope config

The envelope inequality must hold:

```
price_funding_loss_N + liq_fee_N ≤ mm_req_N
```

for every notional N. The `prove_envelope` ix checks this at init.

### Recommended envelope params

| Asset | `max_price_move_bps_per_slot` | `max_accrual_dt_slots` | `maintenance_bps` | Total budget |
|---|---|---|---|---|
| Tier-1 | 14 | 100 | 3_000 (30%) | 14% (with 30% MMR ⇒ safe) |
| Tier-2 | 20 | 100 | 3_000 (30%) | 20% |
| Mid-cap | 30 | 80 | 4_000 (40%) | 24% |
| Long-tail | 50 | 60 | 5_000 (50%) | 30% |

For each, `liquidation_fee_bps = 50`, `min_liquidation_abs_lots = 1`,
`min_nonzero_mm_req_lots = 100`, `max_abs_funding_e9_per_slot =
10_000`.

## H-haircut config

| Asset class | `h_min_slots` | `h_max_slots` | Initial Residual |
|---|---|---|---|
| Tier-1 | 10 (~4s) | 200 (~80s) | sum of expected fees over 1d |
| Tier-2 | 25 (~10s) | 500 (~200s) | sum of expected fees over 1d |
| Mid-cap | 50 (~20s) | 1_000 (~400s) | sum of expected fees over 1d |
| Long-tail | 100 (~40s) | 2_500 (~17min) | sum of expected fees over 1d |

Longer warmup for thinner markets reduces oracle-spike attack
surface.

## OI-scaled MMR

For each market:
- `oi_mmr_slope_bps_per_million_lots`: 50–200 depending on liquidity
- `oi_mmr_max_extra_bps`: cap at 500 (+5% MMR) for tier-1, 1000 for
  long-tail

## Insurance fund targets

| Asset class | Target balance | Replenish trigger |
|---|---|---|
| Tier-1 | 5% of average daily OI | NAV > 1.05× target |
| Tier-2 | 7% of average daily OI | NAV > 1.05× target |
| Mid-cap | 10% of average daily OI | NAV > 1.05× target |
| Long-tail | 20% of average daily OI | NAV > 1.10× target |

`pause_threshold_quote_lots = 50%` of target. When insurance drops
below this, auto-deleverage becomes eligible.

## Tuning workflow

1. **Initialize** market with the recommended params for its asset class.
2. **Observe** for 1 week: volatility, OI growth, liquidation count,
   funding rate range, FLP NAV.
3. **Adjust** parameters via `update_market_params`. Each update is
   logged via `MarketParamsUpdatedEvent`.
4. **Re-prove envelope** with `set_envelope_config` if scenarios change.
5. **Burn authority** (`burn_market_authority`) only after multiple
   months of stable operation. One-way; no further tuning possible.

## Things to never change post-launch

These fields are layout-immutable (enforced in
`update_market_params`):

- `tick_size`
- `base_lot_size`
- `quote_lot_size`
- `min_base_lots`

Migration to a new market is the only way to change these. Plan them
carefully at init.

## Things to change only with positions closed

These mutate live exposure in unsafe ways:

- `maintenance_margin_ratio_bps` increase — would mass-liquidate
- `initial_margin_ratio_bps` decrease — opens leverage above
  intended cap
- `max_leverage` decrease — same

Best practice: announce parameter change > 24h in advance; transition
market through `PostOnly` for the change window; re-enable `Active`
after.

## Reference: live mainnet venues for comparison

When tuning, compare against:

- **Hyperliquid**: per-asset parameter pages (UI-only, no API).
- **Drift**: `Custody.PricingParams` in their on-chain Pool accounts.
- **dYdX**: governance proposals reference real param history.
- **GMX V2**: `DataStore.getUint(Keys.X(market))` direct on-chain reads.

Flash Book is the only protocol shipping the **envelope inequality**;
the other DEXes have ad-hoc parameter validation. The envelope makes
mis-configured markets structurally impossible — a strong defense
against operator error.
