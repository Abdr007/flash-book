//! Pod account layouts for the Pinocchio port. `#[repr(C)]`, 8-byte aligned,
//! NO native u128/i128 (stored as `[u8;16]` — a 16-byte-aligned field is
//! incompatible with the disc+8 data offset; see docs/CU_OPTIMIZATION.md).
// Pubkey is [u8;32] (matches pinocchio::pubkey::Pubkey) — kept local so the
// pure account math is host-testable without pulling Solana syscalls.
pub type Pubkey = [u8; 32];

pub const POSITION_DISC: [u8; 8] = [0xF1, 0x05, 0xB0, 0x0C, 0x50, 0x53, 0x00, 0x02];
/// Bootstrap-account discriminators (stamped on init, checked on every use).
pub const MARKET_DISC: [u8; 8] = [0x77, 0x4B, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01];
pub const TRADER_STATE_DISC: [u8; 8] = [0x75, 0x5A, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01];
pub const INSURANCE_DISC: [u8; 8] = [0x19, 0x5F, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01];

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
    pub _pad0: [u8; 3],
    /// Per-position max-leverage cap, set by the trader via
    /// `set_position_leverage` (bounded by `Market::max_leverage`). `0` = unset.
    /// Carved from the old 7-byte pad (4-aligned at offset 124); size unchanged
    /// (128 bytes). Enforced at order placement (a later batch).
    pub leverage_cap: u32,
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
    /// Cumulative net fees booked to the protocol (gross, before the insurance
    /// contribution split). Carved from `_reserved`; total layout size unchanged.
    pub total_fees_collected: u64,
    /// Maintenance-margin requirement in bps, fed to `assess_margin` /
    /// `verify_solvency`. Placed here (8-aligned) so the layout stays
    /// padding-free. Carved from `_reserved`; size unchanged (1152 bytes).
    pub maintenance_margin_bps: u32,
    /// Admin authority for this market (rotate sequencer, pause, set params).
    /// Set to the creator at init. Carved from `_reserved`; size unchanged.
    pub authority: Pubkey,
    /// Trading status: 0 = active, 1 = paused. Carved from `_reserved`.
    pub status: u8,
    /// Explicit padding so the following `u64`s are 8-aligned (status sits at an
    /// odd offset). Keeps the layout padding-deterministic, not compiler-implicit.
    pub _pad_liveness: [u8; 3],
    /// L1 slot of the last mark/fill update — part of the liveness signal with
    /// `last_heartbeat_slot`. A market that has never stamped it reads 0, which
    /// the liveness check treats as "no data" (never a false auto-pause). Carved
    /// from `_reserved`; size unchanged.
    pub last_mark_update_slot: u64,
    /// L1 slot of the last ER heartbeat (`er_heartbeat`). A live-but-quiet ER
    /// keeps this fresh, so `verify_market_invariants` does not auto-pause a
    /// healthy market with no recent fills. Carved from `_reserved`; size
    /// unchanged (1152 bytes).
    pub last_heartbeat_slot: u64,
    /// Concentration penalty: a position whose `size_lots` reaches this threshold
    /// pays `concentration_extra_mmr_bps` extra maintenance margin. `0` disables
    /// (the carved default — existing markets behave exactly as before). 8-aligned
    /// at offset 176. Carved from `_reserved`; size unchanged (1152 bytes).
    pub concentration_threshold_lots: u64,
    pub concentration_extra_mmr_bps: u32,
    /// OI-scaled MMR: extra maintenance bps per million lots of same-side open
    /// interest, capped at `oi_mmr_max_extra_bps` (where `0` = uncapped, matching
    /// `MarketSnapshot::effective_mmr_bps`). Both default `0` = disabled.
    pub oi_mmr_slope_bps_per_million_lots: u32,
    pub oi_mmr_max_extra_bps: u32,
    /// Max position leverage the market admits; `set_position_leverage` caps a
    /// position's `leverage_cap` against it. `0` = unset (no max enforced).
    /// 4-aligned. Carved from `_reserved`; size unchanged (1152 bytes).
    pub max_leverage: u32,
    /// 1 once `initialize_haircut_state` has enabled the haircut engine on this
    /// market (sticky). The consuming settlement check is a later batch. `u8`
    /// (align 1) carved from `_reserved`; size unchanged (1152 bytes).
    pub haircut_enabled: u8,
    pub _reserved: [u8; 951],
}

