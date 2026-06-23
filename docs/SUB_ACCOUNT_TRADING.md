# Sub-Account Trading — Phase 2 Scope Discovery

The Phase 1 commit `58ed3e9` deferred sub-account trading to "a focused,
awake-eyes-on-the-diff session" and characterised it as relaxing
`seeds = [TraderStateAccount::SEED, trader.key().as_ref()]` on ~15
Accounts structs in favour of handler-side checks.

This document captures the finding that **the work is materially larger
than that** because of an architectural coupling Phase 1 didn't call
out: `PositionAccount` PDAs key on the trader's **wallet**, not their
TraderState PDA. Until that's resolved, relaxing the TraderState seeds
alone produces broken semantics (main + sub aliasing onto the same
position).

The intent of this doc is to enumerate every site that has to change
and surface the one architectural question that has to be answered
before a single line of seeds-relaxation is safe to merge.

## 0. Status

- **Phase 2c (foundation): SHIPPED.** Position PDAs now key on
  `trader_state.key()` instead of `wallet.key()`. The migration ix
  `migrate_position_to_trader_state_key` moves legacy
  `(market, wallet)` positions to the new `(market, trader_state)`
  address. All Position PDA derivations in lib.rs + the SDK have been
  updated. Main and sub-accounts now have distinct position addresses
  per market — the prerequisite for sub-account risk isolation.
- **Phase 2d: SHIPPED.** trader_state seed relaxation on the ~18
  trade-path Accounts structs. Sub-accounts are accepted as the
  trader_state for direct-state ixs (Deposit, Withdraw, Liquidate,
  ADL, ApplyFill, ApplyFlpFill, PartialWithdraw,
  SetPositionMarginMode, SettleFunding, PlaceBasket*,
  LiquidatePortfolio, SweepCollateral, SetTraderReferrer /
  Delegate / Builder / FeeTier, ViewPortfolioRisk). The handler
  enforces `trader_state.trader == signer.key()` (or, for
  permissionless ixs, identity comes from the position seed pair).
- **Phase 2i: SHIPPED.** ApplyFill / ApplyFlpFill re-derive the
  expected TraderState PDA from `(trader, sub_index)` and assert
  against the passed account key. Closes the last remaining
  routing-attack surface — a hostile sequencer can no longer pass a
  different sub_index's TraderState while claiming sub_index = 0 in
  the ix data. Phase 2j adds the end-to-end integration test.
- **Phase 2e: SHIPPED.** RestingOrderV2 now carries `sub_index: u8`
  (repurposed from the prior `_pad` byte — layout-compatible, existing
  on-disk nodes read back with sub_index = 0 = main).
  `place_limit_order_v2` and `place_taker_order_v2` take an explicit
  `sub_index: u8` ix parameter and write it into the order. The
  matcher carries `maker_sub_index` through each `FillEntry` and the
  emitting `FillBatchEvent` also carries `taker_sub_index`, so the
  off-chain sequencer can derive
  `[STATE_SEED, trader.as_ref(), &[sub_index]]` and pass the right
  TraderState to ApplyFill / ApplyFlpFill.
- **Phase 2f: SHIPPED.** sub_index threaded through every secondary
  order primitive:
  - V3 trigger orders (`place_trigger_order_v3` + `execute_trigger_order_v3`)
  - V3 TWAP orders (`place_twap_order_v3` + `execute_twap_slice_v3`)
  - V3 iceberg orders (`place_iceberg_order_v3` + `replenish_iceberg_v3`)
  - V3 bracket orders (`place_bracket_order_v3` — parent + TP + SL)
  - JIT liquidation offers (`place_jit_liquidation_offer` —
    `maker_sub_index`)
  - V1 trigger / TWAP / iceberg execute paths (legacy state structs
    each gained a trailing `sub_index: u8` field, layout-compatible).
  - `modify_order_v2` preserves the original order's `sub_index`.
  - Basket orders (V2 + V2N) inject all legs with the trader's
    `sub_index`.

  Plus `TraderStateAccount` gained its own `sub_index` field (filled
  in by `open_trader_state` = 0 and `open_trader_sub_account` = idx).
  The liquidation synthetic-close in `liquidate_position_v2` /
  `liquidate_portfolio_v2` reads `trader_state.sub_index` and writes
  it into the synthetic `RestingOrderV2`, so a sub-account's
  liquidation routes the close fill back to the same TraderState.

  Vault orders intentionally remain `sub_index = 0` — vault accounts
  are their own TraderState family.

