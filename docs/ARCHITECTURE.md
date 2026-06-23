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

## Recent additions (waves 6-13)

The base architecture is unchanged; the additions slot into existing
seams. Every new feature is **additive** — no instruction's signature
breaks, no account layout migrates (existing accounts default the new
fields to zero/equivalent). The matcher's hot path doesn't grow.

### Native order types beyond limit/taker
- **Trigger orders** (`TriggerOrderAccount`) — permissionless `execute_*`
  reads oracle, inserts the resulting order into the regular buffer.
  Optional OCO link, reduce-only, GTT expiry, trailing offset.
- **TWAP orders** (`TwapOrderAccount`) — permissionless `execute_twap_slice`
  inserts one slice per interval.
- **Bracket orders** (`place_bracket_order`) — atomic parent + 2 OCO
  triggers (TP + SL) in one tx. Parent fill → triggers become
  reduce-only-eligible; one fires → other auto-deactivates.
- **Iceberg orders** (`IcebergOrderAccount`) — hidden reservoir;
  permissionless `replenish_iceberg` inserts the next chunk when the
  visible child fills.
- **Trailing stops** — `trailing_offset_bps` on TriggerOrderAccount;
  permissionless `update_trailing_stop` ratchets in the favorable
  direction with conservative tick rounding.

### Risk + safety
- **Per-position leverage cap** (`set_position_leverage`) — enforced at
  intake against projected post-fill notional.
- **Concentration margin tier** (FLP-keyed; smarter than HL's flat MMR)
  — `MarketSnapshot::effective_mmr_bps(size_lots)` used in stress
  lattice.
- **Symmetric-OI funding dampener** (smarter than HL's premium-only
  funding) — when `funding_oi_dampening`, rate × |skew| / total scales
  funding. Balanced book → 0 funding.
- **Funding-premium TWAP** — last-N-batch clearing-price TWAP as the
  premium input; kills 1-batch microbursts at our 50ms cadence.
- **Mark sanity cap** — per-batch ±X bps clamp on post-clearing mark.
- **Per-market OI cap** — whole-market hard ceiling at intake.
- **STP modes** — CancelNewest / CancelOldest / CancelBoth via flag bits
  4-5 on the OrderSlot; matcher applies the newer-order's mode.
- **GTT order expiry** — `expires_at_slot` on every order; matcher
  silently skips expired slots; cleanup-keeper reclaims rent.

### Permissionless markets (HIP-3 + bond)
- **`permissionless_initialize_market`** — anyone deploys a market;
  envelope-clamped params; caller is creator + earns
  `creator_share_bps`.
- **HIP-3 deployer bond** (`MarketBondAccount`) — slashable stake with
  7-day unbond delay. `slash_market_bond` is authority-gated.

### Capital primitives
- **Multi-LP NAV vault** (`LpPositionAccount`) — already present; share
  math now also covers per-deposit + per-withdraw bookkeeping.
- **User-managed trading vaults** (`VaultAccount` +
  `VaultPositionAccount`) — strategist trades via the existing
  delegate path; deposit + withdraw use mark-to-market NAV via market
  walk in `remaining_accounts`. HWM perf-fee in shares.
- **Cross-margin sweep** (`sweep_collateral`) — position-aware via
  joint stress-lattice gate; same MTM walk pattern as vaults.

### Liquidation + ADL
- **Auto-Deleverage** (`auto_deleverage`) — when insurance is below
  pause_threshold, force-close highest-ranked profitable counter at
  the bankruptcy price. Permissionless; eligibility re-checked on chain.
- **Multi-threshold margin alerts** — per-fill emit at 250%/200%/125%
  of MMR for off-chain pre-liq pushes.
- **Mass cancel** (`cancel_all_orders_in_market`) — single-tx flatten.

### Fee + reward primitives
- **Builder codes** (`set_trader_builder`) — frontend earns up to user-
  approved cap.
- **Referral program** (`set_trader_referrer`) — one-time-write,
  anti-rotation.
- **Negative-fee top tier** — `discount_bps` up to 12_000 (120%) →
  taker is paid for routing flow; sourced from insurance contribution.
- **Trading-rewards eligibility** — per-fill emit for off-chain HYPE-
  style accrual.

### View ixs (UI primitives via tx simulation)
- `view_predicted_funding` — emits `PredictedFundingEvent` with
  rate + premium + cum_index; SDK simulates the tx.
- `view_quote_ladder` — re-runs `generate_quotes` with current state;
  emits `QuoteLadderSnapshotEvent` (top-level summary; full ladder is
  deterministically recoverable off-chain).

### MagicBlock ER compatibility
Every new ix and view operates through Anchor's standard PDA
derivation + Borsh accessors that work transparently when the market
account is delegated to an ER. The in-house `cpi_delegate` /
`cpi_undelegate` ixs (`programs/flash-book/src/er.rs`) bypass the
upstream SDK's Solana version conflict by re-implementing the
delegation discriminators directly. New state (vault, iceberg,
trigger, twap, bond) participates in the same delegation lifecycle —
no special-casing needed.
