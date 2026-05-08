//! Flash Book — pool-backed CLOB matched by FBA on MagicBlock ER.
//!
//! Production Solana program. The matcher core is in [`matcher`] — a pure
//! integer-arithmetic library with checked overflow throughout, fully
//! testable without Solana runtime. Account types and instructions wrap
//! it for on-chain execution.
//!
//! Phase 1 status: matcher core complete; instruction set is a skeleton
//! with the four critical instructions (initialize, place, run_batch,
//! delegate). Full instruction surface will land in Phase 2.

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod matcher;
pub mod state;

pub use errors::FlashBookError;

declare_id!("FBookV1111111111111111111111111111111111111");

#[program]
pub mod flash_book {
    use super::*;

    /// Initialize a new market account. Called by the market authority on L1.
    pub fn initialize_market(
        _ctx: Context<InitializeMarket>,
        _params: state::MarketParams,
    ) -> Result<()> {
        // Skeleton: full implementation in Phase 2.
        Ok(())
    }

    /// Submit a resting limit order. Routed to the order buffer for the
    /// next batch.
    pub fn place_limit_order(
        _ctx: Context<PlaceOrder>,
        _side: u8,
        _size_lots: u64,
        _limit_ticks: u64,
        _post_only: bool,
    ) -> Result<()> {
        Ok(())
    }

    /// Submit a commit hash for a future taker reveal.
    pub fn submit_commit(
        _ctx: Context<SubmitCommit>,
        _hash: [u8; 32],
        _bond: u64,
    ) -> Result<()> {
        Ok(())
    }

    /// Reveal a previously committed taker order.
    pub fn submit_reveal(
        _ctx: Context<SubmitReveal>,
        _side: u8,
        _size_lots: u64,
        _limit_ticks: u64,
        _nonce: [u8; 32],
    ) -> Result<()> {
        Ok(())
    }

    /// Run one batch — the heart of the matcher. Called by the sequencer
    /// every `batch_interval_ms`. Internally:
    ///   1. advance funding index
    ///   2. recompute OI
    ///   3. detect liquidations from prior-batch mark
    ///   4. generate FLP virtual quotes
    ///   5. clear FBA Walrasian
    ///   6. apply fills (positions, fees, VPIN)
    ///   7. update mark = TWAP banded by oracle
    ///   8. process bankruptcies via insurance / ADL
    pub fn run_batch(_ctx: Context<RunBatch>, _now_ms: u64) -> Result<()> {
        Ok(())
    }

    /// Delegate the market's accounts to MagicBlock ER.
    /// Wraps `ephemeral_rollups_sdk::cpi::delegate_account`.
    pub fn delegate_market(_ctx: Context<DelegateMarket>) -> Result<()> {
        Ok(())
    }

    /// Commit ER state and undelegate back to L1.
    pub fn undelegate_market(_ctx: Context<UndelegateMarket>) -> Result<()> {
        Ok(())
    }
}

// ─── Account contexts ───────────────────────────────────────────────────

#[derive(Accounts)]
pub struct InitializeMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: Market PDA created in handler.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceOrder<'info> {
    pub trader: Signer<'info>,
    /// CHECK: validated by seed in handler.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
    /// CHECK: validated by seed in handler.
    #[account(mut)]
    pub trader_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SubmitCommit<'info> {
    pub trader: Signer<'info>,
    /// CHECK: market PDA.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
    /// CHECK: commit buffer PDA.
    #[account(mut)]
    pub commit_buffer: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SubmitReveal<'info> {
    pub trader: Signer<'info>,
    /// CHECK: market PDA.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
    /// CHECK: commit buffer PDA.
    #[account(mut)]
    pub commit_buffer: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct RunBatch<'info> {
    pub sequencer: Signer<'info>,
    /// CHECK: market PDA.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
    /// CHECK: insurance fund PDA.
    #[account(mut)]
    pub insurance_fund: UncheckedAccount<'info>,
    /// CHECK: FLP exposure PDA.
    #[account(mut)]
    pub flp_exposure: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct DelegateMarket<'info> {
    pub authority: Signer<'info>,
    /// CHECK: market PDA being delegated.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct UndelegateMarket<'info> {
    pub authority: Signer<'info>,
    /// CHECK: market PDA being undelegated.
    #[account(mut)]
    pub market: UncheckedAccount<'info>,
}
