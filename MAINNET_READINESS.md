# Flash Book — Mainnet Readiness

Status: **devnet-functional, mainnet-blocked on external audit + ops
items below**. This document is the punch list. Items are categorized
P0 (blocks mainnet), P1 (should ship before launch), P2 (post-launch
tracked).

## Updated punch list (post Wave 24-65 build-out)

| # | Item | P | Status |
|---|---|---|---|
| 0 | **External audit engagement** | P0 | **Not yet started** |
| 1 | Real oracle integration (Pyth) | P0 | ✅ shipped (`update_oracle_from_pyth`) |
| 2 | Authority = multisig (Squads V4) | P0 | Pre-deploy ops; pubkey swap, no code |
| 3 | Mark-price plumbing audit | P0 | ✅ Internal audit passed (`AUDIT.md` §15) |
| 4 | Liquidator reward economics audit | P0 | ✅ Internal audit passed (`AUDIT.md` §7) |
| 4a | Oracle staleness on triggers + TWAP | P0 | ✅ Fixed in Wave 27c (`AUDIT.md` §9.1/9.2/9.3) |
| 4b | `update_market_params` parameter bounds | P0 | ✅ Fixed in Wave 28b (`AUDIT.md` §11) |
| 5 | Test coverage gaps | P1 | ✅ 571 Rust + 236 TS = 807 tests passing |
| 6 | Devnet → mainnet config diff | P1 | See checklist below |
| 7 | Off-chain components (Docker images) | P1 | Pending |
| 8 | Compute-budget validation | P1 | Pending profiling |
| 9 | JIT liquidation auction tooling | P2 | Spec ready, impl pending |
| 10 | Stress benchmark suite | P2 | Pending |
| 11 | Frontend / SDK polish + npm publish | P2 | Pending |
| 12 | Wire-in Wave 25c (settle_funding rewrite) | P2 | Pure module ready |
| 13 | Wire-in Wave 29b-65b (10+ other waves) | P2 | Pure modules ready, see `FEATURES.md` |

---

## P0 — Blocks mainnet

### 1. Real oracle integration
**Status:** `update_oracle` is currently authority-gated and writes raw values trusted from the caller. Acceptable for devnet, **unacceptable for mainnet**.

**Action:** Add `update_oracle_from_pyth(ctx)` ix that:
- Reads a live `PriceUpdateV2` account (Pyth Solana Receiver)
- Validates staleness + confidence on-chain via `pyth_solana_receiver_sdk::price_update::get_price_no_older_than`
- Writes price + confidence + publish_time

**Implementation notes:**
- `pyth-solana-receiver-sdk = "0.6.1"` (latest at the time of writing; verify before depending)
- `MarketParams` needs a `pyth_price_feed_id: [u8; 32]` (the feed ID for the asset). To avoid a third migration, put this on a separate PDA `MarketOracleConfig` rather than expanding `MarketParams`.

**Switch path on mainnet:**
- Devnet: `update_oracle` (trusted) keeps working
- Mainnet: market authority calls `update_oracle_from_pyth` per market; old `update_oracle` is rate-limited or removed by an authority op

### 2. Authority = multisig
**Status:** All authority gates (`market.authority`, `insurance_fund.authority`, fee tier authority, leverage tier authority, market params authority) use a single `Pubkey`. A single-key compromise drains the protocol.

**Action:** Migrate the protocol authority to a Squads V4 multisig before mainnet deploy. The multisig pubkey replaces the single keypair in `initialize_*` ixs.

No code change required — Squads creates a regular Pubkey that the ixs accept. Operational change only.

### 3. Mark-price plumbing audit
**Status:** V3 added `apply_fill` EMA blend + `settle_mark` ix + dual-source health gate. Need to confirm:

- **Race condition**: can a trader place an order whose `apply_fill` updates mark such that an immediately-following ix sees a different health computation? (Likely yes — by design.)
- **Spam**: `settle_mark` is rate-limited via `mark_settle_min_slots` (default 10). For mainnet, raise to 50 (~20s) to reduce noise.
- **EMA params**: `mark_ema_alpha_bps = 2000` (20%) — verify this dampens flash-crash impact appropriately. Backtest against historical volatility.

### 4. Liquidator reward audit
**Status:** `liquidator_reward_bps = 50` is the V3 default. Verify the economics are sustainable — Bob earned 0.43 USDC on a $87 notional close (~0.5%), which the close fill funded. Confirm this never exceeds the close penalty (`liq_penalty_bps = 50`) so the insurance fund isn't drained by liquidator rewards.

---

## P1 — Should ship before launch

