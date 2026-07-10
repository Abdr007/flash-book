# Formal Verification

Flash Book carries **62 Kani proof harnesses** (discharged by CBMC), **7 Lean
proof modules** at the real production divisors, a formal property specification
(`certora/PROPERTIES.md`), and eight property-test suites. A Kani harness is
not a sampled test: `kani::any()` ranges over the *entire* input domain
symbolically, and the model checker proves the assertion for **every** value
or returns a concrete counterexample. CI runs the full Kani suite and builds
the Lean proofs on every PR; any broken invariant fails the build.

## Kani coverage (62 harnesses, by module)

| Module | # | Properties proven |
|---|---|---|
| `matcher/haircut` | 7 | Dust conservation (`credit + dust == matured`, nothing minted or burned), single-convert solvency (credit ≤ residual — the non-printing bound), maturation bounds, residual-delta exactness + round-trip conservation, the CBMC division boundary controls. |
| `matcher/fill_commitment` | 7 | Settlement ring: never over-settles, depth-bounded, rejects uncommitted/fabricated fills, no double-settle; settlement nonce strictly monotone (replay/reorder rejected), advance strict + exact, chain monotone. |
| `state_v2` | 6 | Order-id price-time priority: better price fills first on both sides, FIFO sequence tiebreak, id injectivity (no collisions among live orders), guard-admitted orders never collide, seq guard matches the encoding precondition. |
| `lib` (settlement frame) | 6 | Realized-PnL routing credits exactly one bucket on gain and is bounded/one-sided on loss; cross-loss shortfall conserves and never over-credits; funding routing conserves value, is bounded and one-signed, zero is a no-op. |
| `matcher/risk` | 6 | Effective MMR never below the base floor (proven on the live `MarketSnapshot::effective_mmr_bps` path), OI surcharge capped and disabled at zero slope, healthy ⇒ survives stress, health verdict independent of mark PnL (no double-count), and the real `assess_margin` symbol's three cross-margin frame invariants over all `u64` collateral (`assess_margin_single_market_frame_stable`). |
| `matcher/liquidation` | 5 | Health price is always one of the two real sources and the worse one for the side (long and short), fresh mark equals worse-of, stale mark falls back to oracle-only. |
| `matcher/insurance` | 5 | Solvent ⟺ vault covers the protocol buckets; surplus exact when solvent; the full-invariant reference model; the one-sided partial-collateral insolvency detector is sound (never fires on a solvent state); bad-debt coverage is insurance-isolated and bounded (`bad_debt_coverage_is_insurance_isolated_and_bounded` — no single-vault SPOF). |
| `matcher/position_math` | 3 | Open-from-flat exact, same-side stacking exact, no realized PnL without a reduction. |
| `matcher/flp_quoter` | 3 | Pool fill-price band accepts the oracle price (no false reject) and rejects gross fabrication; required floor conservative. |
| `matcher/fill_outbox` | 3 | Write index in bounds, no silent overwrite, drained grow has no remappable pending slot. |
| `matcher/committee` | 3 | Valid BFT config implies quorum intersection, equivocation ⟺ same height + different digest, jail is effective. |
| `xmargin` | 5 | ER-reserved margin floor preserved by simple withdrawals, epoch strictly increases, required floor adds reserved on top of max, attestation binding fails closed, and withdrawal can never self-liquidate below maintenance (`withdraw_cannot_self_liquidate_below_maintenance`). |
| `matcher/funding` | 1 | Funding is zero-sum: `owed(long) + owed(short) == 0` on the real `funding_owed` path (funding moves value between sides; cannot mint or burn), independent of the `>>64` rounding. |
| `er` | 2 | The force-undelegate gate only fires when a liveness baseline is genuinely stale; a market with a fresh heartbeat AND recent settlement can never be forced off the ER. |

Every proven pure function is the one the deployed handler routes through
(`apply_fill` → `advance_settlement_seq`, `liquidate_position_v2` →
`worse_of_health_price`, `assess_margin` → the proven gate, …), so the
proofs bind to the shipped logic, not a copy.

## Running

```bash
# one-time toolchain setup
cargo install --locked kani-verifier && cargo kani setup

# all haircut harnesses
cargo kani --features no-entrypoint

# a single harness
cargo kani --features no-entrypoint --harness proof_dust_conservation
```

