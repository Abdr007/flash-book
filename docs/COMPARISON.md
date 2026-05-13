# Flash Book vs other perp DEXes

Honest head-to-head against Hyperliquid, Drift v2, dYdX v4, Phoenix, GMX
v2, and Aevo. Claims about Flash Book are backed by file:line references
into `programs/flash-book/`; claims about competitors are sourced to
their public docs and linked at the bottom.

This document supersedes the marketing draft. The earlier version
overclaimed "FBA Walrasian clearing" and "commit-reveal" as on-chain
features — neither is in the on-chain program. The TypeScript simulator
in `src/` has FBA + commit-reveal as research artifacts; the actual
Anchor program is continuous CLOB on a hypertree. This rewrite reflects
the on-chain reality.

## TL;DR

Flash Book's defensible novelty is the **risk + liquidation engine**, not
the matching engine.

- **Matching:** continuous CLOB on a hypertree, no different in mechanics
  from Hyperliquid / dYdX v4 / Phoenix.
- **Margin:** stress-lattice (CME SPAN-style) with strict cross/isolated
  bucket independence (Phase 2). No competitor combines both.
- **Liquidations:** dual-source price gate + Dutch-auction reward +
  per-position cooldown + per-bucket reward routing + JIT-liquidation
  auction. No single competitor has all of these.
- **Funding:** cumulative-index per-block, settled lazily on-chain.
  Granular than Hyperliquid / dYdX / Drift hourly snapshots.
- **Sub-accounts:** full trading via distinct Position PDAs per
  TraderState. Phase 2c–2f.

Where Flash Book demonstrably loses:
- Battle-testedness (Hyperliquid has years and billions in OI).
- Speed (Hyperliquid's in-consensus orderbook at ~70 ms median is
  unmatched).
- Realized PnL doesn't materialise to collateral on close (real bug,
  documented).

## The matrix

| | Hyperliquid | Drift v2 | dYdX v4 | GMX v2 | Phoenix | **Flash Book** |
|---|---|---|---|---|---|---|
| Chain | own L1 (HyperBFT) | Solana | own Cosmos chain | Arbitrum | Solana | **Solana (+ MagicBlock ER target)** |
| Consensus / block time | HotStuff variant, ~70 ms [1] | Solana, ~400 ms slot | CometBFT, ~1 s, < 2 s final [4] | Arbitrum ~250 ms | Solana | **Solana ~400 ms** |
| Matching engine | continuous CLOB at L1 consensus [1] | DLOB (off-chain keepers) + JIT auction + vAMM [3] | continuous CLOB in validator mempool [4] | oracle-priced, pool counterparty [5] | atomic CLOB (no perps) | **continuous CLOB on hypertree** |
| Orderbook in chain state | yes (L1) | no (keeper-maintained) | no (mempool, not consensus) | n/a | yes (transient) | **yes (`market_book` PDA)** |
| Matching primitive | price-time priority FIFO | DLOB walk + Dutch JIT for takers | price-time FIFO | n/a | price-time FIFO | **price-time FIFO** |
| Funding cadence | hourly settlement [2] | hourly [3] | hourly epoch, governance-tunable [4] | continuous borrow fees; funding to weak side [5] | n/a | **per-block cumulative index** |
| Funding settlement | implicit (charged on touch) | implicit | implicit | implicit | n/a | **explicit `settle_funding` ix** |
| Liquidation executor | HLP vault [2] | open keeper bots [6] | validators / keepers [4] | keeper bots, Chainlink [5] | n/a | **open keeper bots + JIT-offer makers** |
| Liquidation reward | HLP captures spread [2] | per-size fee | governance-set fee | keeper fee | n/a | **Dutch auction 0% → 100% over `liquidation_auction_duration_slots`** |
| Liquidation price gate | mark = blend of CEX prices + own book [2] | maintenance margin breach | maintenance margin breach | oracle [5] | n/a | **dual-source: worse-of(mark, oracle); refuses on stale oracle** |
| Partial liquidation | yes | yes | yes | n/a | n/a | **yes (`requested_close_lots`)** |
| Per-position cooldown | not documented | no | no | no | n/a | **`liquidation_cooldown_slots`** |
| JIT-liquidation auction (open) | HLP only (single vault) | no | no | no | n/a | **`place_jit_liquidation_offer` — any maker** |
| Cross margin | yes | yes (cross-collateral, asset weights) [6] | yes | n/a | n/a | **stress-lattice (CME SPAN-style)** |
| Isolated margin | yes | yes | yes | n/a | n/a | **yes (Phase 2; strict bucket insulation)** |
| Sub-accounts | yes | yes | yes | n/a | n/a | **yes (Phase 2c–2f; main + 255 sub PDAs per wallet)** |
| Insurance fund | yes | yes | yes | n/a | n/a | **yes (`InsuranceFundAccount`)** |
| ADL | yes (after HLP backstop) [2] | yes | yes | n/a | n/a | **yes (`auto_deleverage` ix, bankruptcy-price math)** |
| Multi-oracle quorum | internal aggregation | partial | yes | Chainlink Data Streams | n/a | **median-of-3 + dispersion gate (`update_oracle_quorum`)** |
| Mark price | TWAP + CEX blend [2] | oracle + DLOB | oracle + book | oracle | last trade | **EMA-blended cleared-trade prices, banded by oracle** |
| Open source | partial (HyperEVM contracts only) | yes | yes | yes | yes | **yes** |
| Production mainnet record | billions in OI | hundreds of millions | hundreds of millions | hundreds of millions | live | **devnet only, no audit** |

