//! Compile-time constants. Anything tunable per-market lives in
//! `MarketParams` instead.

/// USD values use 6 decimals throughout — matches Flash V2's existing
/// convention. Token decimals are *separate* and per-mint.
pub const USD_DECIMALS: u8 = 6;
pub const USD_UNIT: u64 = 1_000_000;

/// Basis points denominator. 1 bp = 1/10_000.
pub const BPS_DENOM: u32 = 10_000;

/// Maximum stress scenarios per batch — capped to keep margin compute bounded.
/// At 60 scenarios × 8 markets × 16 positions = 7680 evaluations per batch.
pub const MAX_STRESS_SCENARIOS: usize = 64;

/// Maximum positions per trader, used for stress-lattice loops.
pub const MAX_POSITIONS_PER_TRADER: usize = 16;

/// Maximum FLP quote levels per side per batch.
pub const MAX_FLP_QUOTE_LEVELS: usize = 16;

/// Maximum orders processed per batch (compute-budget bounded).
pub const MAX_ORDERS_PER_BATCH: usize = 256;

/// In-program order buffer capacity (per market, per pending batch).
/// Sized to fit in a regular Anchor account (≤ 10 KB).
pub const ORDER_BUFFER_CAP: usize = 64;

/// Maximum recent clearing prices retained for TWAP / volatility.
pub const MARK_HISTORY_LEN: usize = 16;

/// Cumulative funding index uses fixed-point Q64.64 — enough for
/// 100+ years of accumulation at any reasonable rate without overflow.
pub const FUNDING_INDEX_FRACTIONAL_BITS: u32 = 64;

/// VPIN EMA uses fixed-point Q32.32.
pub const VPIN_FRACTIONAL_BITS: u32 = 32;
pub const VPIN_FIXED_ONE: u64 = 1u64 << VPIN_FRACTIONAL_BITS;

/// Lot epsilon — sizes below this are treated as zero (rounding noise).
pub const LOT_EPSILON: u64 = 1;

/// Reserved sequence-number range for synthesized FLP virtual orders.
/// User-submitted orders use [0, FLP_SEQ_RESERVED_OFFSET); FLP virtual
/// quotes use [FLP_SEQ_RESERVED_OFFSET, ∞). Keeps user FIFO ordering
/// untouched by FLP injection.
pub const FLP_SEQ_RESERVED_OFFSET: u64 = 1u64 << 56;

/// Per-trader per-batch limit on submitted orders. Spam-protection.
pub const MAX_ORDERS_PER_TRADER_PER_BATCH: u32 = 16;
