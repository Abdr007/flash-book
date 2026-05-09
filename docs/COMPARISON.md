# Flash Book vs every modern orderbook DEX

How Flash Book compares to the state of the art (2026 Q2 snapshot).
Updated to reflect everything shipped through commit `7beda3a`
(waves 1–5: native trigger/TWAP orders, builder codes, negative-fee
tier, HIP-3 permissionless markets, multi-threshold margin alerts,
trading-rewards eligibility events).

## TL;DR

- **Matcher**: only venue running Frequent Batch Auction (FBA) with Walrasian
  uniform-price clearing at HFT cadence (50 ms).
- **MEV resistance**: only venue with both **commit-reveal** AND **batched
  matching** — sandwich attacks are mathematically impossible within a batch.
- **Pool-backed CLOB**: only venue where the LP pool participates in its OWN
  orderbook as a permanent maker-of-last-resort.
- **Liquidations**: only venue with **in-loop** liquidations (resolve in the
  same batch they're triggered in) PLUS partial-close + Dutch-reward auction
  + per-position cooldown.
- **LP model**: ERC-4626-style NAV vault with multi-LP shares, position-aware
  withdraw guard. Most other venues are single-pool (GLP, FLP, JLP) or
  no-pool (Drift, Phoenix, dYdX).
- **Order types**: Phoenix-grade (post_only, reduce_only, IOC, JIT) on
  the on-chain matcher; OCO, Iceberg, Trailing stop in the off-chain bot.
- **Bot suite**: only project that ships a venue-agnostic MM bot + 4-keeper
  suite + backtester + telemetry + smart router across V2 + V3 in the
  same repo.

## The matrix — perp orderbook DEXes

| | Hyperliquid | Drift v2 | dYdX v4 | Aevo | Phoenix | OpenBook | Vertex | Injective | **Flash Book V3** |
|---|---|---|---|---|---|---|---|---|---|
| Chain | own L1 | Solana | Cosmos chain | OP L2 | Solana | Solana | Arbitrum | own Cosmos | **Solana + ER** |
| Latency | ~50 ms | ~400 ms | ~1 s | ~2 s | block | block | ~50 ms | ~600 ms | **~50 ms** |
| Matching primitive | continuous CLOB | DLOB + JIT + vAMM | continuous CLOB | RFQ batch | atomic CLOB | continuous CLOB | CLOB + AMM | continuous CLOB | **FBA Walrasian** |
| Within-engine MEV resistance | partial | no | partial | no | atomic only | no | no | no | **FBA + commit-reveal** |
| Pool-backed CLOB | no | partial (vAMM) | no | no | no | no | hybrid | no | **yes (virtual FLP)** |
| Funding cadence | 1 h | 1 h | 1 h | 1 h | n/a | n/a | 1 h | 1 h | **per-block (~10 ms)** |
| On-chain funding settlement | no (off-chain) | no (lazy) | no (lazy) | no (lazy) | n/a | n/a | no | no | **`settle_funding` ix** |
| Liquidation model | keeper bots | JIT + keepers | keeper bots | keeper bots | n/a | n/a | keeper bots | keeper bots | **in-loop + cooldown + Dutch reward** |
| Partial liquidation | yes | yes | partial | no | n/a | n/a | partial | partial | **yes (Hyperliquid + Drift patterns)** |
| Cross-margin | yes | yes | yes | yes | n/a | no | yes | yes | **stress-lattice** |
| Insurance fund waterfall | yes (with ADL) | yes (with ADL) | yes (with ADL) | yes (with ADL) | n/a | n/a | yes | yes | **fund + ADL + governance withdraw cap** |
| Multi-LP NAV vault | no (single GLP) | no | no | no | n/a | n/a | partial | no | **yes (ERC-4626 shares)** |
| Position-aware FLP withdraw | n/a | n/a | n/a | n/a | n/a | n/a | partial | n/a | **yes (mark-aware)** |
| Spot markets via same matcher | no (separate) | yes | yes | no | yes | yes | yes | yes | **yes (param recipe)** |
| Cross-market basket orders | no | no | no | no | no | no | no | no | **yes (`place_basket_order_n`, ≤4 legs)** |
| Multi-oracle quorum | yes | partial | yes | yes | n/a | n/a | partial | yes | **median-of-3 + dispersion gate** |
| Real-time invariant monitor | partial | no | no | no | no | no | no | no | **`verify_market_invariants` + auto-pause** |
| Reduce-only / IOC / JIT flags | yes | yes | yes | yes | yes | partial | partial | yes | **yes (flag bitfield)** |
| Tiered fees | volume tiers | volume tiers | volume tiers | volume tiers | n/a | n/a | volume tiers | volume tiers | **discount + NEGATIVE-fee top tier (-20% of base)** |
| Native on-chain trigger (SL/TP) orders | yes | partial | partial | partial | no | no | partial | partial | **yes (`place_trigger_order`)** |
| Native on-chain TWAP orders | yes | no | no | no | no | no | no | no | **yes (`place_twap_order` + permissionless slice exec)** |
| Builder codes (frontend fee share) | yes | no | no | no | no | no | no | no | **yes (`set_trader_builder` + `BuilderFeeOwedEvent`)** |
| Referral / affiliate program | yes | no | no | no | no | no | no | no | **yes (`set_trader_referrer`, one-time-write, anti-rotation)** |
| Multi-threshold pre-liq margin alerts | yes | partial | partial | no | no | no | no | no | **yes (3 tiers, on-chain emit per fill)** |
| Permissionless market creation (HIP-3) | **yes** | no | no | no | no | no | no | no | **yes (`permissionless_initialize_market`, safe envelope)** |
| Pre-launch (pre-TGE) perp markets | yes | no | no | no | no | no | no | no | **yes (`is_pre_launch` flag)** |
| Trading rewards / points eligibility | yes (HYPE) | no | no | no | no | no | no | partial | **yes (per-fill `TradingRewardEligibleEvent`)** |
| Trader delegate / subaccount | yes | subaccounts | subaccounts | sub | n/a | n/a | partial | yes | **delegate slot (master/hot-key split)** |
| Decentralized | mostly | yes | yes | partial | yes | yes | yes | yes | **yes** |
| Settles to Solana mainnet | no | yes | no | no | yes | yes | no | no | **yes** |

## What's genuinely novel about Flash Book

A design is novel iff no other shipped venue has the *combination*. Subsets
exist; the union does not.

### 1. Pool-backed CLOB — Virtual FLP Quoter

Closest cousins:
- **Drift's vAMM** is virtual — no real LP capital backs it. Losses fall on
  insurance fund + protocol.
- **GLP / GLM / JLP** are real LP pools but counterparty-only — they don't
  participate in any orderbook.
- **Phoenix** is spot CLOB without any pool.
- **Vertex** has hybrid CLOB + AMM but the AMM serves price discovery, not
  permanent maker-of-last-resort.

Flash Book makes the FLP pool a **participant in its own book**. Real LP
capital backs every quote level. Human MMs compete *inside* the pool's
spread; flow MMs decline falls back to the pool at the same price.
LPs earn (1) maker rebates from FLP fills, (2) realized PnL from FLP
positions, (3) toxicity tax share — all flowing into NAV automatically
across multi-LP shares.

### 2. FBA Walrasian clearing at HFT cadence

Closest cousins:
- **CowSwap** is FBA but slow (per-block on Ethereum, 12 s).
- **Penumbra** is batch swap with shielded orders, Cosmos-native, not perp.
- **Phoenix** is atomic continuous matching, not FBA.
- **IEX** has a 350μs speed bump but is continuous after that.

Flash Book runs Walrasian clearing every 50 ms. The 240× cadence advantage
over CowSwap is what makes it competitive with continuous CLOBs while
preserving FBA's mathematical MEV neutrality (no within-batch frontrunning;
all fills clear at the same price).