/// Market trading-status values.
pub const MARKET_STATUS_ACTIVE: u8 = 0;
pub const MARKET_STATUS_PAUSED: u8 = 1;

impl Market {
    #[inline] pub fn cum_funding(&self) -> i128 { i128::from_le_bytes(self.cum_funding_index) }
}

#[repr(C)]
pub struct TraderState {
    pub disc: [u8; 8],
    pub trader: Pubkey,
    pub collateral_quote_lots: u64,
    /// Per-trader taker-fee discount in bps (0..=BPS_DENOM), set by the protocol
    /// authority via `set_trader_fee_tier` and applied in `apply_fill`. Placed
    /// here (4-aligned) so the `repr(C)` layout stays padding-free. Carved from
    /// `_reserved`.
    pub fee_discount_bps: u32,
    /// Number of the trader's positions with size > 0. Maintained by
    /// `apply_fill` on every open (0 → >0) / close (>0 → 0) transition; gates
    /// `withdraw_collateral` (no full withdrawal while positions are open).
    pub open_positions: u8,
    /// Sub-account index: 0 = the wallet's main account (PDA
    /// `[b"trader_state", wallet]`); 1..=255 = a sub-account (PDA
    /// `[b"trader_state", wallet, [sub_index]]`). All carry `.trader = wallet`,
    /// which is what deposit/withdraw bind to.
    pub sub_index: u8,
    /// Affiliate referrer (set once via `set_trader_referrer`). Default zero =
    /// unset. Carved from `_reserved`; size unchanged (200 bytes).
    pub referrer: Pubkey,
    /// Authorized delegate that may act for this trader (`set_trader_delegate`).
    /// Default zero = none. Carved from `_reserved`.
    pub delegate: Pubkey,
    /// Builder-code recipient for order-flow fee share (`set_trader_builder`).
    /// Default zero = none. Carved from `_reserved`.
    pub builder: Pubkey,
    /// Padding so `builder_max_fee_share_bps` is 4-aligned.
    pub _pad_builder: [u8; 2],
    /// Max bps of net fee the builder may receive; forced to 0 when `builder` is
    /// unset. Validated `<= BPS_DENOM` on set.
    pub builder_max_fee_share_bps: u32,
    pub _reserved: [u8; 44],
}

impl TraderState {
    /// New `open_positions` count after one position's size transitions from
    /// `before` to `after`. Opening (0 → >0) increments; closing (>0 → 0)
    /// decrements (saturating); anything else is unchanged. Pure + host-tested.
    #[inline]
    pub fn open_positions_after(current: u8, before: u64, after: u64) -> u8 {
        if before == 0 && after > 0 {
            current.saturating_add(1)
        } else if before > 0 && after == 0 {
            current.saturating_sub(1)
        } else {
            current
        }
    }
}

#[repr(C)]
pub struct Insurance {
    pub disc: [u8; 8],
    pub balance_quote_lots: u64,
    /// Cumulative quote-lots routed into the fund (informational).
    pub total_contributions: u64,
    /// Fraction (bps) of each net fee routed to the fund; the remainder is
    /// protocol revenue. Carved from `_reserved`; total layout size unchanged.
    pub fee_contribution_bps: u32,
    /// SPL mint of the protocol quote currency. Set at init; deposits/withdrawals
    /// verify the trader ATA + vault are for this mint. Carved from `_reserved`.
    pub quote_mint: Pubkey,
    /// The protocol vault token account (authority = the Insurance PDA). Deposits
    /// transfer INTO it; withdrawals transfer OUT signed by the PDA. Carved from
    /// `_reserved`. Total layout size unchanged (200 bytes).
    pub quote_vault: Pubkey,
    /// Protocol admin: the key authorized to set per-trader fee discounts (and,
    /// in future, other global config). Set to the initializer at init. Carved
    /// from `_reserved`; size unchanged.
    pub authority: Pubkey,
    /// Padding so `pause_threshold_quote_lots` is 8-aligned (authority ends at an
    /// offset ≡ 4 mod 8).
    pub _pad_pause: [u8; 4],
    /// Balance floor (quote lots): when the fund's `balance_quote_lots` falls to
    /// or below this, markets are meant to auto-pause (the consuming check is a
    /// later batch). `0` = disabled. Set by `set_insurance_pause_threshold`.
    /// Carved from `_reserved`; size unchanged (200 bytes).
    pub pause_threshold_quote_lots: u64,
    pub _reserved: [u8; 64],
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

