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
    /// The trader_state SUB-ACCOUNT this position belongs to (0 = main). Stamped
    /// from `ts.sub_index` on the first fill and asserted on every read. A wallet
    /// owns one trader_state per sub_index (all carry `.trader = wallet`), so
    /// `(trader, sub_index)` bijectively identifies the trader_state — exactly the
    /// binding Anchor gets from keying the position PDA on `trader_state.key()`.
    /// Without it, a wallet could substitute a sub-account's position into another
    /// trader_state's cross-margin solvency gate (→ bad debt). Carved from the old
    /// `_pad0[3]`; size unchanged (128 bytes), `leverage_cap` still 4-aligned @124.
    pub sub_index: u8,
    pub _pad0: [u8; 2],
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
    /// Liquidation params, fed to `liquidate_position_v2` (all `0` =
    /// disabled/default). `liquidation_auction_duration_slots`: the Dutch-auction
    /// ramp length over which the liquidator reward grows from 0 to the full
    /// `liquidator_reward_bps` (0 = flat reward, no auction).
    /// `liquidation_cooldown_slots`: min slots between liquidations of the same
    /// position (0 = no cooldown). `liq_penalty_bps`: the synthetic-close penalty
    /// moving the fill against the liquidatee. `liquidator_reward_bps`: the base
    /// reward. The two `u64`s precede the `u32`s so the carve is 8-aligned +
    /// padding-free. Carved from `_reserved`; size unchanged (1152 bytes).
    pub liquidation_auction_duration_slots: u64,
    pub liquidation_cooldown_slots: u64,
    pub liq_penalty_bps: u32,
    pub liquidator_reward_bps: u32,
    /// 1 once `initialize_haircut_state` has enabled the haircut engine on this
    /// market (sticky). The consuming settlement check is a later batch. `u8`
    /// (align 1) carved from `_reserved`; size unchanged (1152 bytes).
    pub haircut_enabled: u8,
    /// Pads `book_delegated_at_slot` up to the next 8-aligned offset (232) so the
    /// `repr(C)` layout of every preceding field — and the 1152-byte size — is
    /// unchanged.
    pub _pad_bdas: [u8; 7],
    /// Slot at which this market's book was delegated to the ER (0 = not stamped).
    /// The baseline `stamp_book_liveness_baseline` records, against which a later
    /// force-undelegate / escape measures censorship. Carved from `_reserved`.
    pub book_delegated_at_slot: u64,
    /// 1 once `init_fill_commitment` has armed the settlement-authenticity ring on
    /// this market (sticky): settlement then REQUIRES a matching committed fill.
    /// `u8` (align 1) carved from `_reserved`; size unchanged (1152 bytes).
    pub fill_commitment_required: u8,
    /// Monotonic settlement nonce. Every `apply_fill` / `apply_flp_fill` must
    /// present a `fill_seq` STRICTLY greater than this; the handler advances it
    /// atomically with the fill. Rejects replayed / reordered settlement txs
    /// (parity with the Anchor `Market.last_settlement_seq` + `advance_settlement_seq`).
    /// `[u8; 8]` (align 1, LE) carved from `_reserved` at the non-8-aligned offset
    /// 241; size unchanged (1152 bytes). Pre-field markets read 0.
    pub last_settlement_seq: [u8; 8],
    /// Funding-rate engine state (Wave 25b/37, re-audit 2026-06-30). Stored LE
    /// (align 1) carved from `_reserved` so the 1152-byte layout is unchanged; all
    /// `0` on a pre-field market ⇒ funding stays INERT (skew_factor 0 ⇒ target 0,
    /// velocity 0 ⇒ rate pinned, max_rate 0 ⇒ no accrual — existing behaviour).
    /// `advance_funding` ramps `funding_rate_e9` toward the OI-skew target and
    /// accrues it into `cum_funding_index`; `set_funding_params` (admin) turns it on.
    pub funding_rate_e9: [u8; 8],        // i64 LE — current funding rate (e9, per slot)
    pub last_funding_slot: [u8; 8],      // u64 LE — last advance (0 = unstamped baseline)
    pub funding_skew_factor_e9: [u8; 4], // u32 LE — K: e9 funding per unit normalized skew
    pub funding_velocity_e9: [u8; 4],    // u32 LE — max rate change per slot (ramp velocity)
    pub max_funding_rate_e9: [u8; 4],    // u32 LE — saturating cap on |rate|
    pub _reserved: [u8; 875],
}

