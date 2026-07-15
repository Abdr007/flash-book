# Architecture

Clober is an on-chain central limit order book (CLOB) perpetual-futures
engine for Solana. Matching runs at rollup speed on a MagicBlock Ephemeral
Rollup (ER); custody, risk, and settlement live on the base layer (L1). The
program surface is 162 instructions, 146 events, and 120 typed errors
(`idl/clober.json` is the source of truth).

```
        ┌──────────────── Solana L1 ─────────────────────┐
        │                                                │
        │  MarketAccount        · params, mark, OI,      │
        │                         oracle, status         │
        │  TraderStateAccount   · collateral, fee tier,  │
        │                         sub-accounts           │
        │  PositionAccount      · side, size, entry,     │
        │                         funding snapshot       │
        │  LiquidityPoolAccount   · pool capital + NAV     │
        │  InsuranceFundAccount · waterfall backstop     │
        │  Vaults native            · strategist vaults      │
        │  Oracle configs       · Pyth / Lazer bindings  │
        │  Governance PDAs      · guardian, pending      │
        │                         transfer/params,       │
        │                         committee              │
        │                                                │
        │  apply_fill / apply_lp_fill  ◄── settlement   │
        │  (verifies every fill against the ring)        │
        └───────────────┬────────────────────────────────┘
                        │ delegate market_book + fill ring + outbox
                        ▼
        ┌────────── MagicBlock ER (per market) ──────────┐
        │                                                │
        │  MarketBook (hypertree slab; bids + asks)      │
        │  place/cancel/modify · continuous price-time   │
        │  place_taker_order · walks the book,        │
        │    pushes keccak fill commitments to the ring  │
        │    and full fill records to the outbox         │
        │  LP auto-quoter ladder                        │
        │                                                │
        └────────────────────────────────────────────────┘
```

## The L1/ER split

Exactly three accounts per market are delegated to the ER: the
**market book** (the order book slab), the **fill-commitment ring**, and the
**fill outbox**. Matching on the ER may write only those. Positions,
TraderStates, the vault, and every other account remain L1-owned — on the
rollup they are read-only clones. This is a hard wall: no ER instruction
emits a cross-domain write, and no L1 money movement happens without an L1
transaction.

The settlement loop:

1. **Match (ER).** `place_taker_order` walks the opposite side of the
   book in price-time order. Each fill is appended to the fill-commitment
   ring as a keccak commitment and to the fill outbox as a full record.
2. **Commit (ER → L1).** The book, ring, and outbox are committed back to
   L1 (`commit_*`), either periodically or with undelegation.
3. **Settle (L1).** The sequencer calls `apply_fill` (or `apply_lp_fill`
   for pool fills) per fill. On an armed market the ring is mandatory:
   settlement recomputes the commitment and pops it in FIFO order, so a
   fabricated, altered, repriced, reordered, or replayed fill is rejected.
   Position state, collateral, fees, funding, and OI move here — and only
   here.

Settlement can never reject or resize a *committed* fill (that would wedge
the FIFO ring or break two-sided conservation), so every economic
precondition — margin, reduce-only capacity, OI caps, price bands — is
enforced at intake or match time, before a fill is committed.

## Matching engine

The order book is a slab of fixed-size nodes indexed by a red-black tree
(the vendored `hypertree` library — GPL-3.0, see `LICENSE-HYPERTREE`),
giving O(log n) insert/cancel and O(1) best-price access with zero heap
allocation. Orders carry price, size, sequence number, expiry, flags
(post-only, IOC, FOK, reduce-only, self-trade-prevention mode), and the
owner inline. Price-time priority is machine-proven on the order-id
encoding (see `docs/FORMAL_VERIFICATION.md`).

Order types beyond limit/taker are built as L1 PDAs whose permissionless
`execute_*` instructions inject regular orders into the book when their
condition fires: trigger orders (stop / take-profit, with slippage caps and
OCO links), TWAP orders (sliced execution), icebergs (hidden reservoir +
visible chunk replenishment), brackets (parent + two OCO-linked trigger
legs), and basket orders (multi-leg with a cross-market margin gate).

