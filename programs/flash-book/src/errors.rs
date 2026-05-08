use anchor_lang::prelude::*;

/// All Flash Book error codes. Numbered with stable ranges so downstream
/// clients can detect the family of an error from its code:
///
///   1000-1099  arithmetic / numerical
///   1100-1199  account / authority
///   1200-1299  order intake
///   1300-1399  matcher / clearing
///   1400-1499  margin / liquidation
///   1500-1599  insurance fund
///   1600-1699  commit-reveal
///   1700-1799  delegation (MagicBlock ER)
#[error_code]
pub enum FlashBookError {
    // ── 1000-1099 numerical ─────────────────────────────────────────
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow = 1000,
    #[msg("Arithmetic underflow")]
    ArithmeticUnderflow = 1001,
    #[msg("Division by zero")]
    DivisionByZero = 1002,
    #[msg("Value out of range")]
    OutOfRange = 1003,
    #[msg("Non-finite value")]
    NonFinite = 1004,

    // ── 1100-1199 account / authority ───────────────────────────────
    #[msg("Caller is not the market authority")]
    Unauthorized = 1100,
    #[msg("Account already initialized")]
    AlreadyInitialized = 1101,
    #[msg("Account not initialized")]
    NotInitialized = 1102,
    #[msg("Wrong market for this operation")]
    WrongMarket = 1103,
    #[msg("Wrong trader account")]
    WrongTrader = 1104,
    #[msg("Stale account version")]
    StaleVersion = 1105,

    // ── 1200-1299 order intake ──────────────────────────────────────
    #[msg("Order size below market minimum lot")]
    SizeBelowMinLot = 1200,
    #[msg("Limit price not aligned to tick size")]
    PriceNotOnTick = 1201,
    #[msg("Order size is zero")]
    ZeroSize = 1202,
    #[msg("Order limit price is zero")]
    ZeroPrice = 1203,
    #[msg("Insufficient collateral for initial margin")]
    InsufficientCollateral = 1204,
    #[msg("New positions paused — insurance fund below threshold")]
    NewPositionsPaused = 1205,
    #[msg("Order rejected: leverage exceeds market max")]
    LeverageExceeded = 1206,
    #[msg("Post-only order would have crossed the book")]
    PostOnlyCross = 1207,
    #[msg("Per-batch order rate limit exceeded")]
    RateLimited = 1208,

    // ── 1300-1399 matcher / clearing ────────────────────────────────
    #[msg("Order book buffer at capacity")]
    BufferFull = 1300,
    #[msg("Self-trade prevented")]
    SelfTrade = 1301,
    #[msg("No matchable volume in this batch")]
    EmptyBatch = 1302,
    #[msg("Mark price drifted outside oracle band")]
    MarkOutsideBand = 1303,

    // ── 1400-1499 margin / liquidation ──────────────────────────────
    #[msg("Trader is liquidatable; cannot place new opening orders")]
    TraderLiquidatable = 1400,
    #[msg("Stress-lattice scenario count exceeds compute budget")]
    TooManyScenarios = 1401,
    #[msg("Liquidation injection failed: position already closed")]
    LiquidationStale = 1402,
    #[msg("Trader is healthy; no liquidation needed")]
    NotLiquidatable = 1403,

    // ── 1500-1599 insurance fund ────────────────────────────────────
    #[msg("Insurance fund balance below threshold")]
    InsuranceBelowFloor = 1500,
    #[msg("Insurance fund cannot cover bankruptcy; ADL needed")]
    InsuranceExhausted = 1501,

    // ── 1600-1699 commit-reveal ─────────────────────────────────────
    #[msg("Commit hash does not match revealed payload")]
    CommitMismatch = 1600,
    #[msg("Commit has expired")]
    CommitExpired = 1601,
    #[msg("Duplicate commit hash")]
    CommitDuplicate = 1602,
    #[msg("Commit bond required")]
    CommitBondRequired = 1603,

    // ── 1700-1799 delegation (MagicBlock ER) ────────────────────────
    #[msg("Account not delegated to ER; instruction must run on L1")]
    NotDelegated = 1700,
    #[msg("Account already delegated to ER")]
    AlreadyDelegated = 1701,
    #[msg("Delegation expired")]
    DelegationExpired = 1702,
    #[msg("Force-include from L1 not yet supported in this build")]
    ForceIncludeUnsupported = 1703,
}

/// Convenience trait: `result.or_overflow()` to map None → ArithmeticOverflow.
pub trait OrOverflow<T> {
    fn or_overflow(self) -> Result<T>;
    fn or_underflow(self) -> Result<T>;
    fn or_div_zero(self) -> Result<T>;
}

impl<T> OrOverflow<T> for Option<T> {
    fn or_overflow(self) -> Result<T> {
        self.ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))
    }
    fn or_underflow(self) -> Result<T> {
        self.ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))
    }
    fn or_div_zero(self) -> Result<T> {
        self.ok_or_else(|| error!(FlashBookError::DivisionByZero))
    }
}
