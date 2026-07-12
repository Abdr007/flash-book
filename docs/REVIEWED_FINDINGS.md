# Reviewed findings — accepted tradeoffs and false positives

This records findings that were investigated and deliberately **not** changed,
so a future review does not re-attempt a change that is unsound, unnecessary, or
worse than the current behavior. Each entry states what was checked and why the
code stands as-is.

## Accepted tradeoffs (real, but safer left as-is)

- **Withdraw/sweep health uses the stored mark, not a forced-fresh oracle.**
  Requiring a fresh oracle and failing closed on a stale mark would block
  withdrawals during an ER stall — directly against the trustless exit guarantee
  (a trader must be able to exit when the ER is dark). The stored mark is already
  clamped to the oracle band, and the stress-lattice margin check plus the
  initial-margin buffer bound the residual. Leaving it is the safer choice.

- **Session token scope has no per-session size/leverage/notional cap.**
  `SessionTokenAccount` is a fixed-length (zero-slack) account, so adding a cap
  field is an account migration. The blast radius is already bounded (per-market
  scope, a 24h TTL cap, and revoke), and collateral is never movable by a
  session key. A migration is not warranted for this bound.

- **Session expiry is evaluated against the ER clock.**
  On the ER the clock is the sequencer's, so a rogue sequencer could hold an
  expired session valid. This is bounded by the single-sequencer trust model
  (collateral is L1-authoritative and fills settle through the sequencer-gated
  path regardless), and closing it fully requires an L1-slot-high-water-mark
  redesign. It is the documented single-sequencer residual, not a separate bug.

- **The intake initial-margin gate is not applied on the v3 injection paths**
  (TWAP slice / iceberg / bracket / entry-trigger). The gate is intake-only and
  advisory — the position opens at settlement regardless — and these paths sit
  on the matching hot path. It is a real gap but low-value; if ever applied it
  should be its own isolated hot-path pass with compute measurement, mirroring
  the taker/limit gate exactly (position-aware, reduce-exempt, fail-closed).

- **G-1: an UNDELEGATED-market resting L1 order's initial margin is not reserved
  at withdraw.** On a `book_delegated == false` market a trader can rest an L1
  limit order, withdraw the backing collateral (the withdraw gate reserves only
  FILLED positions + the ER-attested `er_reserved`, never a resting order's IM),
  and have the order later fill into an undercollateralized position. A sound and
  *complete* on-chain reservation to close this is **architecturally precluded**,
  confirmed by two independent investigations:
  1. *No completeness anchor.* There is no persistent per-trader live-order
     count/sum (`TraderStateAccount` has none, and the `reserved_im` accumulator
     in `xmargin.rs` is a pure, unwired arithmetic core). A lazy "walk the books
     the trader provides" gate therefore has an **omission hole** — a trader
     simply omits a book to hide live orders and under-reserves.
  2. *An accumulator field cannot be maintained.* The two removal sites that fire
     without the owner initiating them — bulk `reap_expired_orders` (many traders,
     zero `TraderState` accounts) and the maker side of a taker walk (only the
     taker's `TraderState` loaded) — structurally cannot carry the owner's
     `TraderState` (Solana account limits, multi-trader single-tx). So any such
     field drifts stale-high on every reap/maker-fill and would **permanently
     over-lock** the trader's own collateral.
  Crucially, the residual loss is **BOUNDED and machine-proven bounded**: an
  undercollateralized fill saturates the loser to 0 (`cross_loss_shortfall`),
  draws insurance capped at the fund balance (`cover_bad_debt`), and on
  permissionless markets isolates to ADL + the solvency-gated haircut — never an
  unbacked mint (`bad_debt_coverage_is_insurance_isolated_and_bounded`,
  `matcher/insurance.rs`). The gap is confined to the **undelegated (fallback /
  censorship-escape) mode**; the ER-delegated production path is already closed
  by the sequencer-attested `ErMarginAttestation`. Closing it on-chain would
  require re-architecting the proven `apply_fill` settlement core plus the reap /
  taker-walk paths for a bounded, fallback-mode loss — disproportionate risk. The
  bound is pinned end-to-end by `residual_undelegated_l1_resting_order_*` in the
  integration suite. If ever revisited, the only sound lever is an off-chain
  attested figure (as the ER path already uses), not an L1 placement-time
  reservation.

## False positives (a "fix" would break real behavior — do not re-attempt)

- **Lazer `skip_prop` unknown-property width.** The real, Ed25519-verified Lazer
  payload carries an unknown extension property (id 12) that the parser must
  skip as 2 bytes. Failing closed on an unknown property id **breaks parsing of
  legitimate prices** (it fails the real-message parse test). The 2-byte skip is
  load-bearing for the current wire format; a wider future property would need a
  format update, not a fail-closed change.

- **Oracle staleness bound can be zeroed.** Already enforced: `initialize_market`
  and `update_market_params` both require `oracle_staleness_max_seconds > 0`, so
  no market can be born or updated with the staleness gate disabled.

## Cross-references

The single-sequencer ordering/liveness trust assumption and the external/ops
gates (professional audit, multisig authority migration, MagicBlock
owner-recovery for the trustless censorship exit) are in
[../SECURITY.md](../SECURITY.md) and
[../ER_TRUST_BOUNDARY.md](../ER_TRUST_BOUNDARY.md).
