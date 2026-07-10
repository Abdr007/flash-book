/-
Lean 4 (+ Mathlib) machine-checked proof of the WHOLE-SYSTEM residual identity —
triple-ledger conservation preserved by every money-moving instruction, over
signed unbounded width. Closes gap G4.

The identity (from `matcher/haircut.rs:168`, the spec of record):

    V = C_tot + I + Residual

where
    V        = vault total (SPL vault: deposits + LP capital + insurance + fees)
    C_tot    = Σ committed trader collateral
    I        = insurance fund balance
    Residual = V − C_tot − I  (folds FLP capital + junior-profit backing + surplus)

flash-book does not recompute this each block — it DELTA-TRACKS `Residual` (every
money-moving ix feeds a signed delta through `haircut::apply_residual_delta`). The
per-instruction delta table is documented at `matcher/haircut.rs:460`
(`| Ix | ΔV | ΔC_tot | ΔI | ΔResidual |`). Conservation is preserved iff EVERY
instruction's delta tuple satisfies

    ΔV = ΔC_tot + ΔI + ΔResidual

so no bucket can be credited without an equal, simultaneous debit elsewhere.

Why this is a proof, not just a fuzzer: the on-chain `verify_protocol_solvency`
handler and the #268 conservation SEQUENCE-FUZZER reconcile the identity against
LIVE SPL balances, but only along sampled paths. This proves the delta ALGEBRA of
the FULL instruction set is conservative for ALL magnitudes and ALL interleavings
— the "checked before every commit" guarantee in structural form.

Fidelity note — the model mirrors the `haircut.rs:460` table exactly, with ONE
correction the proof forces out: the table's `convert`/gain row lists only the
Residual leg (`ΔResidual = −credit`); the matching `ΔC_tot = +credit` collateral
credit is `apply_convert` step 3 (`haircut.rs:374`). Stated together they balance
(`0 = credit + 0 − credit`); stated apart they would appear to mint value. This
file pins the paired legs — see `convertGain` and `convert_gain_conserves`.

Theorems are `#print axioms`-clean (no `sorry`).
-/
import Mathlib.Tactic

namespace FlashBook.ResidualConservation

/-- The four protocol-wide value buckets, signed so deltas compose freely. -/
structure Ledger where
  V : ℤ  -- vault total
  C : ℤ  -- Σ committed trader collateral
  I : ℤ  -- insurance fund
  R : ℤ  -- residual = V − C − I

/-- The whole-system residual identity: `V = C + I + R` (`haircut.rs:168`). -/
def conserved (L : Ledger) : Prop := L.V = L.C + L.I + L.R

/-! ### The delta-tuple engine

Every money-moving instruction is a `(ΔV, ΔC, ΔI, ΔR)` step. Its correctness
obligation is a single equation: `ΔV = ΔC + ΔI + ΔR`. -/

/-- Apply a signed delta tuple to the ledger — models one money move applying its
`apply_residual_delta` (the `R` leg) alongside its vault/collateral/insurance legs. -/
def applyDelta (dV dC dI dR : ℤ) (L : Ledger) : Ledger :=
  ⟨L.V + dV, L.C + dC, L.I + dI, L.R + dR⟩

/-- The balance obligation for a single money move. -/
def balanced (dV dC dI dR : ℤ) : Prop := dV = dC + dI + dR

/-- CORE — any BALANCED delta step preserves the residual identity. Every named
instruction below is an instance; this is the whole theorem, once. -/
theorem applyDelta_conserves (dV dC dI dR : ℤ) (hb : balanced dV dC dI dR)
    (L : Ledger) (hL : conserved L) : conserved (applyDelta dV dC dI dR L) := by
  simp only [conserved, applyDelta, balanced] at *
  omega

/-! ### The twelve money-moving instructions (`haircut.rs:460`, corrected)

Each is an `applyDelta` instance whose delta tuple satisfies `ΔV = ΔC + ΔI + ΔR`
(the `applyDelta_conserves` core), so the identity survives it. -/

def depositCollateral  (a : ℤ) : Ledger → Ledger := applyDelta a a 0 0
def withdrawCollateral (a : ℤ) : Ledger → Ledger := applyDelta (-a) (-a) 0 0
def depositFlpCapital  (a : ℤ) : Ledger → Ledger := applyDelta a 0 0 a
def withdrawFlpCapital (a : ℤ) : Ledger → Ledger := applyDelta (-a) 0 0 (-a)
def insuranceDeposit   (a : ℤ) : Ledger → Ledger := applyDelta a 0 a 0
def insuranceWithdraw  (a : ℤ) : Ledger → Ledger := applyDelta (-a) 0 (-a) 0
def flushHaircutDust   (d : ℤ) : Ledger → Ledger := applyDelta 0 0 d (-d)
def feeToFlp           (f : ℤ) : Ledger → Ledger := applyDelta f 0 0 f
def feeToInsurance     (f : ℤ) : Ledger → Ledger := applyDelta f 0 f 0
def liquidationReward  (r : ℤ) : Ledger → Ledger := applyDelta (-r) (-r) 0 0
def realizedPnlLoss    (l : ℤ) : Ledger → Ledger := applyDelta 0 (-l) 0 l
/-- Convert junior profit: `credit` moves from Residual to trader collateral.
The paired legs the `haircut.rs:460` table splits apart (`ΔC = +credit`,
`ΔR = −credit`); modeled together so the balance is visible. -/
def convertGain        (credit : ℤ) : Ledger → Ledger := applyDelta 0 credit 0 (-credit)

