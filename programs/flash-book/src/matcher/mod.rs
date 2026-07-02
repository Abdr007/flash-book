//! Pure-Rust matcher core — no Solana account dependencies, fully unit
//! testable. All arithmetic is integer with checked overflow.

pub mod borrow_fee;
pub mod envelope;
pub mod fill_commitment;
pub mod fill_outbox;
pub mod flp_quoter;
pub mod funding;
pub mod funding_velocity;
pub mod haircut;
pub mod insurance;
pub mod insurance_replenish;
pub mod liquidation;
pub mod lot;
pub mod order;
pub mod cancel_on_disconnect;
pub mod conditional_cancel;
pub mod jit_lp_defense;
pub mod min_fill_size;
pub mod mit_order;
pub mod peg_pricing;
pub mod pro_rata;
pub mod reduce_only;
pub mod risk;
pub mod self_trade;
pub mod stop_limit;
pub mod tiered_lp_rewards;
pub mod trailing_stop;
pub mod side_accrual;
pub mod v2_bookkeeping;
pub mod vpin;

#[cfg(test)]
mod tests;