## What's actually novel in Flash Book (verified against code)

A claim is "novel" only if the combination doesn't exist on any
shipped competitor. Subsets exist; the union does not. Every claim
below links to the source file.

### 1. Isolated-margin with strict bucket independence

`programs/flash-book/src/matcher/risk.rs:assess_margin_split` partitions
a trader's positions into a cross bucket and per-position isolated
buckets. The trader is healthy iff EVERY bucket is independently
healthy. A fat cross pool cannot rescue an under-collateralised
isolated position (test:
`isolated_unhealthy_when_underfunded_even_if_cross_pool_huge`), and an
isolated failure cannot bleed into the cross set
(`cross_set_protected_when_isolated_fails`).

This is stricter than Drift's isolated margin, which permits
cross-collateral spillover in some paths. Hyperliquid's isolated margin
is similar in concept but the bucket math isn't formally documented.
Flash Book's spec lives in `docs/MARGIN_MATH.md` with proptest coverage
(`tests/proptest_isolated.rs`, 6 properties × 2000 cases each).

### 2. Stress-lattice margin

`risk.rs:assess_margin` evaluates every margin check against a finite
scenario lattice (flat + per-market ±2/5/10/20% + all-up/down 10% +
black-swan ±30%). The worst-case loss drives the maintenance
requirement.

CME SPAN does this with hundreds of scenarios; Flash Book uses 13. The
math is in `docs/MARGIN_MATH.md §4`.

No other on-chain perp DEX I can find does this. Hyperliquid uses
linear haircuts. dYdX uses tier-based margin per market. Drift uses
risk buckets.

### 3. Dual-source liquidation price gate

`lib.rs:5222-5244`. For each liquidation health check, picks
`min(mark, oracle)` for long positions, `max(mark, oracle)` for short
positions. A fresh oracle move can tip a position underwater without
waiting for `settle_mark`. Plus oracle-staleness gate at `lib.rs:5202`
refuses to liquidate when the oracle is stale.

GMX uses oracle alone (with Chainlink Data Streams). Most CLOB venues
use mark alone. The dual-source picking is genuinely different.

### 4. Open JIT-liquidation auction

