/-
Lean 4 (+ Mathlib) machine-checked proof of the realized-PnL VALUE on the
reduce/flip settlement path and the stacked VWAP-entry BRACKET, over the REAL
tick-size scaling and at UNBOUNDED width. Closes gap G2.

Why this exists (not Kani): the reduce/flip realized PnL is
`sign · closed · Δticks · tick_size` — a chain of nested 128-bit signed
multiplies — and the stacked entry is `(entry·size + price·fill) / (size+fill)`,
whose BRACKET property (the averaged entry lies between the old entry and the
fill price) requires reasoning about the RESULT of an integer division. CBMC
(via `cargo kani`) bit-blasts both and does not terminate in CI, so
`programs/clober/src/matcher/position_math.rs` covers the PnL value and the
VWAP bracket with host/proptest sweeps at `B = 256` ONLY (see the `NOTE ON
PnL-VALUE COVERAGE` there). Lean proves the same properties over unbounded `Int`
(PnL) and `Nat` (entry), so the identities hold at every magnitude, not just the
solver-bounded grid.

Model — mirrors `apply_fill` in `matcher/position_math.rs`:
  * REDUCE/FLIP realized PnL (quote-lots), from lines 112-123:
        close_size = min(fill_size, pos_size)
        sign       = +1 if long else −1
        pnl        = sign · close_size · (fill_price − entry) · tick_size
  * STACK size-weighted-average entry (a true VWAP), from lines 89-99:
        entry' = (entry·pos_size + fill_price·fill_size) / (pos_size + fill_size)
    (Rust does this in `u128` with `checked_div` = floor division; modeled here
     as `Nat` floor division so the bracket holds for the exact on-chain value.)

The core money-safety facts proven below:
  1. sign correctness — a long realizes a PROFIT iff it closes above entry, a
     short iff it closes below entry (no side/sign confusion can mint value);
  2. breakeven — closing at the entry price realizes exactly zero;
  3. cross-system reconciliation — the clober PnL satisfies the Flash V2
     notional identity `pnl · entry = sign · (price − entry) · notional`
     (`notional = closed · entry · tick`) EXACTLY, at unbounded width;
  4. the closed-lot count is `min(fill, size)` — `fill` on a pure reduce,
     `size` on a flip (value is realized only on the lots that actually close);
  5. VWAP bracket (the CBMC-intractable one) — the stacked entry lies strictly
     within `[min(entry, price), max(entry, price)]`, and equals the common
     price when both legs fill at the same price.

Theorems are intended `#print axioms`-clean (no `sorry`).
-/
import Mathlib.Tactic

namespace Clober.RealizedPnl

/-- Long side (price up = profit). Matches `position_math::SIDE_LONG`. -/
def SIDE_LONG : ℕ := 0
/-- Short side (price down = profit). Matches `position_math::SIDE_SHORT`. -/
def SIDE_SHORT : ℕ := 1

/-- Realized-PnL sign: `+1` for a long, `−1` for a short. Mirrors
`let sign = if pos.side == SIDE_LONG { 1 } else { -1 }`. -/
def sign (side : ℕ) : ℤ := if side = SIDE_LONG then 1 else -1

/-- Closed lots on a reduce/flip: `min(fill, size)` (`fill_size_lots.min(pos.size_lots)`). -/
def closedLots (fillSize posSize : ℕ) : ℕ := min fillSize posSize

/-- Realized PnL in quote-lots on the reduce/flip path:
`sign · closed · (price − entry) · tick`, exactly the `i128` chain in `apply_fill`. -/
def realizedPnl (side closed entry price tick : ℕ) : ℤ :=
  sign side * (closed : ℤ) * ((price : ℤ) - (entry : ℤ)) * (tick : ℤ)

/-- Stacked size-weighted-average entry (a VWAP), floor-divided as in the `u128`
`checked_div`: `(entry·size + price·fill) / (size + fill)`. -/
def vwapEntry (entry posSize price fillSize : ℕ) : ℕ :=
  (entry * posSize + price * fillSize) / (posSize + fillSize)