    /// LP shares minted for a deposit of `amount` quote-lots into a pool with
    /// `outstanding` shares and `nav`. First deposit (outstanding == 0) mints
    /// 1:1. `None` if the pool has shares but non-positive NAV (insolvent —
    /// can't price), or on overflow. Pure + host-tested.
    #[inline]
    pub fn shares_for_deposit(amount: u64, outstanding: u64, nav: i128) -> Option<u64> {
        if outstanding == 0 {
            return Some(amount);
        }
        if nav <= 0 {
            return None;
        }
        let s = (amount as u128).checked_mul(outstanding as u128)? / (nav as u128);
        if s > u64::MAX as u128 {
            None
        } else {
            Some(s as u64)
        }
    }

    /// Quote-lots returned for burning `shares` from a pool with `outstanding`
    /// shares and `nav`. `None` if `outstanding == 0`, `nav <= 0`, or on
    /// overflow. Pure + host-tested.
    #[inline]
    pub fn amount_for_shares(shares: u64, outstanding: u64, nav: i128) -> Option<u64> {
        if outstanding == 0 || nav <= 0 {
            return None;
        }
        let a = (shares as u128).checked_mul(nav as u128)? / (outstanding as u128);
        if a > u64::MAX as u128 {
            None
        } else {
            Some(a as u64)
        }
    }
}

