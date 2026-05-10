#![allow(unexpected_cfgs)]
//! Flash Book Vaults — wave 21 program split.
//!
//! Strategist vaults (`VaultAccount`), depositor share accounting
//! (`VaultPositionAccount`), withdraw queueing. CPIs into
//! `flash-book-core`'s `place_limit_order_v2` / `settle_funding` for
//! vault-on-behalf-of-trader operations.
//!
//! Why a separate program:
//!   1. Vault strategy logic is its own product surface (multiple
//!      strategy variants, performance fees, capacity caps) —
//!      independent upgrade cadence.
//!   2. Composability — third-party strategists can build vault
//!      programs that CPI into core using the same interface.
//!   3. Smaller per-program audit; vault-specific bugs don't risk
//!      the matcher.
//!
//! ── Status: SKELETON (wave 21 phase 1 — deployable, no functionality)
//!
//! Migration plan in `docs/V3_WAVE21_MODULAR.md`. Functional ixs follow
//! per the migration doc's phases 2-4.

use anchor_lang::prelude::*;

declare_id!("GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt");

#[program]
pub mod flash_book_vaults {
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