/// Market trading-status values.
pub const MARKET_STATUS_ACTIVE: u8 = 0;
pub const MARKET_STATUS_PAUSED: u8 = 1;

impl Market {
    #[inline] pub fn cum_funding(&self) -> i128 { i128::from_le_bytes(self.cum_funding_index) }
    #[inline] pub fn set_cum_funding(&mut self, v: i128) { self.cum_funding_index = v.to_le_bytes(); }
    #[inline] pub fn settlement_seq(&self) -> u64 { u64::from_le_bytes(self.last_settlement_seq) }
    #[inline] pub fn set_settlement_seq(&mut self, v: u64) { self.last_settlement_seq = v.to_le_bytes(); }
    // ── funding-rate engine accessors ───────────────────────────────────────
    #[inline] pub fn funding_rate(&self) -> i64 { i64::from_le_bytes(self.funding_rate_e9) }
    #[inline] pub fn set_funding_rate(&mut self, v: i64) { self.funding_rate_e9 = v.to_le_bytes(); }
    #[inline] pub fn last_funding(&self) -> u64 { u64::from_le_bytes(self.last_funding_slot) }
    #[inline] pub fn set_last_funding(&mut self, v: u64) { self.last_funding_slot = v.to_le_bytes(); }
    #[inline] pub fn funding_skew_factor(&self) -> u32 { u32::from_le_bytes(self.funding_skew_factor_e9) }
    #[inline] pub fn funding_velocity(&self) -> u32 { u32::from_le_bytes(self.funding_velocity_e9) }
    #[inline] pub fn max_funding_rate(&self) -> u32 { u32::from_le_bytes(self.max_funding_rate_e9) }
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
    /// 1 once this trader has live ER-reserved margin (set by the ER attestation
    /// path). Sticky gate: an ER-active trader MUST use the xdomain withdraw
    /// variants (which honor the reservation); the strict path fails closed for
    /// them. `u8` carved from `_reserved`; size unchanged (200 bytes).
    pub er_active: u8,
    pub _reserved: [u8; 43],
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
    /// Cumulative quote-lots paid OUT of the fund via `withdraw_insurance_fund`
    /// (informational). 8-aligned. Carved from `_reserved`; size unchanged (200).
    pub total_payouts: u64,
    pub _reserved: [u8; 56],
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