### 5. Test coverage gaps
**Status:** 95/95 Rust unit tests + 116/116 SDK tests pass. Missing:
- Multi-position cross-margin liquidation under stress
- Oracle staleness handling (positions through oracle pause)
- ADL chain (insurance < floor → ADL chain → recovery)
- FLP end-to-end (deposit → quote fills → NAV update → withdraw)
- Funding rate accumulation over 24h slot range
- Migration ix idempotency (covered by manual test — automate)

### 6. Devnet → mainnet config diff
Checklist to flip before deploy:
| Config | Devnet | Mainnet |
|---|---|---|
| USDC mint | test mint (per-deploy) | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` |
| Pyth program | n/a (trusted) | `pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT` |
| Oracle staleness max | 30s | 30s (verify with real Pyth feeds) |
| Oracle confidence max | 100 bps | 100 bps (verify) |
| `mark_settle_min_slots` | 10 (~4s) | 50 (~20s) |
| `liquidator_reward_bps` | 50 | 50 (re-verify in P1.4) |
| Authority | single keypair | Squads multisig |
| Insurance fund initial capital | $0 | mainnet seed amount TBD |
| Permissionless markets | removed | removed (kept gated) |
| Fee tier thresholds | $1M / $5M / $25M (quote-lots) | verify against target volume profile |

### 7. Off-chain components
- **Sequencer**: anyone calls `apply_fill` after a CLOB fill. For mainnet, a public Docker image + monitoring. Sequencer earns no fee currently — verify this works economically (caller pays a tx fee but matches their own taker, so they want their fill landed).
- **Liquidation keeper**: anyone calls `liquidate_position_v2`. Bob's 50 bps reward incentivizes a keeper pool. Need a public Docker image + grafana dashboard.
- **Mark settler**: anyone calls `settle_mark`. No reward yet — consider adding a small fee if mark drift > threshold.
- **Funding settler**: who calls `settle_funding`? Currently authority-only. Consider permissionless with rate limit, like `settle_mark`.

### 8. Computational concerns
- `place_taker_order_v2` walks book up to `MAX_BATCH_ORDERS_PER_SIDE_V2`. Verify this is high enough that legitimate takers don't bin against it.
- `liquidate_position_v2` now walks JIT offers in `remaining_accounts` before falling back to synthetic close. Verify compute budget under N=20 JIT offers + a fill.
- BPF heap usage during liquidation — does the JIT walker allocate Vec<>? Inspect.
- BPF program size: 1.86 MB. Solana limit is ~10 MB. Headroom is fine.

---

## P2 — Post-launch tracked

### 9. JIT liquidation auctions UX
JIT offers exist on-chain (`place_jit_liquidation_offer`) but no off-chain tooling yet. Build:
- Frontend for makers to post JIT offers
- Indexer that streams active offers for the liquidator pool to see
- Analytics: avg savings vs synthetic, JIT-vs-synthetic fill ratio

### 10. Stress benchmark
Goal: validate 50 makers + 50 takers + 10 keepers can run for 1 hour without:
- Orderbook corruption
- Fee accounting drift
- Insurance fund unexplained moves
- Mark price disconnection from oracle > drift_alert_bps for > 5 consecutive slots

### 11. Frontend / SDK polish
- npm publish prep for `@flash-book/sdk`
- React hooks for common ops (`useOrderbook`, `usePosition`, `useFundingRate`)
- Risk preview UI integration

### 12. Audits
- Static analysis: cargo-geiger, cargo-audit, semgrep rules for Solana programs
- Symbolic execution: Halmos / Mythril-style on critical paths (margin gate, fee math, ADL gate)
- External audit: Neodyme / Trail of Bits / Halborn — budget item

### 13. Documentation
- ARCHITECTURE.md exists ✓
- Sequence diagrams: trade lifecycle, liquidation flow, FLP fill flow
- SDK migration guide for partners
- Sequencer/keeper operator guide

---

## What's already shipped (V3)

| Item | Status |
|---|---|
| CLOB-only protocol (FBA removed) | ✅ |
| Monolithic program (4 → 1) | ✅ |
| Permissionless markets removed (authority-gated) | ✅ |
| 3 mark-update paths | ✅ |
| Dual-source health gate | ✅ |
| `settle_mark` ix | ✅ |
| `apply_fill` EMA mark update | ✅ |
| JIT liquidation auctions (on-chain) | ✅ |
| `liquidate_position_v2` walks JIT offers | ✅ |
| Liquidator reward = 50 bps | ✅ |
| V3 market migration ix | ✅ proven on devnet |
| MarkPriceDriftEvent | ✅ |
| MarketMigratedToV3Event | ✅ |
| pre-liquidation preview SDK helper | ✅ |

## What's NOT shipped that we noted

- Real Pyth integration (P0.1)
- Multisig authority migration (P0.2 — operational)
- Off-chain Docker images (P1.7)
- Stress benchmark (P2.10)
- External audit (P2.12)
