# Flash Book — ER Orderbook Deep Audit & Competitive Hardening

Consolidated findings from a 5-track deep audit (hypertree, matching engine,
risk/margin/liquidation, ER-integration + program security) plus a competitive
comparison against the reference ER-orderbook implementations. Each finding cites
`file:line`. Fix status tracked inline.

> Scope reminder: **devnet, not audited, not production-ready.** Encoding/layout
> changes below are safe because there is no live book to migrate (fresh deploys).

---

## 0. Competitive verdict — vs reference ER orderbooks

Repos reviewed: `a native zero-copy CLOB reference` (native zero-copy FIFO CLOB → Percolator), `a TEE private-CLOB reference`
(TEE dark-pool CLOB), `a native perp-router reference`/`a Manifest-on-ER reference`/`a Phoenix-on-ER reference` (ER ports), `an external risk-engine reference` (risk engine).

**flash-book is the better *exchange*; the reference implementations has the better *ephemeral rollup*.**

| Dimension | flash-book | the reference implementations |
|---|---|---|
| Feature breadth | **16+ order types, full perp risk engine** | thinner (FIFO + external Percolator) |
| Book structure | **dynamic RBT hypertree** | array/FIFO + Phoenix/Manifest ports |
| CU | **12.5k–22k measured** (beats Phoenix) | comparable |
| Tests/proofs | 576 tests, Certora hooks | **81 Kani harnesses, full Certora suite + audit PDF (a Manifest-on-ER reference)** |
| **ER lifecycle** | Delegate/Undelegate only — **no commit/Magic-Action path** | **battle-tested receipt-action settlement** (a native zero-copy CLOB reference/a native perp-router reference/private_er) |
| Delegated unit | **incoherent** (book delegated, positions not) | coherent (book is the delegated unit) |
| Privacy | none | **TEE-backed private/dark-pool ER** |

**To beat his ER orderbook:** (1) implement the commit / `process_undelegation`
callback + receipt-action settlement, (2) make the delegated unit coherent,
(3) match the formal-proof depth (Certora end-to-end — a Manifest-on-ER reference proves the
same Manifest-lineage book *can* be fully verified).

---

## 1. CRITICAL findings

### ER-1 — delegate/undelegate completely unauthenticated  ✅ FIXED
`lib.rs` `DelegateMarketBook`/`UndelegateMarketBook`/`DelegateMarket`/`UndelegateMarket`
declared `authority: Signer` bound to nothing — **any anonymous caller could rip
any market out of the ER** (one-tx repeatable DoS + state-consistency attack).
**Fix:** added `constraint = market.authority == authority.key() @ Unauthorized`
to all 5 lifecycle structs (incl. `InitMarketBook`). `cargo build-sbf` clean.

### ER-2 — no commit / `process_undelegation` callback  ✅ WIRED (devnet 7/7 is the last gate)
**UPDATE — fully wired + on-chain-compiling.** Added 3 `#[program]` ixs:
`commit_market_book` / `commit_and_undelegate_market_book` (ER-side, CPI the Magic program)
and `process_undelegation` (base-layer callback). **Key finding:** `EXTERNAL_UNDELEGATE_DISCRIMINATOR`
is *exactly* `sha256("global:process_undelegation")[..8]` → the callback is an ordinary Anchor
instruction (Anchor auto-derives the matching discriminator — verified), so **no entrypoint
fallback is needed** (removes the "wrong fallback breaks all dispatch" risk). 566/566 tests green
(dispatch intact), build-sbf clean. `CommitMarketBook` permissionless (Phoenix-ER pattern; Magic
program/validator enforce semantics; authority gating is on the base `delegate_*` ixs per ER-1).
`ProcessUndelegation` mirrors the SDK `InitializeAfterUndelegation` contract exactly. **Remaining:**
the devnet-ER 7/7 lifecycle test — the only thing a live MagicBlock ER is needed for.

<sub>Original core-done notes below.</sub>

