//! Reserved VPIN accumulator slot on `MarketAccount`.
//!
//! `VpinState` occupies 32 bytes of the serialized `MarketAccount` layout.
//! The accumulator is retired: no instruction advances it, every market
//! carries the zero value, and no pricing or fee path reads it. The struct
//! remains solely to keep the on-chain account layout stable; repurposing
//! or removing these bytes is a state migration.

use anchor_lang::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, AnchorSerialize, AnchorDeserialize, Default)]
pub struct VpinState {
    pub buy_pending: u64,
    pub sell_pending: u64,
    pub buckets_observed: u64,
    pub value_q32_32: u64,
}
