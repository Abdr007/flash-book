# H-Haircut Math

Formal spec for clober's **junior-claim profit gating** primitive,
in clober's lot/decimal conventions (`USD_UNIT = 10^6`,
`BPS_DENOM = 10^4`).

The pure-function core lives at
`programs/clober/src/matcher/haircut.rs`; this document is the
normative reference for that module's behaviour and for its wire-in to
`apply_realized_pnl_delta`.

## 1. Goal

Make solvency of profit extraction a **structural property of the
engine**, not a "we hope the protocol stays funded" property.

Specifically: at any point in time, for any sequence of legal
operations, the sum of every trader's extractable positive PnL is
bounded above by the protocol's actual residual balance sheet.

> **Invariant 1 (solvency).**
> For any market and any history of operations, the cumulative
> credited positive PnL across all positions is ≤ the cumulative
> Residual the market has ever held.

## 2. Definitions

| Symbol | Definition |
|---|---|
| `V` | Vault total: trader deposits + LP capital + insurance fund + accrued fees, in quote lots. |
| `C_tot` | Committed trader collateral: Σ over all positions of `position.collateral_quote_lots` + `trader_state.collateral_quote_lots`. |
| `I` | Insurance fund balance, quote lots. |
| `Residual` | `V − C_tot − I`, quote lots. The protocol's surplus available to back positive PnL. |
| `MaturedPos_i` | Per-position matured positive PnL, quote lots. |
| `MaturedPosTotal` | `Σ_i MaturedPos_i` across all positions in the market. |
| `Reserve_i` | Per-position positive PnL not yet matured. |
| `h` | The haircut ratio (next section). |
| `H_DENOM` | `10^9`. Fixed-point denominator for h. |
| `h_min`, `h_max` | Warmup window endpoints, in slots. |

`V` and `I` are not stored as explicit fields today — they are SPL
token-account balances. For the on-chain math we **delta-track**
Residual: every money-moving ix adjusts it by the same delta it adjusts
the vault/collateral/insurance balances. The init handler seeds
Residual from the live balances at migration time, and a sanity
checker can periodically verify
`Residual ≡ V − C_tot − I` from on-chain balances.

## 3. The haircut ratio

```
h = min(Residual, MaturedPosTotal) / MaturedPosTotal     (if MaturedPosTotal > 0)
h = 1                                                    (if MaturedPosTotal = 0)
```

Stored on-chain as `h_scaled = floor(h × H_DENOM)`. Always in
`[0, H_DENOM]`.

- `h = 1` (full backing): every trader sees 100% of matured profit.
- `h < 1` (under-backing): every trader sees the same fraction; nobody
  is favoured.
- `h = 0` (no residual): matured profit cannot be extracted; it stays
  in `MaturedPos` until residual recovers.

Capital is **never** haircut. Losses bypass this pipeline entirely.

## 4. The reserve → mature → convert pipeline

```
        realized positive PnL delta
                  │
        apply_release(delta, now_slot)
                  ▼
            Reserve_i grows
                  │  warmup over [h_min, h_max] slots
        apply_mature(now_slot, h_min, h_max)
                  ▼
            MaturedPos_i grows by matured_delta
            MaturedPosTotal += matured_delta
                  │
        apply_convert(h_scaled)
                  ▼
            credit_i = floor(MaturedPos_i × h_scaled / H_DENOM)
            dust_i   = MaturedPos_i − credit_i
            MaturedPos_i = 0
            collateral += credit_i
            dust_accrued += dust_i
            Residual    −= credit_i
            MaturedPosTotal −= (credit_i + dust_i)
```

Losses are senior:

```
        realized negative PnL delta
                  ▼
            collateral −= |delta|   (saturating on isolated, checked on cross)
            Residual    += |delta|
            realized_loss_total += |delta|
```

### 4.1 Warmup function

For an account whose reserve was first non-zero at slot `s_0`, the
matured fraction at slot `s` is:

```
matured_fraction(s) =
    0                                   if s − s_0 < h_min
    (s − s_0 − h_min) / (h_max − h_min) if h_min ≤ s − s_0 < h_max
    1                                   if s − s_0 ≥ h_max
```

Floor-rounded when applied to the reserve amount.