pub const TRIGGER_ORDER_V3_DISC: [u8; 8] = [0x71, 0x67, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];
pub const TWAP_ORDER_V3_DISC: [u8; 8] = [0x77, 0xA9, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];
pub const ICEBERG_ORDER_V3_DISC: [u8; 8] = [0x1C, 0xEB, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// V3 native trigger order. Pod mirror of `TriggerOrderAccountV3` (136 B).
/// Fields reordered so the `repr(C)` layout has no implicit padding.
#[repr(C)]
pub struct TriggerOrderV3 {
    pub disc: [u8; 8],
    pub trader: Pubkey,
    pub market: Pubkey,
    pub size_lots: u64,
    pub trigger_price_ticks: u64,
    pub limit_price_ticks: u64,
    pub created_at_slot: u64,
    pub expires_at_slot: u64,
    pub acceptable_price_ticks: u64,
    pub bump: u8,
    pub trigger_id: u8,
    pub side: u8,
    pub kind: u8,
    pub flags: u8,
    pub sub_index: u8,
    pub _reserved: [u8; 10],
}

/// V3 TWAP order. Pod mirror of `TwapOrderAccountV3` (152 B).
#[repr(C)]
pub struct TwapOrderV3 {
    pub disc: [u8; 8],
    pub trader: Pubkey,
    pub market: Pubkey,
    pub slice_size_lots: u64,
    pub total_size_lots: u64,
    pub size_executed_lots: u64,
    pub limit_price_ticks: u64,
    pub start_slot: u64,
    pub slot_interval: u64,
    pub end_slot: u64,
    pub last_slice_at_slot: u64,
    pub acceptable_price_ticks: u64,
    pub bump: u8,
    pub twap_id: u8,
    pub side: u8,
    pub flags: u8,
    pub sub_index: u8,
    pub _reserved: [u8; 3],
}

/// V3 iceberg order. Pod mirror of `IcebergOrderAccountV3` (136 B).
#[repr(C)]
pub struct IcebergOrderV3 {
    pub disc: [u8; 8],
    pub trader: Pubkey,
    pub market: Pubkey,
    pub limit_ticks: u64,
    pub total_size_lots: u64,
    pub remaining_lots: u64,
    pub displayed_size_lots: u64,
    pub child_order_seq: u64,
    pub created_at_slot: u64,
    pub expires_at_slot: u64,
    pub bump: u8,
    pub iceberg_id: u8,
    pub side: u8,
    pub flags: u8,
    pub sub_index: u8,
    pub _reserved: [u8; 3],
}

pub const VAULT_V3_DISC: [u8; 8] = [0x7A, 0x17, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// V3 strategist vault. Pod mirror of `VaultAccountV3` (152 B).
#[repr(C)]
pub struct VaultV3 {
    pub disc: [u8; 8],
    pub strategist: Pubkey,
    pub name: [u8; 32],
    pub shares_outstanding: u64,
    pub total_capital_quote_lots: u64,
    pub hwm_nav_per_share_u64x6: u64,
    pub last_perf_settlement_unix: u64,
    pub total_perf_shares_minted: u64,
    pub perf_fee_bps: u32,
    pub bump: u8,
    pub vault_id: u8,
    pub accept_deposits: u8,
    pub _pad0: u8,
    pub _reserved: [u8; 32],
}

pub const VAULT_POSITION_V3_DISC: [u8; 8] = [0x7A, 0x18, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// V3 vault depositor position. Pod mirror of `VaultPositionAccountV3` (120 B).
#[repr(C)]
pub struct VaultPositionV3 {
    pub disc: [u8; 8],
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub shares: u64,
    pub total_deposited_quote_lots: u64,
    pub total_withdrawn_quote_lots: u64,
    pub bump: u8,
    pub _reserved: [u8; 23],
}

pub const LEVERAGE_TIERS_DISC: [u8; 8] = [0x1E, 0x5E, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01];

/// One rung of the leverage tier table. 16 B, 8-aligned.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LeverageTier {
    pub min_notional_quote_lots: u64,
    pub mmr_bps: u32,
    pub _pad: [u8; 4],
}

pub const MAX_LEVERAGE_TIERS: usize = 8;

/// Per-market leverage / maintenance-margin tier table. Pod mirror of
/// `MarketLeverageTiersAccount` (176 B). `tiers` sorted ascending by notional.
#[repr(C)]
pub struct MarketLeverageTiers {
    pub disc: [u8; 8],
    pub market: Pubkey,
    pub bump: u8,
    pub tier_count: u8,
    pub _pad0: [u8; 6],
    pub tiers: [LeverageTier; MAX_LEVERAGE_TIERS],
}

pub const FLP_PER_MARKET_V3_DISC: [u8; 8] = [0xF1, 0x9D, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-market FLP exposure (v3, independent ER-delegation per market). Pod
/// mirror of `FlpExposurePerMarketAccountV3` (136 B). `side`: 0=long, 1=short,
/// 255=empty.
#[repr(C)]
pub struct FlpExposurePerMarketV3 {
    pub disc: [u8; 8],
    pub market: Pubkey,
    pub authority: Pubkey,
    pub size_lots: u64,
    pub entry_price_ticks: u64,
    pub total_capital_quote_lots: u64,
    pub realized_pnl: i64,
    pub lp_shares_outstanding: u64,
    pub bump: u8,
    pub side: u8,
    pub _reserved: [u8; 22],
}
impl FlpExposurePerMarketV3 {
    /// NAV in quote lots (capital + realized PnL; may go negative).
    #[inline]
    pub fn nav(&self) -> i128 {
        (self.total_capital_quote_lots as i128) + (self.realized_pnl as i128)
    }
}

pub const FLP_POSITION_V3_DISC: [u8; 8] = [0xF1, 0x90, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-LP, per-market FLP shares balance (v3). Pod mirror of
/// `FlpPositionAccountV3` (104 B).
#[repr(C)]
pub struct FlpPositionV3 {
    pub disc: [u8; 8],
    pub market: Pubkey,
    pub lp: Pubkey,
    pub shares: u64,
    pub bump: u8,
    pub _reserved: [u8; 23],
}

pub const LP_POSITION_DISC: [u8; 8] = [0x1B, 0x90, 0x00, 0x12, 0x34, 0x56, 0x78, 0x01];

/// Singleton-pool LP shares balance. Pod mirror of `LpPositionAccount` (104 B).
#[repr(C)]
pub struct LpPosition {
    pub disc: [u8; 8],
    pub lp: Pubkey,
    pub shares: u64,
    pub total_deposited_quote_lots: u64,
    pub total_withdrawn_quote_lots: u64,
    pub bump: u8,
    pub _reserved: [u8; 39],
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
const _: () = assert!(core::mem::size_of::<TriggerOrderV3>() == 136);
const _: () = assert!(core::mem::size_of::<TwapOrderV3>() == 152);
const _: () = assert!(core::mem::size_of::<IcebergOrderV3>() == 136);
const _: () = assert!(core::mem::size_of::<VaultV3>() == 152);
const _: () = assert!(core::mem::size_of::<VaultPositionV3>() == 120);
const _: () = assert!(core::mem::size_of::<LeverageTier>() == 16);
const _: () = assert!(core::mem::size_of::<MarketLeverageTiers>() == 176);
const _: () = assert!(core::mem::size_of::<FlpExposurePerMarketV3>() == 136);
const _: () = assert!(core::mem::size_of::<FlpPositionV3>() == 104);
const _: () = assert!(core::mem::size_of::<LpPosition>() == 104);

pub const HAIRCUT_STATE_DISC: [u8; 8] = [0x4A, 0x12, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-market haircut (positive-PnL warmup) state. The four 128-bit accumulators
/// are stored as `[u8; 16]` LE byte arrays (read via `u128::from_le_bytes`), NOT
/// native `u128`, so the zero-copy account needs no 16-byte alignment — same as
/// `MarketSideAccrual`. Grouped-by-alignment, padding-free `repr(C)` at 208
/// bytes. Feeds the host-tested `haircut` math (`compute_h`, etc.).
#[repr(C)]
pub struct MarketHaircutState {
    pub disc: [u8; 8],
    pub market: Pubkey,
    // ── u64 group ──────────────────────────────────────────────────────
    pub h_min_slots: u64,
    pub h_max_slots: u64,
    pub h_scaled_cached: u64,
    pub h_cached_at_slot: u64,
    // ── u8 + pad ───────────────────────────────────────────────────────
    pub bump: u8,
    pub _pad0: [u8; 7],
    // ── 128-bit accumulators as LE bytes (align 1) ─────────────────────
    pub residual_quote_lots: [u8; 16],
    pub matured_pos_total_quote_lots: [u8; 16],
    pub realized_loss_total_quote_lots: [u8; 16],
    pub dust_accrued_quote_lots: [u8; 16],
    pub _reserved: [u8; 64],
}
const _: () = assert!(core::mem::size_of::<MarketHaircutState>() == 208);

pub const SIDE_ACCRUAL_DISC: [u8; 8] = [0x51, 0xDE, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-market side-accrual (ADL) state — long + short sides packed. The 128-bit
/// indices (`a`/`k`/`f`/`b`) are stored as `[u8; 16]` little-endian (read via
/// `i128/u128::from_le_bytes`), NOT native `u128`, so the zero-copy account never
/// needs 16-byte alignment (Solana account data is only 8-aligned) — the same
/// trick as `Market::cum_funding_index`. Grouped-by-alignment, padding-free
/// `repr(C)` at 280 bytes (a fresh port-only account). Feeds the host-tested
/// `side_accrual` math; `a` starts at `side_accrual::ADL_ONE`.
#[repr(C)]
pub struct MarketSideAccrual {
    pub disc: [u8; 8],
    pub market: Pubkey,
    // ── u64 group (8-aligned) ──────────────────────────────────────────
    pub long_slot_last: u64,
    pub long_price_last: u64,
    pub short_slot_last: u64,
    pub short_price_last: u64,
    // ── u32 group ──────────────────────────────────────────────────────
    pub long_epoch: u32,
    pub short_epoch: u32,
    // ── u8 group ───────────────────────────────────────────────────────
    pub bump: u8,
    pub long_mode: u8,
    pub short_mode: u8,
    pub _pad0: u8,
    // ── 128-bit indices as LE bytes (align 1) ──────────────────────────
    pub long_a: [u8; 16],
    pub long_k: [u8; 16],
    pub long_f: [u8; 16],
    pub long_b: [u8; 16],
    pub short_a: [u8; 16],
    pub short_k: [u8; 16],
    pub short_f: [u8; 16],
    pub short_b: [u8; 16],
    pub _reserved: [u8; 64],
}
const _: () = assert!(core::mem::size_of::<MarketSideAccrual>() == 280);

pub const ORACLE_CONFIG_DISC: [u8; 8] = [0x09, 0xAC, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-market oracle config. `source`: 0 = trusted `update_oracle` (the simplified
/// port default), 1 = Pyth pull (the Pyth consumer is a later batch). Grouped-by-
/// alignment, padding-free `repr(C)` at 88 bytes (a fresh port-only account).
#[repr(C)]
pub struct MarketOracleConfig {
    pub disc: [u8; 8],
    pub market: Pubkey,
    /// 32-byte Pyth feed identifier (when `source == 1`).
    pub pyth_price_feed_id: [u8; 32],
    pub max_staleness_seconds: u32,
    pub max_confidence_bps: u32,
    /// Tick decimal scaling (`scale_exp = pyth.exponent + tick_decimals`).
    pub tick_decimals: i8,
    pub source: u8,
    pub bump: u8,
    pub _pad: [u8; 5],
}
const _: () = assert!(core::mem::size_of::<MarketOracleConfig>() == 88);

/// `source` values for `MarketOracleConfig`.
pub const ORACLE_SOURCE_TRUSTED: u8 = 0;
pub const ORACLE_SOURCE_PYTH: u8 = 1;

pub const ENVELOPE_CONFIG_DISC: [u8; 8] = [0xE0, 0x0E, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-market envelope (price-band / risk-invariant) config. The 7 envelope
/// params are validated by `envelope::prove_envelope` before they are written.
/// Laid out grouped-by-alignment (a fresh, port-only account — no anchor byte
/// parity needed), so the `repr(C)` layout is padding-free at 168 bytes. The
/// `last_observed_*` / `gate_*` runtime fields are owned by the (deferred)
/// matching-side price gate; the config setter leaves them at 0.
#[repr(C)]
pub struct MarketEnvelopeConfig {
    pub disc: [u8; 8],
    pub market: Pubkey,
    // ── u64/i64 group (8-aligned) ──────────────────────────────────────
    pub max_accrual_dt_slots: u64,
    pub max_abs_funding_e9_per_slot: i64,
    pub min_liquidation_abs_lots: u64,
    pub min_nonzero_mm_req_lots: u64,
    /// Slot at which params were last proven + set (bumps on every update).
    pub last_proven_at_slot: u64,
    pub last_observed_slot: u64,
    pub last_observed_price_ticks: u64,
    pub gate_passes: u64,
    pub gate_rejects: u64,
    // ── u32 group ──────────────────────────────────────────────────────
    pub max_price_move_bps_per_slot: u32,
    pub maintenance_bps: u32,
    pub liquidation_fee_bps: u32,
    /// Monotonic version counter, bumped on every successful set.
    pub version: u32,
    // ── u8 + padding ───────────────────────────────────────────────────
    pub bump: u8,
    pub _pad: [u8; 7],
    pub _reserved: [u8; 32],
}
const _: () = assert!(core::mem::size_of::<MarketEnvelopeConfig>() == 168);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flp_share_math_round_trips_and_guards() {
        // First deposit mints 1:1.
        assert_eq!(FlpExposure::shares_for_deposit(1_000, 0, 0), Some(1_000));
        // Second deposit at NAV == capital == 1000, 1000 shares: 1:1 still.
        assert_eq!(FlpExposure::shares_for_deposit(500, 1_000, 1_000), Some(500));
        // After the pool gained (nav 2000 vs 1000 shares), new deposit gets
        // fewer shares per lot: 500 * 1000 / 2000 = 250.
        assert_eq!(FlpExposure::shares_for_deposit(500, 1_000, 2_000), Some(250));
        // Insolvent pool (nav <= 0 with shares outstanding) can't be priced.
        assert_eq!(FlpExposure::shares_for_deposit(500, 1_000, 0), None);
        assert_eq!(FlpExposure::shares_for_deposit(500, 1_000, -5), None);

        // Redeem is the inverse: burning 250 shares at nav 2000 / 1000 shares
        // returns 500 lots.
        assert_eq!(FlpExposure::amount_for_shares(250, 1_000, 2_000), Some(500));
        assert_eq!(FlpExposure::amount_for_shares(100, 0, 1_000), None);
        assert_eq!(FlpExposure::amount_for_shares(100, 1_000, 0), None);
    }

    #[test]
    fn open_positions_transitions() {
        // open: 0 -> >0 increments
        assert_eq!(TraderState::open_positions_after(0, 0, 5), 1);
        assert_eq!(TraderState::open_positions_after(2, 0, 5), 3);
        // close: >0 -> 0 decrements (saturating)
        assert_eq!(TraderState::open_positions_after(1, 5, 0), 0);
        assert_eq!(TraderState::open_positions_after(0, 5, 0), 0);
        // increase / reduce (both nonzero) — unchanged
        assert_eq!(TraderState::open_positions_after(1, 5, 10), 1);
        assert_eq!(TraderState::open_positions_after(1, 10, 3), 1);
        // no position either side — unchanged
        assert_eq!(TraderState::open_positions_after(2, 0, 0), 2);
    }
}
