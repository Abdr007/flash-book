# P-D — off-chain sequencer cutover to the fill-outbox

**Status:** scope / not started. **Owner:** the off-chain sequencer service (NOT in
this repo — this is a contract spec for it). **Depends on:** P-A→P-C (done, deployed
to devnet). **Goal:** the sequencer reconstructs fills from the on-chain **outbox
account** (`getAccountInfo`) instead of parsing `FillBatchEvent` from transaction
logs, so a market can run the 256-deep batch cap without the 10 KB log truncation
that pins log-based markets at 96.

This is purely a **read-path** change. The settlement write path (`apply_fill` /
`apply_flp_fill`) is unchanged — same instruction, same args, same authenticity
gate. Nothing on-chain needs further work for P-D.

---

## 1. What the sequencer consumes today vs after

**Today (log path).** The sequencer captures each `place_taker_order_v2` tx
(Geyser / `logsSubscribe` / `getTransaction`), Borsh-decodes the `FillBatchEvent`
log (`market, taker, taker_side, taker_id, fills: Vec<FillEntry>, taker_sub_index`),
and emits one `apply_fill` per `FillEntry`. Bounded by the 10 KB log ⇒ ≤ ~96 fills.

**After (outbox path).** For an outbox-armed market the same tx emits the slim
`FillBatchOutboxEvent` instead (a wake-up hint), and the fill DATA lives in the
outbox account. The sequencer reads the account and emits `apply_fill` per slot. No
log size bound.

## 2. The on-chain contract (what the sequencer must speak)

### 2a. Wake-up event — `FillBatchOutboxEvent` (latency hint only)
```
market: Pubkey, taker: Pubkey, taker_side: u8, taker_id: u64,
taker_sub_index: u8, produced_from: u64, produced_to: u64
```
`[produced_from, produced_to)` are absolute fill indices this tx wrote. The event is
a hint — **correctness comes from the account's `produced` cursor**, never the event
alone (events can be missed; the account is durable truth).

### 2b. Outbox account `[b"fill_outbox", market]` — the data source
Raw PDA, disc `FBoutbx\0`. **Header (64 B):** `disc[0..8]`, `produced: u64 @8`,
`settled: u64 @16`, `cap: u32 @24`, `bump @28`, `market[32..64]`. **Slots** start at
offset 64, **96 B each**, slot `i` at `64 + (i % cap)*96`:

| off | len | field |
|----:|----:|-------|
| 0   | 32  | `taker` |
| 32  | 32  | `maker` — `Pubkey::default()` (all-zero) ⇒ FLP fill → `apply_flp_fill` |
| 64  | 8   | `size_lots` (u64 LE) |
| 72  | 8   | `price_ticks` (u64 LE) |
| 80  | 8   | `maker_id` (u64 LE — maker's resting order id; bookkeeping) |
| 88  | 1   | `taker_side` |
| 89  | 1   | `taker_sub_index` |
| 90  | 1   | `maker_sub_index` |
| 91  | 1   | `taker_was_jit` |

The absolute fill index of a slot is implicit in the read window (see §3); the
physical slot is `index % cap`.

### 2c. Settlement call assembly (per fill)
`apply_fill(size_lots, price_ticks, taker_side, taker_was_jit, taker_sub_index,
maker_sub_index, fill_seq)` with accounts:
- `sequencer` = the market's authorized sequencer signer (`== market.sequencer`).
- `market`, `insurance_fund` — PDAs.
- `taker_trader_state` = `[STATE_SEED, taker]` (sub 0) or `[STATE_SEED, taker, [taker_sub_index]]`.
- `maker_trader_state` = `[STATE_SEED, maker]` / `[STATE_SEED, maker, [maker_sub_index]]`.
- `taker_position` / `maker_position` = `[POSITION_SEED, market, <trader_state>]` (init_if_needed).
- haircut accounts if `market.haircut_enabled` (mandatory then).
- the `fill_commitment` ring PDA in `remaining_accounts` (**mandatory** on an armed market).

`maker == Pubkey::default()` ⇒ dispatch `apply_flp_fill` instead (FLP virtual-quote
fill), per the existing FLP routing.

### 2d. `fill_seq` — the monotonic settlement nonce (UNCHANGED rule)
`apply_fill` requires `fill_seq` STRICTLY greater than `market.last_settlement_seq`
(Kani-proven `advance_settlement_seq`). The sequencer assigns a per-market
strictly-increasing `fill_seq` (the absolute fill index `produced_from + k` is a
natural, gap-free choice and doubles as the dedup key). Settlement order MUST equal
production order (the commitment ring pops FIFO), so settle slots in ascending
index.

## 3. The new read loop

Per outbox-armed market, keep a persisted cursor `consumed` (highest absolute index
already settled). On wake-up (event) or poll:

```
acct      = getAccountInfo(fill_outbox)            # durable truth
produced  = u64(acct.data[8..16])
cap       = u32(acct.data[24..28])
for idx in consumed .. produced:                   # ascending = FIFO
    slot  = decode(acct.data, 64 + (idx % cap)*96)
    if slot.maker == 0:  build apply_flp_fill(slot, fill_seq=idx)
    else:                build apply_fill(slot,     fill_seq=idx)
    submit; on success: consumed = idx + 1; persist(consumed)
```

Key properties this gives, that the log path can't:
- **Stream semantics over a snapshot:** `produced - consumed` is exactly the
  outstanding fills, regardless of how many events were missed.
- **Restart-safe:** the account *is* the durable log. On crash/restart, re-derive
  `consumed` from `market.last_settlement_seq` (or persisted cursor) and replay
  `consumed..produced` — no lost fills, no double-settle (the on-chain `fill_seq`
  monotonic guard + the ring's consume-and-clear make re-submits idempotent).

## 4. Dual-mode routing (no flag day)

A market is **outbox-mode** iff its `fill_outbox` PDA exists (and/or
`market.max_batch_orders > 96`); otherwise **log-mode**. The sequencer:
- log-mode market → existing `FillBatchEvent` path (unchanged).
- outbox-mode market → §3 read loop; the slim event is just the wake-up.

Both paths can run side-by-side indefinitely. Cutover is per-market, driven by
whether the authority armed an outbox — the on-chain side already emits the right
event for each.

## 5. Backpressure — the sequencer MUST keep up

The ring caps depth at `ring_cap` (256): `place_taker_order_v2` reverts
`FillRingFull` once `produced - settled == cap`. So a sequencer that falls > `cap`
fills behind **stalls new taker matching** on that market (a liveness, not a safety,
failure — no fills are lost or corrupted). Operational implication: alert when
`produced - consumed` approaches `cap`; the read loop must drain faster than takers
produce. This is the same bound OpenBook enforces with its heap-full panic; here it
degrades to backpressure instead of a crash.

## 6. ER interaction

On a delegated market the outbox is delegated alongside the book + ring
(`delegate_fill_outbox`). On the ER the sequencer reads ER account state (same
layout) and settles on the ER; `commit_fill_outbox` mirrors it to L1. The read loop
is identical — only the RPC endpoint (ER vs L1) differs. Delegate and commit the
book, ring, and outbox **together** so a committed L1 snapshot is internally
consistent.

## 7. Edge cases

- **FLP fills:** `maker == 0` → `apply_flp_fill`. Same `fill_seq` ordering.
- **Reorgs:** outbox writes are atomic with the book mutation + ring push in one tx;
  a dropped tx drops all three together. Re-read from `consumed` after reorg.
- **Cap mismatch / corrupt account:** the matcher refuses to write an outbox whose
  `cap < ring_cap` (fail-closed), so a readable account is always ≥ ring-sized; the
  reader should still validate disc + `market` binding before trusting slots.
- **`grow_fill_outbox` mid-session:** `cap` only grows; absolute indices are stable,
  so the reader re-reads `cap` from the header each pass (it already does).

## 8. Rollout

1. **Shadow** — run the §3 read loop in parallel on a test outbox-armed market,
   asserting it reconstructs the same fills the log path would (where both exist) and
   that every fill settles. No production settlement from the new path yet.
2. **Cutover (per market)** — when the authority arms an outbox + raises the cap, the
   sequencer routes that market through the outbox path.
3. **Retire** — the fat `FillBatchEvent` stays for log-mode markets; no removal
   needed. Optionally drop the fat event once all live markets are outbox-armed.

## 9. Test plan (off-chain)

- Unit: slot decoder vs the on-chain layout (golden vectors from
  `fill_outbox_deep_sweep_256`'s account dump).
- Replay: against the devnet smoke market (`23_outbox_smoke.mjs` already produced a
  256-cap outbox with real fills) — read the account, reconstruct, assert byte-exact.
- Chaos: kill/restart mid-batch → assert no lost/duplicate settlement (cursor +
  `fill_seq` monotonic guard).
- Backpressure: stop the reader, confirm takers eventually revert `FillRingFull`,
  resume, confirm catch-up.

## 10. Effort / ownership

On-chain: **nothing** — the contract is shipped and devnet-verified. Off-chain: a
read-path module + cursor persistence + dual-mode routing — bounded, since the
settlement call assembly is identical to today's log path (only the *source* of the
fill tuples changes). The durable-account model actually **simplifies** the
sequencer (restart recovery becomes "re-read the account" instead of "replay the tx
stream"). Owned by the off-chain team; this repo's side is complete.
