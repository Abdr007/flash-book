# flash-book-pin — Pinocchio port (WIP migration)

Zero-copy, zero-allocation `no_std` reimplementation of the Flash Book orderbook
on [Pinocchio](https://github.com/anza-xyz/pinocchio). Accounts are pointer-cast
directly from the SVM input buffer — no Borsh, no heap — eliminating the Anchor
framework overhead that dominates the hot path.

## Why (measured)

`apply_fill` is the matcher settlement hot path. Same account surface, same
`solana-program-test` CU harness:

| Instruction | Anchor CU | Pinocchio CU | Δ |
|---|---|---|---|
| `apply_fill` | 37,779 | **1,469** | **−96%** |
| `settle_funding` | ~5,050 | **676** | **−87%** |
| `place_limit_order` | ~12,500 | **411** | **−97%** |
| `cancel_order` | ~12,500 | **550** | **−96%** |
| `place_taker_order` (crosses 3 resting) | ~12,500+ | **1,166** | **≈−90%** |
| `place_taker_order` (rests, empty book) | ~12,500 | **899** | **≈−93%** |
| `modify_order` (cancel + replace) | ~12,500 | **931** | **≈−93%** |
| `cancel_all` (3 orders, both sides) | ~12,500 | **1,315** | **≈−90%** |

(Measured on the same `solana-program-test` harness, real account sizes. The
matcher math is host-unit-tested for exact equivalence with the Anchor version.
`place`/`cancel` are the **floor** cases — insert into an empty book / remove a
single resting order; a deep book adds RB-tree traversal CU, but the framework
overhead Pinocchio eliminates is the dominant, constant term either way. The
taker walk's CU scales with the levels crossed — 1,166 CU for a 3-level walk
vs Anchor's ~12.5k+ for the same.)

~28k of the difference is pure Anchor framework (Borsh deser + reserialize +
entrypoint). The core matcher math is identical (ported from
`apply_fill_to_position`). A feature-complete port (adding the event emit,
funding-index settlement, realized-PnL materialization, and the optional
fee-tier/haircut/referral paths) lands in the low single-digit thousands —
still an ~85–90% reduction, matching the published SPL-token Pinocchio result.

## Status

- ✅ Foundation: `no_std` entrypoint, 1-byte instruction dispatch, Pod account
  layouts (8-aligned, `[u8;16]` for the i128 funding index — a native 128-bit
  field is incompatible with the disc+8 data offset).
- ✅ `apply_fill` + `settle_funding` + `place_limit_order` + `cancel_order`
  + `place_taker_order` + `modify_order` + `cancel_all` (**7 of 112**), build
  clean, measured above; the ported math is host-equivalence-tested.
- ✅ **Taker matching walk ported** — best-first cross, self-trade prevention
  (skip / cancel-oldest / cancel-both), expiry skip, post-only/IOC/FOK,
  residual-rest-as-limit; matches collected in fixed-size stack buffers (no
  heap), bounded at 64 levels/ix.
- ✅ **Hypertree ported** (matching-engine RB-tree core, 4,138 lines) — 38 tests pass.
- ✅ **MarketBookHandle ported** (book account wrapper + zero-copy book types,
  de-anchored via a compat shim) — 17 RBT/best-bid-ask/expand tests pass.
- ✅ **Pure `matcher/` modules de-anchored** into the shared `no_std` core,
  ported verbatim with their full test vectors:
  - `fees.rs` — `resolve_fee_tier` + `tier_index_for_volume` (6 tests)
  - `borrow_fee.rs` — utilization borrow-rate / cum-index / settle (9 tests)
  - `concentration.rs` — per-trader OI share cap (5 tests)
  - `position_cap.rs` — per-trader notional cap + max-incremental (8 tests)
  - `daily_loss_limit.rs` — session loss-limit halt-opens gate (6 tests)
  - `min_fill_size.rs` — minimum-fill-size gate (5 tests)
  - `reduce_only.rs` — reduce-only intake check (6 tests)
  - `funding_velocity.rs` — funding ramp-rate / skew-target (11 tests)
  - `constants.rs` — `BPS_DENOM`
  These unblock the fee/risk paths in `apply_fill` / `apply_flp_fill` / intake.
- ⬜ De-anchor the remaining `matcher/` modules into the shared `no_std`
  core (several still pull `anchor_lang` for `Pubkey`).
- ⬜ Remaining 105 instructions; events; CPI (token, ER); IDL/client.
- ⬜ Re-pass the full functional suite (569 tests) + 5 Kani proofs against the port.

## Build

```bash
cargo build-sbf --manifest-path programs/flash-book-pin/Cargo.toml
```

Isolated workspace — excluded from the main Anchor workspace so it can never
affect the production program's build or lockfile. Uses `pinocchio 0.8.4` (the
SBF toolchain here is rustc 1.84; pinocchio 0.11 needs 1.89).

## Roadmap

See `docs/QUASAR_MIGRATION_SCOPE.md`. Recommended sequencing: external-audit the
current Anchor program first, then complete this port as a dedicated project.
