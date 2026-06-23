# Instruction reference

Complete reference for the 16 Anchor instructions exposed by the
Flash Book program. Each entry lists accounts, arguments, gates,
events, and the most likely error families.

The IDL at `idl/flash_book.json` is the canonical machine-readable
source; this document is the human-readable companion.

---

## Setup

### `initialize_insurance_fund`

Creates the global `InsuranceFundAccount` PDA (one per protocol).

| Account | Mode | Notes |
|---|---|---|
| `authority` | signer, mut | Pays rent. |
| `insurance_fund` | init, mut | PDA seed: `["insurance_fund"]`. |
| `system_program` | readonly | |

Args:
- `fee_contribution_bps: u32` — fraction of taker fees added to fund.
- `toxicity_tax_contribution_bps: u32` — fraction of toxicity tax.
- `liq_penalty_contribution_bps: u32` — fraction of liq penalty.
- `pause_threshold_quote_lots: u64` — fund balance below this halts new positions.

---

### `initialize_market`

Creates the `MarketAccount`, `OrderBufferAccount`, and `CommitBufferAccount`
PDAs for a (base_mint, quote_mint) pair. Insurance fund and FLP exposure
must already exist.

| Account | Mode | Notes |
|---|---|---|
| `authority` | signer, mut | |
| `base_mint`, `quote_mint` | readonly | Mint pubkeys (not validated as SPL in v1). |
| `base_vault`, `quote_vault` | readonly | Token vault addresses. |
| `oracle_account` | readonly | Pyth or equivalent. |
| `market` | init, mut | Seed: `["market", base_mint, quote_mint]`. |
| `order_buffer` | init, mut | Seed: `["order_buffer", market]`. |
| `commit_buffer` | init, mut | Seed: `["commit_buffer", market]`. |
| `insurance_fund` | readonly | Must exist. |
| `flp_exposure` | readonly | Must exist. |
| `system_program` | readonly | |

Args:
- `params: MarketParams` — full market configuration.
- `initial_oracle_ticks: u64` — starting mark/oracle price.

Emits: `MarketInitializedEvent`.

---

### `open_trader_state`

Creates a per-trader `TraderStateAccount` PDA.

| Account | Mode | Notes |
|---|---|---|
| `trader` | signer, mut | |
| `trader_state` | init, mut | Seed: `["trader_state", trader]`. |
| `system_program` | readonly | |

---

## Account lifecycle

### `deposit_collateral(amount_quote_lots)`

Credits the trader's `collateral_quote_lots`. SPL transfer is added in
a follow-up; v1 increments the accounting counter.

Errors: `ZeroSize`, `ArithmeticOverflow`.
Emits: `CollateralDepositedEvent`.

---

### `withdraw_collateral(amount_quote_lots)`

Debits collateral. **Blocked while `open_positions > 0`.** Production
will iterate Position PDAs via `remaining_accounts` to enforce the
initial-margin requirement on the post-withdraw balance.

Errors: `InsufficientCollateral`, `ArithmeticUnderflow`.
Emits: `CollateralWithdrawnEvent`.

---

## Order intake

### `place_limit_order(side, size_lots, limit_ticks, post_only)`

Appends a resting limit order to the next batch's order buffer.

| Account | Mode | Notes |
|---|---|---|
| `trader` | signer, mut | |
| `market` | readonly | |
| `order_buffer` | mut | |
| `trader_state` | mut | Rate limit + open-position counters. |
| `position` | init-if-needed, mut | First-time orders init the Position PDA. |
| `system_program` | readonly | For init-if-needed. |

Gates:
- Status: `Active` or `PostOnly` only.
- Lot size: `size_lots ≥ params.min_base_lots`.
- Tick alignment: `limit_ticks % params.tick_size == 0`.
- Stress-lattice: if `position.size_lots > 0`, `assess_margin` must
  be `is_healthy = true`. Otherwise rejected with `TraderLiquidatable`.
- Rate limit: `trader_state.orders_this_batch < MAX_ORDERS_PER_TRADER_PER_BATCH`
  (default 16; resets on new batch).
- Reserved sequence range: order seq stays below `FLP_SEQ_RESERVED_OFFSET`.

Errors: `OutOfRange`, `SizeBelowMinLot`, `PriceNotOnTick`,
`TraderLiquidatable`, `RateLimited`, `BufferFull`, `WrongTrader`,
`WrongMarket`.

---

### `submit_commit(hash, bond)`

Phase 1 of the commit-reveal MEV-resistant taker flow. Stores a hash
in the per-market `CommitBufferAccount`.

Args:
- `hash: [u8; 32]` — keccak hash of `(market, trader, side, size, limit, nonce)`.
- `bond: u64` — bond seized if reveal doesn't land before expiry.

Errors: `CommitDuplicate`, `BufferFull`.

---

### `submit_reveal(side, size_lots, limit_ticks, nonce)`

Phase 2. Verifies the hash matches and synthesizes a `Taker`-priority
order in the next batch's order buffer.

Errors: `CommitMismatch`, `CommitExpired`, `BufferFull`, `OutOfRange`,
`ZeroSize`, `ZeroPrice`.

---

## Batch execution

### `run_batch(now_ms)`

