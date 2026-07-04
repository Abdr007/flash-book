# F-3 — Airtight reduce-only across the match→settle gap (implementation spec)

Status: **SPEC — migration-gated, NOT shipped.** Author: audit-remediation follow-up, 2026-07.

This is the design for the *complete* fix of F-3. It is deliberately **not** implemented as a
patch: the sound fix requires ER-writable **per-position** state written during matching, which
the current delegation model does not allow (see §3). Rushing a cross-domain write into the hot
settlement path is exactly the break-one-fix-another failure the remediation protocol forbids, so
this is spec'd and gated on a migration + its own devnet ER cycle.

---

## 1 · The residual (what F-3 actually is, precisely)

The M-7 "airtight" reduce-only cap in `place_taker_order_v2` caps a crossed reduce-only maker's
fill to the maker's **reducible** size, read from a **per-call snapshot** of the maker's
`PositionAccount` (supplied via `remaining_accounts`), decrementing an in-memory `maker_positions`
entry within that one walk (`lib.rs`, the `FLAG_REDUCE_ONLY` blocks). The snapshot **ignores
matched-but-unsettled reductions of the same position** — the exact match/settle async gap that
M-6 added a persisted counter (`unsettled_fill_volume`) for on OI, but which M-7 has no
equivalent of for per-position reduction.

Positions are written only at `apply_fill` (settlement), a *separate* instruction from
`place_taker` (matching). So between a taker's match (fill pushed to the FIFO ring) and its settle,
the `PositionAccount` still shows the pre-reduction size — on L1 *and* on the ER clone. Two
reduce-only orders on one position, crossed by **separate** taker calls inside that gap, each read
the full position size and each cap to it, collectively **over-reducing → flipping** the position.

**Why the flip is a security bug, not just wrong-intent:** the flipped position is a *maker-side
open at settlement*, where M-2's intake initial-margin gate never runs (it only gates
`place_taker`/`place_limit` intake, and settlement cannot reject a committed fill without wedging
the FIFO). So the flip mints an **under-margined position that bypasses initial margin** →
free option, bad debt socialized to insurance/LPs. The profitable actor is the maker itself
(self-cross two of its own reduce-only orders + delay its own keeper's `apply_fill`).

## 2 · What already bounds the risk (shipped — do not re-do)

- **Injection-time clamp.** `execute_trigger_order_v3` loads the position and requires
  `trigger.size_lots <= position.size_lots` (plus `size > 0` and opposite side) before injecting a
  reduce-only close order. A **single** reduce-only order therefore can never exceed the position
  at fire time — the single-oversized-order vector is closed.
- **OCO (HIGH-7).** When a bracket leg fires, `execute_trigger_order_v3` deactivates the validated
  mutual sibling (`FLAG_ACTIVE` cleared, reverts if the sibling is omitted/wrong), so a bracket's
  TP+SL can never both inject. The bracket double-leg vector is closed.
- **Per-call cap.** Within one `place_taker` walk, two reduce-only orders on the same position map
  to the same `maker_positions` entry and share its decremented reducible, so a single taker call
  cannot over-reduce.

**Remaining residual after the above:** two **standalone** (non-OCO) reduce-only stops on one
position, both fired (price whipsaw), crossed by **two separate** taker calls in the ER→L1
settle gap. Narrow, self-cross-shaped, and attacker-timed — but real, because the injection clamp
is per-order (not cumulative) and the matcher's reducible read is a stale snapshot.

## 3 · Why it can't be a small patch (the architecture wall)

The fix needs the matcher to cap `reducible = position.size − reduce_in_flight(position)`, where
`reduce_in_flight` is the base-lot volume of reductions of *this position* already pushed to the
ring but not yet settled. That value must be:
- **written during matching** (incremented when a reduce-only fill is produced in `place_taker`), and
- **decremented at settlement** (`apply_fill`/`apply_flp_fill`).

`place_taker` runs on the **ER** and may only write **delegated** accounts. The only delegatable
accounts are `market`, `market_book`, `fill_commitment`, `fill_outbox` (grep: four `delegate_*`
handlers; **no `delegate_position`**). Positions live on L1 and are **read-only clones** on the ER.
So per-position reduce-in-flight has **no ER-writable home** today:
- A per-**market** aggregate (the M-6 shape) does not work — it would over-constrain, blocking
  legit reduces on *other* positions.
- Writing the position clone on the ER is rejected (not delegated / not owned there).
- Storing it on the resting order (ER-writable) doesn't compose — the value is per-position and
  spans multiple orders; the matcher would need an O(book) scan of all reduce-only orders on the
  position per cap (CU-prohibitive on the hot path).

## 4 · Proposed design (migration)

Add a delegated, ER-writable **per-position reduce-pending** PDA, mirroring the M-6 lifecycle but
at position granularity.

**Account.** `ReducePendingAccount { position: Pubkey, pending_reduce_lots: u64, bump: u8 }`,
PDA `[REDUCE_PENDING_SEED, position_key]`. Created lazily the first time a reduce-only order is
injected for that position (payer = the trigger executor / trader). Delegated to the DLP alongside
the market/book so it is ER-writable during trading; committed/undelegated on the same lifecycle.

**Matcher (produce, ER).** In `place_taker`, when producing a fill against a `FLAG_REDUCE_ONLY`
maker order, compute
`reducible = position.size_lots.saturating_sub(reduce_pending.pending_reduce_lots)`
(instead of the raw snapshot), cap the fill to it, and `pending_reduce_lots += fill`. Pass the
maker's `ReducePendingAccount` in `remaining_accounts` next to its position (fail-closed: an
omitted reduce-pending account ⇒ treat pending as “unknown-max” ⇒ reducible 0 ⇒ skip, so omission
can only *reduce* fills, never bypass — same discipline as the position account today).