### 3. Sub-100 ms commit-reveal

No deployed perp DEX uses commit-reveal at all. Closest analogue: Flashbots
sealed-bid auctions (12 s on Ethereum). Flash Book runs 50 ms commit + 50 ms
reveal = 100 ms total. Imperceptible to humans, devastating to MEV searchers.

### 4. In-loop liquidations + Dutch reward auction + cooldown

Every other venue uses external keeper bots. Drift's "JIT auction" gets
closest — MMs Dutch-auction over user orders — but liquidations themselves
are still keeper-driven first-to-confirm-wins. Flash Book has THREE layers:

1. **In-loop**: matcher injects the close order into the SAME batch that
   detected the unhealthy state. No external race.
2. **Dutch reward auction**: when keepers DO call `liquidate_position`,
   the reward scales 0% → 100% over `liquidation_auction_duration_slots`
   (typical 8–16 slots). Spreads keeper participation across slots.
3. **Per-position cooldown**: same position can't be liquidated twice
   within `liquidation_cooldown_slots`. Anti-cascade.
4. **Partial liquidation**: keeper specifies size; chain validates ≤
   position size. Hyperliquid pattern — avoids over-closing traders who
   only briefly dipped under maintenance.
5. **Liquidator reward** from liquidatee collateral (capped). Drift/dYdX
   tip-based incentive for a competitive keeper pool.

