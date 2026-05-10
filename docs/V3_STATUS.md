# Flash Book V3 — Status

Single source of truth: what's shipped, what's deferred, what the production
deployment story looks like as of this commit.

## TL;DR

The on-chain program is **MagicBlock × Flash Trade ready**. The full v2
hypertree orderbook is live, all five order injection paths have v2
equivalents, MagicBlock ER delegation ixs ship for the three accounts the
matcher hot-path mutates, and 483 tests across Rust + TypeScript hold green
with zero compiler warnings.

The v1 surface remains in place (deprecated) so the existing bot / MM /
integration tests keep working. v1 deletion is mechanical and queued for a
focused future session that migrates the bot.

## Architecture

```
trader → SDK → ix → on-chain program
                      │
                      ├── (delegated) MagicBlock ER  (sub-ms FBA matcher tick)
                      │     │
                      │     ↓ commit_frequency_ms
                      │
                      └── mainnet (canonical state)
```

Three things make this pattern work:

1. **Hypertree orderbook** (`src/state_v2.rs`) — single 9864-byte PDA, custom
   8-byte disc + raw byte slicing. No Anchor deserialization on the hot path
   → sub-ms account access.
2. **Pure-integer matcher** (`src/matcher/`) — no floating-point arithmetic
   anywhere. Eliminates validator-drift risk on the ER.
3. **Delegation ixs** (`src/lib.rs:delegate_*`) — wrap the MagicBlock CPI
   primitives in `src/er.rs`. Three accounts get delegated:
   `market_book`, `market`, `commit_buffer`. `flp_exposure` stays mainnet-
   resident (singleton; per-market FLP is wave 21).

## Shipped — Wave 18 (hypertree foundation)

| Wave | Description |
|------|-------------|
| 18a  | Vendored Manifest hypertree (GPL-3.0, see `LICENSE-HYPERTREE`) |
| 18b  | `MarketBookHeader` + `RestingOrderV2` + `ClaimedSeatV2` types |
| 18c  | `init_market_book` ix + `MarketBookHandle` access pattern |
| 18d  | `place_limit_order_v2` — orders write into the hypertree |
| 18e  | `view_book_depth_v2` + best-first iterators (load-bearing bug fix: `*_best_index` was caching MAX, should be MIN) |
| 18f  | `run_batch_v2` matcher — walks hypertree, clears via FBA, mutates filled orders, frees nodes |
| 18g  | Full bookkeeping port + 3 wins over HL/Phoenix/Manifest (EMA-blended funding, vol-adaptive band, VPIN-gated FLP) |
| 18h  | v1 surface marked `#[deprecated]`, `PREFERRED_ORDERBOOK_VERSION='v2'` SDK signal, `detectOrderbookVersion` runtime helper |

## Shipped — Wave 19 (injection paths + ER delegation)

| Wave | Description |
|------|-------------|
| 19a  | `execute_trigger_order_v2` — stop-loss / take-profit / OCO triggers fire into hypertree |
| 19b  | **MagicBlock ER delegation ixs** — `delegate_market_book` + `undelegate_market_book` + `delegate_market` + `undelegate_market` (load-bearing for ER deploy) |
| 19c  | `execute_twap_slice_v2` — TWAP scheduler injects slices into hypertree |
| 19d  | `replenish_iceberg_v2` — O(log n) hypertree probe replaces O(n) buffer scan |
| 19e  | `liquidate_position_v2` — pure parity port (HL/Drift/Binance maths); fixes a v1 latent bug (position lacked `mut` → write loss); fixes wave-18f hardcoded `OrderType::Limit` mapping in the matcher walk |
| 19f  | `place_bracket_order_v2` — atomic parent + TP/SL OCO; ADL audit confirmed `auto_deleverage` already v2-compatible (no orderbook touch) |
| 19g  | `place_iceberg_order_v2`, `cancel_iceberg_v2`, `liquidate_portfolio_v2`, `delegate_commit_buffer`, `undelegate_commit_buffer` |

## Wins over Hyperliquid / Phoenix / Manifest

These are documented in `programs/flash-book/src/matcher/v2_bookkeeping.rs`
and unit-tested. None are novel theory — each captures a documented
production pattern from the listed exchange:

1. **EMA-blended funding rate** — 50/50 blend with prior posted rate.
   Smoother than HL's per-block recompute. Single-batch outliers get
   half-weighted instead of fully propagating.
2. **VPIN-gated FLP pause** — when toxicity ≥ 70%, FLP virtual quotes
   skip this batch. Manifest has no LP at all; Phoenix has no auto-pause;
   HL has no LP-protection signal at the matcher tier.