`lib.rs::place_jit_liquidation_offer`. Any maker pre-commits a tighter
close price for a specific or wildcard underwater trader. When
`liquidate_position_v2` fires and a JIT offer beats the synthetic
`oracle ± liq_penalty_bps` price, the close fills at the JIT price.

Hyperliquid's HLP backstop is conceptually similar — a vault always
ready to absorb underwater positions — but it's one entity. Flash
Book's JIT offers are open and competitive. Multiple makers can post
offers simultaneously; the best price wins per fill.

Caveat: opportunistic, not always-on. If no JIT bids exist, Flash Book
falls back to synthetic close at `oracle ± liq_penalty`. HLP, being
always-funded LP capital, is structurally more reliable in tail events.

### 5. Per-bucket Dutch reward routing (Phase 2)

`lib.rs:5495-5547` (verified — I edited this in Phase 2). The
Dutch-auction liquidator reward scales 0% → 100% over
`liquidation_auction_duration_slots`. When the target position is
isolated, the reward debits `position.collateral_quote_lots` (the
per-position bucket). The cross pool is never touched.

This is the practical payoff of the strict bucket independence: a
trader's isolated-position liquidation cannot drain the collateral
backing their other positions.

### 6. Per-position cooldown

`lib.rs:5184-5189`. Same position cannot be liquidated twice within
`liquidation_cooldown_slots`. Anti-cascade primitive — prevents a
liquidator from re-firing on a position that briefly recovered and
dipped again.

Standard in CEX matching engines. Less standard on-chain — I can't find
this in Drift / dYdX / GMX source.

### 7. Funding routes per bucket (Phase 2)

`lib.rs::settle_funding`. Funding owed by or received on an isolated
position now moves between the per-position bucket and the protocol,
not the trader's pooled `trader_state.collateral_quote_lots`. Cross
positions settle to the pool as before. Same isolation principle as
the liquidation reward routing.

### 8. Sub-accounts with distinct positions

Phase 2c migrated Position PDAs from `[POS_SEED, market, wallet]` to
`[POS_SEED, market, trader_state_pda]`. Main and sub-accounts now have
distinct positions per market — the prerequisite for risk isolation.

Phase 2d relaxed `trader_state` seeds on the trade-path Accounts
structs so sub-accounts can drive deposit / withdraw / liquidate / ADL
/ settle-funding / set-margin-mode / basket-place.

Phase 2e added a `sub_index: u8` to `RestingOrderV2` (repurposing the
prior `_pad` byte, layout-compatible), and to `FillEntry` /
`FillBatchEvent`. The sequencer reads sub_index to route ApplyFill
correctly.

Phase 2f threaded sub_index through trigger, TWAP, iceberg, bracket,
and JIT-offer state structs so every secondary order primitive routes
fills correctly. Spec: `docs/SUB_ACCOUNT_TRADING.md`.

Hyperliquid and Drift both have sub-accounts. The differences:

- Flash Book's strict bucket independence at the risk-math level is
  stronger than Drift's.
- Flash Book's sub-account TraderStates have INDEPENDENT positions per
  market (Option B from `SUB_ACCOUNT_TRADING.md §2`), so isolated
  risk strategies are mechanically guaranteed at the PDA level, not
  just at the bookkeeping level.

## Math: where Flash Book is genuinely sound

### Stress lattice (`risk.rs::assess_margin`)

```
equity      = collateral + Σ unrealized_pnl(P, mark) − Σ funding_owed(P, mark)
required    = max_{σ ∈ scenarios}  Σ_P  loss(P, stressed(P, σ)) + mm(P, stressed(P, σ))
is_healthy  = equity ≥ required
```

Saturating arithmetic at every step; clamps to `i128` boundaries before
final cast. Implemented in integer math; no floats. 6 risk proptests +
6 isolated proptests × 2000 random cases each prove the invariants in
`docs/MARGIN_MATH.md §9`.

### Funding (cumulative-index pattern)

