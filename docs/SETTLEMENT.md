# Settlement & Compute

How a fill goes from a book crossing to a position mutation, why settlement
can never be forged, resized, or replayed, and what the engine costs in
compute units — with the commands that reproduce every number below.

Related: `../ER_TRUST_BOUNDARY.md` (where matching runs and what is trusted),
`MATH.md` / `MARGIN_MATH.md` (the position and margin arithmetic settlement
applies).

---

## 1 · Settlement authenticity — the fill-commitment ring

The fill lifecycle is split across two instructions:

| Stage | Instruction | Where it runs |
|-------|-------------|---------------|
| Match: walk the hypertree, decrement/remove maker orders, produce fills | `place_taker_order` | on-chain (L1, or the delegated ER) |
| Settle: mutate collateral / position / PnL from the fill's economics | `apply_fill` / `apply_lp_fill` | base layer |

Matching is trustless — the book is mutated on-chain and fills are a
deterministic function of book state. Settlement takes the fill's economics
as instruction arguments, so on its own it would trust the caller. The
**fill-commitment ring** closes that gap by binding settlement to the
matcher's on-chain output:

```
 place_taker_order (matcher)                 apply_fill (settlement)
 ───────────────────────────────                ────────────────────────
 for each fill it produces:                     for each fill it settles:
   c = keccak(fill preimage)      ──ring──▶        c' = keccak(supplied args)
   ring.push(c); produced += 1                     require c' == oldest pending slot
                                                   ring.pop(); settled += 1
```

- The commitment is a keccak hash over the fill's full economic content —
  `(market, taker, taker_side, maker, size_lots, price_ticks,
  taker_sub_index, maker_sub_index, produced_index)` — domain-separated and
  bound to its production index, so every commitment is unique and
  order-locked.
- **Authenticity.** `apply_fill` recomputes the commitment from the supplied
  arguments and requires it to equal the oldest pending ring slot. A fill
  that the matcher never produced cannot hash to a committed slot;
  fabrication is rejected (`FillNotCommitted`).
- **No double-settle, no replay.** The FIFO pop advances `settled`; a slot
  is consumed exactly once. Independently, each settlement must supply the
  exact next per-market `fill_seq` (`advance_settlement_seq`, machine-proven),
  so replays and skipped sequence values reject before state mutation.
- **Ordering.** Settlement order must equal production order — the ring pops
  FIFO.
- **Backpressure, not overwrite.** Once `produced − settled` reaches the
  ring cap, `place_taker_order` reverts (`FillRingFull`). A slow
  settlement keeper stalls new matching on that market (a liveness bound);
  it can never lose or corrupt a committed fill.

### The account

The ring is a raw PDA `[fill_commit, market]`: a 64-byte header
(discriminator, `produced: u64`, `settled: u64`, `cap: u32`, bump,
`market: Pubkey`) followed by `cap × 32` keccak slots. It is allocated by
`init_fill_commitment(cap)` and delegated to the ER together with the market
book (`delegate_fill_commitment` / `commit_fill_commitment` /
`commit_and_undelegate_fill_commitment`), so the matcher writes it wherever
matching runs and it travels back to L1 with the book.

Arming is **sticky**: once `init_fill_commitment` arms a market, settlement
requires the ring — the optional-account bypass does not exist for armed
markets, and the flag can never be cleared.

### Two hard walls (design constraints, not bugs)

1. **Settlement can neither reject nor resize a committed fill.** A fill is
   a two-sided exchange already committed to the FIFO ring; rejecting it at
   settlement wedges the ring, and partial application breaks two-sided
   conservation. Every economic policy (margin, reduce-only, caps) is
   therefore enforced at **intake or match time**, never at settlement.
2. **Positions are read-only clones on the ER.** The matcher may write only
   delegated accounts (market, book, ring, outbox). Anything the matcher
   must track per-position across the match→settle gap has to live inside a
   delegated account — which is exactly how reduce-only in-flight tracking
   works (§3).

### The LP path

