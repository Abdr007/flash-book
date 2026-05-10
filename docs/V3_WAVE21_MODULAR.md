# Wave 21 — Modular Wrapper Programs

Design spec for splitting Flash Book v3 from a monolithic Anchor program
into a 4-program system. Goal: independent upgrade lifecycle per
subsystem, smaller per-program audit surface, and per-market FLP
exposure (which unlocks ER-delegating the FLP account too).

## Why split

Today's `flash-book` program is one Anchor crate with ~10K LOC of ixs:

  • Core matcher (init market, place/cancel/run_batch, apply_fill)
  • Triggers (place/execute/cancel + bracket OCO + trailing stops)
  • TWAP / iceberg schedulers
  • Vaults (strategist deposit / withdrawal management)
  • HIP-3 permissionless market deployment
  • FLP exposure singleton

This means:

  1. A bug in any subsystem requires a full program upgrade (= governance
     vote + freeze window + re-deploy of everything).
  2. Audit cost scales with the whole program LOC.
  3. The FLP exposure account is a singleton — delegating it to MagicBlock
     ER bottlenecks ALL markets to one ER instance. Per-market FLP needs
     a separate program owning its accounts.

## Target topology

```
                       ┌──────────────────────────────┐
                       │  flash-book-core             │
                       │  (matcher, FBA, Position,    │
                       │   TraderState, MarketBook,   │
                       │   funding, mark, VPIN)       │
                       │  Program ID: FBookV1...      │
                       └──┬─────────────┬─────────────┘
                          │ CPI         │ CPI
              ┌───────────┘             └────────────────┐
              ↓                                          ↓
  ┌───────────────────────┐                ┌────────────────────────┐
  │ flash-book-orders     │                │ flash-book-flp         │
  │ (triggers, TWAP,      │                │ (per-market FLP        │
  │  iceberg, brackets,   │                │  exposure, LP shares,  │
  │  trailing stops)      │                │  capital deposit/      │
  │  Program ID: FBOrd... │                │  withdraw)             │
  └───────────────────────┘                │  Program ID: FBflp...  │
                                           └────────────────────────┘
              ┌──────────────────────────────────┐
              │ flash-book-vaults                │
              │ (strategist vaults, depositor    │
              │  shares, withdraw queueing)      │
              │  Program ID: FBVault...          │
              └──────────────────────────────────┘
```

## Per-program responsibility

### `flash-book-core`

- Owns: `MarketAccount`, `MarketBookAccount` (the v2 hypertree),
  `CommitBufferAccount`, `PositionAccount`, `TraderStateAccount`,
  `InsuranceFundAccount`.
- Ixs: init market, init market_book, init commit_buffer,
  place_limit_order_v2, cancel_order_v2, run_batch_v2, apply_fill,
  apply_flp_fill (CPI to flash-book-flp), settle_funding,
  liquidate_position_v2, liquidate_portfolio_v2, auto_deleverage,
  withdraw_collateral, deposit_collateral.
- ER-delegatable accounts: `MarketAccount`, `MarketBookAccount`,
  `CommitBufferAccount`.

### `flash-book-orders`

- Owns: `TriggerOrderAccount`, `TwapOrderAccount`, `IcebergOrderAccount`.
- Ixs: place/execute/cancel for each order type; bracket atomic;
  update trailing stop.
- CPIs into core's `place_limit_order_v2` to inject the synthesized
  order. Uses `invoke_signed` with the order PDA's seeds as the signer.
- Owner check on the core side: the `place_limit_order_v2` ix accepts
  EITHER the trader as direct signer OR a signer matching the
  documented orders-program ID (= `FBOrd...`).

### `flash-book-flp`

- Owns: `FlpExposurePerMarketAccount` (PDA at `[b"flp", market]`,
  one per market — solves the singleton bottleneck).
