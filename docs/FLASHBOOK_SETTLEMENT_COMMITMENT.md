# Flash Book — Settlement Authenticity (H1 part B / #35)

> The battle-tested design for binding settlement to real matches, so a
> compromised sequencer **cannot fabricate fills**. Grounded in the actual
> code paths (cited `file:line`); honest about the residual trust boundary.

## 1. The gap (what we are closing)

Today the lifecycle is split into an **authentic** half and a **trusted** half:

| Stage | Where | Trust |
|-------|-------|-------|
| Match: walk the hypertree, decrement/remove maker orders, emit `FillBatchEvent` | `place_taker_order_v2` (`lib.rs:628-904`), **on-chain** (L1 or ER) | **Trustless** — the book is mutated on-chain; fills are a deterministic function of book state |
| Settle: mutate collateral / position / PnL from `(size, price, maker, taker, …)` | `apply_fill` (`lib.rs:3141`), `apply_flp_fill` (`5870`) | **Trusted** — args taken at face value; the book is **not** in the `ApplyFill` context, so nothing checks the fill against a real resting order |

H1 part A (shipped, `8a41078`) closed **replay** with a monotonic `fill_seq`.
It does **not** stop a malicious sequencer from posting a *fresh-seq* fill for a
trade that never crossed — fabricating positions and draining the quote vault.

**Key realization (changes the design):** the authentic fills *already exist
on-chain* — `place_taker_order_v2` produces them when it mutates the book. We do
**not** need an off-chain Merkle proof. We only need to **bind** `apply_fill` to
the matcher's on-chain output. That is cheaper, simpler, and stronger.

## 2. The mechanism — Fill Commitment Queue (consume-and-clear)

A per-market FIFO of compact per-fill commitments, **written by the matcher**
and **drained by settlement**:

```
 place_taker_order_v2 (matcher)                 apply_fill (settlement)
 ───────────────────────────────                ────────────────────────
 for each fill it produces:                     for each fill it settles:
   c = commit(fill)              ──ring──▶         c' = commit(args)
   ring.push(c); produced += 1                     require c' == ring.peek_tail()   ← authenticity
                                                    ring.pop(); settled  += 1        ← consume-and-clear
```

- `commit(fill) = H(market, taker, taker_side, maker, size_lots, price_ticks,
  taker_sub_index, maker_sub_index, produced_seq)` — a 128-bit domain-separated
  hash of the fill's full economic content **bound to its production index**, so
  it is unique and order-locked.
- **Authenticity:** `apply_fill` can only settle a fill whose `commit` already
  sits in the on-chain ring. The sequencer cannot manufacture a `c'` that equals
  a ring slot without the real fill inputs the matcher hashed — fabrication is
  rejected (`FillNotCommitted`).
- **No double-settle / no replay:** FIFO `pop` advances `settled`; a slot is
  consumed exactly once. Composes with H1's `fill_seq` guard.
- **Backpressure:** ring full ⇒ the matcher applies backpressure (or the ring is
  sized to the deepest taker sweep); settlement drains it.

### Where it lives  *(as implemented)*
A dedicated **`FillCommitmentAccount`** raw PDA (`[fill_commit, market]`, mirroring
the `market_book` UncheckedAccount + manual-handle pattern), holding its own
64-byte header (`produced` / `settled` / `cap` / `bump` / `market`) followed by a
flat `cap × 32` slot region. Allocated by `init_fill_commitment`, and
**co-delegated to the ER alongside the `MarketBookAccount`** (shipped) via
`delegate_fill_commitment` / `commit_fill_commitment` /
`commit_and_undelegate_fill_commitment` / `undelegate_fill_commitment`, reusing
the generic `process_undelegation` callback — so the matcher writes it wherever
matching runs and it travels back to L1 with the book. (End-to-end commit/
undelegate is ER-gated: it needs a live MagicBlock ER, same caveat as the existing
`market_book` ER instructions.)

**Two implementation choices that differ from the first sketch (recorded for
honesty):**
1. *Counters live in the account, not in `MarketAccount`.* The `produced`/`settled`
   cursors sit in the `FillCommitmentAccount` header — simpler and self-contained
   than repurposing the dormant `current_batch`/`last_settlement_batch` fields
   (those remain untouched).
