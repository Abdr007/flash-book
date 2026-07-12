// Silence cosmetic warnings from the vendored upstream code (test-helper
// lifetimes, debug-only methods); the crate's zero-warning rule applies to
// first-party code.
#![allow(dead_code)]
// The vendored tests keep upstream's stylistic patterns (index arithmetic
// like `WIDTH * 0`, drop-for-lifetime, module-per-file inception).
#![allow(clippy::erasing_op)]
#![allow(clippy::unnecessary_mut_passed)]
#![allow(clippy::drop_non_drop)]
#![allow(clippy::module_inception)]
// `mismatched_lifetime_syntaxes` is a nightly-only lint; older stable rustc
// rejects the attribute. We don't need to silence it on stable, and `#[allow]`
// for an unknown lint emits `unknown_lints` warning — defeating the purpose.
#![cfg_attr(feature = "nightly", allow(mismatched_lifetime_syntaxes))]

// Vendored third-party code.
// Source: https://github.com/Bonasa-Tech/manifest/tree/main/lib (hypertree)
// License: GPL-3.0-only — see LICENSE-HYPERTREE in the repo root.
// Author (upstream): Britt Cyr <britt@manifest.trade> for Bonasa-Tech.
//
// Vendored (rather than a cargo dependency) to pin a known-good snapshot
// that compiles under the Solana-bundled toolchain. The upstream surface
// is re-exported unchanged; the upstream `llrb` module is excluded (see
// note below).

pub use free_list::*;
pub use hypertree::*;
pub use red_black_tree::*;
pub use utils::*;

pub mod free_list;
pub mod hypertree;
pub mod red_black_tree;
pub mod utils;

// The upstream `llrb` module is deliberately not vendored: the book uses
// `RedBlackTree` exclusively, and upstream `LLRB::remove_by_index` corrupts
// its tree (it discards the recursive new root, and leaf-delete passes `NIL`
// into `swap_node_with_successor`). Nothing here may export a broken data
// structure; restore it from upstream only with real tests if a left-leaning
// RB tree is ever needed.
