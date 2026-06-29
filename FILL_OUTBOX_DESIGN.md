# Fill-Outbox — design scope

**Status: IMPLEMENTED + DEPLOYED + DEVNET-SMOKED (2026-06-29).** Deployed to devnet
(upgrade sig `5tXKx9DPtTqb…`); smoke `23_outbox_smoke.mjs` PASSED on the live
cluster — `init_fill_outbox` + `grow_fill_outbox`×2 → 24,640 B account at cap 256;
a cap-256 market hard-rejected an omit-outbox taker (`FillOutboxRequired`); an
8-level sweep delivered all fills off-log (outbox cursor = 8, every fill read from
the account). IDL patched (5 new ix + 4 events + error + field). **P-A→P-C on-chain,
all tests green.** New module
`matcher/fill_outbox.rs` (11 host tests) + `init/grow/delegate/commit_fill_outbox`
instructions + `max_batch_orders` field + heap-frugal matcher write + slim
`FillBatchOutboxEvent`. Proven end-to-end by `fill_outbox_deep_sweep_256`:
**256-level single-tx sweep = 292,089 CU in the default 32 KiB heap**, all fills
reconstructed from the outbox account, omit-outbox path hard-rejected. 446 host +
68 integration tests pass; `build-sbf` clean. Off-chain sequencer cutover (P-D) and
optional settlement-reads-outbox hardening (P-E) remain. **Goal:** lift the per-tx
taker batch cap above the ~115-fill program-log
ceiling (see `DEEP_BOOK_CU.md`) by moving fill **data** off the transaction log
into an on-chain account the sequencer reads via `getAccountInfo`. Targets a usable
batch cap of **256** (and structurally removes the log as the bound entirely).

**Build-readiness check (2026-06-29) — all open items resolved:** ✅
`max_batch_orders` tail-append is safe — `size_of::<MarketAccount>()` measured at
**896 B**, 256 B free under the 1152 bound (§8). ✅ no size cap in flash-book's ER
delegate path — already delegates the 8 KB ring via the same code (§10). ✅
**MagicBlock DLP supports the 24 KB outbox** — verified in
`magicblock-labs/delegation-program`: `AccountSizeClass` enumerates up to `Huge`
≤ 1 MB + `Dynamic` (any legal value); a 24 KB outbox is a `Large` (≤64 KB) account,
and commit/finalize resizes only when state grows (constant-layout outbox → no
realloc cap). The "~10 KB ER limit" was a myth. Everything else is a faithful
copy-adaptation of the proven `fill_commitment` family. **Build-ready, no blockers.**

---

## 1. Why

`FillBatchEvent` is an `#[event]`. The off-chain sequencer reconstructs each
`apply_fill` from the fill **data** (maker / size / price / sub-indices /
`taker_was_jit`) carried in that event's log. Solana caps *all* program-log output
at `LOG_MESSAGES_BYTES_LIMIT = 10_000` bytes/tx and base64-inflates `sol_log_data`
by ×1.333, so one event overflows ~125 fills and silently truncates the tail. A
truncated-but-crossed fill is unsettleable → its ring slot never pops → settlement
wedges (the H-2 griefing class). Hence the cap is pinned at **96**.

The log is the bound, not the heap and not CU. Remove the log from the fill-data
path and the bound is gone.

## 2. Current data flow (grounded)

```
place_taker_order_v2 (matcher, producer)
  walk book → matches: Vec<(idx, maker_id, size, price, maker_pk, maker_sub)>
  for each match:
     ring_push(keccak(fill_preimage(... produced_index ...)))   # ring: 32B hash/slot
  emit FillBatchEvent { market, taker, side, taker_id, fills: Vec<FillEntry>, taker_sub }
                                              └── 57B/fill, THIS is the log-limited part

sequencer (reads FillBatchEvent log)  →  for each fill: apply_fill(size, price, side,
     taker_was_jit, taker_sub, maker_sub, fill_seq)  with taker/maker trader_state accounts

apply_fill (settlement, consumer)
  verify sequencer == market.sequencer            # C-1 auth
  advance_settlement_seq(fill_seq)                # H1 replay guard
  buffer_settle: recompute keccak(fill_preimage) from the SUPPLIED data + FIFO
     settle_index → must equal oldest pending ring slot → consume-and-clear
```