The heart of the matcher. Executed by the sequencer every `batch_interval_ms`.

Per call:
1. Advance funding index.
2. Read FLP per-market exposure → compute signed pool position.
3. Generate FLP virtual quote ladder (Avellaneda-Stoikov + VPIN +
   utilization + OI imbalance + depth + realized vol).
4. Run FBA Walrasian uniform-price clearing on
   buffered-orders + FLP-quotes.
5. Update mark = TWAP(recent clearing prices) banded by oracle.
6. Update VPIN from each fill.
7. Sweep expired commit-reveal entries, return seized bond total.
8. Clear the order buffer.
9. Increment `current_batch`.

Emits: `BatchClearedEvent` with clearing price, volume, fill count,
funding rate, seized bonds.

| Account | Mode | Notes |
|---|---|---|
| `sequencer` | signer | |
| `market` | mut | |
| `order_buffer` | mut | |
| `commit_buffer` | mut | Sweep expired. |
| `insurance_fund` | mut | (Reserved for future fee accrual.) |
| `flp_exposure` | readonly | Read-only here; mutated by `apply_flp_fill`. |

---

## Settlement

### `apply_fill(size_lots, price_ticks, taker_side)`

Settles a fill where both sides are real traders.

| Account | Mode | Notes |
|---|---|---|
| `sequencer` | signer, mut | Pays for init-if-needed Positions. |
| `market` | mut | OI updates. |
| `taker_trader_state`, `maker_trader_state` | mut | open_positions transitions. |
| `taker_position`, `maker_position` | init-if-needed, mut | Lifecycle math. |
| `system_program` | readonly | |

Position lifecycle math:
- empty → open(side, size, entry = price)
- same side → volume-weighted average entry
- opposite ≤ existing → reduce, realize PnL on closed portion
- opposite > existing → flip side, realize PnL on full close,
  remaining size opens at fill price

Emits: `FillAppliedEvent`.

---

### `apply_flp_fill(size_lots, price_ticks, taker_side)`

Settles a fill where the FLP pool is the *maker*. Mutates the
`FlpMarketExposure` slot in `FlpExposureAccount.per_market`. Same
lifecycle math as `apply_fill_to_position` but on the FLP slot.

Emits: `FlpFillAppliedEvent` carrying the FLP's post-fill side+size.

---

## Liquidation

### `liquidate_position`

Permissionless. Anyone may submit; the matcher decides.

Flow:
1. Validate the position is non-empty and matches the trader/market.
2. Build single-position snapshot, market snapshot, default scenarios.
3. Run `assess_margin`. If `is_healthy = true` → reject with `NotLiquidatable`.
4. Synthesize a `Liquidation`-priority order on the *opposite* side at
   `oracle ± liq_penalty_bps`, append to `order_buffer`.
5. Next `run_batch` clears it at the batch uniform clearing price.
6. `apply_fill` settles the position.

Emits: `LiquidationInjectedEvent` with worst-scenario index.

Errors: `NotLiquidatable`, `LiquidationStale`, `WrongTrader`, `WrongMarket`,
`BufferFull`.

---

## Governance (authority-only)

### `update_oracle(price_ticks, confidence)`

Authority writes oracle. Production replaces this with a Pyth read
inside `run_batch`.

Errors: `Unauthorized`, `ZeroPrice`.

---

### `set_market_status(new_status)`

Circuit breaker. Status enum:
| Value | Name | Meaning |
|---|---|---|
| 0 | Inactive | Pre-launch (rare). |
| 1 | Active | Full trading. |
| 2 | PostOnly | New limits OK; takers blocked. |
| 3 | Paused | No order intake; existing positions held. |
| 4 | Closed | Terminal. Cannot reopen. |

Errors: `Unauthorized`, `OutOfRange` (if currently Closed).
Emits: `MarketStatusChangedEvent`.

---

### `update_market_params(new_params)`

Tunes mutable market parameters. **Immutable post-init:** `tick_size`,
`base_lot_size`, `quote_lot_size`, `min_base_lots` (changing these would
silently invalidate every existing order/position). All other fields
tunable with sanity bounds.

Errors: `Unauthorized`, `OutOfRange`.
Emits: `MarketParamsUpdatedEvent`.

---

### `transfer_market_authority(new_authority)`

Safe key rotation. Atomic swap of `market.authority`.

Errors: `Unauthorized`.
Emits: `MarketAuthorityTransferredEvent`.

---

## Error families (numeric ranges)

| Range | Family | Examples |
|---|---|---|
| 1000–1099 | numerical | ArithmeticOverflow, DivisionByZero |
| 1100–1199 | account/authority | Unauthorized, WrongTrader, WrongMarket |
| 1200–1299 | order intake | SizeBelowMinLot, PriceNotOnTick, RateLimited |
| 1300–1399 | matcher | BufferFull, SelfTrade |
| 1400–1499 | margin/liquidation | TraderLiquidatable, NotLiquidatable |
| 1500–1599 | insurance | InsuranceBelowFloor, InsuranceExhausted |
| 1600–1699 | commit-reveal | CommitMismatch, CommitExpired |
| 1700–1799 | delegation (ER) | NotDelegated, DelegationExpired |

The full enum is in `programs/flash-book/src/errors.rs`.