`matcher/funding.rs::funding_owed`. Per-block `cum_funding_index`
advances; on settlement, `owed = (cum_funding_now -
cum_funding_at_entry) * notional`. Same pattern as Compound/Aave
interest accrual — exact, no drift. Hourly snapshots
(HL/dYdX/Drift) approximate this.

### Tiered MMR (Hyperliquid-pattern)

`matcher/risk.rs::tiered_mmr_bps`. Per-market tier table maps
`(min_notional, mmr_bps)` ascending. Effective MMR is the largest
tier's `mmr_bps` whose `min_notional ≤ position.notional`. Whales
on big positions get charged higher maintenance margin. Sort order
enforced at write-time in `lib.rs::init_market_leverage_tiers`.

### Multi-oracle quorum (`lib.rs::update_oracle_quorum`)

3 prices in, median out. Rejects if `(max - min) / median >
oracle_quorum_max_dispersion_bps`. Conservative aggregation: takes the
max confidence interval of any input as the accepted confidence. Plus
oracle-staleness gates in `liquidate_position_v2:5202` and
`auto_deleverage`.

## Trading speed — head to head

| Venue | Effective trading latency |
|---|---|
| **Hyperliquid** | ~70 ms median, < 500 ms p99 [1] — in-consensus orderbook |
| **Phoenix** | one Solana slot (~400 ms) — atomic CLOB |
| **Drift v2** | ~5 s for JIT-auction path (deliberate Dutch window) [3] |
| **dYdX v4** | ~1 s block, < 2 s finality [4] — orderbook in mempool, fills hit consensus |
| **Flash Book (Solana base)** | one Solana slot (~400 ms) for limit orders; same for taker walk |
| **Flash Book (MagicBlock ER target)** | depends on ER validator; targeted 10–50 ms but unverified in production on this branch |

Hyperliquid is the only venue running an in-consensus orderbook at
sub-100 ms. Their architectural choice (HyperBFT custom L1) is what
makes that possible — every place / cancel / fill is a L1 transaction
[1].

Flash Book's hot path on Solana base layer is bounded by Solana slot
time. The MagicBlock ER delegation path could be competitive but needs
the ER validator to be running.

## Smoothness — fill UX

For takers, the experience matters more than raw block time:

- **Hyperliquid:** no off-chain step. Order in, fill out, no auction
  wait. Smoothest UX.
- **Drift:** deliberate 5 s JIT auction window — MMs Dutch-bid the
  fill price. Trader gets a better fill on average but waits.
- **dYdX:** orderbook is in mempool; fills happen mid-block; finality
  ~1 s.
- **Flash Book:** `place_taker_order_v2` walks the book and produces
  fills in the same tx (one Solana slot). Comparable to Phoenix /
  Manifest pattern.
- **GMX:** oracle keeper round-trip; minutes-to-seconds depending on
  congestion. Smoothest pool-only experience but no real orderbook.

## Liquidation — head to head

| Property | HL | Drift | dYdX v4 | GMX | **Flash Book** |
|---|---|---|---|---|---|
| Trigger condition | maintenance margin breach [2] | maintenance margin breach [6] | maintenance margin breach [4] | oracle [5] | **stress-lattice margin breach + dual-source price** |
| Executor | HLP vault [2] | open keepers | validators | keepers | **open keepers + JIT makers** |
| Partial close | yes | yes | yes | n/a | **yes** |
| Reward to executor | HLP captures spread [2] | flat fee per size | governance-set fee | flat fee | **Dutch auction 0% → 100%** |
| Per-position cooldown | not documented | no | no | n/a | **yes** |
| ADL trigger | HLP backstop fails [2] | insurance fund deficit | insurance fund deficit | n/a | **insurance fund below pause threshold** |
| Reward routing on isolated positions | not strict | not strict (can touch cross) | n/a | n/a | **strict per-position bucket (Phase 2)** |
| Stale-oracle refusal | not documented | not documented | not documented | n/a | **yes (`lib.rs:5202`)** |