**Settlement (consume, L1/ER).** In `apply_fill`/`apply_flp_fill`, when settling a reduce-only
fill, `pending_reduce_lots = pending_reduce_lots.saturating_sub(size_lots)`. 1:1 with the ring
produce/settle, so it self-balances exactly like `unsettled_fill_volume`.

**Reconcile.** Same ER-seam hazard as M-6 (separate delegated account, no atomic co-undelegate):
add a permissionless `reconcile_reduce_pending(position, fill_commitment)` that resets
`pending_reduce_lots = 0` when the ring is drained (`produced == settled`) — provably correct then,
identical soundness argument to `reconcile_unsettled_fill_volume` (F-4).

**Invariant.** For every position, `Σ(reduce-only fills produced but unsettled) = pending_reduce_lots`,
so `reducible ≥ 0` and total reduction across all in-flight reduce-only fills ≤ `position.size` ⇒
**a reduce-only order can never flip a position**, even across separate taker calls in the settle gap.

## 5 · Verification plan (before it can ship)

- Kani/proptest on the pure cap math: `reducible = size − pending`, `pending += fill`,
  `pending -= settle`, across interleaved produce/settle sequences ⇒ never flips.
- BanksClient: two standalone reduce-only stops on one long position, two separate `place_taker`
  crosses with `apply_fill` deferred between them ⇒ the second cross caps to 0 (position already
  fully reduce-pending) and cannot flip; then settle both and assert the position closes to flat.
- Devnet ER: the real match(ER)→settle(L1) gap with a delegated `ReducePendingAccount`, plus the
  reconcile path — the piece BanksClient cannot exercise. **This is the gate.**
- CU: measure the matcher with reduce-only makers + the extra account load; confirm no regression
  near the 1.4M budget or the 4 KB stack.

## 6 · Interim posture (until the migration ships)

Documented residual, not a silent gap: single-order over-reduce and bracket double-leg are closed
(§2); the remaining vector is two standalone reduce-only stops self-crossed across the settle gap,
bounded further by `REDUCE_ONLY_TRIGGER_ORDER_TTL_SLOTS` (~5 min) on how long a stale reduce-only
order can linger. A cheap, sound **narrowing** that could ship independently (still not complete):
enforce **≤ 1 active standalone reduce-only trigger per (position, side)** at
`place_trigger_order_v3` (brackets are exempt — they are OCO-protected), removing the
two-standalone-stops setup. It needs a per-position marker, so it is itself a (smaller) state
addition and should be evaluated against the full migration rather than layered ad hoc.
