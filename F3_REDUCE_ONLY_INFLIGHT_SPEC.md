# F-3 — Airtight reduce-only across the match→settle gap (implementation spec)

Status: **FULLY CLOSED (shipped).** Primary vector via the injection clamp; the last edge via the
v1 fill-commitment migration below. Author: audit-remediation follow-up, 2026-07.

### The migration is now SHIPPED (2026-07) — co-located reduce-in-flight, no ER seam
The "requires ER-writable per-position state" blocker below is resolved by a key design choice: the
per-position reduce-in-flight tracker is **co-located inside the fill-commitment account** (a v1
layout), so it commits **atomically with the settlement ring** — no separate account, no
cross-account ER seam, and **no change to the fill preimage** (authenticity untouched). It is
backward-compatible + version-gated: v0 rings (and every pre-existing market/test) are byte-identical;
a market opts in via `upgrade_fill_commitment_v1` (authority-gated, drained ring). On a v1 ring the
matcher caps a reduce-only cross by `position − in-flight[position]` and adds the fill to in-flight;
`apply_fill` releases it on settlement (the maker + sub_index are preimage-committed, so the position
key is authentic). This closes the shrink edge (a second taker reading a stale position snapshot now
sees `reducible = position − in-flight`, capping it to 0). Shipped in commits `679766c` (layout),
`bbcad82` (upgrade ix), `17978aa` (matcher), `7265efd` (settlement), `fde8c22` (e2e test). The
sections below are retained as the design rationale.

### Shipped mitigation (the safe part) — injection-time cumulative capacity clamp
A safe, complete fix for the **multi-order** flip — the demonstrated exploit — ships in
`execute_trigger_order_v3`: before injecting a reduce-only close order, it sums this position's
EXISTING resting reduce-only orders (same trader + sub_index + close side, via the book scan idiom
already used on the liquidation path) and clamps the new order so **total resting reduce-only for a
position can never exceed the position size**. Two reduce-only orders (two standalone stops,
scale-out legs, or a bracket SL leg + an unrelated stop) can therefore never sum past the position
and flip it. It touches **no hot-matcher code, adds no account, no ER-write, no layout change**, and
is validated on a real `MarketBookHandle` (host test `f3_reduce_only_capacity_clamp_scan`,
state_v2.rs) + full suite green. Scale-out is preserved (partial exits summing ≤ position all fit);
only genuine over-capacity is trimmed.

### Remaining edge (the migration below) — position SHRUNK below its resting reduce-only
The injection clamp bounds against the position size *at injection*. A single reduce-only order sized
to the position, after which the trader **shrinks the position via a separate path** (e.g. a market
sell), leaves that one order larger than the now-smaller position — and two takers can still
over-cross it across the match→settle gap. This narrower edge (a deliberate self-shrink-then-overhang
self-cross, TTL-bounded ~5 min) needs the match-time per-position in-flight state below, which
requires ER-writable **per-position** state written during matching — the current delegation model
does not allow it (see §3). That remains deliberately **not** shipped: rushing a cross-domain write
into the hot matcher is the break-one-fix-another failure the protocol forbids, so it stays gated on
a migration + its own devnet-ER cycle.

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
- **OCO (HIGH-7) — but note its EXACT scope.** When a bracket leg fires, `execute_trigger_order_v3`
  deactivates that bracket's *own* validated mutual sibling, so a single bracket's TP+SL can never
  both inject. **OCO does NOT relate a bracket leg to any UNRELATED reduce-only order.**
- **Per-call cap.** Within one `place_taker` walk, two reduce-only orders on the same position map
  to the same `maker_positions` entry and share its decremented reducible, so a single taker call
  cannot over-reduce.

**Remaining residual after the above (scope corrected 2026-07 after deep review):** the residual is
NOT limited to two *standalone* stops. Because the injection clamp is strictly **per-order**
(`trigger.size ≤ position.size`, no cumulative accounting) and OCO only pairs a bracket's own two
legs, **any two independent injection-capable reduce-only exits on one position** can both inject and
both cap against the stale snapshot — including a **bracket stop-loss leg PLUS an unrelated standalone
stop** on the same position, not just two standalone stops. Both cross via two separate taker calls in
the match→settle gap → collective over-reduce → flip. Narrow (attacker-timed self-cross, TTL-bounded
~5 min by `REDUCE_ONLY_TRIGGER_ORDER_TTL_SLOTS`) but real. The correct complete fix therefore MUST
account for cumulative in-flight reduction across ALL reduce-only orders on a position (either the
persisted per-position counter of §4, or a per-position ≤1-active-exit cap that counts bracket legs
too — the latter closes F-3 but restricts a position to a single protective exit, a product
trade-off).

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

Documented residual, not a silent gap: single-order over-reduce and a bracket's *own* double-leg are
closed (§2); the remaining vector is **any two independent injection-capable reduce-only exits on one
position** (two standalone stops, OR a bracket stop-loss leg + an unrelated standalone stop)
self-crossed across the settle gap, bounded further by `REDUCE_ONLY_TRIGGER_ORDER_TTL_SLOTS` (~5 min)
on how long a stale reduce-only order can linger.

A **≤ 1 active reduce-only exit per (position, side)** cap at injection would close it WITHOUT a
hot-matcher change or live-ER validation — but note two things it is NOT: (a) it must count **bracket
legs too** (an exempt-brackets version, as an earlier draft of this doc wrongly suggested, leaves the
bracket-leg + standalone vector open); and (b) capping to one exit **restricts a position to a single
protective exit** (you could not hold both a take-profit and a stop-loss), a real product trade-off,
and it still needs a per-position active-exit counter with its own cancel/expire liveness reconcile.
So it is not free. **Decision (deep review, 2026-07):** no fix is BOTH low-risk-to-implement AND
functionality-preserving — the complete functionality-preserving fix (§4 `ReducePending`) is
inherently high-risk (new ER-writable account on the hot matcher path + both settlement paths + a new
ER seam, validatable only on devnet-ER), and the low-risk complete fix restricts exits. The residual
being narrow + TTL-bounded, **leaving §4 migration-gated is the defensible posture**; do not ship
either variant reflexively.
