# Flash Book

An open-source perpetual-futures DEX on Solana: a continuous central limit
order book (CLOB) on a hypertree, a risk engine with machine-checked
solvency invariants, and a library of order types and risk controls.

**Devnet. Not audited. Not production-ready.**
Program ID `5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq` (see `Anchor.toml`).

This repository is the **on-chain program only**. Clients, bots, keepers,
and simulators are intentionally not included — build them against the IDL.

## Verified status

Everything below is reproducible from this repo with the commands shown.

| Check | Command | Result |
|---|---|---|
| Build | `cargo build-sbf --manifest-path programs/flash-book/Cargo.toml` | clean |
| Tests | `cargo test -p flash-book` | 569 pass |
| Proofs | `cargo kani --features no-entrypoint` | 5 verified |
| CU bench | `BPF_OUT_DIR=$PWD/target/deploy cargo test -p flash-book --test integration cu_benchmark -- --ignored --nocapture` | `apply_fill` open ≈ 42k CU |

- **Formal verification.** Kani/CBMC proofs of the haircut accounting:
  dust conservation (`credit + dust == matured`, `credit ≤ matured`),
  single-convert solvency (`credit ≤ residual`), and `matured_fraction`
  bounds. Details + method: [`docs/FORMAL_VERIFICATION.md`](docs/FORMAL_VERIFICATION.md).
- **Compute units.** `apply_fill` measured and reduced ~24–28% by removing
  a `find_program_address` bump-search from the hot path. Full breakdown:
  [`docs/CU_OPTIMIZATION.md`](docs/CU_OPTIMIZATION.md).

## What's in the box

```
programs/flash-book/   on-chain Solana program (Rust + Anchor)
idl/                   generated program IDL (interface descriptor)
docs/                  architecture, math, formal-verification, deployment
```

## What it is

A Solana program implementing a perpetual-futures orderbook:

- **Hypertree-backed continuous CLOB** — a `market_book` PDA backed by a
  zero-copy red-black tree, accessed via raw byte slicing on the matcher
  hot path (no Anchor deserialization). Grows in place past its initial
  capacity via `expand_market_book`.
- **Risk engine** — H-haircut junior-claim PnL gating (machine-checked
  solvency), A/K/F/B cumulative side indices for lazy O(1) per-position
  settlement, an initialization-time per-slot envelope check that rejects
  unsafe market parameters, and stress-lattice scenario margin.
- **Order types & controls** — limit, market, IOC, FOK, post-only, trigger
  (stop / take-profit) with slippage caps, TWAP, iceberg, OCO brackets,
  peg, MIT, trailing-stop, reduce-only, min-fill-size, conditional-cancel.
- **Liquidation** — JIT-auction synthetic close with a Dutch reward,
  per-position cooldown, and a dual-source `worse-of(mark, oracle)` price
  gate.
- **Anti-MEV** — self-trade prevention, VPIN toxicity gating on the FLP
  quoter, vol-adaptive oracle band, aggressor round-trip tax.
- **Decentralization** — `burn_market_authority` permanently relinquishes
  per-market authority (one-way).
- **MagicBlock ER** — delegate/commit/undelegate instructions for running
  the matcher on an Ephemeral Rollup (devnet integration).

See [`docs/FEATURES.md`](docs/FEATURES.md) for the module matrix and
[`docs/INSTRUCTIONS.md`](docs/INSTRUCTIONS.md) for the instruction set.

## What it is NOT

- **Not on mainnet.** Devnet only.
- **Not externally audited.** Internal audit only — [`docs/AUDIT.md`](docs/AUDIT.md).
- **No off-chain components in this repo** (SDK, bot, keepers, simulator).
- **No FBA / commit-reveal.** The continuous CLOB is the deliberate pick.
- **The off-chain sequencer is a single point of trust** for fill ordering
  (it is authenticated on-chain and cannot route fills to the wrong
  account, but can reorder/censor). See [`SECURITY.md`](SECURITY.md).

## Build, test, verify

```bash
# Build + test
cargo build-sbf --manifest-path programs/flash-book/Cargo.toml
cargo test -p flash-book

# Formal-verification proofs (one-time: cargo install --locked kani-verifier && cargo kani setup)
cargo kani --features no-entrypoint

# Regenerate the IDL after on-chain changes
anchor idl build -o idl/flash_book.json

# Devnet deploy (requires a funded keypair)
anchor deploy --program-name flash_book --provider.cluster devnet
```

Staged path to mainnet: [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

## Documentation

**Design & math** — [`ARCHITECTURE`](docs/ARCHITECTURE.md) ·
[`MATH`](docs/MATH.md) · [`MARGIN_MATH`](docs/MARGIN_MATH.md) ·
[`HAIRCUT_MATH`](docs/HAIRCUT_MATH.md) ·
[`FORMAL_VERIFICATION`](docs/FORMAL_VERIFICATION.md) ·
[`FEATURES`](docs/FEATURES.md) · [`INSTRUCTIONS`](docs/INSTRUCTIONS.md) ·
[`SUB_ACCOUNT_TRADING`](docs/SUB_ACCOUNT_TRADING.md)

**Operations** — [`DEPLOYMENT`](docs/DEPLOYMENT.md) ·
[`PARAMETER_PLAYBOOK`](docs/PARAMETER_PLAYBOOK.md) ·
[`PYTH_INTEGRATION`](docs/PYTH_INTEGRATION.md) ·
[`INCIDENT_RESPONSE`](docs/INCIDENT_RESPONSE.md) ·
[`LP_GUIDE`](docs/LP_GUIDE.md)

**Audit & performance** — [`AUDIT`](docs/AUDIT.md) ·
[`SAFETY`](docs/SAFETY.md) · [`CU_OPTIMIZATION`](docs/CU_OPTIMIZATION.md)

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

See [`LICENSE`](LICENSE). The vendored hypertree under
`programs/flash-book/src/hypertree/` is GPL-3.0 —
[`LICENSE-HYPERTREE`](LICENSE-HYPERTREE).

## Disclaimer

Open-source research and engineering output. Not financial advice, not a
production system, not a solicitation to deposit capital. Mainnet is gated
on an external audit.
