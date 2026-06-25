// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use anchor_lang::prelude::*;
use crate::ref_impls::*;
use crate::guards;
use qedgen_macros::qed;
use crate::Convert;

impl Convert {
    #[qed(verified, spec = "../haircut_conservation.qedspec", handler = "convert", hash = "4d2956f6a451586b", spec_hash = "53ba0666b7cbe5d9")]
    #[inline(always)]
    pub fn handler(&self, matured: u128, h: u128) -> Result<()> {
        guards::convert(self, matured, h)?;
        // Spec effect (needs fill): credited set (haircut_credit (matured) (h))
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
