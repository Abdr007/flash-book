# Flash Book V3 — The Elite Plan

> **Goal:** Build the most advanced perpetual orderbook DEX shipped to date —
> matching Hyperliquid on user-facing surface, beating it on math + safety,
> matching Phoenix + Manifest on Solana-native efficiency, and uniquely
> backed by Flash Trade's FLP pool. This document is the synthesis after
> deep research into Manifest, Phoenix, Hyperliquid, and the Flash V2 SDK.

## 1. Prior-art audit (what every leader actually does)

### 1.1 Manifest (Bonasa-Tech) — the "free + crankless" Solana CLOB

Manifest's ENTIRE design philosophy is that the base layer should be tiny.
It is the closest thing to a perfect "skeleton" CLOB on Solana.

**Data structure — the killer move:**
- A `MarketState` is a **fixed 256-byte header + dynamic byte array of
  80-byte nodes** (`MarketFixed` struct, `programs/manifest/src/state/market.rs`).
- The dynamic array is a **hypertree**: 3 red-black trees (bids, asks,
  claimed seats) AND 1 linked list (free-list of evictable blocks)
  ALL overlap inside the same byte array. Every node is exactly 80 bytes.
- Header carries `bids_root_index`, `bids_best_index`, `asks_root_index`,
  `asks_best_index`, `claimed_seats_root_index`, `free_list_head_index`.
- Indices are `DataIndex = u32` (byte offset in the dynamic array).
- `Expand` ix grows the account by `MARKET_BLOCK_SIZE`; new bytes go to
  the free list. No fragmentation — freed nodes rejoin the free list.

**Why this matters for us:** **It permanently solves the BPF 4KB stack
problem** that just bit us. The whole market is `bytemuck::Pod`, loaded
via `AccountLoader`, never deserialized onto the stack. No 5KB struct
to materialize.

**RestingOrder = 64 bytes** (`programs/manifest/src/state/resting_order.rs`):
- `price: QuoteAtomsPerBaseAtom` (16) — packed numerator/denominator
- `num_base_atoms: BaseAtoms` (8)
- `sequence_number: u64` (8)
- `trader_index: DataIndex` (4) — points at the trader's seat in same array
- `last_valid_slot: u32` (4) — GTT
- `is_bid: PodBool` (1)
- `order_type: u8` (1) — `Limit / IOC / PostOnly / Global / Reverse / ReverseTight`
- `reverse_spread: u16` (2) — for AMM-like Reverse orders
- `_padding: [u8; 20]`

**Instructions** (`programs/manifest/src/program/processor/`):
`CreateMarket`, `ClaimSeat`, `Deposit`, `Withdraw`, `Swap`, `SwapV2`,
`Expand`, `BatchUpdate` (the mass place/cancel ix), `GlobalCreate`,
`GlobalAddTrader`, `GlobalDeposit`, `GlobalWithdraw`, `GlobalEvict`,
`GlobalClean`. Notably **no fee logic** anywhere — Manifest is forever
feeless at the protocol level.

**Novel ideas:**
- **Global orders**: a single token deposit can back orders on N markets
  via just-in-time CPI fund movement.
- **Reverse orders**: a limit that flips side on fill (AMM mechanics in CLOB).
- **Crankless**: matching is fully synchronous in the place tx; no
  external keeper to advance state.
- **Wrapper architecture**: ClientOrderId, FillOrKill, etc. live in
  separate programs that CPI into the core. Keeps core minimal +
  certifiable.
- **Certora formally verified** across 4 property classes (RBT
  invariants, loss-of-funds, availability, matching correctness).

**Known Flash fork:** the user said the team forked Manifest. Couldn't
locate the fork via public GitHub search; it's likely private. This
plan assumes the fork exists and is the V3 starting point on Flash's
side. Architectural alignment below ensures **our V3 can either be
the fork (replacing it) or interop cleanly with it.**

### 1.2 Phoenix v1 (Ellipsis Labs) — the production Solana atomic CLOB

Phoenix is the production reality on Solana for spot markets — battle-
tested, OtterSec-audited, runs at `PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY`.

**Matching:** FIFO at price level with price-time priority (`fifo.rs`).
Continuous (every fill in its own tx). Bids RBT descending price;
asks RBT ascending. Within a price level, lower seq matches first.