LP fills are quotes against pool liquidity — there is no resting order to
commit, so they cannot ride the ring slot-for-slot. Authenticity is an
**oracle-anchored band**: an honest LP fill always prices within the
quoter's spread of fair value, so `apply_lp_fill` requires the fill price
to lie within `LP_MAX_FILL_DEVIATION_BPS` (300 bps = 3%) of the **fresh**
oracle price (`LpPriceOutsideBand` otherwise). The band anchors to the
oracle — fresh at settlement and immune to quoter-input drift — and caps
what a compromised sequencer could extract per (replay-guarded) fill at 3%
of notional. Exact re-derivation of the quote at settlement is deliberately
not attempted: the quote is a function of pool state that drifts between
quote time and settle time, so re-derivation would reject legitimate fills.
Machine-checked: the band accepts the oracle price and rejects gross
mispricing (Kani), and the ring's state machine carries proofs that
`settled ≤ produced`, depth is bounded by the cap, an uncommitted fill
cannot settle, and a consumed slot cannot settle twice.

---

## 2 · Fill outbox & batch caps

Fill **data** reaches the settlement keeper either through the transaction
log or through an on-chain outbox account. The choice sets the per-tx batch
cap.

**Why the log bounds the cap at 96.** `FillBatchEvent` carries
`86 + 57·N` raw bytes for `N` fills, and the runtime caps all program-log
output at 10,000 bytes per transaction with base64 inflation (×1.333) on
event data — one event overflows at ~125 fills and the tail is **silently
truncated**. A truncated-but-crossed fill is unsettleable: the book is
mutated and its commitment is in the ring, but the keeper never sees the
data, so the ring slot never pops and settlement wedges. The log-mode cap
(`MAX_MATCH_BATCH_ORDERS` = 96) keeps ~2.3 KB of headroom below that
cliff. Chunked emission makes this worse, not better — each chunk repeats
the event header and consumes more of the same 10 KB budget.

**The outbox removes the log from the data path.** `FillOutboxAccount` is a
second raw PDA `[fill_outbox, market]` holding one 96-byte data slot per
fill, addressed by the **ring's own cursors** (the outbox has no independent
cursor — it is a parallel array the ring already indexes). The matcher
writes each slot directly into the borrowed account data in the same loop
that pushes the commitment — no `Vec`, no serialization, no log — and emits
a slim, fixed-size `FillBatchOutboxEvent` (`produced_from`/`produced_to`) as
a wake-up hint. The keeper reads the account. Security properties:

- **Written only by the matcher** (program-signed PDA). The sequencer can
  only read it; planted data is impossible, and even hostile outbox contents
  could not settle a fill the ring didn't commit — `apply_fill` still gates
  on the keccak ring and does not read the outbox.
- **Never clobbers an unconsumed slot.** The slot at `produced % cap` is
  overwritten only after `settled` has passed it; the ring's backpressure
  bounds depth. No-silent-overwrite is Kani-proven.
- **Fail-closed sizing.** The matcher refuses to write an outbox whose cap
  is below the ring cap, so outbox matching stays inert until the outbox has
  been grown to cover the ring.

**The cap is a per-market knob**, chosen once at `init_fill_commitment(cap)`
with `cap ∈ [96, 256]`; `init_fill_outbox` takes no argument and sizes the
outbox (and `market.max_batch_orders`) from the ring cap — the ring is the
single source of truth.

| Configuration | Cap | Property |
|---|---|---|
| No outbox (log mode) | 96 | log-safe by construction |
| Outbox, `cap ≤ 105` | ≤ 105 | fully ER-capable — book + ring + outbox all delegate in one CPI (the delegate buffer is created at full account size in one CPI, bounded at 10,240 B) |
| Outbox, `cap ≤ 256` | ≤ 256 | L1 deep-sweep; a 24,640 B outbox exceeds the one-CPI delegate-buffer bound, so it stays on L1 |

Account lifecycle follows create-then-grow (a CPI can grow an account by at
most 10,240 B per instruction): `init_fill_outbox` allocates 105 slots, then
`grow_fill_outbox` raises the cap (105 → 211 → 256 in two grows).
`grow_fill_outbox` requires the outbox to be **drained** (mirrored cursors
equal) — a non-drained grow would remap every slot's `idx % cap` position.
Rent at cap 256 is ≈ 0.17 SOL, authority-funded.

---

## 3 · Reduce-only enforcement

A reduce-only order must never grow or flip the position it closes. Because
settlement can never reject or resize a committed fill (§1), all of this is
enforced at injection and match time. Three layers compose:

1. **Injection-time cumulative capacity clamp (every market).** Before
   `execute_trigger_order` injects a reduce-only close order, it sums the
   position's existing resting reduce-only orders (same trader, sub-index,
   and close side) and clamps the new order so the **total resting
   reduce-only size can never exceed the position size**. Any set of
   independent exits — two standalone stops, scale-out legs, a bracket
   stop-loss plus an unrelated stop — cannot sum past the position.
   Scale-out is preserved (partial exits summing to ≤ the position all
   fit); only genuine over-capacity is trimmed. Injected close orders also
   carry a bounded TTL (`REDUCE_ONLY_TRIGGER_ORDER_TTL_SLOTS`, ~5 min), so a
   close order whose position has since closed can never rest indefinitely
   and later fill.

2. **Per-walk cap.** Within one `place_taker_order` walk, a crossed
   reduce-only maker is capped to its reducible size, and multiple
   reduce-only orders on the same position share one decremented in-memory
   entry — a single taker call can never over-reduce.

3. **In-flight tracking across the match→settle gap.** Between a fill's match and its settlement the `PositionAccount`
   still shows the pre-reduction size, on L1 and on the ER clone. Without
   further state, two takers crossing the same position's reduce-only orders
   in separate calls inside that gap would each read the stale snapshot and
   collectively over-reduce — and the resulting flip would be a maker-side
   open at settlement, where no intake margin gate can run. The settlement
   layout closes this by **co-locating a per-position
   reduce-in-flight tracker inside the fill-commitment account itself**:
   - The tracker commits **atomically with the ring** — no separate account,
     no cross-domain ER seam, and no change to the fill preimage
     (authenticity is untouched).
   - The matcher caps a reduce-only cross by
     `position − in_flight[position]` and adds the fill to in-flight; a
     second taker reading a stale position snapshot sees reducible capacity
     already consumed and caps to zero.
   - `apply_fill` releases the in-flight amount when the fill settles. The
     maker and sub-index are preimage-committed, so the position key being
     released is authentic.

**Invariant:** across all in-flight reduce-only fills of a position, total
reduction never exceeds the position size — a reduce-only order cannot flip
a position, within a walk, across walks, or across the match→settle gap.
Pinned by host tests (the capacity-clamp book scan), an end-to-end
BanksClient test (two stops, deferred settlement, second cross caps to
zero), and settlement-ring round-trip tests. Every freshly deployed market
uses this complete layout.

### Accepted residual — reduce-only intent vs. an orthogonal position change

All three layers bound reduce-only against the position *as the fill sees
it*. One residual is inherent to asynchronous settlement and is accepted,
not fixed: if a reduce-only maker fill is committed to the ring, and then —
in the window after the ring is undelegated to L1 but before `apply_fill`
drains that fill — the maker's position is independently taken to flat by
`liquidate_position` / `liquidate_portfolio` / `auto_deleverage`,
the committed fill settles against a now-flat position and opens the
opposite side. The reduce-only *intent* is violated.

This is bounded and fund-safe: fund conservation and OI balance hold (the
opposite side is a real two-sided fill, no value is minted); the opened
position is immediately liquidatable, so the system self-heals; the
magnitude is capped by the injected order size and the ~5-minute TTL. It
is not code-fixed because both candidate fixes break a hard wall —
clamping the fill at settlement breaks two-sided conservation (the FIFO
wall), and gating liquidation on a drained ring deadlocks liquidation
whenever the sequencer withholds a pending fill's preimage (a genuinely
bad position could not be liquidated). Match-time enforcement cannot see a
future L1 liquidation. See `ER_TRUST_BOUNDARY.md` §1.

---

## 4 · Sequencer read-path contract

The off-chain settlement keeper speaks this contract; nothing on-chain
assumes anything else about it.

**Mode routing.** A market is outbox-mode iff its `fill_outbox` PDA exists;
otherwise log-mode (`FillBatchEvent` parsing). Both modes can run
side-by-side indefinitely; cutover is per-market.

**Events are hints, accounts are truth.** `FillBatchOutboxEvent`
(`produced_from`, `produced_to`) is a latency hint only. Correctness derives
from the outbox header's `produced` cursor — events can be missed; the
account is durable.

**Read loop** (per outbox market, with a persisted `consumed` cursor):

```
acct     = getAccountInfo(fill_outbox)
produced = u64_le(acct.data[8..16])
cap      = u32_le(acct.data[24..28])
for idx in consumed .. produced:              # ascending = FIFO
    slot = decode(acct.data, 64 + (idx % cap) * 96)
    if slot.maker == Pubkey::default():  apply_lp_fill(slot, fill_seq = idx + 1)
    else:                                apply_fill(slot,     fill_seq = idx + 1)
    on success: consumed = idx + 1; persist(consumed)
```