**Where Hyperliquid wins:** HLP is a dedicated liquidator vault with
real LP capital backing every liquidation. Flash Book's JIT-offer
auction is open and competitive, but opportunistic — if no JIT bids
exist, the protocol falls back to synthetic close which can leave
the insurance fund holding more bag than HLP's design.

**Where Flash Book wins:** the dual-source price gate, per-bucket
reward routing, and per-position cooldown collectively give a more
adversarial-test-tight liquidation surface. HL's HLP is a stronger
backstop in tail events; Flash Book's plumbing is more rigorous in
normal operation.

## Where Flash Book is honestly weaker

These are not in earlier marketing docs but are real:

### 1. No HLP-equivalent backstop vault

The FLP pool participates in its own orderbook as a maker (per the
virtual FLP quoter design), but it isn't a dedicated liquidator vault
that always backstops underwater positions. The JIT-liquidation auction
primitive is the closest substitute — any maker can pre-commit a
tighter close price — but it's opportunistic, not always-on.

In a tail event with no JIT bids, Flash Book falls back to synthetic
close at `oracle ± liq_penalty_bps`. The insurance fund covers
shortfall after that. HLP's design is more reliable in tail events
because it's always capitalised.

### 2. Sub-account fill routing trusts the off-chain sequencer

Phase 2d relaxed the `seeds = [...]` constraint on `taker_trader_state`
and `maker_trader_state` in `ApplyFill`. The off-chain sequencer chooses
which TraderState to pass.

The handler enforces `trader_state.trader == order.trader` (Phase 2d),
which catches a malicious sequencer trying to route fills to a
different wallet. It does NOT verify
`trader_state.key() == find_pda([STATE_SEED, order.trader, &[order.sub_index]])`,
which would catch a malicious sequencer trying to mis-route within the
same wallet (e.g., route a sub_index=1 order's fill to the main
TraderState).

For an honest sequencer this is fine. For a hostile sequencer it's a
1-byte routing attack surface. A future commit can close this by
adding the PDA-derivation check to the ApplyFill handler.

### 3. No proven mainnet record

Hyperliquid has billions in OI and years of real-world liquidation
events. Flash Book has 186 unit + proptest assertions and 34
on-chain integration tests on devnet. Math being correct in isolation
is not the same as math being correct under adversarial economic
conditions with real money.

## Design choices the project has deliberately NOT made

These two appeared in early marketing as "coming soon" features.
They aren't. The on-chain matcher is and remains a continuous CLOB
on a hypertree. Calling them weaknesses would imply we plan to fix
them; we don't.

### Continuous CLOB, not FBA / Walrasian clearing

`place_taker_order_v2` walks the opposite-side hypertree
best-price-first, FIFO at each price level — same mechanics as
Hyperliquid, dYdX, Phoenix.

The TypeScript reference simulator in `src/matcher.ts` has Walrasian
uniform-price FBA as a research artifact (and it's well-tested in
the simulator), but it doesn't run on-chain and won't. The
`batch_interval_ms` market parameter is a bookkeeping period for
mark TWAP and funding accrual — not a clearing window. There is no
clearing-price solver in the on-chain code.

Continuous CLOB is the deliberate pick for two reasons:

1. **Same-tx fills.** A taker order matches and settles in the same
   transaction. FBA would defer the fill to a `clear_batch` ix —
   strictly worse latency for the taker UX.
2. **Mechanical compatibility with the existing CLOB venue
   landscape.** Bots and integrators that already speak Phoenix /
   Hyperliquid / dYdX semantics don't need a different matching
   model to read Flash Book.

If you want within-batch MEV resistance you get it via shorter
batches (the FBA approach) or via private mempools (Hyperliquid's
approach) or via commit-reveal (next section). Flash Book opts for
"fast continuous matching + multi-oracle quorum + dual-source price
gate + open JIT-liquidation auction" as the protection surface.

