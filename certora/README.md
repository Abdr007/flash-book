# Clober — Certora / Formal Property Specification

`PROPERTIES.md` is the protocol's formal property set — the invariants Clober
must satisfy, each tagged with its current verification status:

- **`[KANI]`** / **`[LEAN]`** — machine-proven *today* (reproducible, see below).
- **`[CERTORA-TARGET]`** — stated precisely, to be discharged by the **Certora
  Prover for Solana** (the production bar set by Manifest & Kamino). The Certora
  Prover requires a license and is **not run in this environment**, so this
  directory ships the *specification*, not yet a Certora run.
- **`[REQUIRE]`** — enforced today by an on-chain `require!`/constraint.

This mirrors how production Solana protocols ship a `certora/` directory: the
property set is version-controlled alongside the program and discharged in CI
once a license is wired.

## Reproduce the proofs that run today
```bash
cargo kani --package clober --features no-entrypoint   # Kani harnesses
cd formal_verification/lean && lake build                  # Lean (4.24 + Mathlib)
```

No marketing, no overclaim: the only "proven" rows are the `[KANI]`/`[LEAN]` ones.
