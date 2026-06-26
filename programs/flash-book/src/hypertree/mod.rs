// Silence cosmetic warnings from the vendored upstream code (test-helper
// lifetimes, debug-only methods). Hygiene is upstream Manifest's concern;
// our crate's "zero warnings ever" rule applies to first-party code only.
#![allow(dead_code)]
// `mismatched_lifetime_syntaxes` is a nightly-only lint; older stable rustc
// rejects the attribute. We don't need to silence it on stable, and `#[allow]`
// for an unknown lint emits `unknown_lints` warning — defeating the purpose.
#![cfg_attr(feature = "nightly", allow(mismatched_lifetime_syntaxes))]

// VENDORED FROM Manifest's `lib/src/lib.rs` (commit @ 2026-05-10)
// Source: https://github.com/Bonasa-Tech/manifest/tree/main/lib
// License: GPL-3.0-only — see LICENSE-HYPERTREE in repo root
// Author (upstream): Britt Cyr <britt@manifest.trade> for Bonasa-Tech
//
// Why vendored vs cargo dependency: Manifest's `hypertree` crate v1.2.0
// targets edition 2024 and pulls deps that don't compile under our
// Solana-bundled rustc 1.84. Vendoring lets us pin to a known-good
// snapshot and patch as needed for our specific node payloads.
//
// We re-export the same surface so call-sites are identical to upstream.
//
// Modifications from upstream:
//   • None on the data-structure side (RBT correctness preserved).
//   • Only the `solana_program` import paths might be patched per
//     our toolchain — see individual modules for any deviations.

pub use free_list::*;
pub use hypertree::*;
pub use red_black_tree::*;
pub use utils::*;

pub mod free_list;
pub mod hypertree;
pub mod red_black_tree;
pub mod utils;

// NOTE: the upstream `llrb` module was REMOVED (audit 2026-06). The live book
// uses `RedBlackTree` exclusively; `LLRB`/`LLRBReadOnly` were never instantiated,
// yet `LLRB::remove_by_index` corrupted the tree (discarded the recursive new
// root) and leaf-delete passed `NIL` into `swap_node_with_successor`. Shipping
// a broken, `pub use`-exported data structure is a latent landmine — deleted
// rather than fixing code nothing exercises. Restore from upstream Manifest if a
// left-leaning RB tree is ever genuinely needed (and add real tests first).