A reduce-only order can never open or flip a position: intake clamps its
size against the position's remaining reducible capacity (cumulative across
all resting reduce-only orders), and on markets with the fill-commitment
ring the matcher additionally tracks reduce-in-flight per position inside
the ring itself, so the cap holds across the match→settle gap.

## LP: the pool as an on-book maker

The LP pool quotes both sides of the book through a deterministic,
inventory-aware ladder (`lp_refresh_quotes`, permissionless with an
anti-churn rate limit). Spread widens with realized volatility, pool
utilization, and inventory skew; a hard inventory cap bounds pool exposure.
Pool fills settle through `apply_lp_fill` under the same ring authenticity
plus an oracle price band (`LP_MAX_FILL_DEVIATION_BPS`) that caps how far
any settled pool fill may sit from a fresh oracle. LP capital enters and
exits through NAV-based shares (deposits/withdrawals price against pool
NAV including realized PnL), with a minimum hold time defeating
just-in-time windfall capture.

## Risk stack

- **Margin.** A stress-lattice portfolio margin: required margin is the
  worst-case loss across per-market shocks, correlated moves, and
  black-swan scenarios, plus maintenance margin on stressed notional.
  Hedged books collapse to maintenance-only. Positions may be
  cross-margined (pooled collateral) or isolated (per-position bucket).
  Initial margin is enforced at order intake; withdrawals re-run the gate.
- **Liquidation.** `liquidate_position` prices health on the *worse of*
  mark and oracle (falling back to oracle-only when the mark is stale — an
  ER stall cannot freeze an adverse mark into liquidations). Rewards are
  bounded by residual equity; self-liquidation is forbidden; nothing
  liquidates while the market is paused. JIT liquidation offers let makers
  bid to absorb liquidations at better-than-synthetic prices.
- **Insurance fund.** Funded from fee/penalty streams; covers bankruptcy
  shortfalls; below its pause threshold the market stops accepting new
  positions and ADL becomes eligible.
- **ADL.** `auto_deleverage` force-closes the most profitable
  counter-positions at the bankruptcy price, only against a truly bankrupt
  position, conserving value: the counter-party's credited gain is capped
  at what the bankrupt side actually forfeits.
- **Haircut.** Profit is junior to capital: released positive PnL matures
  through a time-gated reserve and converts at
  `h = min(residual, matured) / matured`, so aggregate extractable profit
  can never exceed the real residual backing it. Losses settle immediately
  against capital. (Formal spec: `docs/HAIRCUT_MATH.md`.)
- **Funding.** Positions carry a cumulative-index snapshot and settle
  funding on touch; per-side accrual indices let funding/mark/ADL effects
  apply lazily in O(1) per position. The index driver is currently inert
  (no instruction advances the funding index), which the code documents
  explicitly.

## Oracles and the mark

Each market binds an oracle source: authority-pushed (with a quorum
variant), Pyth pull (`PriceUpdateV2` under full verification), or Pyth
Lazer (Ed25519 precompile + strictly-increasing replay nonce). All paths
share a per-slot envelope gate that bounds price movement per slot, and
staleness gates reject future-dated or stale prints. The mark (a fill EMA
the sequencer produces) is always clamped to an effective oracle band —
between 1 bp and 500 bps, defaulting to 200 — so a manipulated mark cannot
stray from the trustless oracle. `lock_oracle_source` permanently disables
the direct-authority paths on a market, leaving only Pyth/Lazer.

## ER lifecycle and liveness

Delegation CPIs (`src/er.rs`) stage the account into a buffer, hand
ownership to the MagicBlock delegation program, and restore it byte-exact
at undelegation — where the callback binds the DLP's signed buffer to the
canonical `["undelegate-buffer", delegated]` PDA and re-derives the target
from its seeds, so a forged buffer cannot materialize state. Liveness is
two-tier: a fast permissionless force-undelegate opens when the ER shows no
signal (no fill, no heartbeat) past a stall timeout, and a censorship
backstop opens on settlement silence alone past a much longer timeout —
a heartbeating-but-censoring sequencer cannot trap funds, and a
healthy-but-quiet market cannot be griefed off the ER (Kani-proven gate).

