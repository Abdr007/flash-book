# HLP-Equivalent Backstop Vault — Scope & Design

The "honest weaknesses" section of `docs/COMPARISON.md` calls out
that Flash Book has no equivalent of Hyperliquid's HLP — a dedicated
always-on liquidator vault that absorbs underwater positions at the
bankruptcy price. The JIT-liquidation auction primitive
(`place_jit_liquidation_offer`) is the closest analogue but
opportunistic, not backstopping.

This document is the scope-discovery artifact for closing that gap.
Same pattern as `SUB_ACCOUNT_TRADING.md`, `FBA_ON_CHAIN.md`,
`COMMIT_REVEAL_ON_CHAIN.md`.

## 0. Status

**Not started.** The FLP pool exists (`FlpExposureAccount`,
`apply_flp_fill`, the LP-units share accounting) but the FLP is a
counterparty for ordinary fills, not a forced-buyer for underwater
positions. Liquidations today inject a synthetic close into the
hypertree and rely on:

1. The JIT-offer pool (if a maker has pre-committed a tighter
   close-price), or
2. Open-market liquidity at the synthetic price (oracle ±
   `liq_penalty_bps`).

If neither fires within a slot window, the underwater position
remains open. In normal conditions a competitive keeper pool fills
the gap (the synthetic price is discounted vs oracle, so it's
profitable). In a tail event the gap can persist long enough to put
the insurance fund under pressure.

## 1. Two ways to ship the primitive

### Option A — extend the existing FLP (recommended)

Reuse `FlpExposureAccount` + per-market `FlpMarketExposure` + the
LP-units share accounting that already exists. Add ONE new ix
`force_close_into_flp` that an open keeper can call when:

1. The underwater position is unhealthy (same gate as
   `liquidate_position_v2`).
2. The synthetic close order has been resting on the book for ≥
   `flp_backstop_grace_slots` without filling.
3. The FLP has enough remaining capital (NAV) to take the position
   without breaching `max_position_ratio_bps`.

The handler then:

- Computes the close price (oracle ± `liq_penalty_bps`, no JIT
  improvement at this stage since JIT had its chance during the
  grace window).
- Closes the underwater position (reduces `position.size_lots` to
  0; debits collateral via the existing `route_adl_loss` /
  `apply_realized_pnl_delta` per-bucket routing).
- Opens the OPPOSITE side on the FLP's `FlpMarketExposure` per-
  market entry. The existing `apply_fill_to_flp_market` helper
  handles this.