### Continuous CLOB, not commit-reveal

The TS simulator's `commit-reveal.ts` is a research artifact for the
academic permutation-invariance argument. The on-chain Anchor program
has no commit / reveal accounts and no two-phase placement ixs.
`grep -rn 'commit_reveal\|sealed_bid' programs/flash-book/src/`
returns zero hits.

Commit-reveal is the right primitive when within-batch sequencer
front-running is a real threat. On Solana the threat model is
different — there's no protocol-level mempool to sniff, and the
existing per-trader rate limits + STP modes plus the dual-source
liquidation price gate cover the realistic adversarial cases without
adding a two-phase placement protocol.

Commit-reveal could be revisited if Flash Book ever moves to a
custom L1 (or to a chain with public mempools), but on Solana it
isn't planned.

## What this means for "fast and smooth + best liquidations"

The fair scoreboard:

| Dimension | Best | Why |
|---|---|---|
| **Raw speed** | Hyperliquid | ~70 ms in-consensus orderbook is genuinely unmatched |
| **Trading smoothness** | Hyperliquid > Drift > Flash Book | HL has no off-chain step; Drift's JIT is clean UX; Flash Book depends on ER latency in practice |
| **Liquidation math** | **Flash Book** (after Phase 2) | Stress-lattice + dual-source price + isolated bucket strict insulation + JIT auction + per-position cooldown |
| **Liquidation tail-event reliability** | Hyperliquid | HLP is always-on; Flash Book's JIT auction is opportunistic |
| **Funding math** | Flash Book | Cumulative-index per-block beats hourly snapshots IF the settle keeper runs |
| **Margin model rigor** | Flash Book | Stress lattice + isolated bucket independence is more formally defined than competitors |
| **Battle-testedness** | Hyperliquid | No contest |
| **Open-source auditability** | Flash Book / Drift (tie) | Both fully open; both at similar audit cost |

**Where Flash Book is most defensible** is in liquidation correctness —
the Phase 2 work (split risk, per-bucket reward routing,
`settle_funding` routing, sub-account isolation) collectively makes it
one of the most rigorously-bucketed liquidation engines on any
DEX, with formal math in `MARGIN_MATH.md` and proptest coverage. That's
the strongest defensible claim.

**Where Flash Book loses today** is raw speed (Solana slot floor), real
mainnet exposure (devnet only), and tail-event backstop strength (no
HLP equivalent).

## Sources

[1] [Hyperliquid Architecture Deep Dive — CleanSky](https://cleansky.io/blog/hyperliquid-architecture-hypercore-hyperevm-2026/)
[2] [Hyperliquid Liquidations docs](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations)
    [Hyperliquid Margining docs](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margining)
[3] [Inside Drift: Architecting a High-Performance Orderbook on Solana — Yong kang Chia](https://extremelysunnyyk.medium.com/inside-drift-architecting-a-high-performance-orderbook-on-solana-612a98b8ac17)
[4] [dYdX v4 Architecture Overview — dydx.xyz blog](https://www.dydx.xyz/blog/v4-technical-architecture-overview)
[5] [GMX V2 Trading docs](https://docs.gmx.io/docs/trading/v2/)
[6] [Drift Protocol — Liquidations docs](https://docs.drift.trade/protocol/trading/liquidations)
    [Drift Protocol — Liquidation Engine docs](https://docs.drift.trade/protocol/trading/liquidations/liquidation-engine)

## Versioning

This document reflects the on-chain protocol at commit `31c4b3a`
("Phase 2f"). The Phase 2 series (`550624e`, `4dc8ad9`, `bd41703`,
`6fb1e34`, `8981652`, `31c4b3a`) is what makes the isolated-margin /
sub-account claims accurate. Earlier marketing material that mentioned
FBA / commit-reveal as on-chain features predated this honesty pass.