Slot layout (96 B): `taker[0..32]`, `maker[32..64]` (all-zero ⇒ LP fill),
`size_lots u64 @64`, `price_ticks u64 @72`, `maker_id u64 @80` (bookkeeping
only), `taker_side @88`, `taker_sub_index @89`, `maker_sub_index @90`,
`taker_was_jit @91`, pad to 96. The reader validates the discriminator
(`FBoutbx\0`) and `market` binding before trusting slots, and re-reads `cap`
each pass (growth keeps absolute indices stable).

**`fill_seq`.** Exactly the next per-market sequence value; with a zero
initial nonce, `outbox_index + 1` is the natural gap-free choice. Settlement
order must equal production order (FIFO ring).

**Restart safety.** The account is the durable log: on restart, re-derive
the cursor and replay `consumed..produced`. The gap-free `fill_seq` guard
plus the ring's consume-and-clear make re-submits idempotent — no lost
fills, no double settlement.

**Backpressure.** Falling more than `cap` fills behind stalls new matching
on that market (`FillRingFull`) — liveness, never safety. Alert when
`produced − consumed` approaches the cap.

**ER interaction.** On a delegated market the keeper reads ER account state
(same layout) and settles on the ER; book, ring, and outbox are delegated
and committed **together** so any committed L1 snapshot is internally
consistent. Only the RPC endpoint differs.

---

## 5 · Compute profile (measured, reproducible)

Real SBF compute units, measured in `solana-program-test` (in-process bank
loading the compiled `.so` — exact SBF costs, no RPC). The book is expanded
to 630 nodes with a 512-slot ring, 511 resting bids, and a taker sweeping
doubling depths, on an **armed** market (every fill pays its keccak
commitment).

Reproduce:

```
cargo build-sbf --tools-version v1.52
SBF_OUT_DIR=$PWD/target/deploy cargo test -p clober --test integration \
    deep_book_matching_cu_curve -- --nocapture
```

### `place_limit_order` — CU vs insertion depth

| depth | CU | | depth | CU |
|------:|-----:|-|------:|-----:|
| 0 | 13,018 | | 255 | 13,750 |
| 1 | 13,091 | | 383 | 13,777 |
| 63 | 13,642 | | 510 | 13,724 |
| 127 | 13,696 | | **spread (all 511)** | **1,059 CU** |

13.0k–14.1k CU, flat across 511 price levels — O(log n) hypertree insertion
holds at depth, not just on an empty book.

### `place_taker_order` — CU vs levels crossed (armed)

| levels crossed | CU | CU/level |
|---------------:|------:|---------:|
| 1 | 15,951 | 15,951 |
| 8 | 23,754 | 2,969 |
| 16 | 33,293 | 2,080 |
| 32 | 52,559 | 1,642 |
| 64 | 90,929 | 1,420 |
| **96 (log-mode cap)** | **129,120** | **1,345** |
| 384 (request) | 129,200 | truncates gracefully to the cap |

Fixed base ≈ 14.7k CU; marginal ≈ 1.2k CU per additional level crossed,
**including** the per-fill keccak commitment. A full 96-level sweep runs in
the default 32 KiB heap with no heap-frame request; an over-cap request
truncates gracefully (the walk drops the residual) instead of failing.

### Deep sweep through the outbox

A 256-level armed single-tx sweep through the fill outbox measures
**292,089 CU in the default 32 KiB heap** (`fill_outbox_deep_sweep_256`) —
no heap-frame request, all 256 fills reconstructed from the outbox account,
and the omit-outbox path hard-rejected (`FillOutboxRequired`). Above the
200k default budget, so a deep-sweep taker attaches `SetComputeUnitLimit`
(well under the 1.4M ceiling). CU, not heap and not logs, is the binding
constraint on this path, and it is comfortable.

### Why the matcher is heap-frugal (and must stay that way)

The SBF bump allocator never frees. The matcher pre-sizes its single O(N)
`matches` buffer to the cap (one exact allocation — no doubling-realloc
leak), and the outbox write path serializes each fill directly into the
borrowed account window with no intermediate `Vec`. Three simultaneous
growing buffers is exactly the shape that exhausts the 32 KiB heap at depth;
any change to the walk must preserve the one-buffer discipline.