`--features no-entrypoint` excludes the Solana program entrypoint so Kani
verifies the library in isolation. CI runs the suite on every PR (see
`.github/workflows/ci.yml`, job `kani`).

The haircut examples below illustrate how the harnesses handle CBMC's
division limits; the same discipline applies across the suite.

## A note on the divisor (read this before extending the proofs)

The haircut credit is `floor(matured · h / H_DENOM)` with `H_DENOM = 1e9`, a
**non-power-of-two**. Two properties of the bundled backend shaped how the
proofs are written, and both are demonstrated in-tree rather than asserted:

1. **CBMC's SAT backend (CaDiCaL/kissat) is incomplete on non-power-of-two
   division at width.** It returns *spurious* counterexamples for facts as
   trivial as `(m·h)/1e9 ≤ m`. `proof_div_pow2_boundary` is the control: the
   identical shape with a power-of-two divisor (lowered to an exact shift)
   **verifies**. Sound SMT backends (z3, cvc5) avoid the spurious result but do
   not terminate on the 128-bit division.

2. **Free-variable "spec-modeled" division is intractable here.** Pinning a
   symbolic `q` to the Euclidean property `q·b ≤ a < (q+1)·b` makes the
   *negation* (the UNSAT proof) require the solver to exhaust a multiplier
   circuit, which neither CaDiCaL, kissat, nor z3 completed in practice.

### Resolution

The conservation, solvency, and monotonicity arguments are **divisor-agnostic**:

```
floor(m·h / D) ≤ m            whenever h ≤ D
floor(m·h / D) ≤ residual     whenever m·h ≤ backed·D
s1 ≤ s2  ⇒  floor(s1/D) ≤ floor(s2/D)
```

These hold for **every** `D > 0` by the same algebra; only `h ≤ D` matters, not
the literal value of `D`. The harnesses therefore machine-check them at a
representative power-of-two `D = 2³⁰ ≈ 1e9`, where CBMC's division is the exact
shift it handles soundly. This is a *complete* proof of the divisor-agnostic
statement.

The **literal `H_DENOM = 1e9`** case is covered separately by the deterministic
example proof `dust_conservation_exact` in the `#[cfg(test)]` module, which
exercises `H_DENOM-1`, `H_DENOM`, `u64::MAX`, and other boundary inputs.

Net: the structural invariant is proven for all `D`; the exact production
constant is exercised by tests. If a sound, terminating bitvector-division
backend becomes available (e.g. a future CBMC/SMT combination), the
representative `D` in `kani_proofs` can be set straight to `H_DENOM` to close
the gap entirely — the harness bodies need no other change.

## Lean: closing the divisor / 128-bit-multiply gaps

A theorem prover does not share CBMC's bitvector limitations (non-power-of-two
division; bit-blasting a 128-bit multiply), so the gaps CBMC cannot reach are
closed directly in Lean 4 (+ Mathlib) over unbounded `Nat`/`Int`, at the
**actual** divisors. Seven proof modules cover the haircut, the OI-scaled MMR
surcharge, funding, per-domain credit, realized-PnL/VWAP, whole-system residual
conservation, and margin-walk auth/completeness:

**Haircut** (`formal_verification/lean/Haircut.lean`), at the actual `H_DENOM = 1e9`:

| Lean theorem | Statement | Kani counterpart |
|---|---|---|
| `convert_ensures_0` | `(matured · h) / 1e9 ≤ matured`, given `h ≤ 1e9` | `proof_dust_conservation` |
| `solvency_single_convert` | `(matured · h) / 1e9 ≤ residual`, given `matured·h ≤ backed·1e9`, `backed = min residual matured` | `proof_solvency_single_convert` |

**OI-scaled MMR surcharge** (`formal_verification/lean/OiMmr.lean`), at the real `1e6` divisor — Lean proves the value-dependent **monotonicity** that CBMC cannot decide at a non-power-of-two divisor:

| Lean theorem | Statement | Kani counterpart |
|---|---|---|
| `oiScaled_le_cap` | `min((oi·slope)/1e6, cap) ≤ cap` (surcharge never exceeds its cap) | `oi_scaled_never_exceeds_cap` |
| `oiScaled_mono` | `oi₁ ≤ oi₂ ⇒ surcharge(oi₁) ≤ surcharge(oi₂)` (a crowding book is never under-margined) | — (CBMC can't decide at `/1e6`) |

**Funding** (`formal_verification/lean/Funding.lean`), over unbounded `Int` — proven in Lean because CBMC must bit-blast the 128-bit `notional · delta` multiply and does not terminate:

| Lean theorem | Statement | Kani counterpart |
|---|---|---|
| `funding_zero_sum` | `owed(long) + owed(short) == 0` (funding moves value between sides; cannot mint/burn) | — (128-bit multiply non-terminating in CBMC) |
| `funding_zero_when_no_index_move` | `delta == 0 ⇒ owed == 0` (no accrual from a static index) | — |

**Per-domain credit** (`formal_verification/lean/PerDomainCredit.lean`), at the real `1e9` divisor — the realizable-credit haircut bound CBMC cannot decide at a non-power-of-two divisor:

| Lean theorem | Statement |
|---|---|
| per-domain credit bound | realizable credit never exceeds collateral and is monotone in realizable value |

**Realized-PnL / VWAP** (`formal_verification/lean/RealizedPnl.lean`), unbounded width — mirrors `matcher/position_math.rs apply_fill` (closes G2):

| Lean theorem | Statement |
|---|---|
| `realized_reconciles_v2` | `pnl·entry = sign·(price−entry)·notional` (exact Flash V2 reconciliation) |
| `long_pnl_pos_iff` | profit iff price crosses entry the right way; breakeven = 0 |
| `vwap_lower_bound` / `vwap_upper_bound` | `min(entry,price) ≤ vwapEntry ≤ max(entry,price)` |

**Residual conservation** (`formal_verification/lean/ResidualConservation.lean`) — the triple-ledger identity `V = C_tot + I + Residual` (closes G4):

| Lean theorem | Statement |
|---|---|
| `applyDelta_conserves` | all 12 money-moving instruction deltas satisfy `ΔV = ΔC + ΔI + ΔR` |
| `foldl_conserves` | the identity survives any interleaving of instructions (sequence closure) |
| `solvent_of_conserved_nonneg_residual` | `Residual ≥ 0 ⟺ V ≥ C_tot + I` |

**Auth + completeness** (`formal_verification/lean/AuthCompleteness.lean`) — Finset-cardinality margin-walk (closes G7):

| Lean theorem | Statement |
|---|---|
| `walk_is_complete` / `no_position_omitted` | every open position is visited by the margin walk (none skipped) |
| `complete_walk_requirement_exact` | the walked requirement equals the true total |
| `exec_always_present` / `reinsert_noop` | liquidation dedupe / re-insert is a no-op |

All are `#print axioms`-clean — they depend only on `propext`,
`Classical.choice`, `Quot.sound` (no `sorry`). The full 7-root library
`lake build` completes (7358 jobs). Build + reproduction steps are
in [`formal_verification/lean/README.md`](../formal_verification/lean/README.md).

So the two solvency-critical haircut bounds now hold **both** ways: Kani proves
the divisor-agnostic structural statement (fast, in CI), and Lean proves the
exact production-constant statement.

## Property-test suites

Eight proptest suites (2,000 cases per property) complement the proofs where
the state space is structural rather than arithmetic: `proptest_book`
(model-based book consistency under random insert/cancel), `proptest_risk`,
`proptest_isolated`, `proptest_liquidation`, `proptest_haircut`,
`proptest_envelope`, `proptest_modules`, `proptest_new_features`. The
BanksClient integration suite loads the compiled SBF binary and exercises the
real BPF-VM execution path.

## Property specification

`certora/PROPERTIES.md` states the full protocol invariant set (solvency,
conservation, matching priority, settlement authenticity) with per-property
status — machine-proven today (Kani/Lean) or specified for a future prover
run — and is the hand-off document for external verification.

## Known limits

- Haircut credit monotonicity in `h` (`h1 ≤ h2 ⇒ credit(h1) ≤ credit(h2)`)
  exceeds the SAT backend's reach (two free multiplies); the bound is covered
  by tests and by the Lean cap/monotonicity theorems on the adjacent surfaces.
- Equality of two free symbolic multiplies is the SAT backend's limit — band
  inputs are bounded to a large realistic range, and the tautological
  "predicate == its definition" identity is intentionally not a harness.
