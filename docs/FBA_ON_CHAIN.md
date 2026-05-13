# FBA / Walrasian Clearing on Chain — Scope & Design

This is the migration plan for porting the TypeScript reference
simulator's Frequent Batch Auction (FBA) Walrasian clearing into the
on-chain Anchor program. As of `v0.2.0` the on-chain matcher in
`programs/flash-book/src/lib.rs::place_taker_order_v2` is **continuous
CLOB on a hypertree** — mechanically the same as Hyperliquid / dYdX /
Phoenix. The TS simulator in `src/matcher.ts` has FBA Walrasian
clearing as a research artifact, but no on-chain ix calls it.

This document captures the gap, the design, the migration order, and
the effort estimate so the work can be picked up in a focused
dedicated session.

Same scope-discovery pattern as `docs/SUB_ACCOUNT_TRADING.md`.

## 0. Status

**Not started.** The TS simulator has the math (`src/matcher.ts`,
`tests/matcher.test.ts`). The on-chain code has none of it. Every
mention of "FBA" or "Walrasian clearing" in pre-v0.2.0 README /
COMPARISON drafts was aspirational. The rewritten v0.2.0 docs are
explicit: continuous CLOB on-chain, FBA in the simulator only.

## 1. What "FBA on-chain" means concretely

A batch interval (e.g. 50–200 ms) closes. The matcher gathers all
order arrivals that fell within the interval. It computes a single
**uniform clearing price `p*`** that maximises the total matched
volume:

```
D(p) = Σ { s_i : order_i is a bid AND limit_i ≥ p }   (demand at p)
S(p) = Σ { s_i : order_i is an ask AND limit_i ≤ p }   (supply at p)
V(p) = min(D(p), S(p))                                 (matchable volume)

p* = argmax_p V(p)
V* = V(p*)
```

All crossing orders fill at **the same price `p*`** — no within-batch
price discrimination. Orders that don't cross at `p*` rest in the
book as standard limit orders.

**Property (MEV-neutral within batch):** For any permutation of order
arrival within a batch, the demand/supply curves D(p)/S(p) are
unchanged (they sum over identical sets). So `p*` and `V*` are
permutation-invariant. No participant can profit from observing
another's order in the same batch — sandwich attacks are
mathematically impossible within a batch.

This is the property the project's earlier marketing claimed but
didn't implement.

## 2. Why this is hard to do on-chain

The continuous CLOB has one nice property the FBA loses: a fill
happens in the same transaction that triggered it. With FBA, an
order is submitted in tx A, the batch closes in tx B (the
`clear_batch` ix), and the fill happens then. This pushes complexity
into:

- **State.** A new persistent buffer per market that holds pending
  orders until the batch clears. The hypertree-backed
  `market_book` PDA already exists for resting orders; a new
  `PendingBatchBuffer` PDA holds the not-yet-cleared arrivals.
- **Compute.** The clearing-price solver runs on-chain per
  `clear_batch` call. The naive O(n log n) sort + sweep is
  acceptable up to ~1000 arrivals per batch given Solana's CU
  budget; beyond that the solver wants amortising tricks.
- **Determinism.** All math integer-only. Tie-break rules need to be
  documented exactly so off-chain simulators replicate on-chain
  behaviour bit-for-bit.

## 3. Design

### 3.1 New state — `PendingBatchBuffer`

Per-market PDA at `[b"pending_batch", market.key()]`. Layout:

```rust
#[account]
pub struct PendingBatchBuffer {
    pub bump: u8,
    pub market: Pubkey,
    pub batch_seq: u64,            // monotonic; increments on each clear
    pub opened_at_slot: u64,        // when current batch started
    pub order_count: u16,           // number of orders in buffer
    pub _pad: [u8; 5],
    pub orders: [BufferedOrder; MAX_PENDING_PER_BATCH],
}

#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BufferedOrder {
    pub trader: Pubkey,
    pub seq: u64,                   // arrival sequence within batch
    pub size_lots: u64,
    pub limit_ticks: u64,
    pub side: u8,                   // 0 long, 1 short
    pub flags: u8,                  // post_only, IOC, FOK, etc.
    pub sub_index: u8,              // Phase 2e
    pub _pad: [u8; 5],
    pub submitted_at_slot: u64,
}
```

