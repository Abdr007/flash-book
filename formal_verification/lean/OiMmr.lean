/-
Lean 4 (+ Mathlib) machine-checked proofs of the OI-scaled / crowded-trade
maintenance-margin surcharge over the REAL denominator `1_000_000`.

Why this exists: the bounded model checker (CBMC, via `cargo kani`) cannot decide
128-bit division by a non-power-of-two, so the Kani proofs in
`programs/clober/src/matcher/risk.rs` (`mod mmr_kani_proofs`) verify the
*bound* and *disable* properties — which hold for any division result — but NOT
the value-dependent MONOTONICITY (more open interest ⇒ a surcharge that never
decreases). Lean closes that gap: it proves monotonicity (and the cap) with the
actual `1e6` divisor, over unbounded `Nat`.

Model: `oiScaled oi slope cap = min ((oi * slope) / 1_000_000) cap`, matching
`oi_scaled_mmr_extra_bps` (the `saturating_mul` never saturates because
`u64 * u32 < 2^96 < u128::MAX`, and the double `.min(cap)` collapses to one).

Both theorems are intended to be `#print axioms`-clean (Lean's three standard
axioms only; no `sorry`). See README.md for reproduction (`lake build`).
-/
import Mathlib.Tactic

namespace Clober.OiMmr

/-- The OI-scaled MMR surcharge over the real `1e6` divisor (see `risk.rs`). -/
def oiScaled (oi slope cap : Nat) : Nat := min ((oi * slope) / 1000000) cap

/-- The surcharge NEVER exceeds its configured cap — a crowded book cannot be
charged more maintenance margin than governance bounded. (Kani proves this too;
restated here for completeness over the real divisor.) -/
theorem oiScaled_le_cap (oi slope cap : Nat) : oiScaled oi slope cap ≤ cap := by
  unfold oiScaled
  exact min_le_right _ _

/-- MONOTONICITY (the property CBMC/Kani cannot decide at `/1e6`): as open
interest grows, the surcharge never DECREASES — so a crowding book cannot be
under-margined by an accounting artifact of the non-power-of-two division. -/
theorem oiScaled_mono (oi₁ oi₂ slope cap : Nat) (h : oi₁ ≤ oi₂) :
    oiScaled oi₁ slope cap ≤ oiScaled oi₂ slope cap := by
  unfold oiScaled
  gcongr

-- Axiom-cleanliness: both theorems depend only on Lean's three standard axioms
-- (propext, Classical.choice, Quot.sound) — no `sorry`, no extra axioms.
#print axioms oiScaled_le_cap
#print axioms oiScaled_mono

end Clober.OiMmr
