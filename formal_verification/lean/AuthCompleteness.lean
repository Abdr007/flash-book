/-
Lean 4 (+ Mathlib) machine-checked proof of the AUTHORIZATION and COMPLETENESS
invariants — margin-walk completeness, liquidation dedupe, and the auth gate.
Closes gap G7. These are enforced today by runtime `require!`s only; this proves
they hold for ALL position counts, at unbounded width.

── 1. Margin-walk completeness ──────────────────────────────────────────────
The attack: omit a risky position from the margin walk so the assessed
requirement understates the true one, then over-withdraw / dodge liquidation.
The `partial_withdraw_core` handler (`lib.rs:13382`, comment "C-2") blocks it with
FOUR runtime checks on the supplied `(market, position)` account pairs:

  * exact count      `remaining.len() == open * 2`   (open = trader.open_positions)
  * PDA-binding      `verify_position_pda(market, trader_state, …)` + trader match
  * live-only        `position.size_lots > 0`
  * market-dedupe    `!market_keys.contains(market)`

Because a position PDA is seeded `[market, trader_state]`, a trader has AT MOST
one position per market, so the trader's open positions are a Finset of markets of
cardinality `open`. The supplied markets are then a duplicate-free (dedupe) subset
(PDA-binding) of that Finset with cardinality `open` (exact count) — which forces
the supplied set to EQUAL the full set. No position can be omitted.

── 2. Requirement monotonicity ──────────────────────────────────────────────
Why completeness matters: the margin requirement aggregates a NON-NEGATIVE
per-position floor, so it is monotone in the position set — a strict subset can
only understate it. Completeness (§1) + monotonicity ⇒ the walked requirement is
exactly the true requirement, never an under-count. (Mirrors the host proof
`n_position_margin_is_collateral_monotone_and_frame_stable`, `risk.rs:1148`.)

── 3. Liquidation dedupe ────────────────────────────────────────────────────
`liquidate_portfolio_v2` (`lib.rs:9839`) seeds a dedup set with the EXECUTION
market and folds supplied markets in. Modeled as a `Finset` accumulation: the
execution market is always present (can't be dropped) and re-supplying an
already-counted market is a no-op (can't double-count a position into the walk).

── 4. Authorization gate ────────────────────────────────────────────────────
The `require_keys_eq!(authority, signer)` gate admits exactly the authority.

Theorems are `#print axioms`-clean (no `sorry`).
-/
import Mathlib

namespace Clober.AuthCompleteness

variable {α : Type*}

/-! ### 1 · Margin-walk completeness -/

/-- COMPLETENESS — a duplicate-free (`dedupe`) list of markets, each a genuine
open position of this trader (`PDA-binding ⇒ ⊆ full`), whose length equals the
trader's `open_positions` count (`exact count`), covers the trader's ENTIRE open
position set. `full` is the trader's actual open positions (a Finset of markets,
one position per market via the `[market, trader_state]` PDA seed). -/
theorem walk_is_complete [DecidableEq α] (full : Finset α) (supplied : List α)
    (hnodup : supplied.Nodup)
    (hsub : ∀ m ∈ supplied, m ∈ full)
    (hcard : supplied.length = full.card) :
    supplied.toFinset = full := by
  apply Finset.eq_of_subset_of_card_le
  · intro x hx; exact hsub x (List.mem_toFinset.mp hx)
  · rw [List.toFinset_card_of_nodup hnodup, hcard]

/-- The exploitable-if-wrong statement: under the same C-2 gate, EVERY open
position appears in the supplied walk — no risky position can be omitted. -/
theorem no_position_omitted [DecidableEq α] (full : Finset α) (supplied : List α)
    (hnodup : supplied.Nodup)
    (hsub : ∀ m ∈ supplied, m ∈ full)
    (hcard : supplied.length = full.card) :
    ∀ p ∈ full, p ∈ supplied := by
  intro p hp
  have hcomplete := walk_is_complete full supplied hnodup hsub hcard
  rw [← hcomplete] at hp
  exact List.mem_toFinset.mp hp

/-! ### 2 · Requirement monotonicity — a subset can only understate -/

/-- The margin requirement is a sum of NON-NEGATIVE per-position floors, so
dropping positions can only lower it: `req(subset) ≤ req(full)`. Hence omission
is strictly profitable for an attacker — exactly what the completeness gate
forbids. -/
theorem requirement_monotone (floor : α → ℤ) (hnn : ∀ a, 0 ≤ floor a)
    (S T : Finset α) (h : S ⊆ T) :
    ∑ p ∈ S, floor p ≤ ∑ p ∈ T, floor p :=
  Finset.sum_le_sum_of_subset_of_nonneg h (fun a _ _ => hnn a)

/-- COMPLETENESS ⇒ NO-UNDERSTATEMENT: a walk that passes the C-2 gate computes
the TRUE full requirement, not an under-count. -/
theorem complete_walk_requirement_exact [DecidableEq α] (floor : α → ℤ)
    (full : Finset α) (supplied : List α)
    (hnodup : supplied.Nodup)
    (hsub : ∀ m ∈ supplied, m ∈ full)
    (hcard : supplied.length = full.card) :
    ∑ p ∈ supplied.toFinset, floor p = ∑ p ∈ full, floor p := by
  rw [walk_is_complete full supplied hnodup hsub hcard]

/-! ### 3 · Liquidation dedupe (exec-seeded, idempotent) -/

/-- The dedup market set: fold supplied markets into a seed via `Finset.insert`
(distinct by construction). Models `liquidate_portfolio_v2`'s exec-seeded walk. -/
def dedupWalk [DecidableEq α] (seed : Finset α) (xs : List α) : Finset α :=
  xs.foldl (fun acc x => insert x acc) seed

/-- The execution market (seed member) is ALWAYS present in the walk — it cannot
be dropped by any sequence of supplied markets. -/
theorem exec_always_present [DecidableEq α] (seed : Finset α) (xs : List α) (exec : α)
    (h : exec ∈ seed) : exec ∈ dedupWalk seed xs := by
  unfold dedupWalk
  induction xs generalizing seed with
  | nil => simpa
  | cons y ys ih =>
      simp only [List.foldl_cons]
      exact ih (insert y seed) (Finset.mem_insert_of_mem h)

/-- Re-supplying an already-counted market is a NO-OP — a position/market cannot
be double-counted into the liquidation walk (here: re-supplying the exec market). -/
theorem reinsert_noop [DecidableEq α] (seed : Finset α) (xs : List α) (x : α) (h : x ∈ seed) :
    dedupWalk seed (x :: xs) = dedupWalk seed xs := by
  unfold dedupWalk
  simp only [List.foldl_cons, Finset.insert_eq_self.mpr h]

/-! ### 4 · Authorization gate -/

/-- The privileged-action gate: `require_keys_eq!(authority, signer)`. -/
def authorized (signer authority : α) : Prop := signer = authority

/-- SOUND — the gate admits exactly the authority: no other signer passes. -/
theorem auth_gate_sound (signer authority : α) :
    authorized signer authority ↔ signer = authority := Iff.rfl

/-- An impostor (`signer ≠ authority`) is rejected. -/
theorem unauthorized_rejected (signer authority : α) (h : signer ≠ authority) :
    ¬ authorized signer authority := h

#print axioms walk_is_complete
#print axioms no_position_omitted
#print axioms complete_walk_requirement_exact
#print axioms exec_always_present
#print axioms reinsert_noop

end Clober.AuthCompleteness
