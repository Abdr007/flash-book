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
    #[msg("Unexpected account type in remaining_accounts")]
    UnexpectedAccountType = 1106,

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
    #[msg("Position size would exceed per-trader concentration cap")]
    PositionSizeCapExceeded = 1209,
    #[msg("Open interest invariant violated: oi_long != oi_short")]
    OpenInterestImbalance = 1210,
    #[msg("Withdraw would leave FLP NAV below required exposure coverage")]
    FlpWithdrawUndercollateralized = 1211,
    #[msg("Required market account missing from remaining_accounts")]
    MissingMarketAccount = 1212,
    #[msg("Order would exceed per-position leverage cap set by trader")]
    LeverageCapExceeded = 1213,
    #[msg("Sweep requires source trader to be flat (no open positions)")]
    SweepRequiresFlat = 1214,
    #[msg("Vault is not accepting deposits")]
    VaultDepositsClosed = 1215,
    #[msg("Vault deposit below configured minimum")]
    VaultDepositTooSmall = 1216,
    #[msg("Vault NAV is non-positive — cannot mint shares")]
    VaultNavNonPositive = 1217,
    #[msg("Vault perf-fee settle: NAV/share at or below high-water mark")]
    VaultBelowHighWaterMark = 1218,
    #[msg("OCO trigger pair link mismatch")]
    OcoPairMismatch = 1219,
    #[msg("ADL not eligible: insurance fund above threshold or counter unprofitable at bankruptcy price")]
    AdlNotEligible = 1220,
    // Codes 1221 and 1222 (BondTooSmall, BondUnbondingDelay) were
    // retired with the removal of the HIP-3 deployer-bond infrastructure
    // in Flash Book V3 — markets are authority-gated only. Codes are
    // intentionally left unallocated so on-chain logs from prior
    // deployments stay decodable.
    #[msg("Vault NAV market mismatch — passed market doesn't match position")]
    VaultNavMarketMismatch = 1223,
    #[msg("Order would push gross OI past the market's `max_oi_base_lots` cap")]
    OpenInterestCapExceeded = 1224,
    #[msg("Mark-price change clamped — batch produced an outlier post-clearing mark")]
    MarkChangeClamped = 1225,
    #[msg("Fill-or-kill order could not be fully filled — aborted")]
    FillOrKillNotFilled = 1226,
    #[msg("Post-only order would cross the book — rejected")]
    PostOnlyWouldCross = 1227,
    #[msg("Resting order price too far from the oracle — anti-stuffing band (#36)")]
    RestingOrderTooFarFromOracle = 1228,

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
    #[msg(
        "Protocol insolvent: summed trader collateral exceeds vault headroom over FLP + insurance"
    )]
    ProtocolInsolvent = 1502,

    // ── 1700-1799 delegation (MagicBlock ER) ────────────────────────
    #[msg("Account not delegated to ER; instruction must run on L1")]
    NotDelegated = 1700,
    #[msg("Account already delegated to ER")]
    AlreadyDelegated = 1701,
    #[msg("Delegation expired")]
    DelegationExpired = 1702,
    #[msg("Force-include from L1 not yet supported in this build")]
    ForceIncludeUnsupported = 1703,
    #[msg("ER still live: settlement-liveness timeout not elapsed; cannot force-undelegate")]
    ErStillLive = 1704,

    // ── 1800-1899 oracle ────────────────────────────────────────────
    #[msg("Oracle price is too stale to be trusted")]
    OracleTooStale = 1800,
    #[msg("Oracle confidence interval exceeds configured maximum")]
    OracleConfidenceTooWide = 1801,
    #[msg("Oracle in fail-safe mode; new positions paused")]
    OraclePausedConfidence = 1802,
    #[msg("Oracle quorum dispersion exceeds configured maximum — sources disagree")]
    OracleQuorumDispersionTooWide = 1803,
    #[msg("Mark price is too stale (ER stalled); no trustworthy price to liquidate against")]
    MarkTooStale = 1804,

    // ── 1900-1999 haircut ───────────────────────────────────────────
    #[msg("Haircut warmup window inverted (h_min > h_max)")]
    HaircutInvertedWindow = 1900,
    #[msg("Haircut warmup window exceeds absolute cap")]
    HaircutWindowTooLarge = 1901,
    #[msg("Haircut release rejected zero gain")]
    HaircutZeroGain = 1902,
    #[msg("Haircut residual would underflow")]
    HaircutResidualUnderflow = 1903,
    #[msg("Haircut state not initialized for this market")]
    HaircutNotInitialized = 1904,
    #[msg("Position haircut state mismatched market/position")]
    HaircutStateMismatch = 1905,
    #[msg("Nothing to mature — reserve is zero or warmup hasn't started")]
    HaircutNothingToMature = 1906,
    #[msg("Nothing to convert — matured_pos is zero")]
    HaircutNothingToConvert = 1907,

    // ── 2000-2099 envelope ──────────────────────────────────────────
    #[msg("Envelope price cap zero or out of range")]
    EnvelopePriceCapInvalid = 2000,
    #[msg("Envelope accrual window zero or too large")]
    EnvelopeAccrualWindowInvalid = 2001,
    #[msg("Envelope funding cap exceeds absolute bound")]
    EnvelopeFundingCapInvalid = 2002,
    #[msg("Envelope maintenance bps zero or ≥ BPS_DENOM")]
    EnvelopeMaintenanceInvalid = 2003,
    #[msg("Envelope liquidation fee bps ≥ BPS_DENOM")]
    EnvelopeLiqFeeInvalid = 2004,
    #[msg("Envelope inequality violated for some notional N")]
    EnvelopeViolated = 2005,
    #[msg("Envelope same-slot price move rejected")]
    EnvelopeSameSlotMove = 2006,
    #[msg("Envelope price move exceeds per-slot cap")]
    EnvelopePriceMoveExceedsCap = 2007,
    #[msg("Envelope config not initialized for market")]
    EnvelopeNotInitialized = 2008,

    // ── 2100-2199 trigger orders ────────────────────────────────────
    #[msg("Trigger slippage cap breached — oracle moved past acceptable_price")]
    TriggerSlippageExceeded = 2100,

    // ── 2200-2299 settlement integrity (H1) ─────────────────────────
    #[msg("Fill sequence not strictly increasing — replayed or out-of-order settlement")]
    FillSeqReplay = 2200,
    // ── settlement-authenticity fill-commitment queue ──────────────────
    #[msg("Settled fill does not match the matcher's committed fill — fabricated or out-of-order")]
    FillNotCommitted = 2201,
    #[msg("Fill commitment ring is full — settlement must drain pending fills before more match")]
    FillRingFull = 2202,
    #[msg("No pending committed fill to settle")]
    FillRingEmpty = 2203,
    #[msg("Fill commitment ring counters corrupt — settled exceeds produced")]
    FillRingCorrupt = 2204,
    #[msg("FLP fill price deviates from the oracle by more than the safety band — fabricated or mispriced")]
    FlpPriceOutsideBand = 2205,
    #[msg("Market is armed: the fill-commitment account is mandatory for settlement (C-1)")]
    FillCommitmentMissing = 2206,
    #[msg("Cross-margined trader with multiple positions must be liquidated via liquidate_portfolio_v2 (H-4)")]
    CrossLiquidationNeedsPortfolio = 2207,
    #[msg("Self-liquidation forbidden: the liquidator must not be the liquidatee (M-2)")]
    SelfLiquidationForbidden = 2208,
    #[msg("Order sequence exhausted: per-market seq exceeded the 24-bit order_id encoding ceiling; reseat the market (H1)")]
    OrderSeqExhausted = 2209,
    #[msg("Liveness baseline already stamped (book_delegated_at_slot != 0) (F1)")]
    BaselineAlreadyStamped = 2210,
    #[msg("Market book is not delegated; nothing to stamp a liveness baseline for (F1)")]
    BookNotDelegated = 2211,
    #[msg("Session token expired (or not yet valid); re-create the session key")]
    SessionExpired = 2212,

    // ── 2300-2399 cross-domain collateral (ER reserved margin, #8) ──────
    #[msg("ER margin attestation epoch not strictly increasing — replayed or stale attestation")]
    ErEpochReplay = 2300,
    #[msg("Withdrawal would leave collateral below the ER-reserved margin for resting orders")]
    ErMarginReserved = 2301,
    #[msg(
        "Trader is ER-active: collateral withdrawals must use the cross-domain (xdomain) variant"
    )]
    UseXDomainWithdraw = 2302,
    #[msg("Signer is not the authorized attestor for this ER margin attestation")]
    ErAttestorMismatch = 2303,
    #[msg("ER margin attestation account does not belong to this trader_state")]
    ErMarginAccountMismatch = 2304,
    #[msg("Owner-initiated force-undelegate is unavailable: the upgraded MagicBlock DLP makes undelegation validator-driven. Undelegate via commit_and_undelegate_market_book on the ER (finalized by process_undelegation)")]
    OwnerForceUndelegateUnavailable = 2305,
    #[msg(
        "Fill-commitment ring must be fully drained (produced == settled) before it can be grown"
    )]
    FillRingNotDrained = 2306,
    #[msg("Market batch cap exceeds the log-safe limit but no fill-outbox account was supplied to carry the fills off-log")]
    FillOutboxRequired = 2307,
    #[msg("FLP pool is insolvent (NAV <= 0 while shares are outstanding) — deposits paused to prevent dilution of the new depositor")]
    FlpPoolInsolvent = 2308,
    #[msg("Stress-lattice scenario count exceeds the compute-safe maximum")]
    TooManyStressScenarios = 2309,
    #[msg("Order sequence counter exhausted for this market — the book must be reseated")]
    OrderSeqExhaustedReseat = 2310,
    #[msg("Order-sequence reseat requires a fully empty, L1-resident book")]
    ReseatRequiresEmptyBook = 2311,
    #[msg("Market is paused — liquidation and ADL are disabled while paused")]
    MarketPaused = 2312,
    #[msg("Haircut residual exceeds the vault-backed surplus (over-stated / unbacked)")]
    HaircutResidualUnbacked = 2313,
    #[msg("Lazer payload timestamp not strictly newer than the last accepted price (replay)")]
    OracleLazerReplay = 2314,
    #[msg("FLP quote refresh rate-limited: the pool's quotes are still fresh")]
    RefreshTooSoon = 2315,
    #[msg("Timelock has not elapsed: the proposed governance action is not yet executable")]
    TimelockNotElapsed = 2316,
    #[msg("Oracle source is locked: direct-authority price writes are disabled (Pyth/Lazer only)")]
    OracleSourceLocked = 2317,
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
