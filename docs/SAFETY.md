# Safety

This document is the threat model and invariant specification. It is the
authoritative reference for what can and cannot go wrong.

## Solvency invariants (always)

These hold at every batch boundary:

| # | Invariant | Where checked (TS sim) | Where checked (Rust program) |
|---|---|---|---|
| S1 | All collateral, capital, fund balances are finite | `engine.checkInvariants()` | checked-arithmetic propagation |
| S2 | Insurance fund balance ≥ 0 | `engine.checkInvariants()` | `InsuranceFund::cover_shortfall` saturates at 0 |
| S3 | No trader has negative collateral without an open position | `engine.checkInvariants()` | `withdraw_collateral` open_positions gate |
| S4 | Σ collateral + LP + insurance ≡ Σ endowments + Σ realized proceeds − Σ realized payouts | property test (12K cases) | property test on matcher core |
| S5 | OI_long = OI_short | `recomputeOpenInterest()` | `update_oi()` per fill, recomputed on each batch |
| S6 | No position has size ≤ 0 (zero-size positions cleared) | `applyFillToTrader` | `apply_fill_to_position` clears on size→0 |
| S7 | No position has entry price ≤ 0 | order intake guard | `place_limit_order` `ZeroPrice` check |
| S8 | Cum funding index finite | `advanceFundingIndex` | `i128` checked overflow |
| S9 | Stress-lattice gate prevents unhealthy traders from opening more positions | n/a | `place_limit_order` margin assessment |
| S10 | Liquidations only fire on actually-unhealthy traders | n/a | `liquidate_position` `assess_margin` check + `NotLiquidatable` reject |
| S11 | Measurement primitives (tick_size, base_lot_size, quote_lot_size, min_base_lots) are immutable post-market-init | n/a | `update_market_params` enforces equality |
| S12 | LP per-batch position growth ≤ pool_capital × max_growth_pct | quoter cap | quoter cap + buffer cap |
| S13 | User order seq < LP_SEQ_RESERVED_OFFSET | n/a | `place_limit_order` reject |
| S14 | Mark price within oracle band (±oracle_band_bps) | `oracleBand()` | `run_batch` clamp |

## Per-trade guards

Every order is checked at intake:

- Size > 0 and finite
- Limit price > 0 and finite
- For takers: trader has sufficient collateral for initial margin on the
  combined post-trade portfolio
- For new positions: insurance fund is above pause threshold

## LP pool safety

The LP pool's exposure is bounded:

- **Per-batch growth cap:** the pool cannot grow its position by more than
  `lp_max_growth_per_batch_pct · pool_capital` in any single batch.
  Default 0.5%. Mathematical floor on the loss rate per unit time.
- **Adaptive spread:** when VPIN spikes (toxic flow), the LP spread widens
  automatically; when pool utilization is high, spread widens more; when
  realized vol spikes, spread widens more. The pool is never forced to
  quote tighter than `s₀` (default 5 bps).
- **Capacity gate:** if pool capital is exhausted, no virtual quotes are
  emitted (orderbook reverts to MM-only liquidity).

## Liquidation safety

- **Single-batch resolution:** all liquidations triggered in a batch clear
  at the same uniform price. Cascades that walk the book in sequence are
  **mathematically impossible** because there's no sequence — all liquidation
  orders enter the same Walrasian clear.
- **Deterministic clearing price:** liquidation price = batch clearing price,
  computed from joint demand-supply curves. No keeper race, no per-liquidator
  MEV.
- **Bankruptcy waterfall:** insurance → ADL. ADL fairness ranked by
  profit/leverage so the most-leveraged-and-profitable positions absorb
  shortfall first.

## MEV / front-running

| Threat | Mitigation |
|---|---|
| Sequencer reorders txs to extract value | batch auction: clearing price is invariant to within-batch ordering. |
| Sequencer observes taker intent before submission | Commit-reveal: hash hides side/size/limit until N+1 batch. |
| Liquidation race by competing keepers | In-loop liquidations: keepers obsoleted; protocol auto-injects. |
| Mark price manipulation via wash trades | Mark = TWAP of clearing prices, oracle-banded. Manipulator pays for every bp moved. |
| Funding-tick sniping (flip flat just before/after tick) | Continuous per-block funding accrual eliminates the discontinuity. |
| Pyth oracle manipulation upstream | Mark-oracle band (±1%) caps divergence; circuit-breaker on Pyth confidence interval. |
| ADL gaming (stay just-not-most-profitable) | ADL ranking is deterministic from public state; gaming requires consistently underperforming. |
| Self-trading to manipulate VPIN | Matcher rejects same-trader-on-both-sides pairings. |

## ER fault recovery

| Fault | Behaviour | Trader outcome |
|---|---|---|
| Sequencer halts mid-session | Periodic L1 commits (every K batches) preserve last known state. | Force-include reveal on L1 + wait for next session, OR settle at last-committed mark. |
| Sequencer censors a specific reveal | Trader posts reveal directly to L1; matcher honors on next sync with original commit timestamp. | Same fill order, with delay. |
| Network partition | ER pauses; commits resume on reconnect. | Positions held; no liquidation while paused. |
| ER outage > timeout (1 h) | Auto-settle on L1 at **last-committed mark**, not current oracle. | Fair valuation; no flash-crash liquidation cascade. |
| L1 reorg of an ER commit | ER state replays from previous commit; affected fills re-clear. | Identical outcome (batch auction is deterministic). |

## Defenses against known production attack patterns

After auditing real 2025 production attacks on major perp DEXes, three
concrete defenses are wired in:

| Attack pattern | Production casualty | Our defense |
|---|---|---|
| Mark-price manipulation via thin upstream sources | Hyperliquid JELLY (Mar 2025), POPCAT (Nov 2025) — ~$5M each | `oracle_staleness_max_seconds` + `oracle_confidence_max_bps` gates on `update_oracle` |
| Coordinated multi-wallet position buildup | Hyperliquid POPCAT — $20M concentrated long via 19 wallets | `max_position_lots_per_trader` cap on `place_limit_order` (per-wallet) |
| Liquidation cascades | October 2025 crash, $5B liquidations overwhelmed funds | In-batch batch auction clearing (atomic; no sequential walk); insurance-fund-first waterfall before ADL |
| Funding-tick sniping | Every CEX with discrete funding | Continuous per-block funding via cumulative-index integral |
| Sequencer front-running | Universal CLOB risk | Commit-reveal (hash hides intent); batch auction uniform-clearing within batch |
| Self-trading wash | Universal | Self-trade prevention in matcher (same-trader pairing skipped) |
| Oracle gaming via stale prices | DeFi-wide | Pyth-style staleness check; wide-confidence rejection |

## ADL trilemma — documented, not solved

The December 2025 paper [*Autodeleveraging: Impossibilities and
Optimization*](https://arxiv.org/abs/2512.01112) proves that no ADL
policy can simultaneously satisfy exchange solvency, revenue, and
trader fairness. As participation scales, a novel form of moral hazard
grows asymptotically, rendering "zero-loss" socialization impossible.

Our design choices, with this trade-off explicit:

- **Insurance fund first** — sized to ~1% of OI (governance-tunable);
  funded by 10% of fees + 50% of toxicity tax + 50% of liq penalty.
  ADL only fires when the fund is exhausted.
- **Profit-ratio × leverage ranking for ADL** — the industry standard;
  most-profitable, highest-leverage positions are deleveraged first.
  This is gameable in principle (a trader near the top can dust their
  position to drop in rank) but the dust is its own cost.
- **Pause-new-positions threshold** — when fund balance falls below
  configured floor, new opening orders are blocked. Existing positions
  can still close.

The trilemma cannot be solved; it can only be navigated. Our defaults
trade slight unfairness to highly-profitable traders for stronger
solvency guarantees.

## What this design does *not* protect against

Honest about open problems and what's outside the protocol's scope:

1. **Pyth oracle bug or compromise.** If Pyth itself reports a fundamentally
   wrong price (e.g. publishers compromised), the mark-oracle band still
   constrains mark within ±1% of the wrong price. Mitigation: governance
   circuit-breaker can pause new positions if Pyth confidence interval
   exceeds threshold; multi-oracle quorum (Pyth + Switchboard + on-chain
   median) is a roadmap item.

2. **Sequencer collusion with maker.** A sequencer that drops *only* its
   colluder's reveals from the next batch can give the colluder a one-batch
   information advantage on that side. Mitigation: sequencer accountability
   bond + censorship-detection from gap-between-commit-and-reveal
   statistics. Requires MagicBlock-side support.

3. **Fundamental black-swan beyond stress lattice.** Our worst-case
   scenario is ±30% (`black_swan_*`). A single-batch ±50% move could
   bankrupt traders faster than the engine can resolve. Insurance fund
   sized to 1% of OI buffers ~50σ of normal-market events; against
   genuine fat-tail shocks, ADL is the backstop, and beyond ADL, socialized
   loss to LPs is the residual exposure.

4. **Smart-contract-level bugs in the deployed program.** The on-chain
   Rust program requires an independent audit before mainnet deployment.

## Audit checklist (for the eventual production deployment)

Status of items already in code:

- [x] **No floating-point arithmetic in matcher path** — all `u64`/`u128`/`i128` with checked ops
- [x] **Account ownership via PDA seeds + bump verification** — Anchor account macros
- [x] **Signed-vs-unsigned arithmetic correctness in funding** — `i128` cumulative index, sign-aware
- [x] **Overflow protection on cumulative funding index** — Q64.64 fixed-point i128 — multi-decade headroom
- [x] **Per-trader rate limit on order submissions** — 16/batch via `TraderState.orders_this_batch`
- [x] **Bond mechanics for orphaned commits** — `sweep_expired` returns total seized
- [x] **Stress-lattice margin gate on order intake** — `place_limit_order` rejects unhealthy
- [x] **Stress-lattice validation on liquidation** — `liquidate_position` rejects healthy
- [x] **Numbered error families** — easy classification for monitors
- [x] **Status circuit breaker** — Active / PostOnly / Paused / Closed
- [x] **Authority transfer with explicit event audit trail**
- [x] **Property tests** — 6 properties × 2K random cases on the matcher core

Pending for production audit:

- [ ] Re-entrancy guards on every state-mutating instruction (Anchor's borrow checker covers most; explicit verification of CPI paths needed)
- [ ] Fraud-proof challenge window for ER state commits — depends on MagicBlock-side
- [ ] Force-include path tested under sequencer-down scenarios — depends on MagicBlock-side
- [ ] Insurance fund cap to prevent over-collection (excess returns to LPs) — design decision needed
- [ ] ADL randomization within tied profit-ratio brackets to prevent gaming
- [ ] Cross-market portfolio liquidation via remaining_accounts iteration
- [ ] SPL token transfer integration on `deposit_collateral`/`withdraw_collateral` (currently accounting-only)
- [ ] Real Pyth oracle CPI inside `run_batch` (currently authority-set)
- [ ] BPF compilation + Mollusk/litesvm integration tests (blocked: upstream platform-tools edition2024)
- [ ] Independent third-party audit (firm to be selected post-production-readiness)
