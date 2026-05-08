# Architecture

## Mapping to Flash V2

Flash Book is **not a replacement** for Flash V2. It is a matcher layer that
sits on top of Flash V2's existing FLP pool program and settles back into
Flash V2 position accounts. Every architectural abstraction in this repo
maps directly to a Flash V2 primitive:

| Flash Book concept | Flash V2 primitive | Notes |
|---|---|---|
| `MarketState` | `Custody` + market PDA in `PoolConfig` | One per pool market (`SOL`, `BTC`, `ETH`, ...). |
| `FlpState` | FLP pool token vault + custody accounts | Read on every batch; never written directly by the matcher. |
| `Position` | Flash V2 `Position` PDA | `side`, `size`, `collateral`, `entryPrice` align 1:1. |
| Trader collateral | Flash V2 collateral custody | USD_DECIMALS = 6 throughout. |
| FLP virtual quoter | Synthesizes orders that *would* be filled by the FLP pool | Pool program is unmodified; matcher reads pool state and quotes against it. |
| Insurance fund | New PDA owned by Flash Book program | Funded from fees, toxicity tax, liq penalty. |
| Engine batch tick | One ER block | 50 ms cadence, 5 ER blocks per batch. |
| Settlement to L1 | Flash V2 `Position` account writes | Every K batches (default 600 ≈ 6 s). |

The integration uses Flash V2's existing `@flash_trade/magic-trade-client`
session-signer pattern (`createSession` / `useSession` / `revokeSession`).
Traders sign once on session start; subsequent batches require no biometric
re-prompt.

## System diagram

```
        ┌──────────── Solana mainnet ────────────┐
        │                                        │
        │   Flash V2 program  ◄──── settlement ──┤
        │   ├── FLP pool                         │
        │   ├── Position PDAs                    │
        │   └── Collateral custody               │
        │                                        │
        │   Pyth oracle  ────────────────────────┤
        │                                        │
        │   Flash Book program (new)             │
        │   ├── Market account                   │
        │   ├── Insurance fund PDA               │
        │   └── Trader book state                │
        │                                        │
        │            ▲                           │
        └────────────┼───────────────────────────┘
                     │ delegate
                     ▼
        ┌──────── MagicBlock ER ─────────────────┐
        │                                        │
        │   Flash Book matcher (per market)      │
        │   ├── Order buffer                     │
        │   ├── Commit-reveal registry           │
        │   ├── FBA Walrasian clear (50 ms)      │
        │   ├── Virtual FLP quoter               │
        │   ├── In-loop liquidation injector     │
        │   ├── Funding index advance            │
        │   ├── VPIN calculator                  │
        │   └── Insurance / ADL waterfall        │
        │                                        │
        └────────────────────────────────────────┘
```

## Lifecycle

### 1. Session start

1. Trader's wallet signs a `createSession` instruction on L1, registering an
   ephemeral session keypair owned by their Flash V2 trader account.
2. The Flash Book program's market account, the trader's position accounts,
   and the FLP pool's custody accounts are **delegated** to the ER.
3. ER becomes authoritative for these accounts until session ends.

### 2. Per-batch tick (every 50 ms)

```
runBatch(nowMs):
  1. advanceFundingIndex on every market
  2. recomputeOpenInterest from authoritative position state
  3. detectLiquidations from prior-batch mark using stress-lattice
  4. for each market:
       a. generate FLP virtual quotes
       b. inject liquidation orders for unhealthy traders
       c. clearBatch(buffer + flp + liq) → uniform clearing price
       d. apply fills (positions, fees, OI, VPIN)
       e. update mark = oracle-banded TWAP of clearing prices
       f. process bankruptcies via insurance / ADL waterfall
  5. sweep expired commits
  6. verify invariants
```

### 3. Settlement (every K batches)

1. State diff (positions, collateral, FLP exposure, insurance fund balance,
   funding index) is committed to L1 via Flash V2 instruction calls.
2. Mainnet position accounts reflect post-batch state.
3. Pyth oracle is re-read from mainnet for the next batch's reference price.

### 4. Session end

1. `revokeSession` instruction on L1 ends delegation.
2. Final state is committed.
3. Account control returns to Flash V2 mainnet program.

## Components in detail

### Matcher (`src/matcher.ts`)

