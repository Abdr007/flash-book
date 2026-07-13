# Changelog

All notable changes to Clober are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] — 2026-07-14

Second devnet release. Merged via PR #347 (`535fe60`) with CI 8/8 green
(tests, SBF 0-warn, clippy/fmt, Kani, Lean, Certora, IDL-drift, cargo-audit)
and deployed to devnet by upgrading program
`5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` (slot 476056517); the deployed
bytecode was hash-verified against the local build.

### Added
- **Bootstrap floor for the OI-vs-insurance circuit breaker.** New per-market
  `oi_insurance_floor_notional` field, `set_oi_insurance_floor_notional`
  instruction, and `OiInsuranceFloorSetEvent`. The effective cap is now
  `max(insurance_balance · multiple_bps / BPS_DENOM, floor_notional)`, so the
  breaker can be enabled on a fresh, thin-insurance market without auto-pausing
  it on the first fill (the insurance-scaled cap alone collapses to ~0 at
  bootstrap). A floor only ever raises the ceiling — it never trips more often
  than the floorless breaker. Opt-in; a zero floor is the prior behaviour, and
  legacy market accounts read the new trailing field as 0 (no migration).

### Security
- **Bounded per-period funding on permissionless markets.** Funding was
  clamped per-second but left unbounded per period on permissionless markets, so
  a predatory crank could transfer unbounded value from a counterparty. Wired
  the per-period funding backstop (`clamp_delta_to_period_cap`) into the crank
  and require bounded funding params on the permissionless path, with a cap-hit
  event.

### Fixed
- **Kani proof tractability.** Extracted division-free cores
  (`notional_exceeds_effective_cap` for the OI breaker, `clamp_to_symmetric_bound`
  for the funding per-period cap) so their bound/monotonicity proofs verify over
  a symbolic cap instead of forcing CBMC to bit-blast a symbolic 128-bit divider
  (which did not terminate). Division-dependent magnitudes remain covered by host
  tests. Behaviour of both functions is unchanged.
- Boxed the `MarketAccount` in the `CancelAuthorityTransfer` and
  `ExecuteParamUpdate` account contexts so the new trailing field does not push
  their `try_accounts` frames past the 4 KiB BPF stack limit (SBF back to 0-warn).

## [0.1.0] — Initial release

First public release of Clober — a formally-verified, on-chain central-limit
order-book (CLOB) perpetuals engine that matches continuously on a MagicBlock
Ephemeral Rollup and settles on Solana.

### Engine
- Slab-backed order book over a hypertree (three overlapping red-black trees:
  bids, asks, claimed seats) with strict price-time priority; every book
  operation stays off the BPF stack.
- Continuous matching on the Ephemeral Rollup; fills are authenticated through
  an on-book commitment ring and applied to base-layer position state by a
  permissionless, ring-verified settlement path.
- Pool-backed liquidity: a singleton liquidity pool auto-quotes a two-sided
  ladder around a fresh oracle, plus optional per-market pools.

### Risk & solvency
- Cross- and isolated-margin engine with a committed-margin reservation on the
  trader state, worse-of(mark, oracle) valuation under a staleness gate, and a
  full-portfolio initial-margin walk on every open and withdraw.
- Liquidation, auto-deleverage, insurance-fund waterfall, and a paper-profit
  haircut with a warmup reserve for junior-claim protection.
- Conservation of value (collateral + insurance + residual) is preserved by
  every money-moving instruction.

### Oracles
- Pyth pull and Lazer feeds behind a single gated reader enforcing freshness,
  confidence, owner, feed, and sign checks; decimal→tick→1e9 scaling verified
  at sub-cent and six-figure marks.

### Formal verification
- Kani proof harnesses over the matcher core, margin frame, fill-commitment
  ring, price bands, liquidation, insurance solvency, and order-id priority.
- Lean theorems (haircut solvency, OI/MMR bounds, funding zero-sum, realized
  PnL, residual conservation, authorization completeness, pool-share accounting)
  proved at the real 1e9 divisors.
- A Certora Solana Prover solvency rule over the real `assess_solvency_full`
  invariant.

### Status
Deployed to devnet, unaudited. A third-party external security audit is the
single remaining dependency before a real-funds mainnet launch.