## Privacy (dark pool)

A market's delegated book can run on a MagicBlock *Private* ER (TEE-backed).
`init_book_permission` / `set_book_privacy` / `close_book_permission` manage
the ephemeral permission account that gates ER reads: allow-listed members
see the book; public observers are denied depth, orders, and flow.
Settlement still lands on L1 and `apply_fill` verifies every fill against
the ring, so privacy is purely additive — no matching, risk, or settlement
path depends on it. Wire format and validation boundary: `docs/PRIVACY.md`.

## Accounts and sessions

TraderStates support sub-accounts (index 0 = main), collateral transfers
between them, delegates, referrers, builder codes, and volume-based fee
tiers. Session keys (`create_session_token`) authorize scoped, expiring
trading sessions — optionally market-scoped — for the session variants of
place/cancel/deposit. Cross-domain (`_xdomain`) withdrawal paths respect
margin reserved by live ER orders, attested via the ER margin-attestation
flow.

## Governance

All admin control is per-market `market.authority` plus the
`insurance_fund.authority`, hardened by: a restrict-only emergency guardian
(can pause, never unpause; can veto pending param updates), two-step
authority transfer (the new key must sign to accept), a 48-hour timelocked
params path bound to a keccak params-hash, a one-way oracle-source lock,
sequencer rotation, and irreversible authority burn. Details:
`docs/GOVERNANCE.md`. The sequencer-committee primitive (quorum-attested
batch roots + equivocation slashing) exists on-chain as an additive step
toward decentralized sequencing: `docs/DECENTRALIZED_SEQUENCER.md`.

## Trust model

Fill *authenticity* is enforced on L1 by the commitment ring; fill
*ordering and liveness* rest on a single sequencer per market, bounded by
the force-undelegate escapes and the oracle-pinned mark. This boundary is
stated precisely in `ER_TRUST_BOUNDARY.md` and `SECURITY.md`.

## Source layout

```
programs/clober/src/
├── lib.rs            handlers, account contexts, events (the on-chain shell)
├── state.rs          market, trader, position, insurance accounts
├── book_state.rs       order-book slab: MarketBookHandle, resting orders
├── extended_state.rs trigger/TWAP/iceberg, oracle configs,
│                     committee, haircut + side-accrual + envelope state
├── er.rs             MagicBlock delegation/commit/undelegate CPIs + liveness
├── er_permission.rs  TEE private-ER read-permission CPIs
├── pyth_oracle.rs    Pyth PriceUpdateV2 reader
├── lazer_oracle.rs   Pyth Lazer payload parser + Ed25519 introspection
├── session.rs        session-token verification
├── xmargin.rs        cross-domain (ER-aware) margin floors
├── hypertree/        vendored red-black-tree slab (GPL — LICENSE-HYPERTREE)
└── matcher/          pure engine math (no Solana account types):
    ├── order, lot            order/side/price-lot primitives
    ├── envelope              per-slot price/funding move proofs
    ├── fill_commitment       keccak settlement ring (+ reduce-in-flight)
    ├── fill_outbox           full fill records for off-log settlement reads
    ├── lp_quoter            deterministic pool quoting ladder
    ├── risk                  stress-lattice margin + fee tiers
    ├── liquidation           worse-of health pricing, shortfall math
    ├── insurance             fund model + solvency detectors
    ├── haircut               junior-profit gating (reserve/mature/convert)
    ├── side_accrual          A/K/F/B per-side lazy indices
    ├── position_math         open/VWAP/reduce/flip + realized-PnL core
    ├── funding               settlement-side funding charge (index inert)
    ├── reduce_only           reduce-only capacity clamp
    ├── jit_lp_defense        LP minimum-hold gate
    ├── committee             BFT quorum membership + equivocation predicates
    └── vpin                  layout-reserved accumulator (retired)
```

Formal verification (62 Kani harnesses, 7 Lean proof modules, property suites):
`docs/FORMAL_VERIFICATION.md`. Math specs: `docs/MATH.md`,
`docs/MARGIN_MATH.md`, `docs/HAIRCUT_MATH.md`. Threat model:
`INVARIANTS.md`.
