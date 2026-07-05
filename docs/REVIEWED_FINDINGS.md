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
