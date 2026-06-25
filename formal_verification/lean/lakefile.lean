import Lake
open Lake DSL

package «flash-book-fv»

require "leanprover-community" / "mathlib" @ git "v4.24.0"

@[default_target]
lean_lib FlashBookFV where
  roots := #[`Haircut, `OiMmr, `Funding]
