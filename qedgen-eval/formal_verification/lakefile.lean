import Lake
open Lake DSL

package haircut_conservationProofs

require qedgenSupport from
  "./lean_solana"

require "leanprover-community" / "mathlib" @ git "v4.24.0"

@[default_target]
lean_lib Haircut_conservationSpec where
  roots := #[`Spec, `Proofs, `Standalone]