**The OrderId trick (the elite move):**
```
FIFOOrderId encodes:
  - leading bit of order_sequence_number = side (0 = ask, 1 = bid)
  - For bids: bits are INVERTED (!seq) so natural u128 sort works
```
Result: a single u128 ordering serves both sides. We can copy this
verbatim.

**Generic Market type:**
```rust
pub struct FIFOMarket<TraderId, const BIDS_SIZE: usize,
                     const ASKS_SIZE: usize, const NUM_SEATS: usize>
```
Concrete instantiation sets compile-time caps. Account size is
deterministic. Uses `sokoban::RedBlackTree` (zero-copy RBT crate).

**Fees:** `(size × fee_bps + 10000 - 1) / 10000` — rounds UP, in
quote atoms. Per-market `fee_collector` is configurable; markets
can run completely free or charge any rate up to 10000 bps.

**Instructions (27 total)** — notably:
- `PlaceLimitOrderWithFreeFunds` — trade against the trader's
  in-market seat balance, no SPL CPI.
- `PlaceMultiplePostOnlyOrders` — single ix for multi-level MM quotes.
- `ReduceOrder` (cheaper than cancel+replace).
- `CancelAllOrders`, `CancelUpTo`, `CancelMultipleOrdersById` —
  three flavors of mass cancel.
- `DepositFunds`, `WithdrawFunds` — explicit settlement layer.
- `RequestSeat` / `RequestSeatAuthorized` / `EvictSeat` /
  `ChangeSeatStatus` — seat lifecycle.
- `CollectFees`, `ChangeFeeRecipient`, `ChangeMarketStatus`,
  `ClaimAuthority`, `NameSuccessor` — governance.

