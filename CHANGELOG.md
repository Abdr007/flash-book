# Changelog

All notable changes to Clober are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