The ring (`matcher/fill_commitment.rs`) is a raw PDA `[fill_commit, market]`:
`FILL_COMMIT_HEADER_LEN = 64` (disc + `produced` u64 + `settled` u64 + `cap` u32 +
bump + `market` [32]) followed by `cap × 32` keccak slots; `FILL_RING_CAP = 256`.
`ring_push` / `ring_settle` are Kani-proven over the FIFO cursors.

## 3. Design principle — additive data mirror

**Do not touch the proven ring or `apply_fill`.** Add a second raw PDA,
`FillOutboxAccount` at `[fill_outbox, market]`, holding the fill **data** in slots
**addressed by the ring's existing `produced` / `settled` cursors** (the outbox has
no independent cursor — it is a parallel array the ring already indexes). The
matcher writes data to `outbox[produced % cap]` in the same loop it pushes the
commit; the sequencer reads the outbox account instead of the log. The keccak ring
remains the trust anchor and `apply_fill` is unchanged.

This keeps the change **purely additive and write-only-by-matcher,
read-only-by-sequencer**: no new authenticity surface, all §3.2 / H1 / C-1
guarantees and proofs carry over untouched.

```
              ┌─ fill_commit  PDA  (UNCHANGED) : cap×32  keccak slots  ── trust anchor
matcher push ─┤                                  produced/settled cursor (shared)
              └─ fill_outbox  PDA  (NEW)        : cap×SLOT data slots   ── sequencer transport
                                                  indexed by the SAME cursor
```

## 4. `FillOutboxAccount` layout