    /// NAV used to PRICE deposits/withdrawals: `min(nav, capital)` — bear realized
    /// losses, ignore un-crystallized gains. Re-audit 2026-06-30 (LOW, defensive):
    /// mirrors the per-market v3 `nav_for_pricing` (#188). Today the singleton's
    /// `realized_pnl` is never advanced so `nav == capital` and this is a no-op, but
    /// pricing both sides on it pre-empts the v3 deposit/withdraw asymmetry should
    /// singleton realized-PnL ever be wired (deposit at raw nav + withdraw capped at
    /// capital would otherwise lock gains / let an early LP skim a late depositor).
    #[inline]
    pub fn nav_for_pricing(&self) -> i128 {
        let cap = self.total_capital_quote_lots as i128;
        let nav = self.nav();
        if nav < cap { nav } else { cap }
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
/// `TriggerOrderV3.flags` bits. ACTIVE is set on placement and cleared when the
/// trigger fires (one-shot). REDUCE_ONLY marks a position-closing trigger
/// (execution is a follow-up).
pub const TRIGGER_FLAG_ACTIVE: u8 = 0x01;
pub const TRIGGER_FLAG_REDUCE_ONLY: u8 = 0x02;
pub const TWAP_ORDER_V3_DISC: [u8; 8] = [0x77, 0xA9, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];
/// `TwapOrderV3.flags` ACTIVE bit (set on placement, cleared when fully executed).
pub const TWAP_FLAG_ACTIVE: u8 = 0x01;
pub const ICEBERG_ORDER_V3_DISC: [u8; 8] = [0x1C, 0xEB, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];
/// `IcebergOrderV3.flags` ACTIVE bit (set on placement, cleared when the last
/// chunk is replenished — `remaining_lots` reaches 0).
pub const ICEBERG_FLAG_ACTIVE: u8 = 0x01;

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
    /// Trailing-stop offset in bps (0 = a plain, non-trailing trigger). Carved
    /// from `_reserved`; `u16` first so the following `u64` stays 8-aligned and
    /// the struct size is unchanged. Set at placement; the stop's
    /// `trigger_price_ticks` ratchets as the mark moves via `update_trailing_stop`.
    pub trailing_offset_bps: u16,
    /// Running anchor (max mark for a long stop / min mark for a short stop) the
    /// trailing trigger is measured from. 0 = unseeded.
    pub trailing_anchor_ticks: u64,
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

    /// NAV used to PRICE deposits and withdrawals: `capital + min(realized_pnl, 0)`
    /// = `min(nav, capital)`. Re-audit 2026-06 (HIGH): pricing both sides on the raw
    /// `nav` was ASYMMETRIC — deposits paid for a realized GAIN (`nav > capital`)
    /// that withdrawals could never return (the payout is capped at `capital`, and a
    /// full redemption priced at `nav > capital` is rejected outright → the gain is
    /// locked AND extractable by an early LP from a later depositor). Pricing both
    /// sides on `min(nav, capital)` is symmetric: a realized LOSS still discounts
    /// both (LPs bear it — pin's H-6 hardening, stricter than anchor's capital-only
    /// pricing), while an un-crystallized GAIN is ignored for share pricing and
    /// stays as a vault buffer (matching anchor, which never distributes it).
    #[inline]
    pub fn nav_for_pricing(&self) -> i128 {
        let cap = self.total_capital_quote_lots as i128;
        let nav = self.nav();
        if nav < cap {
            nav
        } else {
            cap
        }
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
    /// JIT-LP min-hold anchor (Wave 57 / re-audit 2026-06-30): the slot of the most
    /// recent deposit, stored LE (align 1) carved from `_reserved` so the 104-byte
    /// layout is unchanged. `0` = legacy account (deposited "at genesis") ⇒ the
    /// min-hold has long elapsed ⇒ withdrawal allowed, so existing positions are
    /// unaffected. Withdrawal gates on `jit_lp_defense::can_withdraw`.
    pub deposited_at_slot: [u8; 8],
    pub _reserved: [u8; 31],
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

impl FlpExposurePerMarketV3 {
    /// Shares minted for a per-market FLP-v3 deposit of `amount`, priced on the
    /// pool's pre-deposit **NAV = max(0, capital + realized_pnl)** — NOT capital
    /// alone. First deposit (`outstanding == 0`) mints 1:1; a pool with shares but
    /// non-positive NAV is insolvent and can't be priced → `None`. Otherwise
    /// `amount · outstanding / nav`, clamped to `u64::MAX`. Mirrors the singleton
    /// `FlpExposure::shares_for_deposit`. Pricing on NAV (not capital) is the fix
    /// that makes LPs bear the pool's realized losses instead of redeeming at par
    /// (which socialized the loss onto the shared vault). Pure + host-tested.
    #[inline]
    pub fn shares_for_deposit_v3(amount: u64, outstanding: u64, nav: i128) -> Option<u64> {
        if outstanding == 0 {
            return Some(amount);
        }
        if nav <= 0 {
            return None;
        }
        let s = (amount as u128).checked_mul(outstanding as u128)? / (nav as u128);
        if s > u64::MAX as u128 { None } else { Some(s as u64) }
    }

    /// Quote-lots returned for burning `shares_to_burn` of `total_shares`, priced
    /// on **NAV** (not capital). `None` if `total_shares == 0`, `nav <= 0`, or on
    /// overflow. A realized LOSS (nav < capital) discounts the payout (LPs bear
    /// it); a realized GAIN (nav > capital) is capped at the pool's actual capital
    /// by the caller's `amount > total_capital` guard, so the shared vault is never
    /// over-paid. Mirrors the singleton `FlpExposure::amount_for_shares`.
    #[inline]
    pub fn amount_for_shares_v3(
        shares_to_burn: u64,
        nav: i128,
        total_shares: u64,
    ) -> Option<u64> {
        if total_shares == 0 || nav <= 0 {
            return None;
        }
        let a = (shares_to_burn as u128).checked_mul(nav as u128)? / (total_shares as u128);
        Some(if a > u64::MAX as u128 { u64::MAX } else { a as u64 })
    }

    /// Apply a pool-as-maker fill to the exposure's net position, returning the
    /// new `(side, size_lots, entry_price_ticks)`. `side`: 0=long, 1=short,
    /// 255=flat. Pure + host-tested; mirrors the anchor `record_flp_fill_v3`
    /// open / add (weighted entry) / reduce / flip transitions exactly. Callers
    /// pass `fill_size > 0`; the same-side branch's `new_size` is therefore > 0,
    /// so the weighted-entry division never divides by zero.
    #[inline]
    pub fn apply_flp_fill(
        prev_side: u8,
        prev_size: u64,
        prev_entry: u64,
        fill_side: u8,
        fill_size: u64,
        fill_price: u64,
    ) -> (u8, u64, u64) {
        if prev_size == 0 {
            (fill_side, fill_size, fill_price)
        } else if prev_side == fill_side {
            let new_size = prev_size.saturating_add(fill_size);
            let ne = (prev_entry as u128)
                .saturating_mul(prev_size as u128)
                .saturating_add((fill_price as u128).saturating_mul(fill_size as u128))
                / new_size as u128;
            (
                prev_side,
                new_size,
                if ne > u64::MAX as u128 { u64::MAX } else { ne as u64 },
            )
        } else if fill_size <= prev_size {
            let ns = prev_size - fill_size;
            if ns == 0 {
                (255, 0, 0)
            } else {
                (prev_side, ns, prev_entry)
            }
        } else {
            (fill_side, fill_size - prev_size, fill_price)
        }
    }
}

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

pub const ER_MARGIN_DISC: [u8; 8] = [0xE2, 0x70, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-trader ER (ephemeral-rollup) margin attestation. The `attestor` (ER margin
/// sequencer) reports `reserved_margin_quote_lots` — initial margin locked by the
/// trader's live ER resting orders; withdrawals must leave at least that behind.
/// `epoch` is a strictly-increasing replay guard. Padding-free `repr(C)` at 96
/// bytes. The consuming withdraw gate is a later batch.
#[repr(C)]
pub struct ErMarginAttestation {
    pub disc: [u8; 8],
    pub trader_state: Pubkey,
    pub attestor: Pubkey,
    pub reserved_margin_quote_lots: u64,
    pub epoch: u64,
    pub bump: u8,
    pub _pad: [u8; 7],
}
const _: () = assert!(core::mem::size_of::<ErMarginAttestation>() == 96);

pub const SESSION_TOKEN_DISC: [u8; 8] = [0x5E, 0x55, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// A delegated session-signing token: `session_signer` may act for `owner` until
/// `expires_at_unix`. Padding-free `repr(C)` at 88 bytes. `revoked` (1) hard-kills
/// it before expiry. The consuming session-auth check on trade paths is a later
/// batch.
#[repr(C)]
pub struct SessionToken {
    pub disc: [u8; 8],
    pub owner: Pubkey,
    pub session_signer: Pubkey,
    pub expires_at_unix: i64,
    pub bump: u8,
    pub revoked: u8,
    pub _pad: [u8; 6],
}
const _: () = assert!(core::mem::size_of::<SessionToken>() == 88);

/// Max session lifetime (seconds): 24h. Mirrors the anchor bound.
pub const MAX_SESSION_TTL_SECONDS: i64 = 24 * 60 * 60;

pub const POSITION_HAIRCUT_DISC: [u8; 8] = [0x4A, 0x13, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// Per-position haircut (positive-PnL warmup) state. All `u64` (no 128-bit
/// fields), padding-free `repr(C)` at 136 bytes. Feeds the host-tested `haircut`
/// maturation math (`apply_mature`).
#[repr(C)]
pub struct PositionHaircutState {
    pub disc: [u8; 8],
    pub market: Pubkey,
    pub position: Pubkey,
    pub released_reserve_quote_lots: u64,
    pub released_attached_at_slot: u64,
    pub matured_pos_quote_lots: u64,
    pub original_reserve_at_attach: u64,
    pub bump: u8,
    pub _pad0: [u8; 7],
    pub _reserved: [u8; 24],
}
const _: () = assert!(core::mem::size_of::<PositionHaircutState>() == 136);

pub const POSITION_LIQ_STATE_DISC: [u8; 8] = [0x5C, 0x10, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

pub const JIT_LIQ_OFFER_DISC: [u8; 8] = [0x6A, 0x17, 0x00, 0x12, 0x34, 0x56, 0x78, 0x03];

/// A maker's pre-committed JIT liquidation offer (PDA `[b"jit_liq_offer", market,
/// maker, nonce]`). The maker promises to fill up to `remaining_size_lots` of a
/// liquidation on `market` (optionally only for `target_trader`, default zero =
/// any) at `offer_price_ticks` until `expires_at_slot`. No token escrow — the
/// maker's collateral covers the fill. Padding-free `repr(C)` at 152 bytes.
#[repr(C)]
pub struct JitLiquidationOffer {
    pub disc: [u8; 8],
    pub bump: u8,
    pub side: u8,
    pub maker_sub_index: u8,
    pub _pad0: [u8; 1],
    pub nonce: u32,
    pub market: Pubkey,
    pub maker: Pubkey,
    pub target_trader: Pubkey,
    pub offer_price_ticks: u64,
    pub max_size_lots: u64,
    pub remaining_size_lots: u64,
    pub created_at_slot: u64,
    pub expires_at_slot: u64,
}
const _: () = assert!(core::mem::size_of::<JitLiquidationOffer>() == 152);

/// Per-position liquidation state — the timestamps `liquidate_position_v2`
/// needs that don't fit in the full 128-byte `Position`. `unhealthy_since_slot`
/// drives the Dutch-auction liquidator reward (linear ramp since the position
/// first became unhealthy); `last_liquidated_at_slot` is the re-liquidation
/// cooldown anchor. A fresh port-only PDA `[b"position_liq", market, position]`;
/// padding-free `repr(C)` at 120 bytes. Mirrors the `PositionHaircutState`
/// per-position-state pattern.
#[repr(C)]
pub struct PositionLiquidationState {
    pub disc: [u8; 8],
    pub market: Pubkey,
    pub position: Pubkey,
    pub unhealthy_since_slot: u64,
    pub last_liquidated_at_slot: u64,
    pub bump: u8,
    pub _pad0: [u8; 7],
    pub _reserved: [u8; 24],
}
const _: () = assert!(core::mem::size_of::<PositionLiquidationState>() == 120);

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
    pub _pad0: u8,
    /// Max allowed dispersion (bps) across the 3 quorum sources before the update
    /// is rejected (0 = gate off). Carved from `_pad`; the `u32` sits at the
    /// 4-aligned offset 84 so the `repr(C)` layout + 88-byte size are unchanged.
    pub max_dispersion_bps: u32,
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
    fn flp_v3_shares_for_deposit_nav_priced() {
        type E = FlpExposurePerMarketV3;
        // First deposit (no shares) → 1:1 regardless of NAV.
        assert_eq!(E::shares_for_deposit_v3(100, 0, 0), Some(100));
        assert_eq!(E::shares_for_deposit_v3(100, 0, 50), Some(100));
        // Priced on NAV: amount · outstanding / nav.
        assert_eq!(E::shares_for_deposit_v3(100, 1_000, 500), Some(200));
        assert_eq!(E::shares_for_deposit_v3(50, 1_000, 1_000), Some(50));
        // A realized GAIN (nav 2000 > capital) ⇒ FEWER shares per lot.
        assert_eq!(E::shares_for_deposit_v3(500, 1_000, 2_000), Some(250));
        // A realized LOSS (nav 250 < capital) ⇒ MORE shares per lot.
        assert_eq!(E::shares_for_deposit_v3(500, 1_000, 250), Some(2_000));
        // Insolvent pool (shares outstanding but NAV ≤ 0) can't be priced → None
        // (the fix: previously capital==0 minted 1:1, socializing the loss).
        assert_eq!(E::shares_for_deposit_v3(100, 5, 0), None);
        assert_eq!(E::shares_for_deposit_v3(100, 5, -10), None);
        // Floor rounding (dust below one share → Some(0)).
        assert_eq!(E::shares_for_deposit_v3(1, 1, 1_000), Some(0));
        // Overflow ⇒ None (checked, never a silent wrap).
        assert_eq!(E::shares_for_deposit_v3(u64::MAX, u64::MAX, 1), None);
    }

    #[test]
    fn flp_v3_nav_for_pricing_is_min_nav_capital() {
        // Re-audit 2026-06 (HIGH): deposit & withdraw must price on the SAME
        // `min(nav, capital)` — bear realized losses, ignore un-crystallized gains.
        let mut e = FlpExposurePerMarketV3 {
            disc: FLP_PER_MARKET_V3_DISC,
            market: [0u8; 32],
            authority: [0u8; 32],
            size_lots: 0,
            entry_price_ticks: 0,
            total_capital_quote_lots: 1_000,
            realized_pnl: 0,
            lp_shares_outstanding: 1_000,
            bump: 0,
            side: 255,
            _reserved: [0u8; 22],
        };
        // Flat PnL: nav == capital.
        assert_eq!(e.nav_for_pricing(), 1_000);
        // Realized GAIN: nav (1_300) > capital ⇒ priced at capital (gain ignored,
        // stays a vault buffer; this is what stops the late-depositor trap).
        e.realized_pnl = 300;
        assert_eq!(e.nav(), 1_300);
        assert_eq!(e.nav_for_pricing(), 1_000);
        // Realized LOSS: nav (700) < capital ⇒ priced at nav (LPs bear it).
        e.realized_pnl = -300;
        assert_eq!(e.nav_for_pricing(), 700);
        // A full redemption now never exceeds capital (the old `nav` priced a gain
        // above capital → every full burn was rejected, locking the gain).
        e.realized_pnl = 300;
        let payout = FlpExposurePerMarketV3::amount_for_shares_v3(
            e.lp_shares_outstanding, e.nav_for_pricing(), e.lp_shares_outstanding,
        )
        .unwrap();
        assert!(payout <= e.total_capital_quote_lots, "full burn must fit capital");
        assert_eq!(payout, 1_000);
    }

    #[test]
    fn flp_apply_fill_open_add_reduce_flip() {
        type E = FlpExposurePerMarketV3;
        // open from flat.
        assert_eq!(E::apply_flp_fill(255, 0, 0, 0, 10, 100), (0, 10, 100));
        // add same side → weighted entry: (100·10 + 200·10)/20 = 150.
        assert_eq!(E::apply_flp_fill(0, 10, 100, 0, 10, 200), (0, 20, 150));
        // reduce opposite (partial) → entry unchanged.
        assert_eq!(E::apply_flp_fill(0, 20, 150, 1, 5, 999), (0, 15, 150));
        // reduce to exactly flat → side 255, entry 0.
        assert_eq!(E::apply_flp_fill(0, 20, 150, 1, 20, 999), (255, 0, 0));
        // flip: opposite larger → new side, remaining size, new entry.
        assert_eq!(E::apply_flp_fill(0, 20, 150, 1, 30, 250), (1, 10, 250));
    }

    #[test]
    fn flp_v3_amount_for_shares_nav_priced() {
        type E = FlpExposurePerMarketV3;
        // zero outstanding → None; non-positive NAV → None (insolvent).
        assert_eq!(E::amount_for_shares_v3(10, 100, 0), None);
        assert_eq!(E::amount_for_shares_v3(10, 0, 1_000), None);
        assert_eq!(E::amount_for_shares_v3(10, -5, 1_000), None);
        // pro-rata on NAV: burn 200 of 1000 at nav 500 → 100.
        assert_eq!(E::amount_for_shares_v3(200, 500, 1_000), Some(100));
        // burn all at nav 500 → 500.
        assert_eq!(E::amount_for_shares_v3(1_000, 500, 1_000), Some(500));
        // a realized GAIN lifts the payout: burn all at nav 2000 / 1000 → 2000
        // (the caller caps this at the pool's actual token capital).
        assert_eq!(E::amount_for_shares_v3(1_000, 2_000, 1_000), Some(2_000));
        // a realized LOSS discounts it: burn all at nav 250 / 1000 → 250.
        assert_eq!(E::amount_for_shares_v3(1_000, 250, 1_000), Some(250));
        // dust burn that rounds to 0.
        assert_eq!(E::amount_for_shares_v3(1, 1, 1_000), Some(0));
        // clamp, no overflow.
        assert_eq!(E::amount_for_shares_v3(u64::MAX, u64::MAX as i128, 1), Some(u64::MAX));
    }

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

    /// Anti-exploit invariant: an FLP-v3 deposit of `amount` followed by an
    /// IMMEDIATE withdrawal of exactly the minted shares can NEVER return more
    /// than `amount` — share rounding always favors the pool, so a round-trip
    /// creates no value (no risk-free extraction). Proven analytically (with
    /// real arithmetic the round-trip equals `amount`; the two integer floors
    /// only ever round it down), exercised here over a deterministic randomized
    /// sweep including the virgin-pool and outstanding≫capital extremes.
    ///
    /// (A Kani proof of this needs two nested symbolic u128 divisions, which is
    /// intractable for CBMC — so it is locked in as an exhaustive host test.)
    #[test]
    fn flp_v3_share_roundtrip_never_creates_value() {
        type F = FlpExposurePerMarketV3;
        let mut seed: u64 = 0x243F_6A88_85A3_08D3;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        for _ in 0..200_000 {
            let amount = 1 + next() % 1_000_000_000; // 1 ..= 1e9
            // Consistent pre-state: a pool has shares iff it has capital.
            let (outstanding, capital) = if next() % 4 == 0 {
                (0u64, 0u64) // virgin pool — mints 1:1
            } else {
                (1 + next() % 1_000_000_000, 1 + next() % 1_000_000_000)
            };
            // Round-trip invariant holds at any NAV; exercise realized_pnl == 0
            // (nav == capital) so a virgin/healthy pool is the reference case.
            let nav = capital as i128;
            let shares = match F::shares_for_deposit_v3(amount, outstanding, nav) {
                Some(s) if s > 0 => s,
                _ => continue, // on-chain rejects a zero-share / insolvent deposit
            };
            // Post-deposit pool, then redeem exactly the minted shares.
            let nav2 = (capital + amount) as i128;
            let out2 = outstanding + shares;
            let back = F::amount_for_shares_v3(shares, nav2, out2).unwrap();
            assert!(
                back <= amount,
                "round-trip created value: amount={amount} outstanding={outstanding} \
                 capital={capital} shares={shares} back={back}"
            );
        }
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
