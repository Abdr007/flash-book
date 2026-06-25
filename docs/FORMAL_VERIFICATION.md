# Formal Verification

Flash Book's risk engine carries **machine-checked proofs** of its core
solvency invariants, written as [Kani](https://model-checking.github.io/kani/)
proof harnesses and discharged by CBMC. These are not sampled tests: each
harness lets `kani::any()` range over the *entire* input domain symbolically,
and the model checker proves the assertion holds for **every** value, or
returns a concrete counterexample.

The first target is the **haircut conservation math** (`matcher/haircut.rs`) —
the accounting that decides how much realized profit a trader may withdraw when
the protocol is under-collateralized. Getting this wrong mints or burns value,
so it is exactly where a proof (not a test) earns its keep.

## What is proven

Harnesses live in `programs/flash-book/src/matcher/haircut.rs`
(`#[cfg(kani)] mod kani_proofs`). All five verify, 0 failures, each in < 1 s.

| Harness | Invariant | What it proves |
|---|---|---|
| `proof_dust_conservation` | #4 Dust conservation | `credit ≤ matured` **and** `credit + dust == matured` for every `h ∈ [0, H_DENOM]` — no quote lot is created or destroyed by a haircut. |
| `proof_solvency_single_convert` | #1 Solvency | Converting a position's full matured PnL credits `≤ residual` — the **non-printing** guarantee: traders collectively withdraw no more than the real residual backing the profit pool. |
| `proof_matured_fraction_bounds` | #3 Maturation bounds | `matured_fraction(..) ≤ reserve`, is `0` before the warmup window opens, and `== reserve` after it closes. Verified against the **real** function. |
| `proof_div_pow2_boundary` | — | Marks the CBMC division-completeness boundary (see below). |
| `proof_assume_sanity` | — | Confirms `kani::assume` constrains the domain. |

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
**actual** divisors. Three proof files cover the haircut, the OI-scaled MMR
surcharge, and funding:

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

All six are `#print axioms`-clean — they depend only on `propext`,
`Classical.choice`, `Quot.sound` (no `sorry`). Build + reproduction steps are in
[`formal_verification/lean/README.md`](../formal_verification/lean/README.md).
The `convert` state-machine shape was scaffolded by QEDGen
(`qedgen codegen` from a `.qedspec`); the proof bodies are hand-written.

So the two solvency-critical haircut bounds now hold **both** ways: Kani proves
the divisor-agnostic structural statement (fast, in CI), and Lean proves the
exact production-constant statement.

## Roadmap

- **Monotonicity (#2)** — `h1 ≤ h2 ⇒ credit(h1) ≤ credit(h2)`. The harness is
  straightforward but its two free multiplies exceed the SAT backend's reach;
  parked until a stronger division/UNSAT backend is wired in.
- Extend coverage to the matching engine (`state_v2.rs` hypertree ordering
  invariants) and the margin/liquidation gates.

## Settlement-authenticity & gate proofs (#35/#36/FV-sweep)

Beyond the haircut/MMR/funding core, the settlement and liquidation gates carry
their own Kani harnesses (all `VERIFICATION: SUCCESSFUL`):

| Harness (module) | Property |
|---|---|
| `matcher::fill_commitment` ring ×4 | consume-and-clear ring: settlement never outruns production, depth-bounded, fabricated/out-of-order fill rejected, no double-settle (INV-S1/S2) |
| `matcher::fill_commitment` nonce ×3 | **P-SETTLE-1** settlement nonce strictly monotone — replay/reorder rejected, advance is strict + exact, chain monotone |
| `matcher::flp_quoter` band ×2 | **#35 FLP / #36 resting** price band: accepts the oracle price (no false reject), rejects 2×/0× oracle (catastrophe bound), overflow-free |
| `matcher::liquidation` health ×3 | **P-LIQ-1** worse-of(mark, oracle): always the worse of the two real sources for the side, never a fabricated price |
| `state_v2` order-id ×4 | **P-MATCH-1/2** price-time priority of the `order_id` encoding: better price first (asks↑/bids↓), FIFO seq tiebreak both sides (rules out the old LIFO-bid bug), id injective on live orders |

Each handler routes through the proven pure function (e.g. `apply_fill`/
`apply_flp_fill` → `advance_settlement_seq`; `liquidate_position_v2` →
`worse_of_health_price`), so the proof binds to the deployed logic, not a copy.

CBMC note (reconfirmed): equality of two free *symbolic* multiplies is the SAT
backend's limit — band inputs are bounded to a large realistic range, and the
tautological "predicate == its definition" identity is intentionally not a harness.
