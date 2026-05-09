# Market-Maker Bot — Parameter Tuning Guide

The Flash Book MM bot (in `bot/`) implements Avellaneda-Stoikov inventory-aware
quoting + VPIN-scaled spread + drawdown kill switch. This guide explains the
knobs and how to tune them for your market.

## Top-level mental model

The bot computes one bid/ask pair per market per iteration:

```
fair_value     = mark × (1 - inventorySkewBpsPerUnit × inventory_fraction)
half_spread    = baseSpreadBps + vpinSpreadAlpha × VPIN + oiImbalanceCoef × |OI imbalance|
bid            = fair_value × (1 - half_spread / 10_000)
ask            = fair_value × (1 + half_spread / 10_000)
```

You're tuning **three things**: how wide to quote, how much to skew against
inventory, and when to refuse to quote at all.

## Spread parameters

### `baseSpreadBps` — the resting half-spread

Per-side spread off fair value, in basis points. Total spread = 2 × this.

| Market style | Suggested `baseSpreadBps` | Rationale |
|---|---|---|
| Major perp (SOL, BTC, ETH) | 5–15 | Tight spreads, lots of competing MMs |
| Mid-cap | 15–40 | Wider to compensate for thinner flow |
| Long-tail | 40–100+ | Wide — adverse selection risk dominates |
| Spot (defaultSpotMarketParams) | 5–10 | No funding, lower variance to hedge |

**Tradeoff**: wider = fewer fills but better PnL per fill. Tighter = more fills
but more adverse selection risk. Backtest against historical fills (use
`Backtester` in `bot/src/backtester.ts`) to find your sweet spot.

### `vpinSpreadAlpha` — how aggressively to widen on toxic flow

Multiplier on VPIN. When VPIN = 1.0 (max toxicity signal), the spread widens
by `vpinSpreadAlpha × 100%`. So `vpinSpreadAlpha = 0.5` means 50% wider at full
toxicity.

| Tolerance | Suggested | Behavior |
|---|---|---|
| Aggressive (always quote) | 0.0–0.2 | Largely ignores VPIN |
| Default | 0.5 | Balanced |
| Defensive | 1.0–2.0 | Pulls quotes hard during informed flow |

If you see your bot consistently picked off during news events, raise this.

### `oiImbalanceSpreadCoef` — directional imbalance widening

Multiplier on absolute OI imbalance fraction. Same scaling as VPIN.

Most operators set this to 0.05–0.1. Bigger values catch one-sided markets
(e.g. 90% long OI) but can over-react to natural skew.

## Inventory skew

### `inventorySkewBpsPerUnit` — how hard to push fair value against inventory

When you're long, fair value moves DOWN (more attractive ask, less attractive
bid → market wants to take you out of inventory). The number controls
intensity.

```
inventory_fraction = inventory_signed × mark × tick_size / capital
fair_skew_bps      = -inventorySkewBpsPerUnit × inventory_fraction
```

| Style | Suggested | Behavior |
|---|---|---|
| Mean-reversion | 50–100 | Light skew, holds position through chop |
| Hedge-immediately | 200–500 | Strong skew, dumps inventory fast |
| Default | 100 | Balanced |

If your inventory swings wildly and PnL is unstable, raise this. If you never
hit your inventory cap, lower it.

## Risk gates

### `maxInventoryLots` — hard cap per side

Position size at which the bot stops quoting on the side that would breach
it. Set based on your collateral × maximum acceptable leverage.

For a $100k collateral pool at 5x max leverage on SOL @ $100:
```
max_inventory_lots = (100k × 5) / 100 / lot_size = 5000 lots
```

### `maxDrawdownQuoteLots` — kill switch trigger

Negative number. When session realized PnL drops below this, the bot enters
kill-switch state: cancels all open orders, stops quoting until restart.

Standard rule: 5% of starting collateral. For $100k → `-5_000`.

The kill switch is your last line of defense against runaway losses (bad
config, oracle drift, market dislocation). Make it tighter than you think
you need; restarting is cheap, recovering capital isn't.