- Removes the synthetic close from the hypertree (it's served).
- Emits a `BackstopExecutedEvent` so off-chain ops can audit.

This adds **one** new ix + one new event + one new MarketParams
field. Estimated 300-400 LOC. Reuses every existing accounting
primitive.

### Option B — dedicated `BackstopVault` family

Separate `BackstopVaultAccount` at `[b"backstop_vault"]`, separate
LP shares, separate per-market exposure family, separate deposit /
withdraw / NAV ixs. The vault's capital backs only liquidations.

Cleaner separation but ~1500-2000 LOC of new state + ixs +
accounting + LP onboarding flows. Useful if you want backstop LPs
to be a different risk class (higher yield, higher risk) than
ordinary FLP LPs.

**Recommendation: Option A** for v0.5.0. Option B as a possible
v0.6.0 if the LP-class differentiation matters in practice. The
risk-isolation argument for B is weak today because FLP already
takes liquidation-adjacent flow (any synthetic close that an
ordinary FLP-as-maker fill clears).

## 2. Option A — detailed design

### 2.1 New `MarketParams` field

```rust
// inside MarketParams (state.rs)
/// Phase 3 backstop — number of slots a synthetic close order is
/// allowed to rest in the hypertree without filling before
/// `force_close_into_flp` becomes admissible. `0` disables the
/// backstop for this market (legacy behaviour). Recommended
/// production value: 5-10 slots (~2-4 seconds at Solana cadence).
pub flp_backstop_grace_slots: u32,
```

**Layout caveat — read carefully.** `MarketParams` is embedded inside
`MarketAccount.params`, not a top-level Anchor account with
allocation headroom. Borsh-derived structs serialize fields
sequentially with no padding, so adding a field to `MarketParams`
changes the byte length of the serialized `MarketAccount`. Existing
on-chain `MarketAccount`s init'd at the pre-Phase-3 size would read
past their actual data into the next field's bytes — undefined
behaviour.

Mitigation paths:

1. **Field grouping reuse.** If `MarketParams` already has a
   reserved-padding field, the new field can be carved out of it.
   Audit `state.rs` for unused `_pad` / `_reserved` u32s; if one
   exists, use it. (As of v0.2.0 there isn't one in `MarketParams`,
   so this option isn't available.)
2. **MarketAccount version field + migration ix.** Add a `version:
   u8` to MarketAccount itself, init existing markets as v0, set
   the new param's default to 0 when reading a v0 market. Phase 3
   markets are init'd at v1 with the field populated. This is the
   recommended pattern.
3. **Side-car account.** A new `MarketBackstopConfig` PDA at
   `[b"market_backstop", market.key()]` carrying the grace slots
   + max-exposure fields. Init'd alongside the market. No
   `MarketParams` changes; cleanest separation.

**Recommendation: option 3** for v0.5.0 to avoid touching the
core MarketParams layout. Adds one PDA per market but keeps the
risk surface contained to new code.

### 2.2 New `FlpExposureAccount` field

```rust
// inside FlpExposureAccount
/// Phase 3 backstop — capacity ceiling. Force-closes are admissible
/// only when the FLP's gross exposure post-take stays under this
/// limit. Defaults to `FlpExposureAccount.total_capital_quote_lots /
/// max_backstop_utilisation_bps` if 0.
pub max_backstop_exposure_quote_lots: u64,
```

### 2.3 New ix

```rust
/// Phase 3 backstop — force an underwater position to close into
/// the FLP at the synthetic price (oracle ± liq_penalty_bps) after
/// the grace window has elapsed without an open-market or JIT
/// counterparty filling the synthetic close.
///
/// Permissionless. Caller pays tx fee.
///
/// Eligibility (all enforced on-chain):
///   1. Position is unhealthy via the standard dual-source price
///      gate (same as liquidate_position_v2).
///   2. Synthetic close order has been resting at least
///      `flp_backstop_grace_slots` ≥ slot offset from
///      `position.unhealthy_since_slot`.
///   3. FLP per-market exposure post-take stays under
///      max_backstop_exposure_quote_lots.
///
/// Action:
///   - Computes close price (oracle ± liq_penalty_bps).
///   - Drains the synthetic close from the hypertree.
///   - Closes underwater position to size 0 via
///     apply_fill_to_position (taker side = close_side; maker = FLP).
///   - Mutates FLP per-market exposure via apply_fill_to_flp_market.
///   - Routes the underwater PnL via the existing Phase 2g/2h
///     per-bucket helpers (isolated → position.collateral_quote_lots;
///     cross → trader_state.collateral_quote_lots).
///   - Emits BackstopExecutedEvent.
pub fn force_close_into_flp(
    ctx: Context<ForceCloseIntoFlp>,
    synthetic_close_seq: u64,    // the hypertree node to drain
) -> Result<()> { ... }
```

### 2.4 New Accounts struct

```rust
#[derive(Accounts)]
pub struct ForceCloseIntoFlp<'info> {
    pub caller: Signer<'info>,
    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,
    #[account(mut, seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()], bump)]
    pub market_book: UncheckedAccount<'info>,
    #[account(mut, seeds = [InsuranceFundAccount::SEED], bump = insurance_fund.bump)]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,
    #[account(mut)]
    pub trader_state: Box<Account<'info, TraderStateAccount>>,
    #[account(mut, seeds = [state::PositionAccount::SEED,
                            market.key().as_ref(),
                            trader_state.key().as_ref()],
              bump = position.bump)]
    pub position: Box<Account<'info, state::PositionAccount>>,
    #[account(mut, seeds = [FlpExposureAccount::SEED], bump = flp_exposure.bump)]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,
}
```

### 2.5 New event

```rust
#[event]
pub struct BackstopExecutedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub executor: Pubkey,
    pub close_size_lots: u64,
    pub close_price_ticks: u64,
    pub grace_slots_waited: u64,
    pub flp_exposure_post_take_quote_lots: u64,
}
```

## 3. Composition with existing primitives

The backstop sits AFTER the existing liquidation pipeline:

```
Position becomes unhealthy
    │
    ▼
Keeper calls liquidate_position_v2
    │
    ├─ Health gate (Phase 2 dual-source price) → passes
    ├─ JIT auction (place_jit_liquidation_offer pool) → finds JIT bid?
    │       yes: fills at JIT price → DONE
    │       no: ↓
    ├─ Synthetic close injected into hypertree at
    │       oracle ± liq_penalty_bps with order_type = 3 (Liquidation)
    │       priority
    │
    ▼
Synthetic close rests in book. Market makers (or FLP via standard
fills) consume it at the discounted price.
    │
    ├─ Fills within flp_backstop_grace_slots → DONE
    │
    └─ Doesn't fill within grace window
            │
            ▼
        Phase 3 force_close_into_flp ix becomes admissible.
        Keeper calls it.
            │
            ├─ Same health re-check → still unhealthy?
            │       no: reject (position recovered).
            ├─ Grace slots elapsed?
            │       no: reject (too early).
            ├─ FLP capacity available?
            │       no: reject (escalate to ADL via auto_deleverage).
            │
            ▼
        FLP takes the position at synthetic price.
        Trader's position closes.
        FLP's FlpMarketExposure entry grows on the opposite side.
        BackstopExecutedEvent emitted.
