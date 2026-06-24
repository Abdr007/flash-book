//! Pod account layouts for the Pinocchio port. `#[repr(C)]`, 8-byte aligned,
//! NO native u128/i128 (stored as `[u8;16]` — a 16-byte-aligned field is
//! incompatible with the disc+8 data offset; see docs/CU_OPTIMIZATION.md).
// Pubkey is [u8;32] (matches pinocchio::pubkey::Pubkey) — kept local so the
// pure account math is host-testable without pulling Solana syscalls.
pub type Pubkey = [u8; 32];

pub const POSITION_DISC: [u8; 8] = [0xF1, 0x05, 0xB0, 0x0C, 0x50, 0x53, 0x00, 0x02];

#[repr(C)]
pub struct Position {
    pub disc: [u8; 8],
    pub cum_funding_index: [u8; 16], // i128 LE
    pub trader: Pubkey,
    pub market: Pubkey,
    pub size_lots: u64,
    pub entry_price_ticks: u64,
    pub collateral_quote_lots: u64,
    pub realized_pnl_quote_lots: i64,
    pub side: u8, // 0 = long, 1 = short
    pub _pad: [u8; 7],
}
impl Position {
    #[inline] pub fn cum_funding(&self) -> i128 { i128::from_le_bytes(self.cum_funding_index) }
    #[inline] pub fn set_cum_funding(&mut self, v: i128) { self.cum_funding_index = v.to_le_bytes(); }
}

#[repr(C)]
pub struct Market {
    pub disc: [u8; 8],
    pub sequencer: Pubkey,
    pub cum_funding_index: [u8; 16],
    pub long_oi_lots: u64,
    pub short_oi_lots: u64,
    pub tick_size: u64,
    pub taker_fee_bps: u32,
    pub maker_rebate_bps: i32,
    pub mark_price_ticks: u64,
    pub min_base_lots: u64,
    pub max_oi_base_lots: u64,
    pub _reserved: [u8; 1040],
}
impl Market {
    #[inline] pub fn cum_funding(&self) -> i128 { i128::from_le_bytes(self.cum_funding_index) }
}

#[repr(C)]
pub struct TraderState {
    pub disc: [u8; 8],
    pub trader: Pubkey,
    pub collateral_quote_lots: u64,
    pub _reserved: [u8; 152],
}

#[repr(C)]
pub struct Insurance {
    pub disc: [u8; 8],
    pub balance_quote_lots: u64,
    pub _reserved: [u8; 184],
}

pub const FEE_TIERS_DISC: [u8; 8] = [0xFE, 0xE7, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01];

/// One rung of the fee-tier table. Signed `maker_rebate_bps`: positive = rebate
/// to maker, negative = fee from maker. 16 B, 8-aligned.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FeeTier {
    pub min_volume_quote_lots: u64,
    pub maker_rebate_bps: i32,
    pub taker_fee_bps: u32,
}

pub const MAX_FEE_TIERS: usize = 10;

/// Volume-based fee-tier table. Pod mirror of `FeeTiersAccount` — feeds
/// `fees::resolve_fee_tier`. `tiers` is sorted ascending by `min_volume`.
#[repr(C)]
pub struct FeeTiers {
    pub disc: [u8; 8],
    pub authority: Pubkey,
    pub bump: u8,
    pub tier_count: u8,
    pub _pad0: [u8; 6],
    pub volume_window_slots: u64,
    pub tiers: [FeeTier; MAX_FEE_TIERS],
}

pub const FLP_EXPOSURE_DISC: [u8; 8] = [0xF1, 0x9E, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01];

/// Per-market FLP exposure entry. Fields reordered vs the Anchor struct so the
/// `repr(C)` layout has no implicit padding. `side`: 0 = long, 1 = short,
/// 255 = empty. 56 B, 8-aligned.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FlpMarketExposure {
    pub market: Pubkey,
    pub size_lots: u64,
    pub entry_price_ticks: u64,
    pub side: u8,
    pub _pad: [u8; 7],
}

/// FLP pool exposure / NAV terms. Pod mirror of `FlpExposureAccount`.
#[repr(C)]
pub struct FlpExposure {
    pub disc: [u8; 8],
    pub authority: Pubkey,
    pub total_capital_quote_lots: u64,
    pub realized_pnl: i64,
    pub lp_shares_outstanding: u64,
    pub bump: u8,
    pub markets_count: u8,
    pub _pad0: [u8; 6],
    pub per_market: [FlpMarketExposure; 16],
}
impl FlpExposure {
    /// Net Asset Value in quote lots (capital + realized PnL; may go negative).
    #[inline]
    pub fn nav(&self) -> i128 {
        (self.total_capital_quote_lots as i128) + (self.realized_pnl as i128)
    }
}

// Compile-time size checks (8-aligned, sized to the real accounts).
const _: () = assert!(core::mem::size_of::<Position>() == 128);
const _: () = assert!(core::mem::size_of::<Market>() == 1152);
const _: () = assert!(core::mem::size_of::<TraderState>() == 200);
const _: () = assert!(core::mem::size_of::<Insurance>() == 200);
const _: () = assert!(core::mem::size_of::<FeeTier>() == 16);
const _: () = assert!(core::mem::size_of::<FeeTiers>() == 216);
const _: () = assert!(core::mem::size_of::<FlpMarketExposure>() == 56);
const _: () = assert!(core::mem::size_of::<FlpExposure>() == 968);
