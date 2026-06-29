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
fill-commitment ring cap (256) stays ≥ the batch cap, as M-2 requires.

## Why the cap is 96 and NOT 256 — the program-log byte limit

After the heap fix, the obvious next step is "chunk the event emission to lift the
cap back to 256." **It can't be done with a log-based fill feed, and chunking makes
it worse.** This was investigated and ruled out from the runtime source, not
guessed:

- `FillBatchEvent` is an `#[event]`. Every fill's data (maker / size / price /
  sub-index) reaches the off-chain sequencer through the transaction log
  (`sol_log_data` → `stable_log::program_data`, which **base64-encodes** the
  payload, ×1.333).
- The Solana runtime caps **all** program-log output per transaction at
  `LOG_MESSAGES_BYTES_LIMIT = 10 * 1000` = **10,000 bytes**
  (`solana-svm-log-collector`), and `sol_log_data` routes through that bounded
  collector. Past the limit the log is **silently truncated.**

A single event is `86 + 57·N` raw bytes (8 disc + 32 market + 32 taker + 1 side +
8 taker_id + 4 vec-len + 1 taker_sub, then 57 per fill):

| N fills | log bytes (×1.333 + framing) | vs 10,000 |
|--------:|-----------------------------:|-----------|
| **96 (cap)** | ~7,670 | safe, ~2.3 KB headroom |
| 115 | ~9,100 | tight |
| 125 | ~10,010 | **over → tail truncated** |
| 256 | ~19,750 | **2× over → ~half the fills dropped** |

**Why truncation is a settlement wedge, not a cosmetic loss.** A truncated fill is
still crossed on-chain — the book is mutated *and* a keccak commitment is pushed to
the ring (§3.2). But the sequencer never sees the fill's data, so it can't call
`apply_fill`, so that ring slot never pops, so the ring can't drain. That is the
exact H-2 griefing class remediated this cycle. Hence the cap **must** stay
log-safe; 96 leaves margin.

**Chunking is strictly worse here.** Each chunk repeats the 86-byte event header,
so more chunks consume *more* of the 10 KB budget — chunked emission lowers the
log-safe fill count, it doesn't raise it. (It would fix the heap, but the heap is
not the binding constraint.)

**The only real path to 256+** is to stop carrying fill data in logs: write fills
into an on-chain *fill-outbox* account the sequencer reads via `getAccountInfo`
(accounts hold up to 10 MB, no log limit). That is an architectural change — new
PDA, rent, and a sequencer read-path switch coordinated with the off-chain stack —
deliberately **not** done as a matcher tweak. Given real taker crosses are almost
always 1–5 levels, 96 is ample headroom and the outbox is deferred until a concrete
deep-sweep demand exists.

Verified by `deep_book_matching_cu_curve` (the 384-level graceful-truncation case
is asserted). 435 lib + 67 integration tests pass.

## Update (2026-06-29) — the fill-outbox lifts the cap to 256

The "only real path to 256+" above is now **implemented** (see `FILL_OUTBOX_DESIGN.md`).
A market that arms an on-chain **fill-outbox** delivers each crossed fill's data
into a persistent PDA the sequencer reads via `getAccountInfo` — off the program
log entirely — so the 10 KB ceiling no longer bounds the cap. Such a market raises
its per-market `max_batch_orders` to 256.

Measured (`fill_outbox_deep_sweep_256`, real SBF): a **256-level single-tx sweep =
292,089 CU in the DEFAULT 32 KiB heap** — no heap-frame request — with all 256
fills reconstructed from the outbox account (cursor + slot data) and the
omit-outbox path hard-rejected (`FillOutboxRequired`). The matcher is heap-frugal on
this path (no `Vec<FillEntry>`; the fill data is written straight into the borrowed
account), so `matches` is the only O(N) heap. The no-silent-overwrite property is
Kani-proved (`outbox_no_silent_overwrite`). Markets without an outbox keep the
log-safe 96 default unchanged. 446 lib + 68 integration tests pass.
