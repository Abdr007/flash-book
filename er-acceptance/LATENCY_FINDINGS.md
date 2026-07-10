# Track B — ER fill-latency benchmark: findings (devnet, reproduced)

**Verdict on the "sub-50ms fills" clause: NOT verified as a client-observed number.
Measured client round-trip ≈ 165–275 ms on public devnet infra — reported as the real
number, not rounded toward the claim.**

## What was built + confirmed (real, on live devnet)
- `er-acceptance/latency_benchmark.mjs` — genesis a fresh cap-105 market, delegate
  book+ring+outbox+market to the MagicBlock ER validator (`MAS1Dt9…`), fund a fresh
  maker + taker (0-position keypairs — the reused authority hit the real `MAX_POSITIONS`
  intake gate `TooManyOpenPositions/2323`, working as designed), then loop
  rest-bid → **timed** `placeTakerOrderV2` fill on the ER.
- **Real taker fills executed on the ER** (delegated market `FG8Yyh…` et al.), each a
  genuine on-book match: **compute units = 21,492** per fill (constant).

## Measured data (steady-state, before the public endpoint rate-limited)
| # | client round-trip (ms) | CU |
|---|---|---|
| warmup | 274.9, 271.0, 170.1, 166.3, 166.1 | 21,492 |
| sample | 165.7 | 21,492 |

Steady-state client round-trip clustered at **~165–170 ms**.

## Methodology (disclosed)
- Clock: `performance.now()` around `sendAndConfirmTransaction(…, "confirmed")` on the ER
  connection — **includes client↔ER network RTT**. The client is **not** co-located with
  the ER validator, so this is an **upper bound** on ER-side execution, not execution time.
- The ER-side *execution* is a fraction of a slot (CU 21,492 ≈ 1.5% of a 1.4M budget; the
  MagicBlock ER slot cadence is ~sub-50ms) — but that is **not** what this client harness
  measures.

## Why a full p50/p95/p99 distribution was NOT captured
Both public devnet RPCs (`api.devnet.solana.com` and `devnet-as.magicblock.app`)
rate-limit (**HTTP 429 "Too many requests for a specific RPC call"**) well below a
sustained ~15–30 tx benchmark; web3.js's confirmation poller throws uncaught on the burst
even with backoff. A clean distribution needs a **dedicated/paid RPC endpoint**
(Helius / Triton / a MagicBlock dedicated validator), not the free public endpoints.

## Honest status of the clause
- **Client-observed end-to-end fill latency ≈ 165–275 ms on public devnet infra** —
  network + rate-limit dominated. This does **NOT** support a client-experience
  "sub-50ms" claim, and I do not make one.
- The **ER execution** (the mechanism the claim is really about) is plausibly sub-50ms
  (tiny CU, sub-50ms slot cadence) but is **not reproduced with an artifact** here.
- **To earn "sub-50ms" honestly:** re-run this harness against a dedicated (non-rate-
  limited) RPC with a co-located client, OR capture the ER validator's own per-tx
  execution/slot timing. That is infrastructure/access, not code — the harness is ready.

## Reproduce
```
L1_RPC=https://api.devnet.solana.com ER_RPC=https://devnet-as.magicblock.app \
  SAMPLES=15 node er-acceptance/latency_benchmark.mjs
# (needs a dedicated RPC to complete a full distribution; the public one 429s.)
```
