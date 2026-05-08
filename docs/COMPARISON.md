# Flash Book vs every modern perp DEX

How Flash Book compares to the state of the art (2026 Q2 snapshot).

## The matrix

|  | Hyperliquid | Drift v2 | dYdX v4 | Aevo | Lighter | GMX v2 | Phoenix | Flash V2 (today) | **Flash Book** |
|---|---|---|---|---|---|---|---|---|---|
| Chain / venue | own L1 | Solana | own Cosmos | OP | zk-rollup | Arbitrum | Solana | Solana | **Solana + ER** |
| Latency | ~50 ms | ~400 ms | ~1 s | ~2 s | ~100 ms | block | block | block | **50 ms** |
| Matching primitive | continuous CLOB | DLOB + JIT + vAMM | continuous CLOB | RFQ batch | continuous CLOB | pool only | atomic CLOB | pool only | **FBA Walrasian** |
| MEV resistance (within-engine) | partial | no | partial | no | no | n/a (oracle priced) | atomic | n/a (oracle priced) | **FBA + commit-reveal** |
| Pool-backed | no | partial (vAMM) | no | no | no | yes (GLP) | no | yes (FLP) | **yes (virtual FLP)** |
| Funding cadence | 1 h | 1 h | 1 h | 1 h | 1 h | n/a (borrow fee) | n/a | hourly-ish | **per-block (~10 ms)** |
| Liquidation model | keeper bots | keeper bots | keeper bots | keeper bots | keeper bots | force close at oracle | n/a | force close at oracle | **in-loop, in-batch** |
| Cross-margin | yes | yes | yes | yes | yes | no | n/a | partial | **stress-lattice** |
| Insurance fund waterfall | yes (with ADL) | yes (with ADL) | yes (with ADL) | yes (with ADL) | yes (LLP+ADL) | counterparty pool | n/a | LP loss | **fund + ADL** |
| Decentralized | mostly | yes | yes | partial | yes | yes | yes | yes | **yes** |
| Settles to Solana mainnet | no | yes | no | no | no | no | yes | yes | **yes** |
| Builder-deployed markets | HIP-3 yes | no | no | no | no | no | no | no | **roadmap** |

## What's novel about Flash Book

A design is novel if and only if no other shipped venue has the *combination*.
Subsets of Flash Book exist; the union does not.

### 1. Pool-backed CLOB (Virtual FLP Quoter)

Closest cousins:
- **Drift's vAMM** is virtual — there's no real LP capital behind it; it's a
  curve seeded with notional reserves rebalanced by funding. Real losses fall
  on Drift's insurance fund and ultimately the protocol.
- **GLP / FLP / JLP** are real LP pools, but counterparty-only — they don't
  participate in any orderbook.
- **Phoenix** is spot CLOB without any pool.

Flash Book's Virtual FLP Quoter makes the existing Flash V2 FLP pool a
participant in its own book. Real LP capital backs the quotes. Human MMs
compete inside the pool's spread. **No protocol has shipped this.**

### 2. FBA Walrasian clearing at HFT cadence

Closest cousins:
- **CowSwap** is FBA but slow (per-block on Ethereum).
- **Penumbra** is batch swap with shielded orders — closer in spirit but
  Cosmos-native and not perp.
- **Phoenix** is atomic continuous matching, not FBA.
- **IEX** has a 350μs speed bump but is continuous after that.

Flash Book runs Walrasian clearing every 50 ms. The 50× cadence-vs-CowSwap
is what makes it competitive with continuous CLOBs while preserving FBA's
mathematical MEV neutrality.

### 3. Sub-100 ms commit-reveal

No deployed perp DEX uses commit-reveal at all. The closest analogue is
Flashbots-style sealed-bid auctions, which run at L1 block cadence (12 s on
Ethereum). 50 ms commit + 50 ms reveal = 100 ms total, perceptible but
acceptable for perp trading.

### 4. In-loop liquidations

Every other venue uses external keeper bots. Drift's "JIT auction" gets
closest — MMs Dutch-auction over user orders — but liquidations themselves
are still keeper-driven. Flash Book's matcher injects liquidation orders
into the same batch that detected them; clearing is uniform across all
liquidations + regular flow.

### 5. Continuous per-block funding

Hyperliquid, dYdX, Drift, Aevo, GMX all use ≥ 1-hour funding cadence.
Per-block funding is technically possible on any chain, but economically
unjustifiable when each tx costs gas. ER's free compute makes per-block
funding feasible. Continuous integral form (cumulative index) is borrowed
from Compound/Aave's rate-accrual pattern.

### 6. Stress-lattice cross-margin

Hyperliquid's portfolio margin uses linear haircuts with cross-asset
recognition. dYdX uses tier-based margin per market. Lighter has
multi-tier (Initial, Maintenance, Close-Out). World Markets' ATLAS engine
(Feb 2026) does portfolio-level netting but isn't yet documented enough
to compare in detail.

Flash Book's stress lattice evaluates the portfolio under a finite set of
correlated stress scenarios. Hedged positions naturally collapse to zero
directional risk in every scenario, leaving only maintenance margin on
stressed notional. This is the right model — borrowed from CME SPAN, but
simpler (45 scenarios vs SPAN's hundreds of risk arrays).

## Where Flash Book is NOT first

Honest about what's not novel:

- **VPIN** has been used in HFT firms since 2012 (Easley et al.). Applying
  it on-chain inside a matcher is novel; the metric itself is not.
- **Avellaneda-Stoikov inventory model** is from 2008. Using it as the
  basis for the FLP quoter applies known theory to a new context.
- **Walrasian clearing** is from 1874 (Walras). FBA proposals at HFT
  cadence go back to Budish 2015.
- **Cumulative-index interest accrual** is the Compound/Aave pattern.
- **Insurance fund + ADL waterfall** is standard CEX practice (BitMEX 2018,
  Bybit, FTX, etc.) ported to DEX context (Hyperliquid, dYdX).

The novelty is the **synthesis** — combining all of these into one
matcher, on one rollup, settling to Solana, with a real LP pool as
backstop maker.

## Why this matters for Flash

Flash V2 today is a pool-only oracle-priced perp protocol. As volume
scales, two structural problems compound:

1. **Toxic flow eats the pool.** Informed traders pick off the pool when
   oracle lags reality. The pool's defenses (spread, OI fees, funding) are
   blunt. LP yield decays.
2. **No real price discovery.** Without an orderbook, Flash has no native
   price discovery mechanism — it's a price-taker of Pyth. Pyth bugs or
   manipulation propagate directly.

Flash Book fixes both:

1. **MMs absorb informed flow first.** The pool only takes flow MMs decline,
   which is structurally less profitable for MMs. By selection, the
   remaining pool flow is more profitable for the LPs than today's
   undifferentiated flow.
2. **Real price discovery.** Mark price is the TWAP of actual cleared
   trades (banded by oracle). Manipulating it requires actually clearing
   volume — the manipulator pays for every basis point moved.

This is the Pareto improvement claim: retail UX is identical-or-better than
today, LP yield is structurally higher, and the protocol gains real price
discovery. **Every column where Flash Book has advantage over Flash V2 is
a structural improvement, not a side feature.**