### 5. Continuous per-block funding + on-chain settlement

Hyperliquid, dYdX, Drift, Aevo, GMX all use ≥ 1-hour funding cadence,
settled lazily off-chain or only when positions touch. Per-block funding
is technically possible on any chain but economically unjustifiable when
each tx costs gas. ER's free compute makes it feasible.

Flash Book additionally adds an explicit **`settle_funding` instruction**:
funding actually moves into trader collateral on-chain rather than
implicit accrual that requires position-touch to realize. A keeper sweep
keeps stale positions current.

### 6. Stress-lattice cross-margin

Hyperliquid uses linear haircuts with cross-asset recognition. dYdX uses
tier-based margin per market. Mango uses health ratio. Drift uses risk
buckets.

Flash Book evaluates the portfolio under a finite set of correlated stress
scenarios. Hedged positions collapse to zero directional risk in every
scenario, leaving only maintenance margin on stressed notional. Borrowed
from CME SPAN — simpler (45 scenarios vs SPAN's hundreds).

### 7. Multi-LP NAV vault (ERC-4626-style)

Other DEXes:
- **GLP / GLM / JLP** are single-pool with mint/burn at oracle index price.
  Late depositors don't dilute (they buy in at fair NAV) but it's not a
  share-based system.
- **Drift** doesn't have an LP pool for the orderbook side.
- **Phoenix** has no LP pool.

Flash Book has per-LP `LpPositionAccount` PDAs holding shares of total
`lp_shares_outstanding`. NAV = `total_capital + realized_pnl`. Maker rebates,
realized PnL, and toxicity-tax shares all flow into NAV automatically;
LPs benefit pro-rata. Withdraws burn shares for proportional NAV claim.

### 8. Toxicity tax routed to MAKERS, not the protocol

Most venues that compute a VPIN-like toxicity signal use it for spread
widening (the FLP quoter does this too) but don't route value back. We
do both:

1. The taker pays a toxicity tax on top of the regular fee, scaled by the
   current VPIN.
2. The tax is split between the insurance fund (resilience) and the maker
   (compensation for absorbing toxic flow).

This is structurally important: it makes maker-side compensation match the
adverse-selection cost they bear. Maker yield rises specifically during
high-VPIN periods, which is when MMs would otherwise pull back.

### 9. Cross-market basket orders with joint margin gate

`place_basket_order` (2-leg explicit) and `place_basket_order_n` (≤4 legs
via `remaining_accounts`). Both run a SINGLE cross-market stress-lattice
gate against the projected post-leg state. The cross-margin hedge benefit
materializes natively — a long-short basket sees reduced required margin
in correlated stress scenarios, mirroring the existing `liquidate_portfolio`
cross-margin recognition.

No other Solana orderbook DEX has this. Hyperliquid has bracket orders
(child orders triggered by parent fill) but not single-tx multi-market
basket placement with joint risk evaluation.

### 10. Position-aware FLP withdraw

Most LP-pool DEXes block withdrawals if the pool has open exposure (or
allow them and let the pool become undercollateralized — silent risk).
Flash Book walks `remaining_accounts` of all active markets, computes
gross exposure at current marks, and requires post-withdraw NAV ≥ gross
exposure. LPs can withdraw safely while the pool carries positions, up
to the point where remaining capital still covers all open exposure.

### 11. Real-time invariant monitor + kill switch

`verify_market_invariants` (permissionless) checks documented solvency
invariants (S5: OI balance, with S4/S12/S14 plumbed for follow-up) and
auto-flips the market to `Paused` on breach. Off-chain monitors page
operators on the emitted `InvariantBreachDetectedEvent`. No comparable
on-chain primitive on any other DEX — most rely on off-chain monitoring
and manual response.

### 12. Multi-oracle quorum

`update_oracle_quorum` accepts 3 prices, takes the median, rejects if
the dispersion exceeds `oracle_quorum_max_dispersion_bps`. Hyperliquid
runs an internal oracle aggregation; Pyth itself uses publisher quorum.
Flash Book brings the quorum check to the consumer side — even with a
single Pyth feed, additional sources can be wired by the operator.

### 13. Native trigger + TWAP orders that survive bot downtime

Hyperliquid is the only other venue where stop-loss and TWAP orders live
on-chain (most DEXes leave them as off-chain bot logic — your stop fires
ONLY if your bot is online). Flash Book matches this: `place_trigger_order`
and `place_twap_order` create durable PDAs; permissionless keepers
(`execute_trigger_order` / `execute_twap_slice`) fire them when the
condition is met. Trigger orders support reduce-only + expiry. TWAPs
slice into time-spaced limit orders at a capped price. Both gracefully
handle the final-slice residual to preserve `min_base_lots` invariants.

### 14. HIP-3 + Flash V2's pool backing — the synthesis

Hyperliquid's HIP-3 is the gold standard for permissionless market
deployment: anyone stakes HYPE, deploys a market, earns fees forever.
Flash Book matches the deployment surface (`permissionless_initialize_market`,
caller becomes creator + earns `creator_share_bps` of net fee) but
backs it with Flash V2's pool model — every quote level is real LP
capital, not a vAMM or a virtual book. The result: anyone can list a
market, the FLP backstops liquidity from day one, and the deployer
earns alongside the LPs and protocol.

The on-chain safe envelope (max 20× leverage, fees in [10, 200] bps,
maint margin ≥ 2%, ≤1% of FLP per trader, etc.) prevents the obvious
griefing attacks that other "permissionless" venues quietly enable.

### 15. Builder codes + negative-fee top tier — making professional flow profitable

Builder codes (`set_trader_builder`): a frontend/aggregator earns up to
the user's approved cap (`builder_max_fee_share_bps`) per fill. Trader
signs the install — protocol authority cannot install a builder against
the user's will (anti-rug). HL parity.

Negative-fee top tier: `set_trader_fee_tier` accepts up to 12_000 bps
(120%). Values > 10_000 enable rebates *to the taker* — the protocol
pays the top-volume MMs out of its own insurance contribution (never
from maker rebates, never overdrawing insurance). Direct port of the
HL VIP / MM-pro tier model. Backward-compatible: 0..10_000 still works
as a positive-fee discount.

## Where Flash Book is NOT first

Honest about what's not novel:

- **VPIN** has been used in HFT firms since 2012 (Easley et al.). Applying
  it on-chain inside a matcher is novel; the metric itself isn't.
- **Avellaneda-Stoikov inventory model** is from 2008. Used here as the
  FLP quoter basis.
- **Walrasian clearing** is from 1874. FBA at HFT cadence: Budish 2015.
- **Cumulative-index interest accrual** is the Compound/Aave pattern.
- **Insurance fund + ADL waterfall** is standard CEX practice (BitMEX
  2018) ported to DEX context (Hyperliquid, dYdX, Drift).
- **Reduce-only / IOC / FOK** order types are decades-old TIF semantics
  from FIX gateways.
- **OCO / Iceberg / Trailing stop** are commodified CEX features.
- **JIT auction** for taker orders is Drift's (and Robinhood's) pattern.
- **Subaccount delegation** is Hyperliquid + dYdX standard.