- Ixs: deposit_flp_capital, withdraw_flp_capital, generate FLP virtual
  quotes for a market (called via CPI from core's run_batch_v2).
- ER-delegatable per-market: each market's FLP exposure account can be
  delegated to its market's ER instance independently.

### `flash-book-vaults`

- Owns: `VaultAccount`, `VaultPositionAccount`, `VaultDepositorShares`.
- Ixs: create_vault, vault_deposit, vault_withdraw, vault_redeem.
- CPIs into core's place_limit_order_v2 / settle_funding for
  vault-on-behalf-of-trader operations.

## Migration plan

This is a hard migration because account ownership changes mid-flight.
Cannot be done with a simple program upgrade.

### Phase 1 — Deploy the 3 new programs (zero traffic) ✅ SHIPPED

  1. ✅ `flash-book-orders` — program ID
     `2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5`
  2. ✅ `flash-book-flp` — program ID
     `eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M`
  3. ✅ `flash-book-vaults` — program ID
     `GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt`
  4. ✅ Each program ships as `pub mod flash_book_orders` etc. with
     a single liveness ix `ping` that emits a `Pong` event. Lets
     operators verify the program is deployed + callable on a target
     cluster before wiring any state. `target/deploy/*.so` ready to
     ship via `solana program deploy`.

  All 4 programs build clean. IDLs generated in `target/idl/`.
  Workspace + `Anchor.toml` updated. SDK exports the 3 new program
  IDs (`FLASH_BOOK_ORDERS_PROGRAM_ID`, `_FLP_`, `_VAULTS_`).

### Phase 2 — Add CPI surface in core

  5. `flash-book-core` adds: `place_limit_order_v2` accepts an
     authorized-program signer in addition to the trader signer.
     The whitelist of accepted programs is hardcoded (`FBOrd...`,
     `FBflp...`, `FBVault...`).
  6. Core ships: `apply_flp_fill_per_market` ix that takes the
     per-market FLP exposure account from `flash-book-flp` instead
     of the singleton.

### Phase 3 — Migrate per-market data

For each market in production:

  7. Pause the market (`change_market_status` to Paused).
  8. Snapshot trigger / TWAP / iceberg accounts. Re-create them under
     `flash-book-orders` ownership.
  9. Snapshot the singleton FLP exposure's per-market entry. Initialize
     a `FlpExposurePerMarketAccount` under `flash-book-flp` with the
     same state.
  10. Snapshot vault accounts. Re-create them under `flash-book-vaults`.
  11. Resume the market.

### Phase 4 — Sunset the legacy ixs in core

  12. Mark core's trigger / TWAP / iceberg / vault ixs as `#[deprecated]`.
  13. After 1 release cycle, delete them.
  14. Singleton `FlpExposureAccount` becomes a wind-down account: only
      `withdraw_flp_capital` allowed.

## Backward compatibility

- Existing `Position` + `TraderState` PDAs stay under core (no migration).
- `MarketBookAccount` stays under core (it's the matcher's hot-path
  account; CPI'ing from a wrapper program would add latency).
- `BatchClearedEvent`, `FillAppliedEvent`, `OrderPlacedV2Event` — emitted
  by core unchanged. Off-chain consumers (sequencer, indexer) need no
  reconfiguration.

## Not in scope

- Splitting the matcher itself across programs. The FBA clearing must
  run in-process for atomicity; CPI'ing each fill out would cost
  CU + latency.
- Cross-program CPI for liquidations. Liquidation paths stay in core
  for the same atomicity reason.

## Open questions

1. Does Anchor's `#[program]` macro support multiple authorized signers
   for the same ix? Need to verify CPI-from-trusted-program pattern
   works with current Anchor version.
2. Migration phase 3: do we accept a brief downtime per market during
   re-creation, or implement live migration with state-shadowing?
3. Should `flash-book-vaults` itself be split into `vaults-strategy`
   and `vaults-shares` for further granularity? Probably not for v1
   modular split — overkill.

## Risk assessment

- **High** if attempted in a single deploy. Multi-program CPI debugging
  on-chain is painful; needs extensive devnet testing.
- **Medium** if phased per the above plan. Core unchanged; new programs
  are additive; per-market migration is well-defined.
- **Low** for the long-term operational benefit (independent upgrade
  cadence + smaller per-program audit + ER-delegatable per-market FLP).

## Effort estimate

- 4 weeks of focused work: 1 week per program (core CPI surface,
  orders, flp, vaults) + 1 week migration tooling + extensive devnet
  testing.
- Audit: 2-3 weeks (each program needs separate audit; core re-audit
  is needed because of the new CPI surface).

## What this unlocks

1. **Per-market FLP ER delegation** — biggest win. ER instances can
   serve a single market without contention with other markets'
   matcher ticks.
2. **Independent upgrade cadence** — bug in trigger logic doesn't
   require freezing the matcher.
3. **Smaller audit surface per upgrade** — only the changed program
   needs re-audit.
4. **Composability** — third parties can build trigger / vault
   wrappers that CPI into core, same as our wrappers do.
