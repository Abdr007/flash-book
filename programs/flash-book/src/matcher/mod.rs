//! Pure-Rust matcher core — no Solana account dependencies, fully unit
//! testable. All arithmetic is integer with checked overflow.

pub mod committee;
pub mod envelope;
pub mod fill_commitment;
pub mod fill_outbox;
pub mod flp_quoter;
pub mod funding;
pub mod haircut;
pub mod insurance;
pub mod jit_lp_defense;
pub mod liquidation;
pub mod lot;
pub mod merkle;
pub mod order;
pub mod position_math;
pub mod reduce_only;
pub mod risk;
pub mod side_accrual;
pub mod vpin;

#[cfg(test)]
mod tests;