theorem deposit_collateral_conserves (a : ℤ) (L) (h : conserved L) :
    conserved (depositCollateral a L) := by
  simp only [conserved, depositCollateral, applyDelta] at *; omega
theorem withdraw_collateral_conserves (a : ℤ) (L) (h : conserved L) :
    conserved (withdrawCollateral a L) := by
  simp only [conserved, withdrawCollateral, applyDelta] at *; omega
theorem deposit_flp_capital_conserves (a : ℤ) (L) (h : conserved L) :
    conserved (depositFlpCapital a L) := by
  simp only [conserved, depositFlpCapital, applyDelta] at *; omega
theorem withdraw_flp_capital_conserves (a : ℤ) (L) (h : conserved L) :
    conserved (withdrawFlpCapital a L) := by
  simp only [conserved, withdrawFlpCapital, applyDelta] at *; omega
theorem insurance_deposit_conserves (a : ℤ) (L) (h : conserved L) :
    conserved (insuranceDeposit a L) := by
  simp only [conserved, insuranceDeposit, applyDelta] at *; omega
theorem insurance_withdraw_conserves (a : ℤ) (L) (h : conserved L) :
    conserved (insuranceWithdraw a L) := by
  simp only [conserved, insuranceWithdraw, applyDelta] at *; omega
theorem flush_haircut_dust_conserves (d : ℤ) (L) (h : conserved L) :
    conserved (flushHaircutDust d L) := by
  simp only [conserved, flushHaircutDust, applyDelta] at *; omega
theorem fee_to_flp_conserves (f : ℤ) (L) (h : conserved L) :
    conserved (feeToFlp f L) := by
  simp only [conserved, feeToFlp, applyDelta] at *; omega
theorem fee_to_insurance_conserves (f : ℤ) (L) (h : conserved L) :
    conserved (feeToInsurance f L) := by
  simp only [conserved, feeToInsurance, applyDelta] at *; omega
theorem liquidation_reward_conserves (r : ℤ) (L) (h : conserved L) :
    conserved (liquidationReward r L) := by
  simp only [conserved, liquidationReward, applyDelta] at *; omega
theorem realized_pnl_loss_conserves (l : ℤ) (L) (h : conserved L) :
    conserved (realizedPnlLoss l L) := by
  simp only [conserved, realizedPnlLoss, applyDelta] at *; omega
/-- MARQUEE (the corrected row) — convert conserves ONLY because the `+credit`
collateral leg is paired with the `−credit` residual leg. -/
theorem convert_gain_conserves (credit : ℤ) (L) (h : conserved L) :
    conserved (convertGain credit L) := by
  simp only [conserved, convertGain, applyDelta] at *; omega

/-! ### Sequence closure — the identity survives ANY interleaving

Any list of identity-preserving transitions, applied in sequence, preserves the
identity. This is the "checked before every commit" guarantee: no reachable
sequence of money moves can break `V = C + I + R`. -/

theorem foldl_conserves :
    ∀ (fs : List (Ledger → Ledger)),
      (∀ f ∈ fs, ∀ L, conserved L → conserved (f L)) →
      ∀ L, conserved L → conserved (fs.foldl (fun L f => f L) L)
  | [], _, _, hL => hL
  | f :: rest, hf, L, hL => by
      simp only [List.foldl_cons]
      exact foldl_conserves rest (fun g hg => hf g (by simp [hg]))
        (f L) (hf f (by simp) L hL)

/-- The concrete money-move alphabet: every instruction preserves the identity,
so `foldl_conserves` applies to any program built from them. -/
theorem all_instructions_conserve
    (fs : List (Ledger → Ledger))
    (hmoves : ∀ f ∈ fs, ∀ L, conserved L → conserved (f L))
    (L : Ledger) (hL : conserved L) :
    conserved (fs.foldl (fun L f => f L) L) :=
  foldl_conserves fs hmoves L hL

/-! ### Solvency corollary

The identity turns the scalar `Residual ≥ 0` into the protocol solvency baseline
`V ≥ C_tot + I` — exactly the invariant `haircut.rs:449` names. So a
conservation-preserving system that never lets Residual go negative is provably
solvent at every step. -/

theorem solvent_of_conserved_nonneg_residual (L : Ledger)
    (hL : conserved L) (hR : 0 ≤ L.R) : L.C + L.I ≤ L.V := by
  simp only [conserved] at hL; omega

/-- Contrapositive, the detector direction: an insolvent ledger
(`V < C + I`) has a strictly negative residual — there is no way to be short on
the vault while the tracked residual still reads non-negative. -/
theorem insolvent_iff_negative_residual (L : Ledger) (hL : conserved L) :
    L.V < L.C + L.I ↔ L.R < 0 := by
  simp only [conserved] at hL; omega

#print axioms applyDelta_conserves
#print axioms convert_gain_conserves
#print axioms foldl_conserves
#print axioms solvent_of_conserved_nonneg_residual

end FlashBook.ResidualConservation