## 1. The aliasing problem

Today every Position PDA is derived as

```rust
seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader.key().as_ref()],
```

where `trader.key()` is the signing **wallet**. The sub-account's
`TraderStateAccount.trader` field is **the same wallet** (sub PDAs only
distinguish themselves by extending the seed with `&[sub_index]`).

So if we relax the trade-path seeds and let a sub-account be the
`trader_state` for `place_limit_order_v2`, the matched
`PositionAccount` is **the same PDA the wallet's main account would
mutate**. Main and sub end up sharing one position per market.

That defeats the whole point of sub-accounts. Risk-isolation can't
exist if a sub-account's losing trade decrements the main account's
shared position size.

## 2. Three resolutions, ranked by implementation cost

### Option A — Position PDA includes `sub_index`

```rust
seeds = [POS_SEED, market.key().as_ref(), trader.key().as_ref(), &[sub_index]]
```

Compact. `sub_index = 0` matches the legacy main PDA bit-for-bit when
the seed system uses the same `find_program_address` call with an
explicit `0` byte appended (it doesn't — appending `&[0]` produces a
**different** PDA than the legacy `[..., trader.key()]`). So this
silently breaks every existing position.

**Migration cost:** every existing PositionAccount needs a migration ix
that re-derives at the new address. Existing live positions on devnet
break on first read. Off-chain indexers all re-key.

### Option B — Position PDA keys on `trader_state` PDA

```rust
seeds = [POS_SEED, market.key().as_ref(), trader_state.key().as_ref()]
```

Cleanest mental model: each trader_state — main or sub — has its own
positions, keyed by the trader_state's PDA address. Trade ix Accounts
structs pass `trader_state` already; the seeds expression has access
to its key.

Same migration cost as Option A: legacy positions are at the
`(market, wallet)` address, new ones at `(market, trader_state_pda)`.
For the main account they ARE the same address (because the main
trader_state PDA derives from `trader.key()` alone), but only if you
deliberately match seed lengths — which Anchor doesn't do, so a
migration is needed.

### Option C — Sub-accounts share positions, isolate via collateral only

Keep position PDAs at `(market, wallet)`. Sub-accounts are extra
collateral pools the wallet's position can draw against. Pure
collateral-bucketing.

This is the cheapest path but it isn't sub-account trading — it's
"multiple wallets you can put collateral in." Drift / HL sub-accounts
are independent trading scopes; option C doesn't provide that.

**Recommendation:** Option B. Cleaner address derivation, no `sub_index`
plumbing in every PositionAccount user, and the trader_state PDA is
already a stable identity off-chain consumers track.

## 3. Migration plan if we pick Option B

### 3.1 New position PDAs

For new `init_if_needed` positions:

```rust
seeds = [POS_SEED, market.key().as_ref(), trader_state.key().as_ref()]
```

For lookups (non-init), same.

### 3.2 Migration ix

```rust
pub fn migrate_position_to_trader_state_key(
    ctx: Context<MigratePositionToTraderStateKey>,
) -> Result<()> {
    // Reads legacy_position (closed = trader),
    // writes a new init'd position at (market, trader_state.key()),
    // copies size_lots, side, entry_price_ticks, cum_funding_index_at_entry,
    //   collateral_quote_lots, realized_pnl_quote_lots, funding_paid_quote_lots,
    //   last_settlement_batch, unhealthy_since_slot, last_liquidated_at_slot,
    //   bump.
    // Asserts legacy_position.trader == trader_state.trader (main only — subs
    //   have no legacy positions to migrate).
    // Closes legacy_position, refunding rent to trader.
    // Emits PositionMigratedEvent { trader, market, legacy_pda, new_pda }.
}
```

The op is per-(wallet, market) and only runs once per main account.
Sub-account positions are new and don't need migration.

### 3.3 Off-chain reads

Every indexer / UI / keeper that derives a Position PDA needs to
update its derivation. The Position PDA derivation
(`[POS_SEED, market, trader_state]`) is:

```ts
export function positionPda(
  market: PublicKey,
  traderStatePda: PublicKey,    // was: trader: PublicKey
  programId: PublicKey,
): { address: PublicKey; bump: number };
```

And every call site that today passes `trader` needs to first derive
the `traderStatePda` (or accept it as input).

For backwards compatibility during migration we'd want an explicit
`positionPdaLegacy(market, wallet, programId)` that returns the OLD
address. Front-ends use it to read legacy state until migrated.

## 4. Trade-path Accounts structs requiring relaxation (Option B)

These are the seeds on `trader_state` that need to drop the
`trader.key().as_ref()` constraint. With Option B these structs accept
ANY `Account<'info, TraderStateAccount>` whose
`.trader == ctx.accounts.trader.key()` (so the signer always matches
the trader_state's owner field, whether main or sub).

| # | Accounts struct | Current line | Trader-state seed | Constraint already present |
|---|---|---|---|---|
| 1 | DepositCollateral | ~8640 | yes | yes |
| 2 | WithdrawCollateral | ~8849 | yes | yes |
| 3 | PartialWithdrawCollateral | ~8867 | yes | yes |
| 4 | PlaceLimitOrderV2 | ~8909 | yes | yes |
| 5 | PlaceTakerOrderV2 | ~8948 | yes | yes |
| 6 | CancelOrderV2 | ~8994 | yes | yes |
| 7 | ModifyOrderV2 | ~9037 | yes | yes |
| 8 | PlaceBasketOrderV2 | ~9119 | yes | yes |
| 9 | PlaceBasketV2N | ~9188 | yes | yes |
| 10 | OpenPosition | (find) | check | check |
| 11 | SetPositionMarginMode (set_position_iso/cross) | ~9100 | yes | yes |
| 12 | SettleFunding | ~8531 | self-ref via `trader_state.trader.as_ref()` | n/a |
| 13 | ApplyFill | ~9027 / 9034 | both taker_/maker_trader_state | n/a |
| 14 | LiquidatePositionV2 | ~9410 | self-ref | n/a |
| 15 | AutoDeleverage | ~9503 / 9540 | self-ref both sides | n/a |

For the self-referential seeds (`trader_state.trader.as_ref()`) the
constraint is structurally tighter — the seed reads its own data
field, so the PDA must be at exactly `[SEED, trader.field]`. That
only matches the main PDA (sub PDAs append a byte). Same relaxation
applies: drop the seeds, keep the `.trader == signer.key()` handler
check, accept any TraderStateAccount whose owner field matches.

### 4.1 Worked example: `DepositCollateral` (Option B)

Before:

```rust
#[account(
    mut,
    seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
    bump = trader_state.bump,
    constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
)]
pub trader_state: Box<Account<'info, TraderStateAccount>>,
```

After:

```rust
#[account(
    mut,
    constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
    // The seeds constraint is intentionally dropped to allow this ix
    // to operate on either the trader's main TraderState PDA or any of
    // their sub-account TraderState PDAs. The `.trader` field check
    // proves the signer owns this TraderState; Anchor's
    // Box<Account<...>> still enforces program ownership of the
    // account data. See docs/SUB_ACCOUNT_TRADING.md §5 for the safety
    // analysis.
)]
pub trader_state: Box<Account<'info, TraderStateAccount>>,
```

**No `bump`** because we're no longer asserting a specific derivation
— the trader_state's own `.bump` field is informational only.

### 4.2 Worked example: `ApplyFill` (Option B)

`ApplyFill` has TWO trader_state accounts. Both relax. AND the two
position accounts must switch to the new derivation:

```rust
#[account(
    init_if_needed,
    payer = sequencer,
    space = state::PositionAccount::space(),
    seeds = [
        state::PositionAccount::SEED,
        market.key().as_ref(),
        taker_trader_state.key().as_ref(),  // ← was taker_trader_state.trader.as_ref()
    ],
    bump,
)]
pub taker_position: Box<Account<'info, state::PositionAccount>>,
```

Note this is a SCHEMA-BREAKING change for off-chain consumers.

## 5. Safety analysis of the relaxed pattern

Without `seeds = [...]`:

- **Account ownership.** `Account<'info, TraderStateAccount>` (or
  `Box<Account<...>>`) still verifies the account is owned by our
  program. An attacker cannot pass an arbitrary account here.
- **Schema.** Anchor deserialises into `TraderStateAccount`. The
  discriminator gate rules out other account types owned by us.
- **Identity binding.** The handler check
  `trader_state.trader == ctx.accounts.trader.key()` proves the
  account's owner field matches the signer. Both main and sub satisfy
  this (Phase 1 sets `.trader` to the parent wallet on every sub
  account at open time, `lib.rs:1448`).
- **What does NOT verify:** that the account is a "real" PDA derived
  by `find_program_address` from a known seed pattern. With ownership
  + schema + identity in place, this gap is benign — a malicious
  account would have to be `init`-able by our program (only via
  `open_trader_state` / `open_trader_sub_account`, which both already
  derive the correct PDA).

The single remaining residual risk is **type confusion** with another
account-type the program owns whose serialisation happens to start
with the TraderStateAccount discriminator. Anchor's discriminator
mechanism makes that essentially zero by construction.

## 6. Test additions required

For each relaxed ix:

- **Happy path with sub-account.** A test that opens
  `trader_sub_account(index=1)`, deposits collateral via the relaxed
  `deposit_collateral` with the sub PDA as `trader_state`, asserts
  the sub's `.collateral_quote_lots` rises (not the main's).