The novelty is the **synthesis** — combining all these into one matcher,
on one rollup, with the bot/keeper/backtester suite + smart router that
ships *with* the protocol.

## Bot + ops suite — vs other DEXes

Most DEXes ship the protocol and let market makers build their own bots.
Flash Book ships:

| Component | Hyperliquid | Drift v2 | dYdX v4 | **Flash Book** |
|---|---|---|---|---|
| Reference MM bot | community | community | community | **`@flash-book/bot` ships** |
| Multi-market quoting in one process | community | community | community | **`MultiMarketBot`** |
| Quote diffing (skip re-quote on small moves) | community | community | community | **per-market `priceDiffBps`** |
| Backtester (replay tape through strategy) | community | community | community | **`Backtester` class** |
| Liquidation keeper | open-source | open-source | open-source | **`LiquidationKeeper` ships** |
| Funding sweep keeper | n/a (lazy) | n/a (lazy) | n/a (lazy) | **`FundingKeeper` ships** |
| Invariant monitor keeper | community | community | community | **`InvariantMonitor` ships** |
| ATA cleanup keeper | n/a (no SPL ATA) | community | n/a | **`AtaCleanupKeeper` ships** |
| Keeper auto-discovery | community | community | community | **`getProgramAccounts` scanner ships** |
| Multi-venue smart router | community | community | community | **`SmartRouter` (V2+V3)** |
| Telemetry (Prometheus) | community | community | community | **`MetricsRegistry` + push** |
| Hot config reload | community | community | community | **`HotConfigReloader`** |
| Advanced order types (OCO/Iceberg/Trailing) | community | partial | community | **`OcoOrder`, `IcebergOrder`, `TrailingStopOrder`** |

