//! Flash Book — Pinocchio port (WIP migration; see docs/QUASAR_MIGRATION_SCOPE.md).
//!
//! Zero-copy, zero-allocation `no_std` program (on the SBF target). Accounts are
//! pointer-cast directly from the SVM input buffer — no Borsh, no heap. This
//! eliminates the Anchor framework overhead measured at ~28k CU on apply_fill
//! (Anchor 37,779 → Pinocchio 1,520).
//!
//! The pure fill/settlement math (`fill_math`) is host-unit-tested for exact
//! equivalence with the Anchor implementation; the pinocchio glue
//! (`instructions`, entrypoint) compiles only for the Solana target.
#![cfg_attr(target_os = "solana", no_std)]
// `nightly` (perf gate) and `certora` (formal-verification harness) are
// out-of-tree cfgs; acknowledge them so the build is warning-clean.
#![allow(unexpected_cfgs)]

pub mod hypertree;
pub mod book;
pub mod state;
pub mod constants;
pub mod error;
pub mod lot;
pub mod order;
pub mod fill_math;
pub mod funding;
pub mod fees;
pub mod borrow_fee;
pub mod concentration;
pub mod position_cap;
pub mod daily_loss_limit;
pub mod min_fill_size;
pub mod reduce_only;
pub mod funding_velocity;
pub mod self_trade;
pub mod volume_rate_limit;
pub mod peg_pricing;
pub mod stop_limit;
pub mod trailing_stop;
pub mod mit_order;
pub mod side_accrual;
pub mod envelope;
pub mod insurance_replenish;
pub mod jit_lp_defense;
pub mod tiered_lp_rewards;
pub mod v2_bookkeeping;
pub mod pending_claim;
pub mod stable_collateral;
pub mod conditional_cancel;
pub mod cancel_on_disconnect;
pub mod arg;
pub mod haircut;
pub mod pro_rata;
pub mod vpin;
pub mod risk;
pub mod liquidation;
pub mod seeds;
pub mod solvency;
pub mod leverage;
pub mod trigger_order;
pub mod twap_order;
pub mod leverage_tiers;
pub mod fee_tiers;
pub mod cpi;
pub mod guard;

#[cfg(target_os = "solana")]
mod program {
    use crate::instructions;
    use pinocchio::{
        account_info::AccountInfo, entrypoint, program_error::ProgramError, pubkey::Pubkey,
        ProgramResult,
    };

    entrypoint!(process);