`MAX_PENDING_PER_BATCH` = 256 to start (audit-tunable). At 80 bytes
per `BufferedOrder` that's ~20 KB of state per market, well within
account size limits.

### 3.2 Ix surface changes

```
place_limit_order_v2_fba(market, side, size, limit, flags, expires, sub_index)
  → ENQUEUES into PendingBatchBuffer instead of inserting into
    hypertree. Replaces the current immediate-insert behaviour.

place_taker_order_v2_fba(market, side, size, limit, flags, expires, sub_index)
  → Same — enqueues. IOC / FOK semantics now mean "clear at next
    batch close and cancel residual immediately" rather than "walk
    the book now".

clear_batch(market)
  → New ix. Permissionless. Reads PendingBatchBuffer + the resting
    book in market_book PDA. Runs the Walrasian solver to find p*.
    Emits the uniform-price fills as a FillBatchEvent. Residual
    pending orders that didn't cross at p* are migrated into the
    hypertree book as resting limits.
```

The existing `apply_fill` / `apply_flp_fill` pipeline is unchanged —
the sequencer continues to dispatch them per FillEntry.

### 3.3 Walrasian solver (integer arithmetic)

Inputs: `bids` (sorted descending by `limit_ticks`, then by `seq`)
and `asks` (sorted ascending by `limit_ticks`, then by `seq`).

```
Algorithm WalrasianClear:
  candidate_prices = unique({bid.limit_ticks ∪ ask.limit_ticks})
  best_V = 0
  best_p = 0
  for p in candidate_prices:
      D(p) = sum of bid.size_lots where bid.limit_ticks ≥ p
      S(p) = sum of ask.size_lots where ask.limit_ticks ≤ p
      V(p) = min(D(p), S(p))
      if V(p) > best_V:
          best_V = V(p)
          best_p = p

  // Tie-break: among all p that achieve best_V, pick the one closest
  // to the prior mark. If exactly two are equidistant, pick the
  // midpoint of [min, max] of the indifference interval if that
  // midpoint also achieves best_V, else pick the lower.
  candidates_with_best_V = [p : V(p) == best_V]
  p_star = tie_break(candidates_with_best_V, prior_mark)
```

`tie_break` is a separate pure function:

```rust
fn tie_break(candidates: &[u64], prior_mark: u64) -> u64 {
    candidates.iter().copied()
        .min_by_key(|&p| abs_diff(p, prior_mark))
        .unwrap()
}
```

The sweep is O(n log n) for the sort + O(n²) worst-case for the
candidate-price evaluation. With n ≤ 256 this is at most 256² =
65,536 comparisons — well under the BPF CU budget. For larger n we'd
amortise via the standard supply/demand cumulative-sum trick (O(n
log n) end-to-end).

### 3.4 Pro-rata fill allocation

Once `p*` is found, total matched volume `V* = min(D(p*), S(p*))`.
The matching side that is "long" (e.g. S(p*) > D(p*) — more asks than
bids at p*) has surplus orders that won't fill in full. We allocate
fills proportional to order size, with deterministic tie-break by
arrival sequence:

```
For the over-subscribed side:
  - All orders with limit strictly more favourable than p* fill in
    full (these are "in the money"; they would have crossed at any p
    ≥ their limit).
  - The order(s) at exactly limit == p* are filled pro-rata to size,
    tie-break by ascending arrival seq.
```

This matches Phoenix's pro-rata-at-clearing-price semantics.

### 3.5 Tie-break audit invariants

The math here is property-testable. Required proptests:

1. **Permutation invariance.** Generate a random batch; permute the
   arrival order; the clearing price and total fill volume must be
   identical. This is THE FBA property.
2. **Monotonicity in mark.** If the prior mark is shifted by δ, the
   chosen p* (when multiple are optimal) shifts in the same
   direction (assuming the optimal set spans the shift).