Flash Book is the only protocol where every box above is shipped first-party,
audited together, and tested against the on-chain state machine in the same
CI.

## CEX comparison — feature parity

How does Flash Book stack up against the major centralized exchanges traders
already know?

| Feature | Binance / OKX / Bybit / Coinbase | **Flash Book** |
|---|---|---|
| Sub-block matching latency | yes (microseconds) | **yes (~50 ms FBA cadence)** |
| Limit / market / IOC / FOK / Post-only | yes | **yes (flag bitfield)** |
| OCO / Iceberg / Trailing stop | yes | **yes (off-chain bot)** |
| Reduce-only | yes | **yes** |
| Cross-margin / portfolio margin | yes | **yes (stress-lattice)** |
| Subaccounts | yes | **delegate slot (foundation)** |
| Tiered fees by 30-day volume | yes | **yes (off-chain volume + on-chain `set_trader_fee_tier`)** |
| Maker rebates | yes | **yes (+ JIT bonus)** |
| MEV-resistant matching | yes (centralized) | **yes (FBA + commit-reveal)** |
| Insurance fund | yes | **yes (with governance withdraw cap)** |
| Live PnL / position UI | yes | (separate `flash-ui` repo) |
| Volume rebates | yes | **yes (off-chain tier table → `set_trader_fee_tier` on-chain, with negative-fee top tier)** |
| Custody | centralized (counterparty risk) | **non-custodial (you sign)** |
| Permissionless market creation | no | **yes (HIP-3-style: `permissionless_initialize_market` with safe envelope)** |
| Pre-TGE perp listings | partial | **yes (`is_pre_launch` flag, governance-set oracle)** |
| Native stop-loss / TWAP that survive bot downtime | yes | **yes (on-chain `TriggerOrderAccount` + `TwapOrderAccount`)** |
| Frontend revenue share / builder codes | n/a | **yes (`set_trader_builder` + `BuilderFeeOwedEvent`)** |
| Referral / affiliate program | yes | **yes (`set_trader_referrer`, one-time-write)** |
| Trading-rewards / points eligibility (HYPE-style) | n/a (token rewards) | **yes (per-fill `TradingRewardEligibleEvent`)** |
| Open-source matcher | no | **yes (this repo)** |
| Open-source MM bot | no | **yes (`@flash-book/bot`)** |

