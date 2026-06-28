# Deep-Book Matching CU — Flash Book

Reproducible, real SBF compute-unit measurements of the matching engine under a
**deep, production (armed) order book**. Not single-order / shallow-book numbers —
the thing a fair reviewer asks for.

**Reproduce:**
```
cargo build-sbf --tools-version v1.52
BPF_OUT_DIR=$PWD/target/deploy cargo test --test integration deep_book_matching_cu_curve -- --nocapture
```
Runs in `solana-program-test` (in-process bank, loads the real `.so` → exact SBF
CU). Book expanded to 630 nodes, fill-commitment ring grown to 512, 511 resting
bids, a separate taker sweeping doubling depths. Self-contained, no RPC.

## Results

### `place_limit_order_v2` — CU vs insertion depth (book up to 511 levels deep)

| depth | CU | | depth | CU |
|------:|-----:|-|------:|-----:|
| 0 | 13,018 | | 255 | 13,750 |
| 1 | 13,091 | | 383 | 13,777 |
| 63 | 13,642 | | 510 | 13,724 |
| 127 | 13,696 | | **spread (all 511)** | **1,059 CU** |

**13.0k–14.1k CU, flat across 511 levels** → confirms **O(log n)** hypertree
insertion *at depth*, not just on an empty book.

### `place_taker_order_v2` — CU vs levels crossed (armed: +1 keccak commitment / fill)

| levels crossed | CU | CU/level |
|---------------:|------:|---------:|
| 1 | 15,951 | 15,951 |
| 2 | 16,268 | 8,134 |
| 4 | 18,816 | 4,704 |
| 8 | 23,754 | 2,969 |
| 16 | 33,293 | 2,080 |
| 32 | 52,655 | 1,645 |
| 64 | 91,121 | 1,423 |

**Fixed base ≈ 14.7k CU; marginal ≈ 1,193 CU per additional level** crossed,
*including* the §3.2 per-fill anti-fabrication keccak commitment. A 64-level
single-tx sweep = **91k CU**.

### Comparison (real mainnet competitor txns)

| Operation | Flash Book | Competitor |
|---|---|---|
| Place order | ~13.3k (flat to 511 deep) | Phoenix place/cancel batch 93k–182k |
| Sweep N levels | ~14.7k + 1.2k·N | Drift place-and-make budget 400k–800k |

## Honest finding (surfaced by this benchmark)

A single taker's `fills` Vec plus the `FillBatchEvent` clone exhaust the **default
32 KiB SBF heap at ~100 crossed levels** — the matcher OOM-panics — *below*
`MAX_BATCH_ORDERS_PER_SIDE_V2 = 256`. So the practical single-tx sweep ceiling is
**heap-bound**, not the 256 batch cap.

- `solana-program-test` does **not** honor a `RequestHeapFrame` ix, so the harness
  ceiling is ~64 levels (what's measured above).
- On the live runtime a client can request up to a 256 KiB heap frame, which
  should lift the ceiling toward the batch cap — **to be confirmed on the live
  runtime** (the test harness cannot).
- Recommended hardening (team decision): either lower the matcher `walk_limit` so
  deep crosses **truncate gracefully** (the audit's `walk_truncated` path) instead
  of OOM-panicking under the default heap, or make the matcher heap-frugal (avoid
  cloning all fills into the event) to keep the 256 cap usable without a heap
  request. Either removes a reachable panic; the latter preserves throughput.

This is reported, not hidden: a taker on a very deep book that doesn't request a
heap frame and tries to cross >~100 levels will fail. Most crosses are 1–5 levels,
so day-to-day impact is nil, but it's a real edge a deep-book reviewer should know.