### 4.2 Reserve clock + original-reserve anchor

The reserve's attachment slot `s_0` is set when the reserve transitions
from 0 to positive. Subsequent gains while reserve > 0 **do not reset
the clock** — they ride the existing warmup. This is intentional: a
trader cannot reset their warmup by drip-feeding tiny realizations.

The clock is cleared (set back to 0) only when the reserve fully drains
to 0. The next non-zero release starts a fresh warmup.

Each position also tracks `OriginalReserveAtAttach` — the total reserve
amount in the current warmup window (incremented by subsequent releases
while warming, cleared on full drain). `apply_mature` uses this anchor
to compute the **target cumulative matured amount**:

```
target_cumulative = matured_fraction(OriginalReserveAtAttach, s_0, s, h_min, h_max)
already_drained   = OriginalReserveAtAttach − reserve
mature_delta      = max(0, target_cumulative − already_drained)
```

This makes `apply_mature` **idempotent at the same slot** — repeated
calls produce zero delta after the first. Without the anchor, repeated
calls would each drain a fraction of the *current* reserve, bypassing
the intended warmup schedule. Caught by
`wave24b_haircut_ix::mature_idempotent_at_same_slot`.

## 5. Conservation

For every convert step, **matured = credit + dust, exact**. No bits
lost. The dust accumulates on the market state in
`dust_accrued_quote_lots`, drained to the insurance fund by a
permissionless `flush_haircut_dust` ix.

> **Invariant 4 (conservation).**
> For every call to `apply_convert(matured, h)`,
> `convert(matured, h).credit + convert(matured, h).dust == matured`.

## 6. Floor-rounding monotonicity

A key subtlety in distributing scaled amounts across many accounts: if
we use floor on each individually, the sum-of-floors can be less than
the floor-of-sum. We accept this gap as the *dust*. The total credited
across all positions is therefore:

```
Σ_i floor(MaturedPos_i × h)  ≤  floor(MaturedPosTotal × h)
```

This guarantees solvency: the protocol never overpays. The "missing"
satoshis from individual flooring are routed to the insurance fund, so
no value is destroyed.

> **Invariant 2 (floor-monotonicity).**
> If Residual grows (e.g. via fees or new deposits), no account's next
> convert credit shrinks.

## 7. Edge cases

### 7.1 MaturedPosTotal = 0

`h = 1` by convention. No effect on any account (every account has
`matured_pos_quote_lots = 0`).

### 7.2 Residual < 0

Cannot happen: Residual is delta-tracked from a non-negative seed and
every operation that could decrement it is gated by `checked_sub` on
the wire-in side. If a sanity check ever observes
`Residual < 0`, the kill switch (`set_market_status(Paused)`) is
tripped.

### 7.3 h_min = h_max

Degenerate but legal: reserves mature instantly once `elapsed ≥ h_min`.
Useful for fully-trusted environments (e.g. ER-internal markets) or
for tests.

### 7.4 Concurrent matures + new releases

The reserve clock only advances; never rewinds. New gains while old
reserves are still maturing extend the reserve linearly but do not
interrupt the warmup of existing dollars. A partial mature drains the
reserve by the matured fraction; the remaining dollars continue on
the same clock.

### 7.5 Position close while reserve > 0

The reserve and matured_pos are sibling-PDA state. Closing the
position does not destroy them — the PDAs persist until a separate
`reclaim_haircut_state` ix is called, which fails if either field is
non-zero. This prevents profit being lost if the trader closes their
position before warmup completes.

### 7.6 Flat-account safety

> **Invariant 5 (flat-account safety).**
> An account with `released_reserve = 0` and `matured_pos = 0` is
> unaffected by any value of `h`, by any change in Residual, and by
> any change in MaturedPosTotal driven by other accounts.

This is the structural reason capital is never haircut: capital lives
on `trader_state.collateral_quote_lots` and
`position.collateral_quote_lots`, both of which are entirely outside
the H pipeline.

## 8. Loss seniority

Losses are realized to capital immediately via the existing
`compute_realized_pnl_routing` path. They never touch the H pipeline.