```

ADL (`auto_deleverage`) remains the absolute last resort when the
insurance fund is exhausted AND the FLP has reached its backstop
capacity ceiling.

## 4. NAV semantics for LPs

The existing `LpPositionAccount` shares mechanism already handles
NAV walks across `FlpMarketExposure` per-market entries. When the
backstop takes a position, the FLP's per-market exposure changes,
which immediately changes NAV for everyone holding LP shares.

LPs accept the risk by depositing — same as today. The only thing
that changes is that the FLP's exposure can grow more abruptly via
the backstop path than via the steady fill flow. LPs read
`max_backstop_exposure_quote_lots` and the recent
`BackstopExecutedEvent` log to size their exposure.

## 5. Force-close vs ADL — when each fires

| Condition | Fire |
|---|---|
| Position unhealthy, JIT offer exists at better-than-synthetic price | Standard liquidation (existing) |
| Position unhealthy, no JIT, synthetic close fills within grace | Standard liquidation via open-market take (existing) |
| Position unhealthy, synthetic close doesn't fill, FLP has capacity | **`force_close_into_flp` (new Phase 3)** |
| Position unhealthy, FLP exhausted, insurance fund OK | Synthetic close keeps resting + keeper retries; ADL not yet triggered |
| Position unhealthy, FLP exhausted, insurance fund < pause threshold | `auto_deleverage` (existing) |

The backstop covers the gap between "synthetic close didn't fill
fast enough" and "insurance fund is genuinely under threat." ADL
remains for the latter — the tail tail-event.

## 6. Effort estimate

| Slice | LOC | Notes |
|---|---|---|
| MarketParams.flp_backstop_grace_slots + FlpExposureAccount.max_backstop_exposure_quote_lots | ~30 | Trailing fields, layout-compatible |
| ForceCloseIntoFlp Accounts struct | ~80 | Mirror of LiquidatePositionV2 surface |
| force_close_into_flp handler | ~250 | Eligibility checks + synthetic-close drain + apply_fill_to_position + apply_fill_to_flp_market + per-bucket PnL routing |
| Hypertree node drain helper | ~50 | Read + remove the synthetic-close node by its (limit, seq) order_id |
| BackstopExecutedEvent + emission | ~30 | |
| Proptests (3 properties × 2000 cases) | ~200 | (a) backstop only fires when grace elapsed (b) FLP capacity respected (c) PnL conservation across backstop |
| Integration tests (3 scenarios) | ~400 | Happy path, capacity-rejected, too-early-rejected |
| SDK builder + keeper code in bot/ | ~150 | forceCloseIntoFlpIx + a BackstopKeeper that watches for stale synthetic closes |
| Docs + MARGIN_MATH update | ~100 | New §X for backstop math; update §9 invariants with I-9 (backstop conserves PnL) |
| **Total** | **~1,290** | |

Best across 3-4 commits:

1. State field additions + Accounts struct
2. Handler + hypertree drain helper + PnL routing wiring
3. Proptests + integration tests
4. SDK + keeper + docs

## 7. Properties for proptest

```
tests/proptest_backstop.rs
  1. grace_window_enforced — backstop ix rejects when
     current_slot < position.unhealthy_since_slot + grace_slots.
  2. capacity_enforced — backstop ix rejects when post-take FLP
     exposure exceeds max_backstop_exposure_quote_lots.
  3. pnl_conservation — sum of (trader collateral change + FLP
     exposure cost) == size × (entry - close_price) × tick + fees +
     insurance fund delta.
  4. bucket_isolation — for an isolated underwater position,
     backstop debits position.collateral_quote_lots, NOT the cross
     pool. (Phase 2 invariant I-3 must hold through the new path.)
```

## 8. Threats this primitive addresses

- **Stale synthetic close during volatility.** When mark moves
  fast and the synthetic close at `oracle - liq_penalty_bps` is
  below the current best bid (the trader is so unhealthy that
  open-market liquidity has stepped away), the backstop covers the
  gap.
- **Keeper coordination failure.** If the keeper pool happens to
  be idle for a few slots (rare but possible during network stress),
  the backstop is a self-healing primitive — any caller can fire it.
- **Insurance-fund preservation.** Before ADL, the backstop gives
  the protocol one more layer to absorb tail loss without socializing
  it to other traders.

## 9. Threats this primitive does NOT address

- **FLP exhaustion.** If the FLP capacity is fully consumed by
  backstop takes, subsequent underwater positions still need ADL.
  The `max_backstop_exposure_quote_lots` is the operator's lever
  for trading off backstop coverage vs LP risk.
- **Oracle manipulation.** A manipulated oracle can produce a
  synthetic close price unfair to the trader. The Phase 2 dual-
  source price gate + multi-oracle quorum already protect against
  this, but the backstop inherits whatever weakness those primitives
  have.

## 10. Versioning

Scope-discovery artifact for v0.5.0. The implementing commits
reference back to this document and convert sections to "SHIPPED"
the same way `SUB_ACCOUNT_TRADING.md` did for Phase 2c–2f.

Until then, the COMPARISON.md "honest weaknesses" section
accurately states that there's no HLP-equivalent backstop today.
