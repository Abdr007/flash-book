# Contributing

Thanks for considering a contribution. This project ships an on-chain
Solana program (Rust + Anchor) plus a TypeScript SDK + bot suite. The
ground rules below apply to both.

## Development setup

```bash
# TypeScript side
bun install
bun test
bunx tsc --noEmit
bun run --cwd sdk-ts typecheck

# Rust on-chain side
cargo test -p flash-book
cargo build-sbf --manifest-path programs/flash-book/Cargo.toml

# Regenerate IDL after Anchor program changes
anchor idl build -p flash_book > sdk-ts/idl.json
cp sdk-ts/idl.json idl/flash_book.json
```

## Ground rules

### Math / numerical safety

- **Integer arithmetic only in the matcher and risk modules.** Every
  multiplication of `size × price × tick_size` must be done in `u128`
  with `checked_mul`. Final casts to `u64` saturate at `u64::MAX` or
  return `FlashBookError::ArithmeticOverflow` per the existing
  convention.
- **`i128` for signed sums** (PnL, funding) so intermediate adds don't
  overflow. Clamp to `i64` only at output boundaries.
- **No floats anywhere on-chain.** Off-chain (bot, simulator) is fine.
- **Use the existing helpers.** `BPS_DENOM`, `safeNumber()`,
  `Number.isFinite()` guards in TS — don't roll your own.
- The matcher operates on comparable prices/sizes. If you introduce
  arithmetic that depends on operand order, prove it doesn't change
  clearing outcomes.

### Tests are required

- Every PR ships at least one test demonstrating the change.
- Risk / margin / liquidation code: property tests preferred. Mirror
  the existing `tests/proptest_risk.rs` / `proptest_isolated.rs` /
  `proptest_liquidation.rs` patterns at 2000 random cases per property.
- Anchor handlers: integration tests under
  `programs/flash-book/tests/integration.rs`.
- New SDK ix builders: a happy-path + wrong-trader rejection test.

### TypeScript

- Strict mode plus `noUncheckedIndexedAccess` and
  `exactOptionalPropertyTypes`. Don't paper over with `any` or `!`.
  Index access returns `T | undefined`; handle it.
- All randomness comes through the seeded `Prng`. No `Math.random()` in
  source files. Tests may use it for fuzz inputs but must assert
  invariants, not specific values.

### Rust / Anchor

- Match the existing error-code family in `errors.rs`. Re-use existing
  codes where semantically correct rather than minting new ones.
- For Accounts structs that exceed BPF's 4096-byte stack frame for the
  auto-generated `try_accounts`, `Box<Account<...>>` the heavier
  members. `cargo build-sbf` will warn explicitly when you've crossed
  the line.
- Adding a field to an existing on-chain account: if the new field is
  appended at the end of the struct, the change is layout-compatible —
  legacy accounts read the trailing zero bytes as zero-valued. If you
  need to insert / reorder, ship a migration ix.
- Regenerate the IDL (`anchor idl build`) and commit both `idl/` and
  `sdk-ts/idl.json` in the same PR.

### Documentation

- `docs/MARGIN_MATH.md` is the audit-grade margin spec. Any change to
  risk / margin / liquidation logic must update the corresponding
  section. New invariants get a row in §9.
- `docs/SUB_ACCOUNT_TRADING.md` tracks the Phase 2 sub-account work.
- `docs/COMPARISON.md` claims must be backed by file:line references
  into `programs/flash-book/`. Marketing-grade claims are rejected.

## Commit style

```
<scope>: <imperative summary>

<body explaining why the change is needed; what changed at a high
level; any follow-ups or known limitations>
```

Common scopes:

- `feat` — user-facing capability
- `fix` — bug fix
- `refactor` — no behavior change
- `docs` — documentation only
- `test` — test additions / changes
- `chore` — build, CI, deps
- Plus the module name where applicable: `matcher`, `risk`,
  `liquidation`, `funding`, `insurance`, `flp-quoter`, `sdk`, `bot`

Use HEREDOC-style commit messages for multi-paragraph bodies (see
`git log --oneline` for examples).

## Pull requests

- Open an issue first for non-trivial changes.
- All tests must pass: `cargo test -p flash-book` + `bun test` +
  `bun run --cwd sdk-ts typecheck`.
- `cargo build-sbf` must complete without stack-frame warnings.
- New Anchor ixs require regenerating the IDL and updating
  `sdk-ts/src/client.ts` with a builder helper.
- Mark draft PRs `[wip]` in the title. CI runs anyway.

## Reporting safety issues

Do not open public issues for safety vulnerabilities. See
[SECURITY.md](SECURITY.md) for the disclosure policy.
