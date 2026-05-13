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

- **Not started.** The trade-path Accounts structs are unchanged.
- Sub-accounts (from Phase 1) remain collateral-only. They can be
  created and exchange balance with `main_trader_state` via
  `transfer_main_to_sub` / `transfer_sub_to_main`. They cannot be the
  `trader_state` of any trade-path instruction.

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
update its derivation. The PDA helper in `sdk-ts/src/pdas.ts`
(`positionPda`) gets a new signature:

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

1. Owner picks A / B / C.
2. If A or B: stand up the migration ix + tests in commit 1.
3. Relax the 15 trade-path Accounts structs in commits 2-4 (grouped
   by domain: collateral mgmt → order placement → fills → liquidation).
4. Position PDA migration in commit 5 (only A/B).
5. SDK + IDL in commit 6.

Each commit independently buildable, testable, and reviewable. **Do
not bundle.**
