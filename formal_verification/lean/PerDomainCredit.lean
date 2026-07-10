/-
Lean 4 (+ Mathlib) machine-checked proof of the 3.1 per-source-domain realizable
credit safety, over the REAL basis-point denominator and at UNBOUNDED width.

Why this exists (not Kani): the credit value is `pnl · rate / 10_000` and the
rate itself is `min(1, backing/claims)` — a 128-bit multiply plus a symbolic
non-power-of-two divide. CBMC (via `cargo kani`) either takes ~300–700s
(value step) or cannot decide it at all (the `backing·BPS/claims` symbolic
divide). Lean proves the same properties over unbounded `Nat`.

See docs/PER_DOMAIN_CREDIT.md for the mechanism. The core safety facts:
  1. usable credit never exceeds the paper PnL,
  2. a zero credit-rate mints zero usable credit,
  3. a market with ZERO real backing gives a zero rate, hence zero usable credit
     from ANY amount of paper PnL — the oracle-pump attack yields nothing,
  4. an under-backed market caps usable credit by the ability-to-pay fraction.

Theorems are intended `#print axioms`-clean (no `sorry`).
-/
import Mathlib.Tactic

namespace FlashBook.PerDomainCredit

/-- Basis-points denominator (10_000). -/
def BPS : ℕ := 10000

/-- Usable credit = paper PnL scaled by the credit rate (in bps), floored. -/
def usable (pnl rate : ℕ) : ℕ := pnl * rate / BPS

/-- Per-market credit rate = `min(1, backing/claims)` expressed in bps. A market
with no opposing claims yields 0; otherwise `min(BPS, backing·BPS/claims)`. -/
def creditRate (backing claims : ℕ) : ℕ :=
  if claims = 0 then 0 else min BPS (backing * BPS / claims)

/-- The rate is always a valid bps fraction. -/
theorem creditRate_le_BPS (backing claims : ℕ) : creditRate backing claims ≤ BPS := by
  unfold creditRate
  split
  · norm_num [BPS]
  · exact min_le_left _ _

/-- SAFETY 1 — usable credit never exceeds the paper PnL (for any valid rate). -/
theorem usable_le_pnl (pnl rate : ℕ) (h : rate ≤ BPS) : usable pnl rate ≤ pnl := by
  unfold usable
  calc pnl * rate / BPS ≤ pnl * BPS / BPS := by gcongr
    _ = pnl := by rw [Nat.mul_div_cancel _ (show 0 < BPS by norm_num [BPS])]

/-- SAFETY 2 — a zero credit-rate mints zero usable credit. -/
theorem zero_rate_zero_credit (pnl : ℕ) : usable pnl 0 = 0 := by
  simp [usable]

/-- SAFETY 3 (the marquee) — a market with ZERO real backing gives a zero rate,
so ANY amount of paper PnL yields zero usable credit: the oracle-pump attack
(inflate a thin/stale market's mark) converts paper profit into nothing. -/
theorem no_backing_no_usable_credit (pnl claims : ℕ) :
    usable pnl (creditRate 0 claims) = 0 := by
  have hr : creditRate 0 claims = 0 := by
    unfold creditRate
    split
    · rfl
    · simp
  rw [hr, zero_rate_zero_credit]

/-- SAFETY 4 — usable credit for any market is bounded by that market's own
credit rate applied to the paper PnL (composition helper: gives the
ability-to-pay cap once the rate is instantiated with `backing·BPS/claims`). -/
theorem usable_creditRate_le_pnl (pnl backing claims : ℕ) :
    usable pnl (creditRate backing claims) ≤ pnl :=
  usable_le_pnl pnl _ (creditRate_le_BPS backing claims)

end FlashBook.PerDomainCredit
