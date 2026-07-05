# Contributing

This repository is the on-chain Solana program (Rust + Anchor). The rules
below apply to it.

## Development setup

```bash
# Build + test the program
cargo test -p flash-book
cargo build-sbf --manifest-path programs/flash-book/Cargo.toml

# Formal-verification proofs (one-time: cargo install --locked kani-verifier && cargo kani setup)
cargo kani --features no-entrypoint

# Regenerate the IDL after Anchor program changes
anchor idl build -o idl/flash_book.json
```

## Ground rules

### Math / numerical safety

- **Integer arithmetic only in the matcher and risk modules.** Every
  `size × price × tick_size` is computed in `u128` with `checked_mul`;
  final casts to `u64` saturate at `u64::MAX` or return
  `FlashBookError::ArithmeticOverflow`.
- **`i128` for signed sums** (PnL, funding); clamp to `i64` only at
  output boundaries.
- **No floats on-chain.**
- Document floor/ceil rounding direction per math module.

### Tests are required

- Every PR ships at least one test demonstrating the change.
- Risk / margin / liquidation: property tests preferred — mirror the
  existing `programs/flash-book/tests/proptest_*.rs` patterns.
- Anchor handlers: integration tests in
  `programs/flash-book/tests/integration.rs`.
- Math touching haircut conservation / solvency: extend the Kani proofs
  in `programs/flash-book/src/matcher/haircut.rs` (`#[cfg(kani)]`). See
  [`docs/FORMAL_VERIFICATION.md`](docs/FORMAL_VERIFICATION.md).

### Rust / Anchor

- Re-use the existing error codes in `errors.rs` where semantically
  correct rather than minting new ones.
- For Accounts structs that exceed BPF's 4096-byte stack frame,
  `Box<Account<...>>` the heavier members — `cargo build-sbf` warns when
  you cross the line.
- Prefer `#[account(zero_copy)]` + `AccountLoader` for hot-path accounts
  to avoid Borsh ser/deser CU (see [`docs/SETTLEMENT.md`](docs/SETTLEMENT.md)).
  Pod layouts must have no implicit padding and no `u128` (host/SBF
  alignment differ).
- Regenerate the IDL (`anchor idl build -o idl/flash_book.json`) in the
  same PR as any instruction/account change.

### Documentation

- `docs/MARGIN_MATH.md` and `docs/HAIRCUT_MATH.md` are the audit-grade
  specs; update the relevant section with any risk/margin/liquidation
  change.

## Commit style

```
<scope>: <imperative summary>

<body: why it's needed, what changed, follow-ups / known limits>
```

Scopes: `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `chore`, plus
the module where applicable (`matcher`, `risk`, `liquidation`, `funding`,
`insurance`, `cu`, `fv`).

## Pull requests

- All gates must pass: `cargo test -p flash-book`, `cargo build-sbf` (no
  stack-frame warnings), and `cargo kani --features no-entrypoint`.
- Regenerate the IDL for any instruction/account change.
- Mark draft PRs `[wip]`. CI runs anyway.

## Reporting safety issues

Do not open public issues for safety vulnerabilities — see
[`SECURITY.md`](SECURITY.md).
