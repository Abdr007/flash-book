pub const BPS_DENOM: u32 = 10_000;
pub const USD_UNIT: u64 = 1_000_000;

/// VPIN Q32.32 fixed-point (toxicity / VPIN math — see vpin.rs).
pub const VPIN_FRACTIONAL_BITS: u32 = 32;
pub const VPIN_FIXED_ONE: u64 = 1u64 << VPIN_FRACTIONAL_BITS;

/// Max L1 slots a market may show no liveness signal (no fill AND no ER
/// heartbeat) before `verify_market_invariants` presumes the ER stalled and
/// auto-pauses it. Mirrors the anchor `MARK_STALENESS_MAX_SLOTS`.
pub const MARK_STALENESS_MAX_SLOTS: u64 = 150;

/// Max bps a resting limit may deviate from the mark before it is rejected as
/// anti-stuffing (far-from-market spam). Mirrors anchor (50%).
pub const MAX_RESTING_ORDER_DEVIATION_BPS: u32 = 5_000;

/// Max bps an FLP (pool-as-maker) fill price may deviate from the mark. The pool
/// has no opposing trader to consent to the price, so a settlement outside this
/// band is rejected — stops a compromised sequencer settling an FLP fill far from
/// the mark to drain pool capital. Mirrors anchor `FLP_MAX_FILL_DEVIATION_BPS` (3%).
pub const FLP_MAX_FILL_DEVIATION_BPS: u32 = 300;

/// Max taker fee / |maker rebate| a fee tier may set (bps). Mirrors anchor.
pub const MAX_FEE_TIER_BPS: u32 = 1_000;

/// Slots of total ER silence (no fill / heartbeat / delegation signal) before the
/// permissionless `force_undelegate_market_book` escape opens. Mirrors anchor.
pub const FORCE_UNDELEGATE_TIMEOUT_SLOTS: u64 = 750;
/// Slots of settlement silence (committed fills only, heartbeat ignored) before
/// the censorship backstop opens — catches an alive-but-censoring sequencer.
pub const CENSORSHIP_ESCAPE_TIMEOUT_SLOTS: u64 = 9_000;

/// Hyperliquid-style partial-withdraw floor: post-withdraw collateral must be at
/// least this fraction (bps) of total open notional, on top of the stress IM.
pub const WITHDRAWAL_FLOOR_BPS: u32 = 1_000; // 10%
