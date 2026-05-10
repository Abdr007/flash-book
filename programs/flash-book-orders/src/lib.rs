#![allow(unexpected_cfgs)]
//! Flash Book Orders — wave 21 program split.
//!
//! Wraps trigger / TWAP / iceberg / bracket / trailing-stop ixs that
//! create `*OrderAccount` PDAs in this program's address space and CPI
//! into `flash-book-core` (`FBookV1...`) to inject the synthesized
//! limit order into the hypertree market_book on trigger fire.
//!
//! Why a separate program: independent upgrade lifecycle. A bug in
//! trigger logic shouldn't require freezing the matcher. Separate
//! audit surface; smaller per-program LOC.
//!
//! ── Status: SKELETON (wave 21 phase 1 — deployable, no functionality)
//!
//! Migration plan in `docs/V3_WAVE21_MODULAR.md`. The current ix
//! surface is a single `ping` that emits a `Pong` event — proves the
//! program is wired correctly on a target cluster without touching
//! real state. Functional ixs (place_trigger_order_v3,
//! execute_trigger_order_v3 with CPI into core, etc.) follow per the
//! migration doc's phases 2-4.

use anchor_lang::prelude::*;

declare_id!("2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5");

#[program]
pub mod flash_book_orders {
    use super::*;

    /// Liveness check. Emits `Pong` with the caller's pubkey + slot.
    /// Returns Ok(()). Used to verify the program is deployed +
    /// callable on a given cluster before wiring the migration ixs.
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