/-! ### Sign of the settlement sign -/

theorem sign_long : sign SIDE_LONG = 1 := rfl

theorem sign_short : sign SIDE_SHORT = -1 := rfl

/-! ### Closed-lot count = min(fill, size) -/

/-- A pure REDUCE (`fill ≤ size`) closes exactly the fill quantity. -/
theorem closed_reduce (fillSize posSize : ℕ) (h : fillSize ≤ posSize) :
    closedLots fillSize posSize = fillSize :=
  min_eq_left h

/-- A FLIP (`size ≤ fill`) closes the entire prior position and no more. -/
theorem closed_flip (fillSize posSize : ℕ) (h : posSize ≤ fillSize) :
    closedLots fillSize posSize = posSize :=
  min_eq_right h

/-! ### Realized-PnL value -/

/-- BREAKEVEN — closing at the entry price realizes exactly zero, for either
side and any closed size / tick. -/
theorem pnl_zero_at_breakeven (side closed entry tick : ℕ) :
    realizedPnl side closed entry entry tick = 0 := by
  simp [realizedPnl]

/-- SIGN (long) — a long realizes a strictly positive PnL iff it closes ABOVE
its entry (given it actually closes lots at a real tick). No mis-signing can turn
a loss into withdrawable profit. -/
theorem long_pnl_pos_iff (closed entry price tick : ℕ)
    (hc : 0 < closed) (ht : 0 < tick) :
    0 < realizedPnl SIDE_LONG closed entry price tick ↔ entry < price := by
  have hc' : (0 : ℤ) < (closed : ℤ) := by exact_mod_cast hc
  have ht' : (0 : ℤ) < (tick : ℤ) := by exact_mod_cast ht
  unfold realizedPnl
  rw [sign_long]
  have hre : (1 : ℤ) * (closed : ℤ) * ((price : ℤ) - (entry : ℤ)) * (tick : ℤ)
      = ((closed : ℤ) * (tick : ℤ)) * ((price : ℤ) - (entry : ℤ)) := by ring
  rw [hre, mul_pos_iff_of_pos_left (mul_pos hc' ht'), sub_pos, Nat.cast_lt]

/-- SIGN (short) — a short realizes a strictly positive PnL iff it closes BELOW
its entry. Symmetric to the long case; the `−1` sign is what flips the direction. -/
theorem short_pnl_pos_iff (closed entry price tick : ℕ)
    (hc : 0 < closed) (ht : 0 < tick) :
    0 < realizedPnl SIDE_SHORT closed entry price tick ↔ price < entry := by
  have hc' : (0 : ℤ) < (closed : ℤ) := by exact_mod_cast hc
  have ht' : (0 : ℤ) < (tick : ℤ) := by exact_mod_cast ht
  unfold realizedPnl
  rw [sign_short]
  have hre : (-1 : ℤ) * (closed : ℤ) * ((price : ℤ) - (entry : ℤ)) * (tick : ℤ)
      = ((closed : ℤ) * (tick : ℤ)) * ((entry : ℤ) - (price : ℤ)) := by ring
  rw [hre, mul_pos_iff_of_pos_left (mul_pos hc' ht'), sub_pos, Nat.cast_lt]

/-- On a pure REDUCE the realized value uses the fill quantity as the closed
count — the settlement value is realized only on lots that actually close. -/
theorem realized_on_reduce (side entry price tick fill size : ℕ) (h : fill ≤ size) :
    realizedPnl side (closedLots fill size) entry price tick
      = sign side * (fill : ℤ) * ((price : ℤ) - (entry : ℤ)) * (tick : ℤ) := by
  rw [closed_reduce fill size h]; rfl

