#!/usr/bin/env bash
# Certora Solana Prover build script for the P-SOLV-4 solvency harness.
#
# SCAFFOLD — requires a Certora Solana license and the `cvlr` SDK. This builds
# the program crate (plus the `certora` harness feature) to SBF bytecode that
# `certoraRun certora/solana_solvency.conf` submits to the Prover. Without the
# licensed toolchain this is not exercised; the production build/CI never run it.
set -euo pipefail

cd "$(dirname "$0")/../.."

# The harness (certora/harness/solvency_rules.rs) is `#[cfg(feature="certora")]`
# and pulls the proprietary `cvlr` SDK; the operator wires it as a verification
# dependency under that feature before running (see harness/README.md).
cargo build-sbf \
  --tools-version v1.52 \
  --manifest-path programs/flash-book/Cargo.toml \
  --features certora \
  --sbf-out-dir target/certora