Walrasian uniform-price clearing. For each batch, build the joint demand
and supply curves from all candidate orders (limits, takers, FLP virtual,
liquidations). Find `p* = arg max_p min(D(p), S(p))`. Tie-break by
proximity to prior mark; midpoint of indifference interval if it spans the
mark. Match eligible orders by priority (`liquidation > adl > taker >
flp_virtual > limit`) then FIFO timestamp. Self-trade is filtered.

### FLP virtual quoter (`src/flp-quoter.ts`)

Avellaneda-Stoikov-grade inventory-aware quoter:

- Inventory skew: `skew = -(λ + γ_risk · σ²) · (pool_net / pool_capital)`
- Spread per level:
  `s = s0 + α·VPIN + β·u + γ·|oi_imb| + κ·(Q/depth_floor) + δ·σ`
- Per-batch growth cap: `pool_capital · max_growth_pct`, split across
  N price levels.
- Multi-level depth ladder, each level priced at fair-value ± s(level).

The quoter is **stateless** — given pool state it produces a deterministic
ladder. This is critical: every node running the matcher produces the
same FLP quotes given the same pool state, so consensus is automatic.

### Risk engine (`src/risk.ts`)

Stress-lattice maintenance margin. For each scenario `s ∈ S` (single-asset
shocks ±2/5/10/20%, correlated all-down/all-up at ±10%, black swans at
±30%), compute portfolio loss + maintenance margin on the stressed
notional. Required margin is the worst-case scenario loss. Hedged
positions (long+short same market) cancel directional risk in every
scenario, so required margin collapses to maintenance margin on the
stressed notional only — the design's hedge-aware property.

### Liquidation engine (`src/liquidation.ts`)

In-loop liquidations. Detection runs once per batch on positions priced
against the prior-batch mark; this avoids the race where a position
becomes unhealthy mid-batch. Detected positions get a synthetic
liquidation order injected into the current batch's matcher input, with a
limit price of `oracle ± liq_penalty`. The matcher clears the
liquidation at the batch uniform price (which is at least as good as the
limit).

After the batch clears, each filled liquidation is examined for bankruptcy:
if collateral can't cover the realized loss + penalty, the shortfall flows
through the waterfall:

1. Insurance fund (paid up to fund balance)
2. ADL — most-profitable counter-positions are auto-deleveraged at
   batch mark, ranked by `profit_ratio · leverage` (highest first)

### Funding (`src/funding.ts`)

Continuous funding via cumulative index. Each block, the engine computes:

```
premium = (mark - oracle) / oracle
rate    = clamp(K · premium, ±r_max)
ΔI      = rate · Δt
cum_funding_index += ΔI
```

A position records `cum_funding_index_at_entry`. On every position change,
the position is charged `sign · notional · (I_now - I_at_entry)`. Index
marker is reset on each settlement. This is the same pattern Compound /
Aave use for interest accrual; we apply it to funding at sub-second
resolution because ER tx is free.

### VPIN (`src/vpin.ts`)

Volume-Synchronized Probability of Informed Trading. Volume buckets close
when cumulative volume reaches `bucket_size`. Each bucket records
`|V_buy − V_sell| / bucket_size`; VPIN is an EMA of these bucket
imbalances over the last `ema_window` buckets. Drives the α coefficient
of the FLP spread function (toxic flow widens FLP spread → LPs protected).

### Commit-reveal (`src/commit-reveal.ts`)

Two-phase taker submission:

1. Block N: `submitCommit(hash)` where
   `hash = H(market ‖ trader ‖ side ‖ size ‖ limit ‖ nonce)`
2. Block ≤ N + K: `submitReveal(payload)`; matcher checks the hash matches
   and queues a synthesized taker order for the next batch.
3. K elapses without reveal → bond seized.

Sequencer cannot front-run because the hash hides every value. Cost: ~50–100
ms perceived latency for takers (vs immediate matching). For perps holding
positions for hours, this is irrelevant.

### Insurance fund (`src/insurance.ts`)

Three contribution streams, one waterfall payout. Contributions: 10% of
trading fees + 50% of toxicity tax + 50% of liquidation penalty.
Pause-new-positions threshold halts new positions when fund balance falls
below a configured floor (default $5K); existing positions can continue to
trade (close or reduce only).

## Why each choice

See [`docs/MATH.md`](MATH.md) for the formal math and
[`docs/SAFETY.md`](SAFETY.md) for the threat model and invariants.
