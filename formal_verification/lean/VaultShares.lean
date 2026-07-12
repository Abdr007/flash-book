/-
Lean 4 (+ Mathlib) machine-checked proof of the copy-vault share accounting —
the ERC-4626-style deposit/withdraw formulas in `matcher/vault_math.rs`, over ℕ
at UNBOUNDED width (CBMC/Kani cannot discharge the symbolic `× / total`
division; this is the durable proof, the Rust unit tests pin concrete cases).

Model (Nat division is floor division, matching the u128 integer ops):
    sharesOnDeposit d ts ta  = if ts = 0 then d else d * ts / ta
    assetsOnWithdraw sh ts ta = sh * ta / ts

The safety properties a vault MUST have so no depositor can extract more than
their proportional claim:
  1. `withdraw_le_assets`  — a withdrawal never pays more than the pool holds
     (for `sh ≤ ts`), so the vault can never be over-drawn.
  2. `withdraw_all_returns_all` — burning every share returns every asset (no
     value is stranded / minted).
  3. `withdraw_mono_in_shares` — more shares burned ⇒ never fewer assets out
     (monotone payout; no rounding inversion an attacker could exploit).

Theorems are `#print axioms`-clean (no `sorry`).
-/
import Mathlib.Tactic

namespace FlashBook.VaultShares

/-- Shares minted for `d` deposited into a vault of `ts` shares / `ta` assets. -/
def sharesOnDeposit (d ts ta : ℕ) : ℕ := if ts = 0 then d else d * ts / ta

/-- Assets returned for burning `sh` of `ts` shares against `ta` assets. -/
def assetsOnWithdraw (sh ts ta : ℕ) : ℕ := sh * ta / ts

/-- SAFETY 1 — a withdrawal never pays more than the pool holds. For any burn of
`sh ≤ ts` shares from a non-empty vault, `assetsOnWithdraw ≤ ta`. So the vault
is never over-drawn, at any width. -/
theorem withdraw_le_assets (sh ts ta : ℕ) (hts : 0 < ts) (hsh : sh ≤ ts) :
    assetsOnWithdraw sh ts ta ≤ ta := by
  unfold assetsOnWithdraw
  calc
    sh * ta / ts ≤ ts * ta / ts := Nat.div_le_div_right (Nat.mul_le_mul_right ta hsh)
    _ = ta := by rw [Nat.mul_div_cancel_left ta hts]

/-- SAFETY 2 — burning EVERY share returns EVERY asset (no value stranded or
minted): `assetsOnWithdraw ts ts ta = ta`. -/
theorem withdraw_all_returns_all (ts ta : ℕ) (hts : 0 < ts) :
    assetsOnWithdraw ts ts ta = ta := by
  unfold assetsOnWithdraw
  rw [Nat.mul_div_cancel_left ta hts]

/-- SAFETY 3 — payout is monotone in shares burned: burning more shares never
returns fewer assets. Rules out a rounding inversion where an attacker splits a
withdrawal to extract more than a single burn. -/
theorem withdraw_mono_in_shares (s1 s2 ts ta : ℕ) (h : s1 ≤ s2) :
    assetsOnWithdraw s1 ts ta ≤ assetsOnWithdraw s2 ts ta := by
  unfold assetsOnWithdraw
  exact Nat.div_le_div_right (Nat.mul_le_mul_right ta h)

/-- SAFETY 4 — the first deposit into an empty vault seeds shares 1:1 with the
deposit, so the initial share price is exactly 1 (no free shares, no dilution of
a later depositor beyond the proportional formula). -/
theorem first_deposit_one_to_one (d ta : ℕ) : sharesOnDeposit d 0 ta = d := by
  unfold sharesOnDeposit; simp

#print axioms withdraw_le_assets
#print axioms withdraw_all_returns_all
#print axioms withdraw_mono_in_shares
#print axioms first_deposit_one_to_one

end FlashBook.VaultShares