> **Invariant 6 (loss-seniority).**
> No debit operation reads or writes `released_reserve` or
> `matured_pos` on any position; losses settle exclusively against
> collateral buckets.

This is the structural reason H is a **junior**-claim mechanism: gains
get gated; losses get applied; capital absorbs the asymmetry.

## 9. Compute cost

All H ops are O(1) per position. The market-level state update is one
atomic accrual to four u128 counters. Compute budget for the full
release → mature → convert pipeline is bounded by a fixed constant
independent of the number of positions in the market.

This is what makes H compatible with the matcher tick on the ER — the
hot path stays O(1) per fill.

## 10. Invariants under proptest

Each invariant in §2-§7 is encoded as a property in
`programs/clober/tests/proptest_haircut.rs`. The current suite runs
each property over 2000 random cases.

| Invariant | Property file | Property name |
|---|---|---|
| 1. Solvency | `proptest_haircut.rs` | `sum_credits_le_residual_ever_seen` |
| 2. Floor-monotonicity | `proptest_haircut.rs` | `credit_monotonic_in_residual` |
| 3. Warmup respects window | `haircut.rs` unit tests | `matured_fraction_warmup` |
| 4. Conservation | `proptest_haircut.rs` | `convert_credit_plus_dust_eq_matured` |
| 5. Flat-account safety | `proptest_haircut.rs` | `flat_account_unaffected_by_h` |
| 6. Loss-seniority | `proptest_haircut.rs` | `loss_never_touches_reserve_or_matured` |

## 11. References

- Tarun Chitra, *Autodeleveraging: Impossibilities and Optimization*, arXiv:2512.01112 — motivation for the junior-claim design (Hyperliquid Oct 10 2025 ADL overshoot).
- The "realized PnL doesn't materialise to collateral on close" failure mode this primitive structurally resolves (see `docs/MARGIN_MATH.md`).

## 12. Wire-in (integration contract)

The on-chain wire-in consists of:

1. **Init**: `initialize_haircut_state(market)` seeds
   `MarketHaircutStateAccount` with the Residual derived from the live
   vault / collateral / insurance balances, and enables the engine on
   the market (sticky).
2. **`apply_realized_pnl_delta` positive-gain path**: on
   haircut-enabled markets, positive deltas route through
   `apply_release` onto a `PositionHaircutStateAccount` instead of
   crediting collateral directly. Losses (negative deltas) keep the
   direct path.
3. **`mature_position(market, position)`** (permissionless): advances
   `released_reserve → matured_pos` for a position.
4. **`convert_position(market, position)`**: converts the position's
   `matured_pos` to collateral at the current `h`.
5. **`flush_haircut_dust(market)`** (permissionless): drains
   `dust_accrued_quote_lots` to the insurance fund.
6. **Residual delta-tracking hooks**:
   - `deposit_collateral`: Residual += deposit_amount only if the
     deposit goes to the vault surplus, not to a position bucket
     (it doesn't — collateral deposits go to `trader_state` or
     `position`, which lives in C_tot). So **no** Residual change.
     Insurance contributions DO increment Residual (they raise V
     and I equally, and Residual = V - C_tot - I leaves Residual
     unchanged in this specific case — confirm before shipping).
   - `withdraw_collateral`: same — no Residual change for
     ordinary collateral withdrawals.
   - Fee accrual to insurance / LP: increases V and (insurance ↑ or
     LP capital ↑ which is part of V). For LP fees, Residual
     **increases** by the fee amount. For insurance fees, Residual
     stays flat.
   - Liquidation reward to liquidator: decreases V and
     decreases C_tot (the liquidated position's collateral is
     debited). Residual change = -reward + collateral_freed.
     Net: Residual increases by `collateral_freed - reward` ≥ 0
     when reward ≤ collateral_freed.
   - The single source of truth for these adjustments is the
     `apply_residual_delta()` helper that money-moving paths call.
7. **Sanity check ix**: `verify_haircut_invariants(market)` reads
   on-chain balances, recomputes Residual, asserts equality with
   `MarketHaircutStateAccount.residual_quote_lots` within a tolerance,
   trips the kill switch if not.

The pure math and the wire-in are reviewable independently: the module
owns no account types, and the handlers are direct passthroughs to the
proven pure functions.
