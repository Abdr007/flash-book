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
//! ── Status: PHASE 2 — CPI surface live, state migration pending
//!
//! Wave 21 phase 2 ships `place_order_via_core` — a CPI into core's
//! `place_limit_order_v2_cpi` that demonstrates the full plumbing
//! end-to-end (PDA signer derivation, account threading, return-path).
//! Phase 3+ will move trigger/TWAP/iceberg account ownership into this
//! program and route their execute paths through this CPI.

use anchor_lang::prelude::*;
use flash_book::cpi::accounts::PlaceLimitOrderV2Cpi as CorePlaceLimitOrderV2Cpi;
use flash_book::program::FlashBook as CoreFlashBook;

declare_id!("2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5");

/// Seed used to derive this program's CPI signer PDA. Mirrors the
/// `CPI_AUTHORITY_SEED` constant in `flash-book/src/lib.rs`. Core's
/// CPI ixs verify the signer matches `find_program_address(&[seed],
/// &orders_program_id)`.
pub const CPI_AUTHORITY_SEED: &[u8] = b"cpi_authority";

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

    /// CPI demo: forwards a limit-order placement into core via
    /// `flash-book::place_limit_order_v2_cpi`, signing with this
    /// program's `[CPI_AUTHORITY_SEED]` PDA.
    ///
    /// In wave 21 phase 3+, the trigger / TWAP / iceberg execute ixs
    /// in this program will route their order injection through this
    /// CPI path instead of holding state in core. For now this ix
    /// proves the wiring works: any caller (subject to wrapper-level
    /// validation we'll add next) can invoke it and core accepts the
    /// resulting CPI as authorized.
    pub fn place_order_via_core(
        ctx: Context<PlaceOrderViaCore>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        flags: u8,
        expires_at_slot: u64,
    ) -> Result<()> {
        let bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[bump]];

        let cpi_accounts = CorePlaceLimitOrderV2Cpi {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader: ctx.accounts.trader.to_account_info(),
            market: ctx.accounts.market.to_account_info(),
            market_book: ctx.accounts.market_book.to_account_info(),
        };
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            cpi_accounts,
            &signers,
        );
        flash_book::cpi::place_limit_order_v2_cpi(
            cpi_ctx,
            side,
            size_lots,
            limit_ticks,
            flags,
            expires_at_slot,
        )?;

        emit!(CpiPlaceForwarded {
            trader: ctx.accounts.trader.key(),
            market: ctx.accounts.market.key(),
            side,
            size_lots,
            limit_ticks,
        });
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Ping<'info> {
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct PlaceOrderViaCore<'info> {
    /// The trader on whose behalf the order is placed. Wrapper-level
    /// authorization (via trigger / TWAP / iceberg account state)
    /// happens upstream of this ix in the production phase-3+ flow.
    /// CHECK: stamped onto the resulting RestingOrderV2 — wrapper
    /// validates trader authority via its own state.
    pub trader: UncheckedAccount<'info>,

    /// This program's CPI signer PDA. Derived via
    /// `find_program_address(&[CPI_AUTHORITY_SEED], &orders_program_id)`.
    /// We sign over it via `invoke_signed` for the core CPI.
    /// CHECK: the seeds + bump constraint enforces the derivation.
    #[account(
        seeds = [CPI_AUTHORITY_SEED],
        bump,
    )]
    pub cpi_authority: UncheckedAccount<'info>,

    /// CHECK: market account, passed through to core.
    pub market: UncheckedAccount<'info>,
    /// CHECK: market_book PDA, passed through to core.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, CoreFlashBook>,
}

#[event]
pub struct Pong {
    pub program: Pubkey,
    pub caller: Pubkey,
    pub slot: u64,
}

#[event]
pub struct CpiPlaceForwarded {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub limit_ticks: u64,
}