Header (mirror the ring's, so `delegate`/`commit`/validation code is copy-adapted):

| off | len | field |
|----:|----:|-------|
| 0   | 8   | disc `FBoutbx\0` |
| 8   | 8   | `produced` (mirror of ring — write-side cross-check, see §10) |
| 16  | 8   | `settled`  (mirror) |
| 24  | 4   | `cap` (u32) — **must equal the ring `cap`** |
| 28  | 1   | bump |
| 29  | 3   | pad |
| 32  | 32  | `market` |

Slot (one per fill, self-contained — the `fill_preimage` fields minus the domain
tag, `market`, and `produced_index` (all implicit), plus `maker_id` for sequencer
bookkeeping):

| off | len | field |
|----:|----:|-------|
| 0   | 32  | `taker` |
| 32  | 32  | `maker` (`Pubkey::default()` ⇒ FLP virtual-quote fill → `apply_flp_fill`) |
| 64  | 8   | `size_lots` |
| 72  | 8   | `price_ticks` |
| 80  | 8   | `maker_id` (resting order id — sequencer bookkeeping/dedup; not used by `apply_fill`) |
| 88  | 1   | `taker_side` |
| 89  | 1   | `taker_sub_index` |
| 90  | 1   | `maker_sub_index` |
| 91  | 1   | `taker_was_jit` |
| 92  | 4   | pad → **96 B/slot** (8-byte aligned) |

`fill_outbox_account_len(cap) = 64 + cap * 96`. At cap 256 → **24,640 bytes**
(rent-exempt ≈ **0.17 SOL**, paid by the market authority at init).

> Alternative considered: store the full 136-byte `fill_preimage` per slot (zero
> packing logic, settlement could read it directly). Rejected as primary for size
> (cap 256 → 34 KB) and redundancy (`market`/`produced_index` repeat per slot); the
> 96-byte packed slot is leaner and the matcher already computes these exact fields
> to build the commit.

## 5. Producer write path (heap-free — the point)

In `place_taker_order_v2`, when an outbox account is present (`find_fill_outbox`,
mirroring `find_fill_commitment`), write each fill's slot **directly into the
borrowed account data** in the existing commit-push loop:

```rust
// already borrowed for the ring; borrow the outbox once, same loop
let mut ob = outbox_ai.try_borrow_mut_data()?;
for (_idx, maker_id, size, price, maker_pk, maker_sub) in &matches {
    let produced = fc::buffer_next_index(&fc_data);          // ring decides the slot
    // ... existing ring_push(keccak(preimage(... produced ...))) ...
    outbox_write_slot(&mut ob, produced % cap, taker_pk, *maker_pk, *size, *price,
                      *maker_id, side, sub_index, *maker_sub, taker_was_jit);
}
```

`outbox_write_slot` is a `copy_from_slice` into a `&mut [u8]` window — **no `Vec`,
no serialization, no log**. The only O(N) heap stays `matches`. So the cap is now
bounded by **CU and account size**, not heap and not logs.

Event becomes **slim and fixed-size** (always log-safe):

```rust
emit!(FillBatchOutboxEvent {
    market, taker, taker_id, taker_side, taker_sub_index,
    produced_from: u64,   // first slot this batch wrote
    produced_to:   u64,    // == produced_from + fills_count
});
```

~98 bytes regardless of N → indexers/UIs learn "fills `[from,to)` for `taker_id`
live in the outbox" and read them on demand. The legacy fat `FillBatchEvent` is
retained for markets without an outbox (≤ log-safe cap), so no existing consumer
breaks.

## 6. Sequencer read path

Market has an outbox ⇒ the sequencer **reads fill data from the outbox account**
(`getAccountInfo`, or `accountSubscribe` / Geyser) rather than parsing logs. It
walks slots `[its_last_read .. produced)`, builds one `apply_fill` per slot (args
from the slot; `fill_seq` is its own monotonic counter), and submits. The
`FillBatchOutboxEvent` is a cheap wake-up/cursor hint; the data source is the
account. **No 10 KB limit, no truncation.**

Backpressure is the existing ring rule: the sequencer must settle within `cap`
fills of production or `ring_push` returns `Full` (the matcher applies
backpressure — never overwrites an unsettled slot). The outbox slot at
`produced % cap` is overwritten only after `settled` has passed it, so a slot the
sequencer hasn't yet consumed is never clobbered. Document the read-within-`cap`
contract.

## 7. Settlement — unchanged (+ optional hardening)

`apply_fill` is **untouched**: it still recomputes `keccak(fill_preimage)` from the
sequencer-supplied args and verify-pops the ring. The outbox is sequencer transport
only; settlement does not read it. All §3.2 authenticity, H1 replay, C-1 auth, and
the Kani proofs hold verbatim.

**Optional later hardening (not required for the cap lift):** pass the outbox to
`apply_fill` and read size/price/side/jit **from the slot** instead of from
sequencer args, then the sequencer cannot even *propose* altered economics (it only
picks the FIFO-next slot, which the ring already forces). This upgrades §3.2 from
"cryptographically detected" to "structurally impossible," at the cost of coupling
settlement to the outbox account. Defer until the transport is proven in
production.

## 8. Per-market, outbox-gated cap

`MAX_BATCH_ORDERS_PER_SIDE_V2` (global const, 96) becomes a floor. Add a market
field `max_batch_orders: u16`, **appended at the tail** of `MarketAccount` — the
established additive-migration pattern (the last six fields, incl.
`last_settlement_seq` and `last_heartbeat_slot`, were added this way), *not* carved
from a `_pad` field (the struct has none). **Verified headroom:**
`size_of::<MarketAccount>()` is **896 B** against the `space() = 8 + 1152` bound →
**256 B free**; a 2-byte field takes it to 898 (254 B still free), and the existing
build-time `assert!(size_of ≤ 1152)` guards it. Pre-existing markets read the tail
field back as `0` (trailing-zero convention); the walk treats **`0` ⇒ the 96
default**, so `0` is never a real cap and migration needs no backfill.
`init_fill_outbox` is the **only** path allowed to raise it (≤ `FILL_RING_CAP`
= 256, and only when ring `cap` ≥ the new value). A market without an outbox stays
log-safe at 96; a market with one can run up to 256. No global flag day.

## 9. Capacity / rent / CU

- **Account size & the create-then-grow lifecycle:** `64 + cap*96`; cap 256 →
  24,640 B. **Correction (caught at build time):** a program **CPI** to
  `create_account` can grow an account by at most `MAX_PERMITTED_DATA_INCREASE`
  = 10,240 B **per instruction** — the BPF-loader limit, *separate* from the
  system-program's 10 MB / 20 MB `can_data_be_resized` checks. So the full 24 KB
  outbox CANNOT be allocated in one ix (an earlier draft claimed it could; the
  build returned `InvalidRealloc` and disproved it). This is exactly why the market
  book (9,600 B) is created small and `expand`ed. The outbox follows the same
  proven lifecycle: `init_fill_outbox` allocates `FILL_OUTBOX_INIT_CAP` = 105 slots
  (10,144 B, one CPI), then `grow_fill_outbox` raises it ≤106 slots/call
  (105 → 211 → 256 = two grows). The matcher's `outbox.cap >= ring.cap` guard keeps
  outbox matching **inert (fail-closed)** until the outbox has been grown to cover
  the ring, so the create-then-grow window is never unsafe. Hard ceiling 10 MB ⇒
  ~100k slots — never the bound.
- **Rent:** ≈ 0.17 SOL at the full cap 256 (authority-funded; ~0.07 SOL at init,
  topped up by each grow).
- **CU (MEASURED, real SBF):** a 256-level armed single-tx sweep through the outbox
  = **292,089 CU in the DEFAULT 32 KiB heap** (`fill_outbox_deep_sweep_256`
  integration test, no heap-frame request). Over the 200 K default, so the taker
  sets `SetComputeUnitLimit` (well under the 1.4 M max). 96 stays ~129 K. CU is the
  new soft bound and it is comfortable.

## 10. ER / delegation

> **CONSTRAINT — verified on the live MagicBlock devnet ER (2026-06-29, the
> `er-acceptance/` suite): a 256-slot (24,640 B) outbox CANNOT be delegated to the
> ER.** `delegate_fill_outbox` → `cpi_delegate` creates the delegate-buffer at the
> full account size via one `create_account` CPI (`er.rs::create_pda`), which hits
> the same 10,240 B/ix BPF-loader cap (`DelegateFillOutbox` reverts "Failed to
> reallocate account data"). Because the matcher requires `fo_cap >= ring_cap`
> (256), the deep outbox is therefore **L1-only** under the current ring cap — book
> + ring delegate fine (both < 10,240 B) and the §3.2 authenticity round-trip works
> on the ER, but the 256-cap deep-sweep is an **L1 feature**. To run a deep outbox
> *on* the ER would need either (a) a chunked delegate-buffer staging (create small
> + grow + stage + delegate across ixs — a new delegate flow), or (b) a smaller
> per-market ring+outbox cap (≤106 slots, both one-CPI-delegatable). Decision
> pending; the L1 256 path is unaffected. The unit/integration suite never caught
> this (it doesn't delegate); only the live-ER suite did — the Tier-2 value.

The outbox mirrors the ring's ER lifecycle: `delegate_fill_outbox` /
`commit_fill_outbox` / `commit_and_undelegate_fill_outbox` (copy-adapted from the
`fill_commitment` versions, `DELEGATION_PROGRAM_ID` / `Magic111…` rails) — usable
once the buffer-create constraint above is resolved. Delegate the ring and the
outbox together; commit them together so L1 sees a consistent `(commit, data)` pair.
The mirrored `produced`/`settled` in the outbox header let a
commit-time assertion catch any ring/outbox cursor divergence (defense-in-depth;
they advance in the same tx so they cannot legitimately differ).

**ER account-size — RESOLVED (no special handling needed).** Verified against the
MagicBlock DLP source (`magicblock-labs/delegation-program`, 2026-06):

- **No hard size cap.** The delegate / commit / finalize paths contain no
  account-size reject (the only `MAX_*` constants are `MAX_PUBKEYS`, unrelated).
  The DLP's `AccountSizeClass` enum explicitly enumerates classes up to
  **`Huge` ≤ 1 MB** and **`Dynamic(u32)` = "any legal value"**; the DLP's own
  program-data account is `Dynamic(350 KB)`. A 24,640-B outbox is a **`Large`
  (≤ 64 KB)** account — squarely supported.
- **Commit is size-safe.** `commit_finalize_internal` does
  `delegated_account.resize(new_state.data_len())` and only enforces a min-balance
  top-up *when the committed state is larger than the current account*. A
  fixed-layout outbox commits at **constant size** → no resize growth → the 10 KB
  realloc cap never applies. (`finalize.rs` / `commit_finalize_internal.rs`.)
- **flash-book side** (`er.rs`) imposes no cap either — it sizes the delegate
  buffer to `data_len` and already delegates the 8,256-B ring via the same code.

The "~10 KiB ER cap" was a **myth** — surveyed competitors' *chosen* account size,
not a MagicBlock limit. The 256/24 KB path therefore works **identically on L1 and
the ER**, no per-environment sizing. The only real cost is that committing a
`Large` account back to L1 spends more CU than the 8 KB ring (the size-class budget
scales with bytes) — a cost, not a ceiling.

## 11. Security analysis

- **Writer = matcher only.** The outbox is a PDA written exclusively by
  `place_taker_order_v2` with the program as signer; the sequencer never writes it.
  A compromised sequencer can only *read* — it cannot fabricate by planting data.
- **Taker can't forge maker identity.** Maker pubkeys/sizes/prices come from the
  resting book nodes the walk crosses, not from taker input.
- **Authenticity unchanged.** `apply_fill` still gates on the keccak ring; the
  outbox adds no trust. Even fully malicious outbox contents (impossible per above)
  could not settle a fill the ring didn't commit.
- **No new DoS.** Outbox `cap == ring cap`, so it never overflows when the ring
  doesn't; the ring's `Full` backpressure already bounds depth.
- **PDA validation** mirrors `buffer_check`: disc + `market`-bound + cap, fail
  closed (`OutOfRange`) on tamper (inherits the ER L-2 hardening pattern).

## 12. Backward compatibility & rollout

Phased, each independently shippable and reversible:

1. **P-A — account + lifecycle.** Add `FillOutboxAccount`, `init/grow/delegate/
   commit_fill_outbox`, `find_fill_outbox`, layout helpers + host tests. No
   matcher change yet. Inert.
2. **P-B — producer write + slim event.** Matcher writes the outbox when present
   and emits `FillBatchOutboxEvent`; legacy fat `FillBatchEvent` kept when no
   outbox. Markets without an outbox are byte-for-byte unchanged.
3. **P-C — per-market cap.** Add `max_batch_orders`; `init_fill_outbox` raises it
   ≤256. Walk reads the field. Existing markets stay at 96.
4. **P-D — sequencer cutover (off-chain).** Sequencer prefers outbox-read for
   outbox markets; log-parse fallback for the rest. Coordinated deploy with the
   off-chain stack — the on-chain side already emits both.
5. **P-E (optional) — settlement reads outbox** (§7 hardening).

Existing armed markets keep working throughout; deep sweeps light up only after a
market opts in (`init_fill_outbox`) and the sequencer is on P-D.

## 13. Test / FV plan

- Host unit tests: outbox slot round-trip; `produced`/`settled` mirror tracks the
  ring; disc/market/cap validation rejects tamper; cap-equality invariant.
- Integration (`solana-program-test`, real SBF): a 256-level armed sweep — assert
  (a) no OOM, (b) every slot reconstructs the exact fill, (c) the slim event is
  fixed-size and log-safe, (d) the full batch settles via `apply_fill` against the
  unchanged ring. Extend `deep_book_matching_cu_curve` to 256 and record the CU
  curve + account-read bytes (replacing the current 96 graceful-truncation case).
- Kani: the proven `ring_push`/`ring_settle` are unchanged (re-run to confirm). Add
  a bounded proof that `outbox_write_slot` index `== produced % cap` stays in
  bounds for all `produced < cap*2` (no slot aliasing across one wrap).
- Real devnet: a throttled multi-place + 128/256-level armed sweep, fetch the
  outbox via `getAccountInfo`, reconstruct and settle — no synthetic data.

## 14. Open decisions (need a call before P-A)

1. **Slot width / `maker_id`:** keep `maker_id` in the slot (96 B, sequencer
   dedup convenience) or drop it (88 B)? Recommend keep — 8 B/slot is noise and it
   matches the event's information.
2. **Settlement-reads-outbox (P-E):** in scope now, or deferred hardening?
   Recommend **deferred** — the cap lift doesn't need it and it couples settlement
   to a new account.
3. **Default `max_batch_orders` ceiling:** 256 (= ring cap) or lower (e.g. 192) for
   CU headroom on busy markets? Recommend 256 with the taker responsible for its CU
   request; revisit if a 256-sweep ever bumps the 1.4 M ceiling alongside other CU.
4. **Account vs `accountSubscribe` for the sequencer:** off-chain choice; both work
   with no log limit. Out of scope for the program.
5. **ER delegated account-size ceiling (§10) — RESOLVED.** Verified from the DLP
   source: no cap; `AccountSizeClass` supports up to 1 MB (`Huge`) + `Dynamic`. A
   24 KB outbox is a `Large` account; commit is constant-size so no realloc cap.
   256 works identically on L1 and ER. No action needed.

## 15. Effort

- P-A + P-B + P-C (on-chain, the cap lift): ~1 focused session — it is a faithful
  copy-adaptation of the `fill_commitment` account family + a heap-free write loop
  + one market field + the slim event. Lowest-risk because it is additive and the
  proven settlement path is untouched.
- P-D (off-chain sequencer): a separate workstream owned by the off-chain team.
- P-E (optional hardening): ~half a session if pursued.

**Net:** the program-log ceiling is removed by an additive, write-only-by-matcher
data mirror that reuses the ring's proven cursor. The cap rises to 256 (CU-bounded,
comfortable) with zero change to `apply_fill` and zero new authenticity surface.
The off-chain sequencer cutover is the gating dependency, not the on-chain code.
