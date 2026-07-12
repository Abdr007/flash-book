/-
Lean 4 (+ Mathlib) machine-checked proof that funding is ZERO-SUM.

Why this exists (not Kani): `funding_owed` multiplies a u64 notional by an i128
funding-index delta and arithmetic-shifts right by 64. CBMC (via `cargo kani`)
must bit-blast the 128-bit multiply and does not terminate in practice (the same
SAT-backend limit noted for the haircut). Lean proves the conservation property
over unbounded `Int` instead — and the proof is independent of the exact `>> 64`
rounding, because both the long and the short use the *same* scaled magnitude, so
their signed contributions cancel exactly.

Model (`risk.rs`/`funding.rs` `funding_owed`):
  owed(isLong, notional, delta) = (±1) · scaled(notional · delta)
where `scaled` is the Q64.64 → linear step (here `· / 2^64`); the zero-sum result
holds for ANY `scaled`.

Theorems are `#print axioms`-clean (no `sorry`).
-/
import Mathlib.Tactic

namespace Clober.Funding

/-- Q64.64 → linear scaling step (the `>> 64`). The conservation result below is
independent of this definition; it is fixed here only to mirror the Rust. -/
def scaled (x : Int) : Int := x / (2 ^ 64)

/-- Funding owed by one side, signed: `+scaled` for a long, `−scaled` for a short. -/
def fundingOwed (isLong : Bool) (notional delta : Int) : Int :=
  (if isLong then (1 : Int) else -1) * scaled (notional * delta)

/-- ZERO-SUM: for the same notional and index delta, a long owes exactly what a
short receives — funding moves value between the two sides and can neither mint
nor burn it. -/
theorem funding_zero_sum (notional delta : Int) :
    fundingOwed true notional delta + fundingOwed false notional delta = 0 := by
  simp [fundingOwed]

/-- No index movement ⇒ no funding owed (cannot accrue from a static index). -/
theorem funding_zero_when_no_index_move (isLong : Bool) (notional : Int) :
    fundingOwed isLong notional 0 = 0 := by
  simp [fundingOwed, scaled]

#print axioms funding_zero_sum
#print axioms funding_zero_when_no_index_move

end Clober.Funding
