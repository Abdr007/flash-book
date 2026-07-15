# Track B — ER fill-latency benchmark: findings (devnet, reproduced)

**Verdict on the "sub-50ms fills" clause: NOT met as a client-observed round-trip.
A COMPLETE distribution over 20 real ER taker fills gives p50 = 161.5 ms — reported
as the real number. The ER *execution* (CU 21,492 ≪ a slot) is plausibly sub-50ms,
but is not observable as a round-trip from a remote client and is not claimed.**

## Reproduced distribution (dedicated Helius devnet L1 + MagicBlock devnet ER)
Genesis a fresh cap-105 market, delegate book+ring+outbox to the ER validator
(`MAS1Dt9…`), fund a fresh maker+taker, then time 20 real `placeTakerOrder` fills.

| metric | value |
|---|---|
| samples | 20 (+4 warmup) |
| **p50** | **161.5 ms** |
| p90 / p95 / p99 | 164.7 ms |
| min / max | 153.6 / 164.7 ms |
| mean | 160.4 ms |
| compute units / fill | 21,492 (constant) |

Raw rows + methodology: `er-acceptance/latency_results.json`.

## Why the earlier public-endpoint run couldn't finish — and this one did
The public `api.devnet.solana.com` L1 rate-limited (HTTP 429) during genesis and
crashed the run before the measurement loop. A **dedicated Helius devnet RPC**
(`devnet.helius-rpc.com`) as `L1_RPC` removed the genesis 429, so the run completes
and yields a full distribution. (The **fills** are still submitted to the MagicBlock
devnet ER endpoint `devnet-as.magicblock.app` — Helius hosts the L1, not the ER — so
the measured latency is client↔ER, not client↔Helius.)

## Methodology (disclosed)
- Clock: `performance.now()` around `sendAndConfirmTransaction(…, "confirmed")` on the
  ER connection — **includes client↔ER network RTT**.
- This client (a CI sandbox) is **not co-located** with the ER validator: a raw
  `getSlot` round-trip to the dedicated RPC is itself ~400–540 ms, so the client
  round-trip is **network-RTT-dominated**, an UPPER BOUND on ER-side execution.
- The tight ~154–165 ms band (p99 ≈ p50) reflects a stable network path + the ER's
  ~sub-50ms slot cadence + the tiny 21,492-CU match — i.e. execution is a small,
  constant slice; the ~161 ms is transport, not compute.

## Honest status of the clause
- **Client-observed end-to-end fill latency: p50 = 161.5 ms** on devnet from a remote
  client. This does **NOT** support a client-experience "sub-50ms" claim, and none is
  made.
- To earn "sub-50ms" as a client number: run this harness from a client **co-located**
  with the MagicBlock ER validator (same region/data-center), or capture the ER
  validator's **own per-tx execution/slot timing**. That is deployment topology +
  operator telemetry, not code — the harness is ready.

## Reproduce
```
L1_RPC="https://devnet.helius-rpc.com/?api-key=<key>" \
ER_RPC="https://devnet-as.magicblock.app" SAMPLES=20 \
  node er-acceptance/latency_benchmark.mjs
```