- **Attack vector — wrong-trader passes.** Signer A passes Signer B's
  TraderState. The handler check must reject with
  `FlashBookError::WrongTrader`.
- **Attack vector — non-TraderState account passes.** Any other
  account-type owned by us is rejected at the Anchor discriminator
  layer; document the test even though it's a no-op proof.
- **Cross-state aliasing.** Sub-account opens a position; verify it
  goes to a DIFFERENT PDA than the main account's position on the
  same market (Option B), or to the same PDA (Option C).
- **Liquidation path.** Sub-account opens a position, market gaps
  against it, liquidate_position_v2 succeeds against the sub's
  position. Reward routing follows the cross/isolated split (already
  covered by Phase 2 §6.3).

## 7. Estimated effort

| Slice | Notes | LOC range |
|---|---|---|
| Decision: Option A / B / C | Architecture sign-off | n/a |
| Migration ix + migration tests | Only if A or B | ~300 |
| Relax seeds + handler checks on the 15 trade-path structs | One commit per group of 3-5 for audit-ability | ~400 |
| Off-chain SDK updates | `pdas.ts`, client helpers, IDL regen | ~150 |
| Position-PDA derivation rename in lib.rs (≈8 sites if Option B) | Each is mechanical | ~100 |
| New tests (happy + attack + aliasing per ix) | 15 ixs × ~3 tests | ~600 |
| MARGIN_MATH and ARCHITECTURE updates | Pin the new invariants | ~50 |
| **Total** | | **~1,600** |

Best executed across 4–6 commits so each piece is independently
auditable.

## 8. Why this isn't in the Phase 2 isolated-margin work

Sub-account trading and isolated margin are orthogonal:

- **Isolated margin** (Phase 2, this branch) partitions a single
  trader's positions into separate risk buckets, where each isolated
  position has its own collateral.
- **Sub-account trading** (this doc) partitions a single trader's
  whole account into multiple independent trading scopes that happen
  to share a signing key.

Both are present in Drift / HL. Both serve different use cases:
isolated margin for single-position risk capping, sub-accounts for
strategy separation (e.g. "delta-neutral book" vs "directional book"
under one wallet).

The Phase 2 isolated-margin commit is functional without sub-accounts.
Sub-accounts will be functional without per-position isolation. They
compose naturally — a sub-account can have its own isolated positions
once both ship.

## 9. Recommended next step

1. ~~Owner picks A / B / C.~~ Option B chosen and shipped in Phase 2c.
2. ~~Migration ix.~~ Shipped — `migrate_position_to_trader_state_key`.
3. **Phase 2d — relax the ~12 trade-path Accounts structs.** Group by
   domain across 2-3 commits: (a) collateral management (Deposit,
   Withdraw, PartialWithdraw, SetPositionMarginMode); (b)
   liquidation (LiquidatePositionV2, LiquidatePortfolioV2,
   AutoDeleverage, SettleFunding); (c) fills (ApplyFill, ApplyFlpFill,
   PlaceBasket*). Each commit independently buildable and testable.
