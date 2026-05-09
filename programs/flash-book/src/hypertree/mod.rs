// Silence cosmetic warnings from the vendored upstream code (test-helper
// lifetimes, debug-only methods). Hygiene is upstream Manifest's concern;
// our crate's "zero warnings ever" rule applies to first-party code only.
#![allow(dead_code)]
#![allow(elided_named_lifetimes)]

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
pub use llrb::*;
pub use red_black_tree::*;
pub use utils::*;

pub mod free_list;
pub mod hypertree;
pub mod llrb;
pub mod red_black_tree;
pub mod utils;
