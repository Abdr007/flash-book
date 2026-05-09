# Keeper Suite — Deployment and Operations Runbook

The Flash Book keeper suite (`bot/src/keepers.ts`) ships four off-chain bots
that watch chain state and trigger the corresponding on-chain instruction.
All four share a common `Keeper` base class with start/stop/tick semantics.

## What's in the suite

| Keeper | Watches | Fires | Cadence hint |
|---|---|---|---|
| `LiquidationKeeper` | (market, trader) pairs | `liquidate_position` when health ≤ threshold | seconds |
| `FundingKeeper` | (market, trader) pairs | `settle_funding` when |owed| ≥ minOwed | minutes |
| `InvariantMonitor` | markets | `verify_market_invariants`, alerts on breach | tens of seconds |
| `AtaCleanupKeeper` | (trader, mint) pairs | `close_trader_ata` when ATA empty | hours |

All four are **permissionless on-chain** — the underlying instructions don't
require a special signer. Multiple keeper instances coexist without
coordination; first-to-confirm wins, others fail cleanly with no double-debit.

## Discovery

Two modes:

### Operator-supplied watchlists (default)
Keepers take an explicit list of (market, trader) tuples. Operators wire
this from their own indexer (Postgres, Geyser, etc.). Predictable RPC
load — bounded by the watchlist size.

### Auto-discovery via `getProgramAccounts`
`bot/src/discovery.ts` exposes `discoverActivePositions` and
`discoverEmptyTraderStates`. Use these to populate watchlists from a slow
periodic scan (5 min cadence is reasonable). Anchor account discriminators
are computed inline so you don't need the IDL twice.

```ts
import { discoverActivePositions } from '@flash-book/bot';
const positions = await discoverActivePositions(connection, {
  marketFilter: SOL_PERP, // optional, narrows scan
  minSizeLots: 100n,      // skip dust
});
const watchlist = positions.map((p) => ({
  market: p.account.market,
  trader: p.account.trader,
}));
```

`getProgramAccounts` is bandwidth-heavy. Don't call it on every iteration —
cache and rebuild on a slow timer.

## Deployment topology

### Single-process (dev / small scale)
Run all four keepers in one bun process via the example CLI:

```bash
bun run bot/examples/keepers.ts \
  --rpc <URL> \
  --keypair <PATH> \
  --config keeper-config.json
```

`keeper-config.json` shape:
```json
{
  "liquidation": {
    "watchlist": [{ "market": "...", "trader": "..." }],
    "refreshMs": 5000,
    "healthThreshold": 1.0
  },
  "funding": {
    "watchlist": [{ "market": "...", "trader": "..." }],
    "refreshMs": 60000,
    "minOwedQuoteLots": "1000"
  },
  "invariant": {
    "markets": ["..."],
    "refreshMs": 30000
  },
  "ataCleanup": {
    "watchlist": [{ "trader": "...", "quoteMint": "..." }],
    "refreshMs": 600000
  }
}
```

### Multi-process (production)
For high-throughput keepers, run each keeper class as its own process. Reasons:

- **Liquidation keeper** is latency-sensitive — give it its own RPC
  connection and dedicated CPU. Run multiple instances on different RPCs
  for redundancy; only one wins each underwater position.
- **Funding keeper** is bursty — runs through the entire watchlist every
  N minutes. Separate process avoids contention with liquidation.
- **Invariant monitor** needs alerting hooks (PagerDuty, Slack). Wire
  `onAlert` to your incident pipeline.
- **ATA cleanup** is the lowest priority; can run on a small cron job
  rather than a long-lived process.

## Keypair + funding

Each keeper instance needs a Solana keypair to pay tx fees. Recommended:

- **Hot wallet** (small SOL balance for tx fees, no other privileges).
- **Top up daily** from a treasury wallet — keeper SHOULDN'T hold significant
  SOL idle.
- **No collateral** beyond what's strictly needed for the InvariantMonitor's
  caller_trader_state (init_if_needed creates one on first liquidation; the
  liquidation reward credits there). Withdraw rewards periodically.

## Telemetry

Wire `bot/src/telemetry.ts` `MetricsRegistry` into each keeper:

```ts
import { MetricsRegistry, TelemetryFlusher, HttpPushSink } from '@flash-book/bot';
const registry = new MetricsRegistry();
const sink = new HttpPushSink('http://prometheus-pushgateway/metrics/job/keeper');
const flusher = new TelemetryFlusher(registry, sink, 10_000);
flusher.start();

// Inside the keeper iteration:
registry.inc('keeper_actions_total', 'liquidations fired', {
  keeper: 'liquidation',
  market: market.toBase58(),
});
```

Metrics worth tracking:
- `keeper_actions_total{keeper, market}` — counter of fired txs
- `keeper_iterations_total{keeper}` — counter of loop ticks
- `keeper_errors_total{keeper, kind}` — counter of failures (rpc, tx, etc.)
- `keeper_active_positions{market}` — gauge from auto-discovery
- `keeper_invariant_breach_total{market, code}` — counter of S5/S4/etc breaches

## Operating the InvariantMonitor

`InvariantMonitor` is the most critical keeper. It's the protocol's
fail-loud signal that something is structurally broken (OI imbalance,
vault under-collateralization).

When it fires (`onAlert` callback runs):

1. **Confirm the alert** — call `verify_market_invariants` manually via
   the SDK to see the breach details (event log carries
   `InvariantBreachDetectedEvent`).
2. **Pause the market** — the on-chain ix attempts to set status to
   Paused, but the tx errors so the change rolls back. Operator must
   call `set_market_status(Paused)` via the authority explicitly.
3. **Investigate** — read the event log, check `oi_long_lots` vs
   `oi_short_lots`, look at recent fills.
4. **Remediate** — depending on the breach: governance ix to correct OI,
   manual ADL run, insurance fund top-up, etc.
5. **Resume** — only after root cause is fixed and tested.

## Failure modes + recovery

### Keeper process crashes
- All four keepers are stateless — restart and they pick up where they
  left off (state is on chain).
- Use `systemd` / `pm2` / Docker restart policy.

### RPC stalls
- Each keeper iteration is bounded by its own timeout (none built-in;
  caller wraps if needed). Stalled RPC means missed liquidations until
  reconnect.
- Run multiple keeper instances on different RPCs for the latency-
  sensitive paths.

### Tx fails (e.g. position already liquidated by another keeper)
- Caught + counted as `keeper_errors_total`. Iteration continues to the
  next item in the watchlist. No crash.

### Auto-discovery times out
- `getProgramAccounts` is the bandwidth-heavy call. If your RPC rate-
  limits, slow the discovery cadence. Watchlist updates lag rather than
  the keeper crashing.

## Security checklist

- [ ] Keeper keypair has minimum SOL balance (just enough for ~1 hour of
      tx fees). Top up via cron from a separate treasury.
- [ ] Keeper keypair is NOT the protocol authority. Liquidation /
      settle_funding / verify_invariants are permissionless; no privileged
      access required.
- [ ] InvariantMonitor's `onAlert` hooks an actual paging system. Don't
      just log to stdout in production.
- [ ] Watchlist source is trusted (your own indexer, not user input).
      Malicious watchlist entries can waste RPC + tx fees but can't
      compromise funds.
- [ ] All keepers have `dryRun` toggleable for staging environments. Use
      it before flipping a new keeper to live.