    // Manual panic handler (version-robust across SBF rustc toolchains — the
    // pinocchio 0.8.4 `nostd_panic_handler!` macro uses #[no_mangle], rejected
    // by newer rustc on the panic lang-item).
    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        loop {}
    }

    /// Instruction discriminator (1-byte compact tag; extend as ix are ported).
    #[repr(u8)]
    pub enum Ix {
        ApplyFill = 0,
        SettleFunding = 1,
        PlaceLimitOrder = 2,
        CancelOrder = 3,
        PlaceTakerOrder = 4,
        ModifyOrder = 5,
        CancelAll = 6,
        ApplyFlpFill = 7,
        // ── lifecycle / collateral (Phase 1 port) ──
        OpenTraderState = 8,
        InitializeInsuranceFund = 9,
        DepositCollateral = 10,
        InitializeMarket = 11,
        WithdrawCollateral = 12,
        // ── admin / config ──
        SetMarketSequencer = 13,
        SetMarketStatus = 14,
        UpdateOracle = 15,
        OpenTraderSubAccount = 16,
        TransferCollateral = 17,
        CloseTraderSubAccount = 18,
        SetTraderFeeTier = 19,
        SetMarketParams = 20,
        TransferMarketAuthority = 21,
        TransferInsuranceAuthority = 22,
        SetInsuranceFeeContribution = 23,
        VerifySolvency = 24,
        SetMarketMaintenanceMargin = 25,
        InitializeFlpExposure = 26,
        InitLpPosition = 27,
        DepositFlpCapital = 28,
        WithdrawFlpCapital = 29,
        VerifyProtocolSolvency = 30,
        VerifyMarketInvariants = 31,
        VerifyCollateralSolvency = 32,
        ErHeartbeat = 33,
        InitMarketLeverageTiers = 34,
        UpdateMarketLeverageTiers = 35,
        SetMarketRiskParams = 36,
        SetTraderDelegate = 37,
        SetTraderReferrer = 38,
        SetTraderBuilder = 39,
        VerifyStressSolvency = 40,
        VerifyPortfolioSolvency = 41,
        InitFeeTiers = 42,
        UpdateFeeTiers = 43,
        VerifyStressLattice = 44,
        SetMarketMaxLeverage = 45,
        SetPositionLeverage = 46,
        VerifyPortfolioStress = 47,
        VerifyLeverageCap = 48,
        PlaceTriggerOrder = 49,
        CancelTriggerOrder = 50,
        PlaceTwapOrder = 51,
        CancelTwapOrder = 52,
        InitFlpPerMarket = 53,
        SetInsurancePauseThreshold = 54,
        BurnMarketAuthority = 55,
        SetEnvelopeConfig = 56,
        VerifyEnvelopeConfig = 57,
        InitMarketOracleConfig = 58,
        InitializeSideAccrual = 59,
        CreateVault = 60,
        InitializeHaircutState = 61,
        VerifyHaircutInvariants = 62,
        InitPositionHaircutState = 63,
        CreateSessionToken = 64,
        RevokeSessionToken = 65,
        InitErMarginAttestation = 66,
        AttestErReservedMargin = 67,
        MaturePosition = 68,
    }

    #[inline(always)]
    fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
        let (&tag, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
        match tag {
            x if x == Ix::ApplyFill as u8 => instructions::apply_fill::process(program_id, accounts, rest),
            x if x == Ix::SettleFunding as u8 => instructions::settle_funding::process(program_id, accounts, rest),
            x if x == Ix::PlaceLimitOrder as u8 => instructions::place_order::process(program_id, accounts, rest),
            x if x == Ix::CancelOrder as u8 => instructions::cancel_order::process(program_id, accounts, rest),
            x if x == Ix::PlaceTakerOrder as u8 => instructions::place_taker_order::process(program_id, accounts, rest),
            x if x == Ix::ModifyOrder as u8 => instructions::modify_order::process(program_id, accounts, rest),
            x if x == Ix::CancelAll as u8 => instructions::cancel_all::process(program_id, accounts, rest),
            x if x == Ix::ApplyFlpFill as u8 => instructions::apply_flp_fill::process(program_id, accounts, rest),
            x if x == Ix::OpenTraderState as u8 => instructions::open_trader_state::process(program_id, accounts, rest),
            x if x == Ix::InitializeInsuranceFund as u8 => instructions::initialize_insurance_fund::process(program_id, accounts, rest),
            x if x == Ix::DepositCollateral as u8 => instructions::deposit_collateral::process(program_id, accounts, rest),
            x if x == Ix::InitializeMarket as u8 => instructions::initialize_market::process(program_id, accounts, rest),
            x if x == Ix::WithdrawCollateral as u8 => instructions::withdraw_collateral::process(program_id, accounts, rest),
            x if x == Ix::SetMarketSequencer as u8 => instructions::set_market_sequencer::process(program_id, accounts, rest),
            x if x == Ix::SetMarketStatus as u8 => instructions::set_market_status::process(program_id, accounts, rest),
            x if x == Ix::UpdateOracle as u8 => instructions::update_oracle::process(program_id, accounts, rest),
            x if x == Ix::OpenTraderSubAccount as u8 => instructions::open_trader_sub_account::process(program_id, accounts, rest),
            x if x == Ix::TransferCollateral as u8 => instructions::transfer_collateral::process(program_id, accounts, rest),
            x if x == Ix::CloseTraderSubAccount as u8 => instructions::close_trader_sub_account::process(program_id, accounts, rest),
            x if x == Ix::SetTraderFeeTier as u8 => instructions::set_trader_fee_tier::process(program_id, accounts, rest),
            x if x == Ix::SetMarketParams as u8 => instructions::set_market_params::process(program_id, accounts, rest),
            x if x == Ix::TransferMarketAuthority as u8 => instructions::transfer_market_authority::process(program_id, accounts, rest),
            x if x == Ix::TransferInsuranceAuthority as u8 => instructions::transfer_insurance_authority::process(program_id, accounts, rest),
            x if x == Ix::SetInsuranceFeeContribution as u8 => instructions::set_insurance_fee_contribution::process(program_id, accounts, rest),
            x if x == Ix::VerifySolvency as u8 => instructions::verify_solvency::process(program_id, accounts, rest),
            x if x == Ix::SetMarketMaintenanceMargin as u8 => instructions::set_market_maintenance_margin::process(program_id, accounts, rest),
            x if x == Ix::InitializeFlpExposure as u8 => instructions::initialize_flp_exposure::process(program_id, accounts, rest),
            x if x == Ix::InitLpPosition as u8 => instructions::init_lp_position::process(program_id, accounts, rest),
            x if x == Ix::DepositFlpCapital as u8 => instructions::deposit_flp_capital::process(program_id, accounts, rest),
            x if x == Ix::WithdrawFlpCapital as u8 => instructions::withdraw_flp_capital::process(program_id, accounts, rest),
            x if x == Ix::VerifyProtocolSolvency as u8 => instructions::verify_protocol_solvency::process(program_id, accounts, rest),
            x if x == Ix::VerifyMarketInvariants as u8 => instructions::verify_market_invariants::process(program_id, accounts, rest),
            x if x == Ix::VerifyCollateralSolvency as u8 => instructions::verify_collateral_solvency::process(program_id, accounts, rest),
            x if x == Ix::ErHeartbeat as u8 => instructions::er_heartbeat::process(program_id, accounts, rest),
            x if x == Ix::InitMarketLeverageTiers as u8 => instructions::init_market_leverage_tiers::process(program_id, accounts, rest),
            x if x == Ix::UpdateMarketLeverageTiers as u8 => instructions::update_market_leverage_tiers::process(program_id, accounts, rest),
            x if x == Ix::SetMarketRiskParams as u8 => instructions::set_market_risk_params::process(program_id, accounts, rest),
            x if x == Ix::SetTraderDelegate as u8 => instructions::set_trader_delegate::process(program_id, accounts, rest),
            x if x == Ix::SetTraderReferrer as u8 => instructions::set_trader_referrer::process(program_id, accounts, rest),
            x if x == Ix::SetTraderBuilder as u8 => instructions::set_trader_builder::process(program_id, accounts, rest),
            x if x == Ix::VerifyStressSolvency as u8 => instructions::verify_stress_solvency::process(program_id, accounts, rest),
            x if x == Ix::VerifyPortfolioSolvency as u8 => instructions::verify_portfolio_solvency::process(program_id, accounts, rest),
            x if x == Ix::InitFeeTiers as u8 => instructions::init_fee_tiers::process(program_id, accounts, rest),
            x if x == Ix::UpdateFeeTiers as u8 => instructions::update_fee_tiers::process(program_id, accounts, rest),
            x if x == Ix::VerifyStressLattice as u8 => instructions::verify_stress_lattice::process(program_id, accounts, rest),
            x if x == Ix::SetMarketMaxLeverage as u8 => instructions::set_market_max_leverage::process(program_id, accounts, rest),
            x if x == Ix::SetPositionLeverage as u8 => instructions::set_position_leverage::process(program_id, accounts, rest),
            x if x == Ix::VerifyPortfolioStress as u8 => instructions::verify_portfolio_stress::process(program_id, accounts, rest),
            x if x == Ix::VerifyLeverageCap as u8 => instructions::verify_leverage_cap::process(program_id, accounts, rest),
            x if x == Ix::PlaceTriggerOrder as u8 => instructions::place_trigger_order::process(program_id, accounts, rest),
            x if x == Ix::CancelTriggerOrder as u8 => instructions::cancel_trigger_order::process(program_id, accounts, rest),
            x if x == Ix::PlaceTwapOrder as u8 => instructions::place_twap_order::process(program_id, accounts, rest),
            x if x == Ix::CancelTwapOrder as u8 => instructions::cancel_twap_order::process(program_id, accounts, rest),
            x if x == Ix::InitFlpPerMarket as u8 => instructions::init_flp_per_market::process(program_id, accounts, rest),
            x if x == Ix::SetInsurancePauseThreshold as u8 => instructions::set_insurance_pause_threshold::process(program_id, accounts, rest),
            x if x == Ix::BurnMarketAuthority as u8 => instructions::burn_market_authority::process(program_id, accounts, rest),
            x if x == Ix::SetEnvelopeConfig as u8 => instructions::set_envelope_config::process(program_id, accounts, rest),
            x if x == Ix::VerifyEnvelopeConfig as u8 => instructions::verify_envelope_config::process(program_id, accounts, rest),
            x if x == Ix::InitMarketOracleConfig as u8 => instructions::init_market_oracle_config::process(program_id, accounts, rest),
            x if x == Ix::InitializeSideAccrual as u8 => instructions::initialize_side_accrual::process(program_id, accounts, rest),
            x if x == Ix::CreateVault as u8 => instructions::create_vault::process(program_id, accounts, rest),
            x if x == Ix::InitializeHaircutState as u8 => instructions::initialize_haircut_state::process(program_id, accounts, rest),
            x if x == Ix::VerifyHaircutInvariants as u8 => instructions::verify_haircut_invariants::process(program_id, accounts, rest),
            x if x == Ix::InitPositionHaircutState as u8 => instructions::init_position_haircut_state::process(program_id, accounts, rest),
            x if x == Ix::CreateSessionToken as u8 => instructions::create_session_token::process(program_id, accounts, rest),
            x if x == Ix::RevokeSessionToken as u8 => instructions::revoke_session_token::process(program_id, accounts, rest),
            x if x == Ix::InitErMarginAttestation as u8 => instructions::init_er_margin_attestation::process(program_id, accounts, rest),
            x if x == Ix::AttestErReservedMargin as u8 => instructions::attest_er_reserved_margin::process(program_id, accounts, rest),
            x if x == Ix::MaturePosition as u8 => instructions::mature_position::process(program_id, accounts, rest),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

#[cfg(target_os = "solana")]
pub mod instructions;