2. *The account is passed via `remaining_accounts`, not an Anchor `Option` in the
   context.* An Anchor optional account is not truly omittable — it needs a
   program-id sentinel slot, which would break every existing caller with
   `AccountNotEnoughKeys`. `remaining_accounts` is genuinely optional: existing
   callers pass nothing (legacy behaviour); an armed market appends the account,
   located in-handler by `find_fill_commitment` (owner == program-id +
   `buffer_check` disc/market binding — unique without a hot-path
   `find_program_address`).

## 3. The FLP path (`apply_flp_fill`) — *shipped*

FLP fills are quotes **against pool liquidity**, not book makers — there is no
resting order to commit, so they cannot ride the ring. Authenticity is recovered
by an **oracle-anchored band** instead of exact quote re-derivation.

*Why not exact re-derivation* (the first sketch, now corrected for honesty): the
FLP quote is a function of pool state — vpin, inventory, OI — that **drifts**
between quote-time (ER) and settle-time (L1). Re-deriving `generate_quotes` at
settlement would produce a different result than the matcher actually quoted and
**reject legitimate fills**. So exact re-derivation is unsound here.

*What ships:* an authenticity **bound**. An honest FLP fill is always within the
quoter's spread of fair value, so it must lie within `FLP_MAX_FILL_DEVIATION_BPS`
(2000 = 20%) of the **fresh** `oracle_price_ticks`
(`matcher::flp_quoter::flp_fill_price_within_band`, gated in `apply_flp_fill`,
`FlpPriceOutsideBand`). The band anchors to the oracle (fresh at settle, immune to
quoter-input drift) and caps how far a compromised sequencer can price an FLP fill
from fair value — blocking the pool-drain fabrication (FLP "selling" cheap /
"buying" rich). 20% sits far above any real FLP spread (sub-2% even in stress), so
it never rejects an honest fill. **Proven:** Kani `flp_band_accepts_oracle_price`
(no false reject) + `flp_band_rejects_gross_fabrication` (2×/0× oracle always
rejected); integration `apply_flp_fill_rejects_price_far_from_oracle`.

*Honest residual:* this is a **bound, not exact pinning** — within-band mispricing
remains, economically capped at 20% of notional per (replay-guarded) fill. Exact
pinning would require committing the quoter's input snapshot on-chain per fill (a
larger change), noted as future hardening.

## 4. Trust boundary — honest statement

This removes the **single sequencer key** as a fabrication point. The residual
trust is the **matcher's execution environment**:

- **L1 path:** fully trustless — `place_taker_order_v2` runs on-chain; the ring
  is on-chain; settlement verifies against it. No trusted party.
- **ER path:** trust shifts from one sequencer key to the **MagicBlock ER
  validator** that runs matching and writes commitments, with the L1 commit/
  undelegate as the settlement anchor. This is strictly better — a leaked
  sequencer key can no longer fabricate fills — but it is **not zero-trust**; the
  ER validator set is the new boundary. We state this rather than claim
  "trustless."
- **Cryptographic assumption:** 128-bit `commit` collision-resistance (stated,
  not machine-proven — Kani proves the *state machine*, not the hash).

## 5. Verification plan

Kani proofs over the pure ring state machine (`matcher::fill_commitment`):

| Proof | Property (INV-S1/S2) |
|-------|----------------------|
| `ring_never_over_settles` | `settled ≤ produced` after any push/pop sequence |
| `ring_depth_bounded` | `produced − settled ≤ RING_CAP` (no overflow/aliasing) |
| `settle_rejects_uncommitted` | `c' ≠ tail ⇒ Err` and `settled` unchanged (no fabricated settlement) |
| `no_double_settle` | a consumed slot cannot be settled again (FIFO + monotone `settled`) |

These are bounded, terminating (small array, `u128` equality — no division/large
multiply), and run in the CI Kani job.

## 6. Scope & sequencing

1. Pure `fill_commitment` ring + 4 Kani proofs. *(foundational, reversible)*
2. Producer side: `place_taker_order_v2` pushes commitments. *(+ `FillCommitmentAccount`, PDA, init)*
3. Consumer side: `ApplyFill` / `ApplyFlpFill` gain the account; verify-and-pop.
4. ER: delegate/commit/undelegate the commitment account with the book.
5. FLP deterministic-quote verification in `apply_flp_fill`.
6. Integration tests (fabrication attempt rejected; honest fill settles) + adversarial re-verify + IDL regen.

Items 2–4 are the architectural core; 1 and 5 are independent and can land first.
