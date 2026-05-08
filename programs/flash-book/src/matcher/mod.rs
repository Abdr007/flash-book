//! Pure-Rust matcher core — no Solana account dependencies, fully unit
//! testable. All arithmetic is integer with checked overflow.

pub mod fba;
pub mod flp_quoter;
pub mod funding;
pub mod lot;
pub mod order;
pub mod vpin;

#[cfg(test)]
mod tests;
