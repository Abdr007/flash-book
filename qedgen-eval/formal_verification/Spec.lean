import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

namespace HaircutConservation

open QEDGen.Solana

-- Reference implementations: pure expressions named so
-- ensures clauses can call them. The user's Rust impl is
-- verified to satisfy the ensures referencing these, not
-- forced to implement them verbatim.
def haircut_credit (matured : Nat) (h : Nat) : Nat := (matured * h) / 1000000000

abbrev H_DENOM : Nat := 1000000000

structure State where
  credited : Nat
  deriving Repr, DecidableEq, BEq, Inhabited

def convertTransition (s : State) (signer : Pubkey) (matured : Nat) (h : Nat) : Option State :=
  if h ≤ 1000000000 then
    some { s with credited := (haircut_credit (matured) (h)) }
  else none

inductive Operation where
  | convert (matured : Nat) (h : Nat)
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .convert matured h => convertTransition s signer matured h

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem convert_aborts_if_OutOfRange (s : State) (signer : Pubkey) (matured : Nat) (h : Nat)
    (h : ¬(h ≤ 1000000000)) : convertTransition s signer matured h = none := by
  unfold convertTransition
  rw [if_neg h]

-- ============================================================================
-- Post-conditions (ensures)
-- ============================================================================

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
    -- matured * h ≤ matured * 1000000000 (since h ≤ 1000000000), then divide both by 1e9
    have key : matured * h / 1000000000 ≤ matured * 1000000000 / 1000000000 := by gcongr
    have eq1 : matured * 1000000000 / 1000000000 = matured :=
      Nat.mul_div_cancel matured (by norm_num)
    omega
  · rw [if_neg hb] at heq
    exact absurd heq (by simp)

-- ============================================================================
-- Frame conditions (modifies)
-- ============================================================================

end HaircutConservation