4. **Phase 2e — sub-account order placement.** See §10. Roughly the
   largest single architectural change still pending.

Each commit independently buildable, testable, and reviewable. **Do
not bundle.**

## 10. Phase 2e — sub-account order placement

The hypertree-backed order book stores resting orders as
`RestingOrderV2` (in `state_v2.rs`). The `trader` field is the WALLET
pubkey, set at insertion time from `ctx.accounts.trader.key()`. The
hypertree itself is keyed by `(price_ticks, seq)` not by trader, but
every fill resolution reads the `RestingOrderV2.trader` field to
look up the matched `taker_trader_state` / `maker_trader_state`.

Today (Phase 2c) the lookup uses
`[STATE_SEED, restingOrder.trader.as_ref()]` — the main TraderState
PDA. There is no way to route a fill to a sub-account's TraderState
because the order doesn't know which sub-account placed it.

**Options for 2e:**

### Option E.1 — Add `sub_index: u8` to RestingOrderV2

```rust
pub struct RestingOrderV2 {
    // ...existing 32 bytes for order_id, seq, price_ticks, size_lots,
    // expires_at_slot, trader, last_valid_slot, side, order_type, flags...
    pub _pad: u8,        // ← was padding; now sub_index
    // ...rest of fields unchanged.
}
```

`flags` byte already exists; we could repurpose an unused flag bit OR
take a single padding byte. Either is a layout-compatible change AS
LONG AS we audit the on-disk hypertree node size invariants.

PlaceLimitOrderV2 / PlaceTakerOrderV2 need a `sub_index: u8` ix
parameter, written into the order at insertion.

ApplyFill resolves the trader_state PDA from
`(restingOrder.trader, restingOrder.sub_index)`:

```
seed = if sub_index == 0 {
    [STATE_SEED, restingOrder.trader.as_ref()]
} else {
    [STATE_SEED, restingOrder.trader.as_ref(), &[sub_index]]
};
```

But Anchor's `seeds = [...]` can't conditionalise — so ApplyFill drops
the strict seed constraint on taker_/maker_trader_state and validates
the trader_state PDA matches expected in the handler.

### Option E.2 — Change `RestingOrderV2.trader` to BE the trader_state PDA

Conceptually cleaner: orders carry the TraderState identity directly,
not the wallet. Eliminates the sub_index ambiguity entirely — every
order's `trader` field is a TraderState PDA, main or sub. Hypertree
node layout unchanged (it's still 32 bytes).

Off-chain consumers that expected `.trader` to be the wallet break
unless they re-derive. SDK provides a `RestingOrderV2.tradedBy(state)`
helper that resolves the wallet from the TraderState PDA.

### Option E.3 — Keep RestingOrderV2 as-is; route fills via remaining_accounts

ApplyFill takes EITHER the main trader_state OR a sub-account
trader_state as a remaining_account pair, and the handler verifies it
matches the resting order's intended target. The off-chain sequencer
that builds ApplyFill ixs has access to the order's source context
(it indexes placements) and can route accordingly.

Cheapest implementation but pushes correctness into the off-chain
sequencer trust boundary.

### Recommendation

**Option E.1 (sub_index byte).** It's a minimal schema change (one
byte that was already padding), keeps the hypertree mechanism
mechanically identical, makes the on-chain identity explicit, and
doesn't break existing off-chain consumers (they continue to read
`RestingOrderV2.trader` as a wallet pubkey, and sub_index defaults to
0 = main for all currently-resting orders).

### Effort estimate (Phase 2e)

| Slice | LOC |
|---|---|
| `RestingOrderV2.sub_index` field + (de)serialisation | ~80 |
| PlaceLimitOrderV2 / PlaceTakerOrderV2 ix param + write | ~120 |
| ApplyFill / ApplyFlpFill: handler-side trader_state PDA derivation | ~200 |
| Hypertree migration ix (sub_index defaults to 0 for legacy nodes) | ~150 |
| SDK updates (PlaceLimit/PlaceTaker builders + RestingOrderV2 decoder) | ~120 |
| New tests (sub places order → sub gets the fill) | ~250 |
| MARGIN_MATH + ARCHITECTURE update | ~50 |
| **Total** | **~970** |

Best executed across 3-4 commits, each independently auditable.
