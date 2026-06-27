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
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

#[cfg(target_os = "solana")]
pub mod instructions;
