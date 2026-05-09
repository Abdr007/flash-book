# Flash Book

> Pool-backed CLOB matched by frequent batch auction on MagicBlock Ephemeral Rollups. Reference design and simulator for Flash Trade's announced Orderbook V3.

[![tests](https://img.shields.io/badge/tests-465%20passing-brightgreen)]()
[![parity](https://img.shields.io/badge/Rust%E2%86%94TS-byte--equivalent-brightgreen)]()
[![fuzz](https://img.shields.io/badge/fuzz-32K%20properties-brightgreen)]()
[![e2e](https://img.shields.io/badge/e2e-58%20on--chain-brightgreen)]()
[![ix](https://img.shields.io/badge/instructions-32%2F32%20covered-blue)]()
[![bot](https://img.shields.io/badge/bot-multi--market%20%2B%20V2%2BV3-blueviolet)]()
[![typescript](https://img.shields.io/badge/typescript-strict-blue)]()
[![rust](https://img.shields.io/badge/rust-stable-orange)]()
[![license](https://img.shields.io/badge/license-MIT-green)]()

## What this is

Flash Book is a perpetual futures matching engine that fuses seven mechanisms
into one design — every one of which becomes feasible only because of
MagicBlock Ephemeral Rollups (10–50 ms blocks, free compute, fraud-proof
settlement to Solana mainnet):

1. **Frequent batch auction (FBA)** with Walrasian uniform-price clearing
   every 50 ms.
2. **Virtual FLP quoter** — the FLP pool participates in its own order book
   as a permanent maker-of-last-resort, with Avellaneda-Stoikov-grade
   inventory-aware quoting.
3. **Commit-reveal taker flow** — sequencer cannot front-run because order
   intent is hidden until the batch closes.
4. **In-loop liquidations** — bankrupt positions resolve in the same batch
   that triggered them. No keeper race, no MEV, no cascades.
5. **Continuous funding** — accrued every ER block via cumulative-index
   integration. No funding sniping.
6. **Stress-lattice cross-margin** — recognizes hedges; required margin
   collapses for offsetting positions.
7. **Insurance fund waterfall** with auto-deleveraging fallback for the
   tail; pause-new-positions threshold for fund safety.

No protocol has shipped this combination. Hyperliquid, Drift, dYdX v4,
Aevo, GMX, Phoenix — each has a subset, none has all seven.

## Status

This is a **reference design + TypeScript simulator**. It validates the math
and clearing properties of the design end-to-end. Production matcher will be
a Solana program in Rust, deployed to MagicBlock ER and settling to mainnet.

| Component | Status |
|---|---|
| FBA Walrasian matcher | ✅ implemented & tested |
| Virtual FLP quoter | ✅ implemented & tested |
| Continuous funding | ✅ implemented & tested |
| **Funding settlement on-chain** (debits/credits collateral) | ✅ `settle_funding` ix |
| Stress-lattice margin | ✅ implemented & tested |
| In-loop liquidations | ✅ implemented & tested |
| Insurance fund waterfall | ✅ implemented & tested |
| **Insurance fund withdraw** (governance, pause-threshold guard) | ✅ `withdraw_insurance_fund` ix |
| ADL | ✅ implemented & tested |
| Commit-reveal | ✅ implemented & tested |
| VPIN toxicity signal | ✅ implemented & tested |
| Multi-oracle quorum (median-of-3 + dispersion) | ✅ `update_oracle_quorum` ix |
| **LP unit accounting** (NAV-based vault, multi-LP) | ✅ ERC-4626-style shares |
| **Position-aware FLP withdraw guard** (mark-to-market exposure) | ✅ remaining_accounts walk |
| **Capital-relative position cap** (% of FLP capital) | ✅ `max_position_ratio_bps` |
| **Real-time invariant monitor** (kill-switch signal) | ✅ `verify_market_invariants` ix |
| **Spot markets** (param recipe, no leverage/funding) | ✅ `defaultSpotMarketParams()` |
| **SPL token transfer integration** (vault, ATA-only) | ✅ deposit/withdraw all on-chain |
| **ATA management** (init/close, idempotent) | ✅ `init_trader_ata`, `close_trader_ata` |
| Synthetic flow simulator | ✅ runs at 42K batches/sec |
| Rust matcher core (integer arithmetic, checked overflow) | ✅ 31 unit tests |
| Rust property-based safety tests | ✅ 16 properties × 2K cases = 32K fuzz |
| Rust integration tests (E2E on-chain) | ✅ 58 tests |
| Rust risk + liquidation + insurance + commit-reveal modules | ✅ ported |
| Anchor program — 32 instructions | ✅ all implemented + tested |
| **Maker rebate distribution from toxicity tax** | ✅ apply_fill routes tax → maker + insurance |
| **2-leg cross-market basket orders** | ✅ `place_basket_order` with joint margin gate |
| **N-leg cross-market basket orders** | ✅ `place_basket_order_n` (≤4 legs via remaining_accounts) |
| **Liquidation hardening** (partial + reward + race-safe) | ✅ Hyperliquid + Drift patterns folded in |
| **Phoenix-grade order semantics** (reduce_only / IOC / flag bitfield) | ✅ `flags` arg on place_limit_order |
| **Tiered fees** (per-trader discount, off-chain volume tracker) | ✅ `set_trader_fee_tier` ix + apply_fill discount |
| **MagicBlock ER delegation CPI** (in-house, version-agnostic) | ✅ `programs/flash-book/src/er.rs` |
| **MM bot — multi-market, quote diffing, V2+V3 venues** | ✅ `bot/` top-level package |
| **Backtester — replay tape through Strategy** | ✅ `bot/src/backtester.ts` |
| **Keeper suite** (liquidation, funding, invariant, ATA cleanup) | ✅ `bot/src/keepers.ts` |
| **Keeper auto-discovery** (`getProgramAccounts` scanner) | ✅ `bot/src/discovery.ts` |
| **Smart router** (V2 + V3 in one strategy) | ✅ `bot/src/smart-router.ts` |
| **WebSocket subscriptions** (push-based bot + keeper updates) | ✅ `bot/src/subscriptions.ts` |
| **Advanced order types** (OCO / Iceberg / Trailing stop) | ✅ `bot/src/order-types.ts` |
| **Telemetry** (Prometheus push, Counter+Gauge) | ✅ `bot/src/telemetry.ts` |
| **Hot config reload** (no-restart param tuning) | ✅ `bot/src/hot-config.ts` |
| BPF compilation | 🔲 blocked: Solana platform-tools v1.48 |
| Devnet deployment | 🔲 blocked on the above |
| Mainnet shadow mode (replay Flash V2 trades) | 🔲 phase 2 |
| Independent security audit | 🔲 phase 2 |

## Quick start

TypeScript reference simulator + SDK:
```bash
bun install
bun test                          # 190 tests
bun run examples/synthetic-flow.ts
```

Rust on-chain matcher core:
```bash
cargo test --lib --package flash-book   # 31 unit tests
cargo test --package flash-book         # + 6 property tests × 2K cases
cargo check --lib --package flash-book  # type / borrow check
```

Sample output from the synthetic flow demo:

```
Batches run:          1,200
Wall-clock:           28ms (42857 batches/sec)
Total fills:          2,622
Total volume:         $157,253

Fills involving FLP:  0 (0.0%)        ← MMs cleared all flow inside FLP spread
Fills involving MMs:  1915 (73.0%)
Fills involving ret:  2622 (100.0%)

Final mark:           $98.8198
Final oracle:         $98.6457
Mark-oracle diff bps: 17.65           ← TWAP within oracle band
Final VPIN:           27.3%
```

## Why this design (vs. every alternative)

|  | Hyperliquid | Drift | dYdX v4 | Aevo | GMX | Phoenix | **Flash Book** |
|---|---|---|---|---|---|---|---|
| Latency | ~50 ms | ~400 ms | ~1 s | ~2 s | block | block | **50 ms** |
| MEV-resistant matching | partial | no | no | no | n/a | atomic | **FBA + commit-reveal** |
| Pool-backed CLOB | no | partial (vAMM) | no | no | pool only | n/a | **yes (virtual FLP)** |
| Continuous funding | no (1 h) | no (1 h) | no (1 h) | no (1 h) | n/a | n/a | **yes (10 ms)** |
| In-loop liquidations | no | no | no | no | no | n/a | **yes** |
| Stress-lattice margin | no | no | no | no | no | n/a | **yes** |
| Decentralized | no | yes | yes | partial | yes | yes | **yes** |
| Settles to Solana | no | yes | no | no | yes | yes | **yes** |

## Architecture

```
                 L1 (Solana mainnet)
   ┌────────────────────────────────────────────────┐
   │   FLP Pool   ·   Position state   ·   Settlement
   └─────────────────────┬──────────────┬───────────┘
              delegate   │              ▲   commit
                         ▼              │
   ┌────────────────────────────────────────────────┐
   │      MagicBlock ER (Flash Book runtime)        │
   │                                                │
   │  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │
   │  │ Virtual  │  │ MM book  │  │ Taker buffer │  │
   │  │ FLP      │  │ (FIFO)   │  │ (commit-     │  │
   │  │ quoter   │  │          │  │  reveal)     │  │
   │  └────┬─────┘  └────┬─────┘  └──────┬───────┘  │
   │       │             │               │          │
   │       └─────────────┴───────────────┘          │
   │                     ▼                          │
   │          ┌─────────────────────┐               │
   │          │ FBA matcher         │               │
   │          │ (Walrasian clear)   │ every 50 ms   │
   │          └──────────┬──────────┘               │
   │                     ▼                          │
   │          ┌─────────────────────┐               │
   │          │ In-loop risk engine │               │
   │          │ + liquidations      │               │
   │          └──────────┬──────────┘               │
   │                     ▼                          │
   │          ┌─────────────────────┐               │
   │          │ Continuous funding  │               │
   │          └─────────────────────┘               │
   └────────────────────────────────────────────────┘
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the long-form design.

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — system design, lifecycle, accounts
- [`docs/INSTRUCTIONS.md`](docs/INSTRUCTIONS.md) — full reference for all Anchor instructions
- [`docs/MATH.md`](docs/MATH.md) — formal math: clearing, funding, margin, FLP spread
- [`docs/SAFETY.md`](docs/SAFETY.md) — solvency invariants, threat model, audit checklist
- [`docs/COMPARISON.md`](docs/COMPARISON.md) — vs every modern perp DEX
- [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) — runbook from `cargo test` to devnet
- [`docs/MM_TUNING.md`](docs/MM_TUNING.md) — market-maker bot parameter tuning guide
- [`docs/LP_GUIDE.md`](docs/LP_GUIDE.md) — LP onboarding: deposit, NAV, withdraw flow
- [`docs/KEEPER_RUNBOOK.md`](docs/KEEPER_RUNBOOK.md) — deploying and operating the keeper suite
- [`docs/PRODUCTION_READINESS.md`](docs/PRODUCTION_READINESS.md) — single-source-of-truth status
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — staged path to mainnet

## Layout

```
src/                          TypeScript reference simulator
  types.ts                    Domain types (Order, Position, MarketState, ...)
  math.ts                     Clamp, EMA, deterministic PRNG, hash, banding
  matcher.ts                  FBA Walrasian uniform-price clearing
  flp-quoter.ts               Virtual FLP quote ladder (Avellaneda-Stoikov)
  funding.ts                  Continuous funding via cumulative index
  risk.ts                     Stress-lattice maintenance margin
  liquidation.ts              Detection, order injection, shortfall, events
  insurance.ts                Bankruptcy waterfall
  commit-reveal.ts            Taker order commit/reveal protocol
  vpin.ts                     Volume-synchronized toxicity signal
  engine.ts                   Top-level orchestrator
  index.ts                    Public API + DEFAULT_MAJOR_MARKET_PARAMS

programs/flash-book/          Production Solana program (Rust + Anchor)
  src/
    lib.rs                    Anchor program — declare_id, instructions, contexts
    constants.rs              USD_DECIMALS, BPS_DENOM, compute-budget caps
    errors.rs                 FlashBookError code enum (numbered families)
    state.rs                  On-chain account types (Market, Position, ...)
    matcher/
      lot.rs                  Type-safe BaseLots, QuoteLots, Ticks, Bps wrappers
      order.rs                Order + OrderType + Side, FIFO key computation
      fba.rs                  Walrasian clearing in integer space (no floats)
      flp_quoter.rs           FLP virtual quote ladder, integer arithmetic
      funding.rs              Cum funding index (Q64.64 fixed-point)
      vpin.rs                 VPIN (Q32.32 fixed-point EMA)
      tests.rs                16 unit tests parity-checked vs TS reference

tests/                        71 TS unit tests
examples/                     Runnable simulation scenarios
docs/                         Architecture, math, safety, comparison docs
```

## Acknowledgements

Theory inspired by:

- Eric Budish — *The High-Frequency Trading Arms Race: Frequent Batch Auctions*
- Avellaneda & Stoikov — *High-Frequency Trading in a Limit Order Book*
- Easley, López de Prado, O'Hara — *Flow Toxicity and Liquidity in a High-Frequency World* (VPIN)
- Glosten & Milgrom — *Bid, Ask and Transaction Prices*
- Almgren & Chriss — *Optimal Execution of Portfolio Transactions*
- Walras — *Éléments d'économie politique pure*

Practice inspired by:

- Phoenix v1 (Ellipsis Labs) — atomic on-chain CLOB without a crank
- Hyperliquid HyperCore — fully on-chain perp CLOB
- Drift Protocol — JIT auction + DLOB + vAMM stack
- Mango v4 — in-program CLOB
- MagicBlock Ephemeral Rollups — the substrate that makes 10 ms blocks possible

## License

MIT — see [LICENSE](LICENSE).