**Root cause confirmed:** the real MagicBlock model is — Delegate (base, disc 0) →
**Commit / CommitAndUndelegate on the ER** (CPI to the *Magic* program `Magic111…`,
NOT the delegation program) → the **delegation program CPIs back** into this program
on base with `EXTERNAL_UNDELEGATE_DISCRIMINATOR` to reopen+copy. flash-book's old
`undelegate_market_book` (disc-3 from base) is the *internal reopen primitive*, which
belongs **inside the callback**, not as a user instruction.

**Implemented + unit-tested (`er.rs`):**
- Constants: `MAGIC_PROGRAM_ID = Magic111…`, `MAGIC_CONTEXT_ID = MagicContext111…`,
  `EXTERNAL_UNDELEGATE_DISCRIMINATOR = [196,28,41,206,48,37,51,167]`, commit enum
  tags `ScheduleCommit=[1,0,0,0]` / `ScheduleCommitAndUndelegate=[2,0,0,0]`
  (byte-verified vs the SDK's bincode test).
- `cpi_commit(payer, magic_context, magic_program, committed, allow_undelegation)` —
  hand-rolled Magic-program CPI (2.1-clean), account order `[payer, magic_context, …committed]`.
- `process_external_undelegate(accounts, seeds_data)` — the base-layer callback:
  verifies `buffer.is_signer` + `buffer.owner == DELEGATION_PROGRAM_ID`, re-derives
  the PDA, `create_pda` reopens it program-owned at `buffer.data_len()`, copies the
  committed buffer back (length-guarded — no panic).
- `create_pda` helper (create_account / allocate+assign), and 4 unit tests
  (magic IDs canonical, commit tags, undelegate discriminator, seed-payload borsh).

**Remaining wiring (needs the ER runtime to validate — do NOT guess-wire untested):**
1. ER-side `#[program]` instructions `commit_market_book` / `commit_and_undelegate_market_book`
   → call `er::cpi_commit(.., false/true)`. Account constraints differ in the ER context
   (delegated `market_book` owner = delegation program), so validate on a devnet ER.
2. The **callback dispatch**: the delegation program invokes this program with the raw
   `EXTERNAL_UNDELEGATE_DISCRIMINATOR` prefix, which Anchor's normal 8-byte dispatch won't
   match — needs the program `fallback` (Anchor 0.31) to route `data[..8]==DISC` →
   `er::process_external_undelegate(accounts, &data[8..])`. The SDK does this via the
   `#[ephemeral]` macro; flash-book must replicate it. **Verify the Anchor-0.31 fallback
   mechanism before wiring — a wrong entrypoint override breaks ALL dispatch.**
3. Keeper sequencing (port `private_er .../app/keeper.ts:102-125`): commit on ER → wait →
   read committed bytes on base (`UncheckedAccount`, parse raw — full deserialize overflows
   the BPF stack) → reset fills on ER.
4. **Validation gate:** a devnet-ER end-to-end lifecycle test (init→delegate→match→commit→
   base-reconcile→undelegate), mirroring the reference implementations' 7/7. This is the real acceptance test;
   ER settlement cannot be trusted on unit tests alone.

**Also:** delegated-unit coherence — decide whether per-user ledgers/positions are delegated
alongside the book (the reference implementations delegates the book + per-user ledgers separately so matching
+ fund-locking both run on the ER). Today flash-book delegates the book only.

### MATCH-1 — bids fill LIFO, not FIFO  ✅ FIXED
`state_v2.rs encode_order_id` inverted the **whole word** for bids, flipping the
seq tiebreak → newest bid filled first at every price level (riskless queue-jump).
**Fix:** invert **only the price field** for bids; seq stays ascending. Added 3
regression tests (`same_price_bids_fill_fifo_not_lifo`, asks variant, price-priority).
All 576 tests pass.

### MATCH-2 — seq truncated to 16 bits (FIFO wrap + order-id collisions)  ✅ MITIGATED
After 65,536 orders, seq wrapped and `order_id`s collided (ordering compares
order_id only → cancel-wrong-order). **Fix:** widened layout to **40-bit price /
24-bit seq** (256× headroom) + **saturating** price clamp (old code masked → could
wrap a high price to a tiny key). Consistent with `FLP_SEQ_RESERVED_OFFSET = 2^56`
(FLP seqs were always low-bits-truncated; no hard guard added — would break FLP).
*Residual:* `order_id` is a (price, 24-bit-seq) tiebreak, not globally unique —
true uniqueness needs a decoupled lookup handle (Manifest's approach). Tracked.

### RISK-1 — funding settlement not zero-sum / not vault-reconciled  ✅ FIXED (residual-tracking)
`settle_funding` moved trader collateral with no counterparty and never touched the
solvency residual — so non-zero-sum funding (entry-priced + lazy) silently minted/burned
protocol collateral and the haircut/kill-switch baseline drifted. **Fix:** `SettleFunding`
now binds the per-market `MarketHaircutStateAccount` (mut), and the handler delta-tracks
the **actual** collateral moved into the residual `V − C_tot − I`:
- trader **pays** `paid` → C_tot ↓ → `residual += paid` (checked_add)
- trader **receives** `received` → C_tot ↑ → `residual −= received` (checked_sub → `HaircutResidualUnderflow` if it would go negative, i.e. insolvency, rejected).
Net-zero-sum funding nets to zero across positions; any genuine drift now moves the residual
so the kill-switch can see and bound it — funding is now in the same "every money-moving ix
delta-tracks residual" invariant as deposit/withdraw/fee/liquidation. 580/580 Rust tests green.
**Follow-ups:** (a) the TS SDK ix builder + IDL need regenerating to pass the new `haircut_state`
account (program-side is correct; TS callers must add it). (b) **RISK-M2** still open — `settle_funding`
uses *entry-price* notional while `assess_margin` uses *mark-price* notional; pick one basis
(mark is standard) so the charged vs assessed funding match. (c) A per-market funding **pool**
(audit option a) remains a cleaner long-term model than residual-absorption.

### RISK-2 — no initial-margin buffer (IM == MM)  ✅ FIXED (4 gates)
Every gate used `maintenance_margin_ratio_bps`; `initial_margin_ratio_bps` existed
+ validated (`lib.rs:3931/3954`) but **was never wired**. Traders could open exactly
at the liquidation boundary → structural bad debt. **Fix:** the 4 open/withdraw gates
now build their risk snapshot with `initial_margin_ratio_bps` (IM > MM buffer):
`sweep_collateral`, `partial_withdraw_collateral`, `place_basket_order_v2`,
`place_basket_order_n_v2`. The **9 liquidation/ADL/view sites keep
`maintenance_margin_ratio_bps`** (verified — using IM there would wrongfully
liquidate healthy positions). **Owner decision deferred:** `set_position_isolated/cross`
(mode switches) left on MM — changing them to IM is the *conservative* direction
(stricter, can't create bad debt) but could block a risk-*reducing* mode switch for a
thin position; confirm intended UX before changing.

---

## 2. HIGH findings

| ID | Where | Issue | Status |
|---|---|---|---|
| HYP-H4 | `utils.rs:10-19` | unchecked `bytemuck` indexing → panic on malformed account | ✅ ADDRESSED — `from_account_data` requires `data.len() == MARKET_BOOK_TOTAL_BYTES` (exact) so short accounts can't load + all indices are tree-derived; added `get_helper_checked`/`index_in_bounds` (defense-in-depth) + test |
| HYP-C1 | `red_black_tree.rs:568-612` | equal-key dual-subtree recursion | ✅ ASSESSED — NOT a fix-needed bug. Re-examined: for a balanced RBT it is O(N) work / O(log N) stack (N ≤ MAX_NODES ≈ 100), not exponential. Equal-key lookup (Ord-by-price, Eq-by-price+id) is a real **tested** capability (`test_lookup_equal`/`TestOrder2`); forcing uniqueness breaks it. Left correct as-is. |
| HYP-C2/H2 | `free_list.rs:55-60`, `state_v2.rs` | freed 96-byte slot only 84 bytes scrubbed | ✅ FIXED — `FreeListPadding` padded 80→92 so `FreeListNode == NODE_TOTAL_BYTES` (96); `add()` now scrubs the **whole** slot, no stale RBNode bytes. Compile-time size assert added. |
| MATCH-H1 | `lib.rs:479-511` | taker path skips OI cap → blow past per-market OI hard limit | ✅ FIXED (mirrored limit-path guard into taker intake) |
| MATCH-H2 | `lib.rs:698-719` | residual taker rests below `min_base_lots` (dust) | ✅ FIXED — residual only rests if `remaining >= min_base_lots`; sub-min remainder dropped (IOC-style) |
| MATCH-H3 | `state_v2.rs:516` | `decrement_size_at` saturating-sub masks over-fill accounting | ✅ FIXED — now `-> Result` with `checked_sub`; over-decrement rejected (caller `?`, test updated) |
| ER-H2 | `lib.rs:10827` | `apply_fill` trader-state accounts unconstrained | ✅ ASSESSED — already enforced. The handler calls `verify_trader_state_pda(sub_index, account.trader, account.key, program_id)` which re-derives the canonical PDA (main `[SEED,trader]` / sub `[SEED,trader,sub_index]`) and asserts it matches — exactly the recommended fix. Audit flagged it from the struct alone; the seeds were dropped *deliberately* for sub-accounts and replaced by this handler check. **Residual (architectural):** the sequencer still selects which (valid) trader's fill to apply; fully closing that needs an on-chain fill receipt the matcher commits + apply_fill verifies against (decoupled-relay model). Documented, not a quick fix. |
| RISK-H1 | `risk.rs:89-150` | crowded-trade OI-scaled MMR fed hardcoded 0 | ✅ RESOLVED — documented INACTIVE (no MarketParams to configure; omission is conservative-safe) + activation steps recorded. No false assurance. |
| RISK-H2 | `cross_margin_weights.rs` | cross-margin netting dead + dangerous-if-wired | ✅ FIXED — module + proptest **deleted** (zero production callers; `assess_margin_unified` sums conservatively). Removes the latent sign-convention footgun. |
| RISK-H3 | `liquidation.rs:129-133` | `compute_shortfall` PnL unchecked `*` → panic/wrap | ✅ FIXED — `pnl` now `checked_sub`/`checked_mul` + `or_overflow`, matching the `penalty` path; downstream i128→u64 saturation kept |

---

## 3. Verified-clean areas (no findings)

- RBT rotations/insert/delete fixup, min/max/successor (price-time order correct);
  matcher snapshots indices before mutating (no iterator invalidation).
- Matching: price priority + crossing-at-maker-price correct; FOK all-or-nothing;
  `lot.rs`/`pro_rata.rs` u128 + conservation; loop integer safety.
- Risk: haircut value-conservation, per-slot envelope, oracle dual-source worse-of
  + staleness + confidence, isolated-margin bucket independence, overflow discipline.
- Security: PDA/bump hygiene, arithmetic safety, authority-burn/sequencer decoupling,
  state migration (`migrate_market_to_v3`).

---

## 4. Recommended fix order

1. **ER-1** ✅ done · **MATCH-1/2** ✅ done
2. **RISK-2** (IM buffer) — in progress, classified above
3. **MATCH-H1** (taker OI cap) — small, high-value
4. **RISK-1** (funding conservation) — solvency-critical, larger
5. **ER-2** (commit/undelegation callback) — the differentiator vs. reference implementations
6. HYP-H4 / HYP-C2 (panic-safety + slot scrub), then remaining H-tier
7. Certora end-to-end (match a Manifest-on-ER reference) for production credibility
