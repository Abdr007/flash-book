# Pyth Oracle Integration

Clober native supports two oracle paths per market:

1. **Trusted `update_oracle`** — authority writes raw values. Acceptable for devnet/testnet, **never for mainnet**.
2. **Permissionless `update_oracle_from_pyth`** — reads a real Pyth `PriceUpdateV2` account on-chain. Validates feed_id, staleness, and confidence. Anyone can call.

This doc shows how to switch a market to the Pyth path.

## One-time setup per market (authority)

```typescript
import { CloberClient } from './client';
// Pyth feed IDs — fetch from https://www.pyth.network/developers/price-feed-ids
// SOL/USD mainnet:
const SOL_USD_FEED_ID = Buffer.from(
  'ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d',
  'hex',
);

const ix = await client.initMarketOracleConfigIx({
  authority: marketAuthority.publicKey,
  market: solUsdcMarket,
  pythPriceFeedId: SOL_USD_FEED_ID,
  maxStalenessSeconds: 30,        // reject pulls older than 30s
  maxConfidenceBps: 100,           // reject pulls with conf/price > 1%
  tickDecimals: 3,                 // 1 tick = $0.001; Pyth exponent is -8, so scale = -8+3 = -5
});
await send(tx);
```

## Continuous oracle updates (anyone)

The Pyth Solana Receiver maintains `PriceUpdateV2` accounts on-chain. To pull the latest price into your market:

```typescript
// Fetch the latest PriceUpdateV2 address for SOL/USD (from Pyth Hermes or
// an indexer). Each shard has a sponsored permanent account.
const priceUpdateAccount = new PublicKey('...');

const ix = await client.updateOracleFromPythIx({
  caller: keeper.publicKey,
  market: solUsdcMarket,
  priceUpdate: priceUpdateAccount,
});
await send(tx);
```

The ix validates on-chain:
- `feed_id` in the `PriceUpdateV2` account matches the one in the market's `MarketOracleConfig`
- `publish_time` is within `maxStalenessSeconds` of the current slot's Unix time
- `conf / price` is within `maxConfidenceBps` (in bps)

On success, it writes the price to `market.oracle_price_ticks`. The mark-engine's dual-source health gate (native) will immediately see the new oracle price.

## Tick scaling explained

Pyth quotes prices as `(price: i64, exponent: i32)` where `real_usd = price * 10^exponent`. SOL/USD typically has `exponent = -8`, so `price = 9_995_000_000` means $99.95.

Clober stores `oracle_price_ticks` as a raw u64 where `1 tick = 10^(-tick_decimals)` USDC. With our default `tickDecimals = 3`:
- 1 tick = $0.001
- $99.95 = 99,950 ticks

The conversion is:
```
ticks = pyth_price * 10^(pyth_exponent + tick_decimals)
      = 9_995_000_000 * 10^(-8 + 3)
      = 9_995_000_000 * 10^-5
      = 99,950 ✓
```

If the scale doesn't match, adjust `tickDecimals` per market. Markets with very low-value bases (e.g. SHIB/USDC) may want `tickDecimals = 8` so 1 tick = $0.00000001.

## Off-chain operator: who pulls?

`update_oracle_from_pyth` is **permissionless** — anyone can call it. Recommended: run a keeper bot that:
1. Subscribes to the `MarkPriceDriftEvent` (fires when `|mark - oracle| / oracle > drift_alert_bps`)
2. On any drift event, fetches the latest `PriceUpdateV2` address and calls `updateOracleFromPythIx`
3. Optionally also pulls every N slots even without drift to keep oracle fresh

For mainnet, also subscribe to `OracleTooStale` errors emitted by trade ixs and pull when seen.

## Devnet vs mainnet

| Item | Devnet | Mainnet |
|---|---|---|
| Pyth program | `pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT` | same |
| SOL/USD feed ID | `0xef0d…b56d` | same |
| Sponsored PriceUpdateV2 | varies by shard | varies by shard |
| Use `update_oracle` (trusted)? | optional, for tests | NO — disable post-launch |

## Mainnet hardening checklist

After installing the Pyth binding via `init_market_oracle_config`:
- [ ] Disable `update_oracle` for the market (authority op — keep ix but never call it)
- [ ] Set `maxStalenessSeconds` to match the asset's Pyth update cadence (30s is conservative)
- [ ] Verify `tickDecimals` matches the asset's price magnitude (BTC may need different)
- [ ] Run a keeper bot pulling every 5–10 slots
- [ ] Wire `MarkPriceDriftEvent` → keeper alert
