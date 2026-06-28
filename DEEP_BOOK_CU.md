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
| 8 | 23,754 | 2,969 |
| 16 | 33,293 | 2,080 |
| 32 | 52,559 | 1,642 |
| 64 | 90,929 | 1,420 |
| **96 (cap)** | **129,120** | **1,345** |
| 384 (request) | 129,200 | — truncated to 96 |

**Fixed base ≈ 14.7k CU; marginal ≈ 1,191 CU per additional level** crossed,
*including* the §3.2 per-fill anti-fabrication keccak commitment. A full **96-level
single-tx sweep = 129k CU in the DEFAULT 32 KiB heap** (no heap-frame request). A
384-level request **truncates gracefully to the 96 cap** — no OOM-panic.

### Comparison (real mainnet competitor txns)

| Operation | Flash Book | Competitor |
|---|---|---|
| Place order | ~13.3k (flat to 511 deep) | Phoenix place/cancel batch 93k–182k |
| Sweep N levels | ~14.7k + 1.2k·N | Drift place-and-make budget 400k–800k |

## Finding → fix (heap-frugal matcher)

The first version of this benchmark surfaced a **reachable OOM panic**: a single
taker's three simultaneous heap buffers — the `matches` Vec, the `fills` Vec, and
the serialized `FillBatchEvent` — exhausted the **default 32 KiB SBF heap at ~100
crossed levels**, *below* the old `MAX_BATCH_ORDERS_PER_SIDE_V2 = 256`. The SBF
**bump allocator never frees**, so a doubling `matches` Vec also leaked every
intermediate buffer.

**Fixed** (commit in this branch):
1. **Pre-size `matches` to the cap** — one exact allocation, no doubling-realloc
   leak (the dominant heap cost of a deep sweep).
2. **Lower `MAX_BATCH_ORDERS_PER_SIDE_V2` to 96** so the three buffers
   (96 × ~175 B ≈ 25 KiB) fit the 32 KiB heap with margin.

Result: a taker crosses up to **96 levels in one tx in the default heap with no
heap-frame request**, and a deeper request **truncates gracefully** (the existing
`walk_truncated` path drops the residual) instead of OOM-panicking. The
fill-commitment ring cap (256) stays ≥ the batch cap, as M-2 requires. Deeper
single-tx crossings (toward 256) would need a heap-frame request *and* a
heap-frugal event (e.g. chunked emission) — a future optimization; 96 levels/tx is
ample (most crosses are 1–5).

Verified by `deep_book_matching_cu_curve` (the 384-level graceful-truncation case
is asserted). 435 lib + 67 integration tests pass.
