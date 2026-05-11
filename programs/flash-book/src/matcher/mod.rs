//! Pure-Rust matcher core — no Solana account dependencies, fully unit
//! testable. All arithmetic is integer with checked overflow.

pub mod flp_quoter;
pub mod funding;
pub mod insurance;
pub mod liquidation;
pub mod lot;
pub mod order;
pub mod risk;
pub mod v2_bookkeeping;
pub mod vpin;

#[cfg(test)]
mod tests;
