# Roadmap

Staged path from this reference simulator to mainnet.

## Phase 0 — Reference simulator (this repo, **complete**)

- [x] FBA Walrasian matcher
- [x] Virtual FLP quoter (Avellaneda-Stoikov-grade)
- [x] Continuous funding via cumulative index
- [x] Stress-lattice cross-margin
- [x] In-loop liquidation engine
- [x] Insurance fund waterfall + ADL
- [x] Commit-reveal protocol
- [x] VPIN toxicity calculator
- [x] 71 unit tests, all passing
- [x] Synthetic flow simulator
- [x] Architecture, math, safety, comparison docs

**Deliverable:** behavioural reference for the Rust program.

## Phase 1 — Production Rust program (next)

Target: deployable to MagicBlock ER devnet.

- [ ] Solana program in Rust (no Anchor — light framework, integer lot space)
- [ ] Account types matching Flash V2 conventions (FLP custody, position
      PDA, market account)
- [ ] FBA matcher in Rust with property-test parity to TS reference
- [ ] FLP virtual quoter as deterministic CPI from FLP pool state
- [ ] Funding integration via cumulative index (u128)
- [ ] Stress-lattice margin computed in compute-budget bounds
- [ ] In-loop liquidation injector
- [ ] Insurance fund PDA with three contribution streams
- [ ] Commit-reveal with bond + L1 force-include path
- [ ] Integration with `@flash_trade/magic-trade-client` session pattern
- [ ] Unit tests in Rust mirroring the TS test suite
- [ ] Anchor IDL or hand-rolled type stubs for TS client
- [ ] Independent security audit (firm to be selected)

**Deliverable:** auditable mainnet-ready program, devnet deployed.

## Phase 2 — Mainnet shadow mode

Run the matcher in **observation mode** against current Flash V2 flow:

- [ ] Devnet → mainnet program deployment
- [ ] Read-only ingestion of mainnet Flash V2 trades
- [ ] Replay each trade through the matcher
- [ ] Compute what FBA + virtual FLP would have done
- [ ] A/B compare against actual Flash V2 pool outcomes
- [ ] 30+ days of shadow data

**Deliverable:** empirical validation that Flash Book outperforms
oracle-only model on LP yield + retail execution quality. Ship gate:
shadow demonstrates ≥ 10% LP yield improvement over the comparison window.

## Phase 3 — Limited production

Open one market (SOL-PERP) to live trading:

- [ ] Whitelist a small set of MMs for resting liquidity
- [ ] Cap per-trader position size at 0.1% of FLP capital
- [ ] Cap insurance fund withdrawals; maintain target balance
- [ ] Real-time invariant monitoring + automatic kill switch
- [ ] 7-day soak with bug bounty program

**Deliverable:** SOL-PERP trading on Flash Book, oracle-priced fallback if
matcher faults, no measurable user-impact regressions.

## Phase 4 — Multi-market rollout

- [ ] BTC-PERP, ETH-PERP onboarded
- [ ] Per-market parameter calibration from shadow data
- [ ] Long-tail markets (per protocol roadmap)
- [ ] Builder-deployed markets (HIP-3-style platform layer)
- [ ] Real-world asset markets (per Flash V3 roadmap)

**Deliverable:** Flash Trade Orderbook V3 fully on mainnet.

## Phase 5 — Continuous improvement

- [ ] Multi-oracle quorum (Pyth + Switchboard + on-chain median)
- [ ] Cross-market netting at the matcher level (basket orders)
- [ ] Maker rebate distribution from toxicity tax pool
- [ ] Spot trading on the same matcher (single-asset markets)
- [ ] Lending integration (cross-margin against spot collateral)

## Open research questions

These don't block any phase but inform design refinement:

1. **Optimal batch interval per market.** 50 ms is calibrated for major
   pairs; long-tail may benefit from 200 ms. Empirical study needed.
2. **VPIN parameter calibration** (α, β, γ, κ, δ). Default values are
   industry-typical; production values should come from 6-month historical
   replay against current Flash V2 flow.
3. **Insurance fund optimal sizing.** 1% of OI is a starting heuristic
   from the October 2025 crash data. Stochastic modeling of bankruptcy
   tail risk should refine this.
4. **ADL fairness improvements.** Current rank by profit/leverage; could
   add tie-breaking randomization within profit-ratio brackets.
5. **Commit bond economics.** Bond size is a parameter; needs analysis of
   spam-vs-friction trade-off on real flow.