/-- MARQUEE — cross-system reconciliation with the Flash V2 notional-return
formula. V2 settles `(mark − entry)/entry · notional` with
`notional = closed·entry·tick`; multiplying out the division, the clober
integer PnL satisfies the exact identity
`pnl · entry = sign · (price − entry) · notional` at UNBOUNDED width — the two
systems settle the same value, with no rounding wedge (this is the host
`matches_v2_notional_return_formula` test lifted to all magnitudes). -/
theorem realized_reconciles (side closed entry price tick : ℕ) :
    realizedPnl side closed entry price tick * (entry : ℤ)
      = sign side * ((price : ℤ) - (entry : ℤ)) * ((closed : ℤ) * (entry : ℤ) * (tick : ℤ)) := by
  unfold realizedPnl; ring

/-! ### VWAP-entry bracket (the CBMC-intractable division-result property) -/

/-- LOWER bracket — the stacked entry is never below the cheaper of the two legs.
Requires reasoning about the RESULT of the floor division (why CBMC cannot). -/
theorem vwap_lower_bound (entry posSize price fillSize : ℕ)
    (h : 1 ≤ posSize + fillSize) :
    min entry price ≤ vwapEntry entry posSize price fillSize := by
  have hk : 0 < posSize + fillSize := h
  have hge : min entry price * (posSize + fillSize) ≤ entry * posSize + price * fillSize := by
    have e1 : min entry price * posSize ≤ entry * posSize := mul_le_mul_right' (min_le_left _ _) _
    have e2 : min entry price * fillSize ≤ price * fillSize := mul_le_mul_right' (min_le_right _ _) _
    calc min entry price * (posSize + fillSize)
        = min entry price * posSize + min entry price * fillSize := by ring
      _ ≤ entry * posSize + price * fillSize := Nat.add_le_add e1 e2
  calc min entry price
      = min entry price * (posSize + fillSize) / (posSize + fillSize) :=
        (Nat.mul_div_cancel _ hk).symm
    _ ≤ (entry * posSize + price * fillSize) / (posSize + fillSize) :=
        Nat.div_le_div_right hge
    _ = vwapEntry entry posSize price fillSize := rfl

/-- UPPER bracket — the stacked entry is never above the dearer of the two legs.
Together with `vwap_lower_bound` this is the true-VWAP sandwich the Rust
`stack_entry_is_vwap_bracketed` host sweep pins at `B = 256`; here it holds for
all magnitudes. -/
theorem vwap_upper_bound (entry posSize price fillSize : ℕ)
    (h : 1 ≤ posSize + fillSize) :
    vwapEntry entry posSize price fillSize ≤ max entry price := by
  have hk : 0 < posSize + fillSize := h
  have hle : entry * posSize + price * fillSize ≤ max entry price * (posSize + fillSize) := by
    have e1 : entry * posSize ≤ max entry price * posSize := mul_le_mul_right' (le_max_left _ _) _
    have e2 : price * fillSize ≤ max entry price * fillSize := mul_le_mul_right' (le_max_right _ _) _
    calc entry * posSize + price * fillSize
        ≤ max entry price * posSize + max entry price * fillSize := Nat.add_le_add e1 e2
      _ = max entry price * (posSize + fillSize) := by ring
  calc vwapEntry entry posSize price fillSize
      = (entry * posSize + price * fillSize) / (posSize + fillSize) := rfl
    _ ≤ max entry price * (posSize + fillSize) / (posSize + fillSize) :=
        Nat.div_le_div_right hle
    _ = max entry price := Nat.mul_div_cancel _ hk

/-- Stacking two legs at the SAME price leaves the entry at that price — no drift
is introduced by the averaging when there is nothing to average. -/
theorem vwap_same_price (entry posSize fillSize : ℕ) (h : 1 ≤ posSize + fillSize) :
    vwapEntry entry posSize entry fillSize = entry := by
  unfold vwapEntry
  have : entry * posSize + entry * fillSize = entry * (posSize + fillSize) := by ring
  rw [this, Nat.mul_div_cancel _ (show 0 < posSize + fillSize from h)]

#print axioms realized_reconciles
#print axioms long_pnl_pos_iff
#print axioms vwap_lower_bound
#print axioms vwap_upper_bound

end Clober.RealizedPnl
