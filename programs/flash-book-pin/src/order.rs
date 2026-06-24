//! Order representation in the matcher's integer lot space.
//!
//! De-anchored port of `matcher/order.rs`: enums + `Order` are verbatim; anchor
//! derives dropped, `Pubkey` is the crate's `[u8;32]` alias.

use crate::lot::{BaseLots, Ticks};
use crate::state::Pubkey;

/// Canonical taker side. (NOTE: `vpin.rs` keeps a small local `Side`; both
/// mirror this — consolidation is a follow-up cleanup.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OrderType {
    Limit = 0,
    Taker = 1,
    FlpVirtual = 2,
    Liquidation = 3,
    Adl = 4,
}

impl OrderType {
    /// Lower number = higher priority (matcher eligible-order sort).
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

/// Self-trade prevention mode (the NEWER order's mode wins on a same-trader cross).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Order in the matcher's integer space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Order {
    pub id: u64,
    pub trader: Pubkey,
    pub side: Side,
    pub order_type: OrderType,
    pub size: BaseLots,
    pub limit_price: Ticks,
    /// Per-batch monotonic sequence — FIFO tie-break within a priority class.
    pub seq: u64,
    pub post_only: bool,
    pub stp_mode: StpMode,
}

impl Order {
    pub fn fifo_key(&self) -> (u8, u64) {
        (self.order_type.priority(), self.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_opposite_and_is_long() {
        assert_eq!(Side::Long.opposite(), Side::Short);
        assert_eq!(Side::Short.opposite(), Side::Long);
        assert!(Side::Long.is_long());
        assert!(!Side::Short.is_long());
    }

    #[test]
    fn order_type_priority_orders_liquidation_first() {
        assert!(OrderType::Liquidation.priority() < OrderType::Adl.priority());
        assert!(OrderType::Adl.priority() < OrderType::Taker.priority());
        assert!(OrderType::Taker.priority() < OrderType::FlpVirtual.priority());
        assert!(OrderType::FlpVirtual.priority() < OrderType::Limit.priority());
    }

    #[test]
    fn is_taker_classes() {
        assert!(OrderType::Taker.is_taker());
        assert!(OrderType::Liquidation.is_taker());
        assert!(OrderType::Adl.is_taker());
        assert!(!OrderType::Limit.is_taker());
        assert!(!OrderType::FlpVirtual.is_taker());
    }

    #[test]
    fn stp_from_u8_defaults_to_newest() {
        assert_eq!(StpMode::from_u8(1), StpMode::CancelOldest);
        assert_eq!(StpMode::from_u8(2), StpMode::CancelBoth);
        assert_eq!(StpMode::from_u8(0), StpMode::CancelNewest);
        assert_eq!(StpMode::from_u8(99), StpMode::CancelNewest);
    }

    #[test]
    fn fifo_key_is_priority_then_seq() {
        let o = Order {
            id: 1,
            trader: [0u8; 32],
            side: Side::Long,
            order_type: OrderType::Taker,
            size: BaseLots(10),
            limit_price: Ticks(100),
            seq: 42,
            post_only: false,
            stp_mode: StpMode::CancelNewest,
        };
        assert_eq!(o.fifo_key(), (OrderType::Taker.priority(), 42));
    }
}