The CEX UX features — order types, fee tiers, hot reload, telemetry —
are all there. What you GAIN by going on-chain: non-custodial,
MEV-neutral matching, audited matcher, open keeper economics, and a
reference bot you can fork.

## Why this matters for Flash Trade

Flash V2 today is a pool-only oracle-priced perp protocol. As volume
scales, two structural problems compound:

1. **Toxic flow eats the pool.** Informed traders pick off the pool when
   oracle lags reality. The pool's defenses (spread, OI fees, funding) are
   blunt. LP yield decays.
2. **No real price discovery.** Without an orderbook, Flash has no native
   price discovery — it's a price-taker of Pyth. Pyth bugs or manipulation
   propagate directly.

Flash Book V3 fixes both:

1. **MMs absorb informed flow first.** The pool only takes flow MMs decline,
   which is structurally less profitable for MMs. By selection, the
   remaining pool flow is more profitable for the LPs. Toxicity tax routes
   directly to the maker who absorbed the toxic flow.
2. **Real price discovery.** Mark price is the TWAP of actual cleared
   trades (banded by oracle). Manipulating it requires actually clearing
   volume — the manipulator pays for every basis point moved.
3. **LPs scale linearly.** Multi-LP NAV vault means yield can be spread
   across N LPs without coordination. Flash today is a single GLP-style
   pool; Flash Book is permissionless to deposit.
4. **Production-ready ops.** The bot + keeper + backtester + telemetry
   suite means Flash doesn't need to wait for community tooling to ship V3.

The Pareto improvement claim: retail UX is identical-or-better than today,
LP yield is structurally higher, the protocol gains real price discovery,
and Flash Trade gains the most advanced on-chain matcher ever shipped.

## What we deliberately did NOT clone from Hyperliquid

To stay honest about scope:

- **Subaccounts as a separate account type.** HL's subaccounts are a UX
  pattern; we cover the same functionality via the existing
  `delegate` slot on TraderStateAccount (master keypair holds funds,
  delegate keypair trades). A separate SubaccountAccount type with
  cross-margin sweep adds a lot of state and account permutations for
  what users can already get from "create a second wallet." If demand
  appears, the slot is reserved.
- **Position-specific leverage cap.** HL lets you set a leverage cap
  per position; we use stress-lattice cross-margin per portfolio plus
  per-market `max_leverage`. Adding per-position leverage requires
  reworking the lattice gate. Tracked but not yet a user pain point.
- **HYPE-style governance token.** We emit `TradingRewardEligibleEvent`
  per fill so any token launch can compute eligibility; the token
  itself is a governance + tokenomics decision, not an engineering
  one.
- **Slashable HIP-3 deployer bond.** Our v1 envelope (max leverage,
  margin floors, position caps) prevents the obvious griefing without
  requiring a bond. A slashable bond can be added on top of the
  existing creator slot when the HYPE-equivalent token exists.
- **User-managed trading vaults.** Achievable with the existing LP-
  share NAV math + delegate slot (a vault is a TraderState with
  delegate = strategist). A first-party "deploy a vault" flow adds
  significant UX surface and is deferred until a real strategist
  wants to ship one.

These exclusions are deliberate scope discipline, not gaps. Each is
documented with the existing primitive that covers most of the value
and the seam where the full feature would slot in.
