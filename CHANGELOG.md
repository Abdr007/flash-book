# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-05-08

### Added
- Initial reference design + simulator.
- FBA matcher with Walrasian uniform-price clearing.
- Virtual FLP quoter — Avellaneda-Stoikov-grade inventory-aware quoting,
  VPIN-driven adverse-selection widening, depth amortization, realized-vol
  spread term.
- Continuous funding via cumulative index (per-block accrual, eliminates
  funding sniping).
- Stress-lattice cross-margin (single-asset shocks ±2/5/10/20%, correlated
  shocks, black-swan ±30%; recognizes hedges).
- In-loop liquidation engine: detection from prior-batch mark, order
  injection into current batch, deterministic clearing.
- Insurance fund with three contribution streams (fees / toxicity tax /
  liq penalty) and bankruptcy waterfall.
- ADL (auto-deleveraging) by profit/leverage rank when insurance is
  exhausted.
- Commit-reveal taker protocol with bond + expiry sweep.
- VPIN volume-synchronized toxicity calculator with EMA over buckets.
- Synthetic flow simulator demonstrating end-to-end behaviour at
  ~42 K batches/sec wall-clock on Apple Silicon.
- 71 unit tests across all modules.
- Architecture, math, safety, comparison, roadmap docs.
