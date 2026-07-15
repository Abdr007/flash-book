# Clober Invariants

This is the authoritative production invariant specification for Clober. A
release is acceptable only when its implementation, IDL, tests, formal checks,
and deployed bytecode agree with these properties. An invariant that cannot be
verified from source or a live transaction is not treated as satisfied.

## Settlement Authenticity

1. Every settled fill is bound to an authenticated FIFO entry in the market's
   commitment ring. `apply_fill` and `apply_lp_fill` recompute the commitment
   preimage and reject omitted, fabricated, replayed, altered, or out-of-order
   fills.
2. Settlement binds the market, both parties, side, size, price, sub-account,
   and JIT flag. A caller cannot redirect value by substituting accounts.
3. Settlement sequence numbers are program-derived. Caller-supplied sequence
   values cannot advance or wedge the market sequence.

## Custody And Accounting

1. Program-owned collateral, insurance, LP, and fee-accrual balances use
   checked arithmetic and reject invalid account ownership or PDA bindings.
2. Every collateral release path enforces the applicable margin and
   ER-reservation floor before value leaves the protocol vault.
3. Fee accruals are capped by collected fee surplus; claims reduce the matching
   on-chain liability and cannot draw from protected insurance or LP balances.
4. A market is halted on a detected invariant breach. Recovery requires an
   explicit, authorized operational action rather than silently continuing.

## Position And Risk

1. Positions are canonical PDAs bound to one trader state and one market.
   Cross-market or cross-sub-account substitution is rejected.
2. Opening exposure requires positive, tick-aligned lots and prices, a live
   market status, configured limits, and sufficient post-trade portfolio margin.
3. Reduce-only capacity includes committed in-flight reductions, so concurrent
   matching and settlement cannot flip an exposure through zero.
4. Long and short open interest are updated symmetrically for each settled fill;
   invariant verification detects a mismatch and halts the market.
5. Liquidation uses the adverse price for the position from mark and oracle
   sources. A stale or unconfigured required source fails closed rather than
   authorizing a liquidation from an untrusted price.

## Oracle And LP Safety

1. Oracle updates enforce configured source identity, publish-time freshness,
   confidence bounds, and movement envelopes. A locked market accepts only its
   configured trustless ingestion path.
2. LP settlement requires the same fill commitment as trader settlement. LP
   prices must also lie within the configured oracle deviation limit and meet
   the oracle freshness requirement.
3. LP capital, inventory, withdrawal, and exposure limits are checked before
   state mutation. A pool cannot withdraw capital that backs open exposure.

## Authority And Availability

1. Market authority changes require two-step acceptance; guardians can restrict
   risk but cannot loosen it. Economic parameter changes are timelocked.
2. The upgrade authority and market authorities must be multisig-controlled in
   production. Deployment is incomplete until this is independently verified
   on Solana Explorer.
3. Sequencer ordering is a documented liveness assumption, never a custody or
   settlement-authenticity assumption. The exact boundary is in
   `ER_TRUST_BOUNDARY.md`.

## Verification Evidence

- The SBF-runtime integration suite exercises settlement, oracle, liquidation,
  custody, and failure paths against the compiled program.
- Kani harnesses and Lean modules prove scoped arithmetic and state-transition
  properties. Their current command and scope are in
  `docs/FORMAL_VERIFICATION.md`.
- `verify_market_invariants` provides an on-chain operational detector for
  market-level accounting checks.

The README and `docs/DEPLOYMENT.md` define the release gates that turn these
source-level properties into deployment evidence.
