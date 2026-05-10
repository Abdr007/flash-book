#![allow(unexpected_cfgs)]
//! Flash Book FLP — wave 21 program split.
//!
//! Owns `FlpExposurePerMarketAccount` (PDA at `[b"flp", market]`, one
//! per market — solves the singleton bottleneck that prevents
//! ER-delegating FLP per-market in the current `flash-book` program).
//!
//! Ixs: deposit_flp_capital_v3, withdraw_flp_capital_v3, and the
//! `generate_flp_virtuals` CPI target that `flash-book-core`'s
//! `run_batch_v2` calls into per batch.
//!
//! Why a separate program:
//!   1. Per-market FLP exposure unlocks per-market ER delegation —
//!      multiple markets can run on different ER instances independently.
//!   2. Smaller per-program audit surface.
//!   3. LP-share accounting can evolve without touching matcher
//!      semantics (e.g. tiered-yield LP shares, programmable
//!      auto-rebalance) without freezing core.
//!
//! ── Status: SKELETON (wave 21 phase 1 — deployable, no functionality)
//!
//! Migration plan in `docs/V3_WAVE21_MODULAR.md`. Functional ixs follow
//! per the migration doc's phases 2-4.

use anchor_lang::prelude::*;

declare_id!("eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M");

#[program]
pub mod flash_book_flp {
    use super::*;

    /// Liveness check. Emits `Pong`.
    pub fn ping(ctx: Context<Ping>) -> Result<()> {
        let slot = Clock::get()?.slot;
        emit!(Pong {
            program: *ctx.program_id,
            caller: ctx.accounts.caller.key(),
            slot,
        });
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Ping<'info> {
    pub caller: Signer<'info>,
}

#[event]
pub struct Pong {
    pub program: Pubkey,
    pub caller: Pubkey,
    pub slot: u64,
}