3. **Conservation.** Σ filled bid size = Σ filled ask size = V*.
4. **No phantom fills.** No bid with `limit_ticks < p*` is filled;
   no ask with `limit_ticks > p*` is filled.

Mirror these in `tests/proptest_fba.rs`. Target 2000 cases per
property.

### 3.6 Migration coexistence with the continuous CLOB

Two viable paths:

#### Path A — replace the continuous matcher

`place_taker_order_v2` and `place_limit_order_v2` route to the FBA
buffer. The hypertree `market_book` is used only for orders that
rested across batches. Cleaner but requires every consumer (bot,
sequencer, integration tests) to switch.

#### Path B — per-market FBA flag

Add `MarketParams.matching_mode: u8` (0 = continuous, 1 = FBA). On
init, markets pick. Coexistence is clean — the matcher dispatches
based on the flag. Lets the FBA roll out per-market with rollback.

**Recommendation: Path B.** Risk-managed rollout.

## 4. Effort estimate

| Slice | LOC | Notes |
|---|---|---|
| `PendingBatchBuffer` state + helpers | ~150 | bytemuck-pod struct, init ix, accessors |
| Walrasian solver (pure-Rust, no Anchor) | ~250 | The math + tie-break + pro-rata. Audited unit-test layer. |
| `place_limit_order_v2_fba` + `place_taker_order_v2_fba` | ~200 | Enqueue paths, fee/fund accounting unchanged |
| `clear_batch` ix | ~300 | Solver invocation + fill emission + residual migration to hypertree |
| `MarketParams.matching_mode` plumbing | ~100 | Per-market dispatch in existing handlers |
| Proptests (4 properties × 2000 cases) | ~250 | Permutation invariance, monotonicity, conservation, no-phantom-fills |
| Integration tests | ~400 | 4-5 batch scenarios end-to-end |
| SDK builders for the new ixs | ~150 | clear_batch helper, fba-mode market init |
| Docs | ~100 | MATH.md update with on-chain math, ARCHITECTURE update |
| **Total** | **~1,900** | |

Best executed across 5-7 commits:

1. State struct + helpers
2. Walrasian solver pure math + proptests
3. clear_batch ix + tests
4. place_*_fba enqueue paths
5. Matching-mode dispatch
6. Integration tests + SDK
7. Doc finalisation

## 5. What this does NOT include

- **MEV-resistant batching across protocols.** FBA gives within-batch
  MEV resistance only; cross-protocol MEV (e.g. atomic arb against
  Phoenix) still exists.
- **Commit-reveal.** That's a separate primitive (see
  `docs/COMMIT_REVEAL_ON_CHAIN.md`). FBA gives within-batch
  permutation invariance even with plaintext orders. Commit-reveal
  hides order intent even from the sequencer until the batch closes.
  Both together close the loop.
- **Auction-style price improvement.** The Walrasian clearing
  produces ONE price per batch. Mechanisms like CowSwap's batch
  auction with surplus capture would be a separate Phase 4 feature.

## 6. Compatibility & risk

The biggest risk is that FBA changes the trader's expected fill
behaviour — a trader who places a limit at $100 expects an immediate
fill at $99 if asks are at $99. Under FBA, the fill happens at the
batch's uniform clearing price, which could be $99.5. This is a
semantic difference, not a bug, but every UI / strategy / bot needs
to know the matching mode.

The per-market flag (Path B) gates this rollout. Each market is
opted in deliberately; until they are, behaviour is unchanged.

## 7. Versioning

This document is the scope-discovery artifact for FBA on-chain. When
the work ships, the implementing commits should reference back here,
and this document is updated section-by-section to "SHIPPED" the same
way `docs/SUB_ACCOUNT_TRADING.md` was for Phase 2c–2f.

The earliest target release for FBA-on-chain is `v0.3.0`. Until then,
the COMPARISON.md "honest weaknesses" section accurately states that
no on-chain FBA exists in the deployed code.
