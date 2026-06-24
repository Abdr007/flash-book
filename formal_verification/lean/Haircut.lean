/-
Lean 4 (+ Mathlib) machine-checked proofs of the haircut conservation/solvency
invariants over the REAL denominator `H_DENOM = 1_000_000_000`.

Why this exists: the bounded model checker (CBMC, via `cargo kani`) cannot decide
128-bit division by a non-power-of-two, so the Kani proofs in
`programs/flash-book/src/matcher/haircut.rs` verify these same invariants with a
*representative* power-of-two divisor (`D = 1 << 30`). Lean closes that gap: it
proves the bounds with the actual `1e9` divisor, over unbounded `Nat`.

Both theorems are `#print axioms`-clean — they depend only on Lean's three
standard axioms (`propext`, `Classical.choice`, `Quot.sound`); no `sorry`.

The `convert` state-machine shape mirrors the QEDGen-generated obligation
(`qedgen codegen` from `haircut_conservation.qedspec`); `Pubkey` is a stub since
the bounds do not depend on it. See README.md for reproduction.
-/
import Mathlib.Tactic

namespace FlashBook.Haircut

def haircut_credit (matured : Nat) (h : Nat) : Nat := (matured * h) / 1000000000

abbrev Pubkey := Nat  -- vestigial: the conservation bound does not depend on it

structure State where
  credited : Nat

def convertTransition (s : State) (signer : Pubkey) (matured : Nat) (h : Nat) : Option State :=
  if h ≤ 1000000000 then
    some { s with credited := (haircut_credit (matured) (h)) }
  else none

theorem convert_ensures_0 (s s' : State) (signer : Pubkey) (matured : Nat) (h : Nat)
    (heq : convertTransition s signer matured h = some s') :
    s'.credited ≤ matured := by
  unfold convertTransition at heq
  by_cases hb : h ≤ 1000000000
  · rw [if_pos hb] at heq
    simp only [Option.some.injEq] at heq
    rw [← heq]
    show haircut_credit matured h ≤ matured
    unfold haircut_credit
    -- h ≤ 1e9  ⟹  matured*h ≤ matured*1e9  ⟹  matured*h/1e9 ≤ matured*1e9/1e9 = matured
    have key : matured * h / 1000000000 ≤ matured * 1000000000 / 1000000000 := by gcongr
    have eq1 : matured * 1000000000 / 1000000000 = matured :=
      Nat.mul_div_cancel matured (by norm_num)
    omega
  · rw [if_neg hb] at heq
    exact absurd heq (by simp)

/-- Invariant #1 — Solvency ("no printing"). Converting a position's matured PnL
    at the market-wide haircut credits no more than the residual backing it.
    `backed = min residual matured`; `compute_h` guarantees `matured·h ≤ backed·D`.
    Then `credit = (matured·h)/D ≤ residual`. Verified with the REAL D = 1e9 —
    the haircut.rs Kani proof (`proof_solvency_single_convert`) had to use D = 1<<30. -/
theorem solvency_single_convert (residual matured h : Nat)
    (backed : Nat) (hbacked : backed = min residual matured)
    (hcompute : matured * h ≤ backed * 1000000000) :
    (matured * h) / 1000000000 ≤ residual := by
  have hbr : backed ≤ residual := by rw [hbacked]; exact Nat.min_le_left _ _
  -- divide the compute_h bound through by D: credit ≤ backed·D/D = backed ≤ residual
  have key : matured * h / 1000000000 ≤ backed * 1000000000 / 1000000000 := by gcongr
  have eq1 : backed * 1000000000 / 1000000000 = backed :=
    Nat.mul_div_cancel backed (by norm_num)
  omega

-- Rigor check: a genuine proof depends only on Lean's 3 standard axioms
-- (propext, Classical.choice, Quot.sound). If it were a `sorry`, this prints `sorryAx`.
#print axioms convert_ensures_0
#print axioms solvency_single_convert

end FlashBook.Haircut
