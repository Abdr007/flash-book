//! Pure-Rust matcher core — no Solana account dependencies, fully unit
//! testable. All arithmetic is integer with checked overflow.

pub mod commit_reveal;
pub mod fba;
pub mod flp_quoter;
pub mod funding;
pub mod insurance;
pub mod liquidation;
pub mod lot;
pub mod order;
pub mod risk;
pub mod vpin;

#[cfg(test)]
mod tests;
