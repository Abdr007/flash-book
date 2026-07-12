import Lake
open Lake DSL

package «clober-fv»

require "leanprover-community" / "mathlib" @ git "v4.24.0"

@[default_target]
lean_lib CloberFV where
  roots := #[`Haircut, `OiMmr, `Funding, `PerDomainCredit, `RealizedPnl, `ResidualConservation, `AuthCompleteness, `VaultShares]