### `minCollateralQuoteLots` — collateral floor

Below this, the bot stops quoting (but doesn't trip the kill switch). Lets
you reduce inventory without the bot fighting you.

Set to ~10× your `quoteSizeLots` so you have headroom for fees and
mark-to-market fluctuation.

## Re-quote cadence

### `refreshMs` — iteration interval

Lower = lower latency, higher RPC + tx fee cost.

| Market | Suggested | Rationale |
|---|---|---|
| Calm major perp | 1000–5000 ms | Quote diff (`priceDiffBps`) skips most re-quotes |
| Active major perp | 250–500 ms | Fast enough to catch real moves |
| News-event / tactical | 100 ms | Aggressive, but watch tx fees |

With WebSocket subscriptions enabled (see `bot/src/subscriptions.ts`), you can
go to event-driven re-quoting where `refreshMs` is just a fallback.

### `priceDiffBps` / `sizeDiffBps` — quote-diff thresholds

How much prices/size must move before you re-quote. Skip == save tx fee.

For a calm market, `priceDiffBps = 5` (re-quote on 5+ bps moves) cuts tx
volume by ~10x vs always-replace.

## Strategy presets

The bot ships with no built-in presets — operators wire their own configs.
Start from one of these:

### Conservative LP-style MM
```ts
{ baseSpreadBps: 25, vpinSpreadAlpha: 1.5, inventorySkewBpsPerUnit: 200,
  oiImbalanceSpreadCoef: 0.1, quoteSizeLots: 1n,
  maxInventoryLots: 100n, maxDrawdownQuoteLots: -5_000n,
  minCollateralQuoteLots: 10_000n, priceDiffBps: 10, sizeDiffBps: 0 }
```

### Tight competitive MM
```ts
{ baseSpreadBps: 8, vpinSpreadAlpha: 0.5, inventorySkewBpsPerUnit: 100,
  oiImbalanceSpreadCoef: 0.05, quoteSizeLots: 5n,
  maxInventoryLots: 500n, maxDrawdownQuoteLots: -50_000n,
  minCollateralQuoteLots: 100_000n, priceDiffBps: 3, sizeDiffBps: 0 }
```

### Hedge-immediate scalper
```ts
{ baseSpreadBps: 15, vpinSpreadAlpha: 0.3, inventorySkewBpsPerUnit: 500,
  oiImbalanceSpreadCoef: 0.05, quoteSizeLots: 2n,
  maxInventoryLots: 50n, maxDrawdownQuoteLots: -2_000n,
  minCollateralQuoteLots: 5_000n, priceDiffBps: 0, sizeDiffBps: 0 }
```

## Validation workflow

1. **Backtest first.** Feed historical fill tape into `Backtester` with your
   proposed config. Check final PnL, fill count, max drawdown. Sweep one param
   at a time.
2. **Paper trade with `dryRun: true`.** The MM bot computes quotes and logs
   them but doesn't send tx. Run for a few hours to see if the quotes look
   reasonable in current market conditions.
3. **Live with conservative limits.** Start with `maxInventoryLots` 10x lower
   than your final target. Watch fills, PnL, hit rate for 24h. Scale up once
   you've validated the math holds in real flow.
4. **Hot reload.** Use `HotConfigReloader` (`bot/src/hot-config.ts`) so you
   can tighten the kill switch or pull quotes without restart if anything
   looks off.

## Anti-patterns

- **Don't set `vpinSpreadAlpha = 0` if you can't afford adverse selection.**
  VPIN is your toxic-flow alarm; ignoring it means getting picked off by
  informed traders during news.
- **Don't set `inventorySkewBpsPerUnit = 0`.** With no skew you ride
  position to the cap and then refuse to quote — you become a ratchet that
  only ever accumulates.
- **Don't run without a `maxDrawdownQuoteLots` floor.** Bugs happen.
- **Don't run without `priceDiffBps > 0` on a calm market.** You'll burn
  tx fees on every iteration with nothing to show for it.