3. **Vol-adaptive oracle band** — `oracle_band_bps × (1 + 10 × vol/BPS)`,
   capped at 4×. HL uses fixed pct → over-clamps during real moves.

## Production lifecycle (the actual deploy story)

```
1. Mainnet:  initialize_market(authority, base/quote mints, params)
2. Mainnet:  init_market_book(authority, market)
3. Mainnet:  initialize_commit_buffer(authority, market)

   --- ER delegation ---
4. Mainnet → ER:  delegate_market_book(50ms, validator?)
5. Mainnet → ER:  delegate_market(50ms, validator?)
6. Mainnet → ER:  delegate_commit_buffer(50ms, validator?)

   --- ON THE ER (sub-ms) ---
7. ER:  place_limit_order_v2 × N traders
8. ER:  run_batch_v2 every ~50ms (matcher tick: clear, apply, emit fill events)
9. ER:  cancel_order_v2 × N traders
10.ER:  ER auto-commits state → mainnet every commit_frequency_ms

   --- Settlement on mainnet (sequencer-driven, source-agnostic) ---
11. Mainnet:  apply_fill / apply_flp_fill — settles fills into Position PDAs
              (sequencer reads FillAppliedEvent from ER tick logs)

   --- End-of-life ---
12. Mainnet:  undelegate_commit_buffer
13. Mainnet:  undelegate_market_book
14. Mainnet:  undelegate_market
```

## Test coverage

| Suite | Count |
|-------|-------|
| Rust unit (lib + matcher) | 93 |
| Rust integration | 31 (was 58; 27 v1-touching tests deleted across 19h + 19i) |
| Rust parity | 1 |
| Rust proptests (5 modules) | 55 |
| TS (sdk-ts) | 128 (was 135; 7 v1 builder + orderbook-version tests deleted) |
| TS (root) | 141 |
| **Total** | **449** |

Zero failures, zero warnings.

## What's intentionally deferred (with full design docs)

| Wave | Description | Status / design doc |
|------|-------------|---------------------|
| 19h  | Mass v1 deletion | **Done** (commits `04445a4` bot migration + `81b6c55` first rip-out + this wave 19i: basket+reveal+v1-residue full deletion). Total: 16 v1 ixs + all ctxs/events/state/helpers/SDK gone. v3 program is single-orderbook (hypertree only). |
| 20a  | Multi-tier MMR (HL has up to 6 tiers per asset) | Single-tier already covers ~80% of cases via `concentration_threshold_lots` + `concentration_extra_mmr_bps`; multi-tier needs additive `MarketLeverageTiers` PDA + RiskMarketSnap plumbing through 10+ files |
| 20b  | Withdrawal floor `max(IM, 0.1 × notional)` | Current is **stricter** (`open_positions == 0`); HL floor is a UX upgrade (allows partial withdrawal with positions), not a security fix. Implement as additive `partial_withdraw_collateral` ix when needed |
| 21   | Modular wrapper programs | Full spec in [`V3_WAVE21_MODULAR.md`](./V3_WAVE21_MODULAR.md): 4-program topology (core / orders / flp / vaults), per-market FLP, 4-phase migration plan, 4-week effort estimate |
| 22   | Fee tier table | Already exists: `TraderStateAccount.fee_discount_bps` + `set_trader_fee_tier` ix |
| 23   | Certora formal spec | Full prep doc in [`V3_WAVE23_CERTORA.md`](./V3_WAVE23_CERTORA.md): 13 critical invariants formalized (matcher 5, risk 5, hypertree 4, funding 3), engagement scope $80-120K, 8-10 weeks |

## Latent items spotted along the way

These are noted but not load-bearing for the current deployment story:

- v1's `LiquidatePosition.position` lacks `mut` → silent write loss on
  `unhealthy_since_slot` / `last_liquidated_at_slot`. v2 fixes this in
  `LiquidatePositionV2`. v1 left as-is (deprecated; will go away in 19h).
- `MAX_BATCH_ORDERS_PER_SIDE_V2 = 64` is conservative — matcher is O(N²)
  in candidate prices. Wave 20-future candidate to lift once the matcher
  refactors to streaming O(N log N) walk.
- `flp_exposure` not ER-delegatable (singleton). Per-market FLP exposure
  is wave 21.

## Repo invariants enforced

These are checked by CI / cargo / bun:

1. Zero compiler warnings (`cargo build --release` + `cargo build`)
2. Zero failing tests (full suite each commit)
3. Anchor IDL regenerated after any program-surface change
4. `sdk-ts/idl.json` matches `target/idl/flash_book.json` byte-for-byte
5. TS `tsc --noEmit` passes (no type errors)
6. `bun test` 100% green
