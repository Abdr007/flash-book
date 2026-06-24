# flash-book-pin — Pinocchio port (WIP migration)

Zero-copy, zero-allocation `no_std` reimplementation of the Flash Book orderbook
on [Pinocchio](https://github.com/anza-xyz/pinocchio). Accounts are pointer-cast
directly from the SVM input buffer — no Borsh, no heap — eliminating the Anchor
framework overhead that dominates the hot path.

## Why (measured)

`apply_fill` is the matcher settlement hot path. Same account surface, same
`solana-program-test` CU harness:

| Implementation | `apply_fill` CU |
|---|---|
| Anchor (current `programs/flash-book`) | **37,779** |
| **Pinocchio (this crate, core path)** | **1,520** |

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
- ✅ `apply_fill` core (1 of 112 instructions), builds clean + measured above.
- ⬜ De-anchor the 36 `matcher/` modules into a shared `no_std` core (9 currently
  pull `anchor_lang` for `Pubkey`).
- ⬜ Remaining 111 instructions; events; CPI (token, ER); IDL/client.
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