**Seat model (the elite move #2):**
- Each (market, trader) has a `Seat` slot in the market account.
- Trader requests/claims a seat ONCE per market.
- Trades settle INTO the seat ("free funds"). Funds only leave the
  market account on explicit `WithdrawFunds`.
- This avoids SPL token CPI on every trade — sub-100µs trade hot path.

**License:** BUSL-1.1 (commercial restriction during the protected
period before going full open-source).

### 1.3 Hyperliquid — the centralised-orderbook gold standard for perps

Hyperliquid is the user-experience benchmark. ~$5B daily volume, native
L1 (HyperBFT consensus), deeply integrated mark + oracle + funding
+ vaults + builder codes + HIP-3.

**Matching:** Continuous CLOB, price-time priority. Mempool/consensus
sorts actions in 3 categories: (1) non-GTC/IOC, (2) cancels, (3)
GTC/IOC, then by proposal order within each. **Margin is checked at
both order-open AND at the moment of each match** — catches oracle
moves between submit and match.

**Margin formulas (exact):**
- Initial Margin Fraction = `1 / leverage_set_by_user`
- Maintenance Margin Fraction = **half of the max-leverage IM** (e.g.
  20× max leverage → 2.5% MMR)
- Liquidation when account equity falls below `MMR × total_notional`
- **Backstop liquidation** triggers when equity drops below
  `(2/3) × MMR × total_notional`
- Withdrawal additionally checked against
  `max(initial_margin_required, 0.1 × total_position_value)` —
  protects against just-in-time leverage abuse

**Margin modes:** Cross (default, shared collateral), Isolated
(per-asset capped), Strict-Isolated (no margin removal), No-Cross
(HIP-3 isolated with withdrawal).

**ADL ranking formula (exact):**
```
adl_score = (mark_price / entry_price) × (notional / account_value)
```
Then by creation time (older first). Backstop-liquidated positions
get **no special priority** — they go in the ADL queue normally.

**Mark price:** Stake-weighted median of each validator's submitted
oracle. Oracle = weighted median of 8 CEXes (Binance × 3, OKX × 2,
Bybit × 2, Kraken × 1, Kucoin × 1, Gate × 1, MEXC × 1, HL spot × 1),
sampled every **3 seconds**.

**Funding:** 1-hour cadence, paid hourly (1/8 of the 8-hour calc).
Rate cap **4%/hr**. Premium sampled every **5s**, averaged over the
hour. Standard perps:
```
premium = impact_price_difference / oracle_price
```
HIP-3:
```
premium = 0.5 × (impact_bid + impact_ask) / oracle - 1
```
Plus a fixed 0.01% / 8h interest term (~11.6% APR).

**No OI dampening** — Flash Book V3 already beats HL on this with
our `funding_oi_dampening` flag.

**Fee tiers (perps):**
| Tier | Volume | Taker | Maker |
|---|---|---|---|
| 0 | 0 | 0.045% | 0.015% |
| 1 | $5M | 0.040% | 0.012% |
| 2 | $25M | 0.035% | 0.008% |
| 3 | $100M | 0.030% | 0.004% |
| 4 | $500M | 0.028% | 0.000% |
- Diamond tier (≥500k HYPE staked): 40% additional discount

**Spot fees:** add 25 bps to taker, 25 bps to maker baseline.

**Vaults:**
- HLP — protocol vault, "fully community-owned", 4-day lock-up,
  withdraw enabled 4 days after most recent deposit.
- User vault — 10% perf fee to leader, 1-day withdrawal cooldown,
  leader needs 100 USDC min + must keep ≥5% ownership; 10k USDC
  creation gas. Withdraw at insufficient margin: open orders
  cancelled in increasing margin order; if still short, **20% of
  positions auto-closed**, repeated until margin freed.

**HIP-3 (permissionless markets):**
- Bond: 500k HYPE staked.
- Slash: up to 100% for invalid state transitions, 50% for brief
  network downtime, 20% for network degradation.
- First 3 assets per deployer: no auction. Additional: Dutch auction.
- Fee share to deployer: **fixed 50%** (user pays 2× standard fees).
- Cross-margin eligibility: needs sufficient liquidity + reliable
  external oracle + ≤ 1 daily 50%+ move per month.

**Builder codes:** max 0.1% perps / 1% spot fee share. Builder must
hold ≥100 USDC.

**Order types:** Market, Limit (TIF: GTC, IOC, ALO/post-only),
Stop Market / Stop Limit / Take Market / Take Limit, Scale,
TWAP (30s slices, 3% max slippage, 3× retry on miss), Reduce-Only,
TP/SL.

**Lessons from incidents:**
- **JELLY** (March 2025): on-chain oracle attack where attacker
  manipulated illiquid spot price → fed back to perp mark → forced
  liquidations → HLP holding bag. Lesson: oracle aggregation must
  exclude illiquid sources; mark TWAP must be longer than the
  manipulation window.
- **POPCAT** cascading liqs: concentrated longs triggered a chain.
  Lesson: concentration margin (which we already have) + per-market
  OI cap are essential.

### 1.4 Flash V2 (`@flash_trade/magic-trade-client` v1.0.11)

The CURRENT production Flash. V3 must be drop-in compatible OR a
clean migration target.

**From `bot/src/flash-v2-venue.ts` and `flash-ui/src/lib/magic-trade.ts`:**

- **Account model:** Each user has ONE `BasketAccount` holding
  `positions[]` AND `orders[]` across ALL markets. No per-market
  trader_state PDA.
- **Markets:** Each (target, lock_custody, side) tuple is a **distinct**
  on-chain `MarketAccount`. Bid and ask are TWO market accounts (long-
  side, short-side), not one shared book.
- **Order ID:** `u8` (0..255 per user-side). Limited.
- **Cancellation:** `editLimitOrder` with `limit_price = 0` AND
  `size_amount = 0` is the canonical cancel pattern (per IDL doc).
- **Mark price:** the oracle (no FBA, no clearing-price TWAP).
- **No VPIN, no orderbook-style discovery** — V2 is pool-quoted.
- **Session signer pattern (MagicBlock ER):**
  - Owner signs ONCE → on-chain session token PDA on mainchain
  - Browser holds an ephemeral session `Keypair` in memory only
  - Subsequent trades signed by session key against ER endpoint
    (~10–50ms confirms)
  - Session token has `validUntil` enforced on-chain, default 8 hours
- **Pool config:** opaque `PoolConfig` (pool name, custody references)
  passed by user, owned by SDK
- **Default ER endpoint:** `https://devnet.magicblock.app`
- **Default cluster:** `mainnet-beta`
- **Default pool name:** `Pool.0`
- **Prioritization fee:** 50_000 microlamports

**Public SDK surface used:**
- `placeLimitOrder(targetSym, collateralSym, side, poolConfig, params)`
- `editLimitOrder(targetSym, collateralSym, side, poolConfig, params)` — cancel pattern
- `accounts.fetchMarket(targetCustody, lockCustody, side)`
- `accounts.fetchBasket(owner)`

**Flash UI (`/Users/abdulrahman/flash-ui`):**
- Next.js app (Next 16 with Turbopack-style runtime per `AGENTS.md`)
- Uses `@flash_trade/magic-trade-client` ^1.0.11 + `flash-sdk` ^15.13.1
- Privy auth + Phantom/Solflare wallets
- `@ai-sdk/anthropic` chat-driven trade entry — natural language → trade
- Trade entry: `components/trade/TradeCard.tsx` (Galileo-style cards)
- Position panel: `components/positions/PositionPanel.tsx`
- ER session control: `components/trade/MagicSessionControl.tsx`
- API routes for limit orders + chat tools
- **No traditional CLOB UI components yet** — chat-driven order entry
  is the current paradigm
- Uses `flash-sdk` for the rest of Flash V2 read-side
- `trade-firewall.ts` — pre-execution risk checks
- `magic-trade-execute.ts` — actual execution path

## 2. The V3 design — the elite synthesis

This is what we build. Each section steals the best primitive from
the prior art and combines them. Numbered = rough implementation order.

### 2.1 Hypertree-backed orderbook (steal Manifest, fix our BPF stack)

**Replace** the current `OrderBufferAccount` (flat `[OrderSlot; 16]` array
with Borsh-Account deserialize that overflows BPF stack at CAP=64) **with**
a Manifest-style hypertree:

```
MarketBookAccount (zero_copy, ~10 KB):
  ├── header (256 bytes, fixed)
  │     ├── disc, version, market_pubkey, base_mint, quote_mint
  │     ├── bids_root_index, bids_best_index
  │     ├── asks_root_index, asks_best_index
  │     ├── claimed_seats_root_index
  │     ├── free_list_head_index
  │     ├── order_sequence_counter
  │     ├── num_bytes_allocated
  │     └── padding
  └── nodes: [u8; ~9750]  (raw byte array, divided into 80-byte slots)
        ├── overlapping RBTree<RestingOrder>  (bids)
        ├── overlapping RBTree<RestingOrder>  (asks)
        ├── overlapping RBTree<ClaimedSeat>   (per-trader state)
        └── overlapping LinkedList<FreeNode>  (evictable slots)
```

- **Every node is exactly 80 bytes** — `bytemuck::Pod`-compatible
- **Loaded via `AccountLoader`** → no stack pressure
- **`Expand` ix** grows the account in `MARKET_BLOCK_SIZE = 800` byte
  chunks (10 nodes at a time), capped at Solana's ~10 KB account limit
  for v1; later phases use the recently-released "account resize"
  mechanism for unbounded growth.
- **Capacity at 10 KB:** ~120 nodes total → ~50 bid orders + 50 ask
  orders + ~20 seats. With one Expand → ~200 orders + 50 seats.
  At Solana's 10 MB max realloc → ~125,000 orders per market.

**Result:** the BPF stack issue we hit (cap forced down to 16 with
Borsh) is **structurally eliminated**. We ship at CAP = 200+ on day one.

### 2.2 Side-encoded OrderId (steal Phoenix)

```rust
// packed u128
struct OrderId {
    price_ticks: u64,        // upper 64 bits
    seq_with_side: u64,      // lower 64 bits; bit 63 = side (1=bid, 0=ask)
                             // for bids, lower 63 bits are !seq for natural sort
}
```

Single u128 sort serves both sides. Bid/ask comparison is identity.
Phoenix uses this in production at scale.

### 2.3 FBA Walrasian + price-time tie-break (Flash differentiator)

The matcher KEEPS the FBA Walrasian uniform-price clearing —
this is our MEV-defense moat that no other CLOB has. But within a
batch, we use **Phoenix-style price-time priority** for tie-breaking,
not arbitrary order:

```
clear_batch(orders):
  1. Compute uniform clearing price p* (Walrasian: max cleared volume)
  2. Eligible buys = {o ∈ buys : o.price ≥ p*}, sorted by OrderId
  3. Eligible sells = {o ∈ sells : o.price ≤ p*}, sorted by OrderId
  4. Walk pairs FIFO at the clearing price
  5. Self-trade prevention via STP modes (already shipped)
```

Same MEV neutrality, fairer time priority. No other DEX combines FBA
+ Phoenix-style FIFO + commit-reveal.

### 2.4 Seat model + free funds (steal Phoenix)

**Add** a `Seat` slot in the hypertree per (market, trader) holding
the trader's working balance ("free funds"):

- First trade on a market: `claim_seat` ix, one-time rent ~$0.0005
- Subsequent trades: settle into Seat (no SPL CPI)
- `withdraw_seat_funds` ix to move free funds → trader ATA
- `Seat` carries `base_free`, `quote_free`, `base_locked`, `quote_locked`

**Result:** trade hot path is **pure on-chain memory mutation** — no
SPL invoke. ~10× faster, ~5× cheaper in CU.

This is fully compatible with Flash V2's `BasketAccount` model: V2's
basket can hold the V3 seat references, so a single user has unified
capital across V2 (legacy pool) and V3 (orderbook).

### 2.5 Hyperliquid-grade margin + ADL (replace ours with HL's exact formulas)

Replace our current ADL ranking `(pnl × leverage)` with HL's:

```rust
adl_score(pos, mark, account_value) =
    (mark / pos.entry_price) * (pos.notional / account_value)
```

Add **backstop liquidation** at `(2/3) × MMR`: if equity falls below
this, the FLP becomes the counterparty (last-resort buyer/seller).
HL routes this through HLP; we route through FLP.

Add HL's **withdrawal-time margin floor**:
```
margin_after_withdraw ≥ max(initial_margin_required,
                            0.1 × total_position_value)
```
Stops just-in-time leverage abuse on withdraw.

Per-asset MMR table (HL pattern):
- Major (BTC, ETH, SOL): 1.25% (max 40× leverage)
- Mid (top 20): 2% (max 25×)
- Long-tail / HIP-3: 4% (max 12.5×)

Combined with our existing **concentration margin tier** (FLP-keyed
extra MMR for whales), Flash V3 has a STRICTLY MORE conservative risk
model than HL on long-tail assets.

### 2.6 Multi-validator-oracle mark (steal HL)

Currently mark = TWAP(clearing prices), banded by Pyth. Add HL's
**stake-weighted-median-of-validators** as a SECOND signal:

```
final_mark = clamp(
  twap_clearing,
  min(pyth, weighted_median(validators)),
  max(pyth, weighted_median(validators))
)
```

In ER context: each MagicBlock ER validator publishes its oracle read
each batch; matcher takes weighted median by stake. Catches single-
oracle spikes (the JELLY attack class) that pure-Pyth misses.

### 2.7 HL-grade fee tier table

Replace our `set_trader_fee_tier(discount_bps)` with an explicit
volume-tier table that mirrors HL:

| Tier | 30d volume | Taker | Maker |
|---|---|---|---|
| 0 | 0 | 4.5 bps | 1.5 bps |
| 1 | $5M | 4.0 | 1.2 |
| 2 | $25M | 3.5 | 0.8 |
| 3 | $100M | 3.0 | 0.4 |
| 4 | $500M | 2.8 | 0.0 |

Plus our **negative-fee top tier** (already shipped, > 100% discount)
for MM-pro flow. Plus future **Flash-stake discount** when a Flash
governance token launches (40% extra discount, mirroring HL's HYPE
diamond tier).

### 2.8 Manifest-style wrappers (keep core minimal)

After the rewrite, the V3 core has ONLY these ixs:
- Setup: `init_market`, `expand_market`, `claim_seat`
- Money: `deposit`, `withdraw_seat_funds`
- Order: `place_limit_order`, `cancel_order_by_id`, `cancel_all_orders`,
  `reduce_order`
- Matching: `run_batch` (per ER batch tick)
- Risk: `liquidate_position`, `auto_deleverage`, `settle_funding`,
  `verify_market_invariants`
- Pool: `deposit_flp_capital`, `withdraw_flp_capital`
- View ixs: `view_predicted_funding`, `view_quote_ladder`,
  `view_portfolio_risk`

**Wrapper programs (separate Anchor programs, CPI into core):**
- `flash-book-triggers` — trigger orders, TWAP, brackets, trailing,
  iceberg (currently in core; refactor to wrappers in wave 18)
- `flash-book-vaults` — user-managed vaults
- `flash-book-hip3` — permissionless market deployment + bond
- `flash-book-builder` — builder codes + referrals + creator share

Result: core is auditable + formally verifiable (Certora). Features ship
without touching the safety-critical hot path.

### 2.9 Flash V2 compatibility (the migration story)

V3 is **additive** — V2 keeps running. Migration paths:

**Day 1 — V3 is a new venue alongside V2:**
- V2 keeps doing pool-quoted oracle trades for liquidity-light pairs
- V3 runs orderbook for SOL-PERP, BTC-PERP, ETH-PERP
- The bot's `MultiMarketBot` already routes between V2 + V3 venues
  via `SmartRouter` (already shipped)
- UI: add a "Pro / Orderbook" toggle on the trade card

**Month 3 — V3 absorbs major-pair flow:**
- V2 pools become DEEP markets only (long-tail, RWA)
- V3 takes 80%+ of perp volume

**Year 1 — V2 is for backwards-compat only:**
- All new markets ship as V3
- V2 closes deposits for major pairs, keeps withdrawals open
- Eventual V2 sunset

**Session-signer compat:** V3 honors V2's session-token PDA. The same
ER session key that signs V2 trades signs V3 trades. UI doesn't need
two session managers.

### 2.10 What we keep that no other DEX has

Already-shipped Flash V3 differentiators that no top-3 competitor matches:

| Feature | HL | Phoenix | Manifest | Flash V3 |
|---|---|---|---|---|
| FBA Walrasian clearing | continuous | continuous | continuous | **✓ batched** |
| Commit-reveal MEV defense | no | no | no | **✓** |
| Pool-backed CLOB (real LP capital backs every quote) | no | no | no | **✓ FLP virtual** |
| In-loop liquidations (no keeper race) | no (auction) | n/a | n/a | **✓** |
| Multi-LP NAV vault | HLP only | n/a | n/a | **✓** |
| Stress-lattice cross-margin | linear haircut | n/a | n/a | **✓ CME SPAN-style** |
| Symmetric-OI funding dampener | no | n/a | n/a | **✓** |
| Funding-premium TWAP dampener | 5s sample, 1h avg | n/a | n/a | **✓ per-batch** |
| Concentration margin (FLP-keyed) | flat MMR | n/a | n/a | **✓** |
| Cross-market basket orders w/ joint margin gate | no | no | no | **✓** |

Each of these is a real moat. None are getting deleted; they all
survive the rewrite.

## 3. Roadmap

### Wave 17 — research synthesis (this doc) ✓

### Wave 18 — hypertree refactor (THE GOAT MOVE)
- Add `hypertree` module — Pod-compatible RBT + LinkedList over a
  byte array, modeled after Manifest's `programs/manifest/lib/hypertree`
- Replace `OrderBufferAccount` + `CommitBufferAccount` with hypertree-
  backed `MarketBookAccount`
- Add `expand_market` ix
- Restore `ORDER_BUFFER_CAP` to unbounded (limited only by realloc)
- Phoenix-style `FIFOOrderId` for sort
- Migrate matcher to walk the hypertree

### Wave 19 — seat + free funds
- Add `Seat` node type in hypertree
- `claim_seat`, `withdraw_seat_funds` ixs
- Refactor trade hot path to settle into seat
- Update `place_limit_order` to deduct from seat free balance
- SDK builders + tests

### Wave 20 — HL-grade margin + ADL
- Replace ADL ranking formula with `(mark/entry) × (notional/equity)`
- Add backstop liquidation at `(2/3) × MMR`
- Add withdrawal-time margin floor `max(IM, 0.1 × notional)`
- Multi-validator oracle mark (HL's weighted median)
- Per-asset MMR table

### Wave 21 — wrapper migration
- Move trigger/TWAP/bracket/trailing/iceberg into
  `flash-book-triggers` wrapper program
- Move vaults into `flash-book-vaults`
- Move HIP-3 into `flash-book-hip3`
- Core shrinks to ~12 ixs

### Wave 22 — fee tier table + Flash governance hooks
- Volume-tier table mirroring HL
- Flash governance token discount tier (when token exists)
- Builder/referrer/creator share recomputed against tiers

### Wave 23 — Flash V2 / UI integration
- Add `OrderbookCard` to flash-ui (CLOB depth view)
- Wire `view_quote_ladder` for live FLP depth
- Add "Pro / Orderbook" toggle to trade entry
- Document V2 → V3 session-token reuse

### Wave 24 — Certora + audit prep
- Write Certora spec for hypertree invariants (RBT correctness,
  loss-of-funds, availability, matching)
- Mirror Manifest's 4 property classes
- Engage external audit firm

## 4. Why this beats Hyperliquid

A reviewer comparing Flash V3 to HL on day one will see:

**Same as HL:**
- Native trigger / TWAP / bracket / trailing / iceberg orders ✓
- HL-style margin (cross/isolated/strict-isolated/no-cross) ✓
- HL-style ADL ranking + backstop liq ✓
- HL-style fee tier table ✓
- HL-style HIP-3 permissionless markets + slashable bond ✓
- User-managed vaults with HWM perf fee ✓
- Builder codes + referrals + creator share ✓
- View ixs (predicted funding, quote ladder, portfolio risk) ✓
- Multi-validator-median oracle mark ✓

**Better than HL:**
- **FBA Walrasian + commit-reveal** — mathematically MEV-neutral
  within batch. HL has none of this.
- **Pool-backed CLOB (FLP virtual quotes)** — every level has REAL
  LP capital backing it. HL relies on HLP as one MM; we have FLP
  IN the book.
- **Symmetric-OI funding dampener** — when book is balanced, no
  funding paid. HL charges full premium-driven funding always.
- **Funding-premium TWAP dampener** — per-batch (50ms cadence),
  vs HL's 5s sample / 1h average.
- **Concentration margin tier (FLP-keyed)** — whales post extra
  margin scaled to FLP capital. HL has flat per-asset MMR.
- **Stress-lattice cross-margin** — joint-portfolio shock
  evaluation across all positions. HL uses linear haircut.
- **In-loop liquidations** — same batch that detects unhealthy state
  injects the close. No keeper race. HL has keeper auction.
- **Cross-market basket orders w/ joint margin gate** — single tx,
  N legs, shared stress-lattice gate.
- **Solana-native + MagicBlock ER** — settles to the deepest L1 +
  $0.00025 tx + fastest finality on the planet. HL is its own L1.

**Better than Phoenix:**
- Perpetuals (Phoenix is spot-only)
- Pool-backed quotes (Phoenix is pure CLOB)
- Margining + liquidations (Phoenix has none)
- Funding (Phoenix has none)

**Better than Manifest:**
- Margining + perps (Manifest is spot-only by design)
- Native risk primitives
- Pool-backed depth

**On par with all three on:**
- Mass cancel
- GTT
- Self-trade prevention modes
- Free trading economics (zero protocol fee at tier 4)

## 5. Open questions for the Flash team

These are decisions for the Flash side, not engineering:

1. **Manifest fork status** — is the team's fork public? If yes, do
   we PR back into upstream or maintain divergence?
2. **Governance token timeline** — the fee-tier discount + creator
   share model is wired but inert until a token exists.
3. **HIP-3 launch sequencing** — which markets go first
   (BTC/ETH/SOL first; long-tail later)?
4. **UI plan** — is the chat-driven trade entry the long-term
   primary surface, with the orderbook UI as "Pro mode"? Or do
   we ship a dedicated orderbook page?
5. **Migration economics** — do V2 LPs auto-migrate to FLP for V3,
   or do we run them as separate vaults during the transition?

## 6. Document version

Synthesized 2026-05-10. Sources audited:
- `Bonasa-Tech/manifest` (commit @ time of read)
- `Ellipsis-Labs/phoenix-v1` (master branch)
- `hyperliquid.gitbook.io/hyperliquid-docs/llms-full.txt`
- `@flash_trade/magic-trade-client` v1.0.11 (npm)
- Local `flash-book` (waves 1-16) + `flash-ui` (production)

This document supersedes all prior comparison/architecture docs as the
canonical V3 plan. Refresh after every wave that changes material
architecture.
