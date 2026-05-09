//! Order representation in the matcher's integer lot space.

use super::lot::{BaseLots, Ticks};
use anchor_lang::prelude::*;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, AnchorSerialize, AnchorDeserialize,
)]
#[repr(u8)]
pub enum Side {
    Long = 0,
    Short = 1,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Long => Side::Short,
            Side::Short => Side::Long,
        }
    }
    pub fn is_long(self) -> bool {
        matches!(self, Side::Long)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, AnchorSerialize, AnchorDeserialize,
)]
#[repr(u8)]
pub enum OrderType {
    /// Resting maker quote. Lowest priority in the matcher.
    Limit = 0,
    /// Revealed-from-commit taker order.
    Taker = 1,
    /// Synthesized FLP virtual quote.
    FlpVirtual = 2,
    /// Liquidation order injected by the risk engine. Highest priority.
    Liquidation = 3,
    /// Auto-deleveraging — when insurance fund is exhausted.
    Adl = 4,
}

impl OrderType {
    /// Lower number = higher priority. Used in matcher's eligible-order sort.
    pub fn priority(self) -> u8 {
        match self {
            OrderType::Liquidation => 0,
            OrderType::Adl => 1,
            OrderType::Taker => 2,
            OrderType::FlpVirtual => 3,
            OrderType::Limit => 4,
        }
    }
    pub fn is_taker(self) -> bool {
        matches!(self, OrderType::Taker | OrderType::Liquidation | OrderType::Adl)
    }
}

/// Self-trade prevention mode. Carried per-order; the mode of the NEWER
/// order (higher seq) wins when the matcher detects two same-trader
/// orders that would cross.
///
///   • CancelNewest — cancel the order with the larger seq (the new one).
///     Default. Best-effort "don't trade with myself" behaviour.
///   • CancelOldest — cancel the older resting order; the new one stays.
///     Useful for MMs replacing quotes — they want the new quote up.
///   • CancelBoth — drop both, no fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AnchorSerialize, AnchorDeserialize)]
#[repr(u8)]
pub enum StpMode {
    CancelNewest = 0,
    CancelOldest = 1,
    CancelBoth = 2,
}

impl StpMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => StpMode::CancelOldest,
            2 => StpMode::CancelBoth,
            _ => StpMode::CancelNewest,
        }
    }
}

/// Order in the matcher's integer space. Trader is a 32-byte Pubkey-equivalent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AnchorSerialize, AnchorDeserialize)]
pub struct Order {
    pub id: u64,
    pub trader: Pubkey,
    pub side: Side,
    pub order_type: OrderType,
    pub size: BaseLots,
    pub limit_price: Ticks,
    /// Per-batch monotonic sequence — used for FIFO tie-breaking within a
    /// priority class.
    pub seq: u64,
    pub post_only: bool,
    pub stp_mode: StpMode,
}

impl Order {
    pub fn fifo_key(&self) -> (u8, u64) {
        (self.order_type.priority(), self.seq)
    }
}
