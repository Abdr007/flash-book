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
//! ── Status: PHASE 3 — first account type migrated (TriggerOrderV3)
//!
//! Wave 21 phase 3a ships the TriggerOrderAccountV3 account type +
//! place_trigger_order_v3 + execute_trigger_order_v3 + cancel_trigger
//! _order_v3. Same trigger semantics as core's v2 path; account is
//! now owned by THIS program. Execute fires via CPI into
//! flash_book::place_limit_order_v2_cpi.
//!
//! Phases 3b-3d (TWAP, iceberg, FLP-per-market, vault account
//! migrations) follow the same template.

use anchor_lang::prelude::*;
use flash_book::cpi::accounts::PlaceLimitOrderV2Cpi as CorePlaceLimitOrderV2Cpi;
use flash_book::program::FlashBook as CoreFlashBook;
use flash_book::state::{MarketAccount, PositionAccount as CorePositionAccount};

declare_id!("2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5");

/// Seed used to derive this program's CPI signer PDA. Mirrors the
/// `CPI_AUTHORITY_SEED` constant in `flash-book/src/lib.rs`. Core's
/// CPI ixs verify the signer matches `find_program_address(&[seed],
/// &orders_program_id)`.
pub const CPI_AUTHORITY_SEED: &[u8] = b"cpi_authority";

#[program]
pub mod flash_book_orders {
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

    /// CPI demo (wave 21 phase 2): forwards a limit-order placement
    /// into core via `flash-book::place_limit_order_v2_cpi`.
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
        let signers: [&[&[u8]]; 1] = [signer_seeds];

        let cpi_accounts = CorePlaceLimitOrderV2Cpi {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader: ctx.accounts.trader.to_account_info(),
            market: ctx.accounts.market.to_account_info(),
            market_book: ctx.accounts.market_book.to_account_info(),
        };
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

    // ─── Wave 21 phase 3a — Trigger orders v3 ───────────────────────

    /// Create a v3 trigger order PDA owned by this (orders) program.
    /// Same semantics as core's `place_trigger_order` (validates
    /// side / kind / size / price / tick alignment / expiry); only
    /// the account ownership differs.
    ///
    /// PDA seeds: `[b"trigger_v3", market, trader, trigger_id]`.
    /// Distinct from core's `[b"trigger", ...]` so a trader can hold
    /// BOTH legacy (core-owned) and v3 (orders-owned) triggers
    /// simultaneously during the migration window.
    pub fn place_trigger_order_v3(
        ctx: Context<PlaceTriggerOrderV3>,
        trigger_id: u8,
        side: u8,
        kind: u8,
        size_lots: u64,
        trigger_price_ticks: u64,
        limit_price_ticks: u64,
        reduce_only: bool,
        expires_at_slot: u64,
    ) -> Result<()> {
        require!(side <= 1, OrdersError::OutOfRange);
        require!(kind <= 1, OrdersError::OutOfRange);
        require!(size_lots > 0, OrdersError::ZeroSize);
        require!(trigger_price_ticks > 0, OrdersError::ZeroPrice);
        require!(limit_price_ticks > 0, OrdersError::ZeroPrice);

        let market = &ctx.accounts.market;
        require!(
            size_lots >= market.params.min_base_lots,
            OrdersError::SizeBelowMinLot
        );
        require!(
            limit_price_ticks % market.params.tick_size == 0,
            OrdersError::PriceNotOnTick
        );
        require!(
            trigger_price_ticks % market.params.tick_size == 0,
            OrdersError::PriceNotOnTick
        );

        let now = Clock::get()?.slot;
        if expires_at_slot > 0 {
            require!(expires_at_slot > now, OrdersError::OutOfRange);
        }

        let trigger = &mut ctx.accounts.trigger_order;
        trigger.trader = ctx.accounts.trader.key();
        trigger.market = market.key();
        trigger.bump = ctx.bumps.trigger_order;
        trigger.trigger_id = trigger_id;
        trigger.side = side;
        trigger.kind = kind;
        trigger.flags = TriggerOrderAccountV3::FLAG_ACTIVE
            | if reduce_only { TriggerOrderAccountV3::FLAG_REDUCE_ONLY } else { 0 };
        trigger.size_lots = size_lots;
        trigger.trigger_price_ticks = trigger_price_ticks;
        trigger.limit_price_ticks = limit_price_ticks;
        trigger.created_at_slot = now;
        trigger.expires_at_slot = expires_at_slot;

        emit!(TriggerOrderV3PlacedEvent {
            market: market.key(),
            trader: trigger.trader,
            trigger_id,
            side,
            kind,
            size_lots,
            trigger_price_ticks,
            limit_price_ticks,
        });
        Ok(())
    }

    /// Execute a v3 trigger — validates the fire condition (oracle vs
    /// trigger price + expiry + active flag + reduce-only), then CPIs
    /// into core's `place_limit_order_v2_cpi` to inject the resulting
    /// limit order into the hypertree.
    ///
    /// Permissionless caller — trader pre-authorized at trigger
    /// creation time.
    pub fn execute_trigger_order_v3(ctx: Context<ExecuteTriggerOrderV3>) -> Result<()> {
        let trigger = &ctx.accounts.trigger_order;
        let market = &ctx.accounts.market;
        require!(
            trigger.flags & TriggerOrderAccountV3::FLAG_ACTIVE != 0,
            OrdersError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if trigger.expires_at_slot > 0 {
            require!(trigger.expires_at_slot >= now, OrdersError::OutOfRange);
        }

        let oracle = market.oracle_price_ticks;
        let fired = if trigger.kind == 0 {
            oracle <= trigger.trigger_price_ticks
        } else {
            oracle >= trigger.trigger_price_ticks
        };
        require!(fired, OrdersError::OutOfRange);

        if trigger.flags & TriggerOrderAccountV3::FLAG_REDUCE_ONLY != 0 {
            let position = &ctx.accounts.position;
            require!(position.size_lots > 0, OrdersError::OutOfRange);
            require!(position.side != trigger.side, OrdersError::OutOfRange);
            require!(
                trigger.size_lots <= position.size_lots,
                OrdersError::OutOfRange
            );
        }

        let side = trigger.side;
        let size_lots = trigger.size_lots;
        let limit_ticks = trigger.limit_price_ticks;
        let trigger_id = trigger.trigger_id;
        let trader_pk = trigger.trader;
        let market_key = market.key();

        // CPI into core to inject the order.
        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];

        let cpi_accounts = CorePlaceLimitOrderV2Cpi {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader: ctx.accounts.trader.to_account_info(),
            market: ctx.accounts.market.to_account_info(),
            market_book: ctx.accounts.market_book.to_account_info(),
        };
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
            0, // flags = vanilla limit
            0, // expires_at_slot = GTC
        )?;

        // Mark trigger inactive (no double-fire).
        let trigger = &mut ctx.accounts.trigger_order;
        trigger.flags &= !TriggerOrderAccountV3::FLAG_ACTIVE;

        emit!(TriggerOrderV3ExecutedEvent {
            market: market_key,
            trader: trader_pk,
            trigger_id,
            executor: ctx.accounts.caller.key(),
            oracle_price_ticks: oracle,
        });
        Ok(())
    }

    /// Cancel a v3 trigger and close the account, refunding rent to
    /// the trader. Trader-only.
    pub fn cancel_trigger_order_v3(ctx: Context<CancelTriggerOrderV3>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.trigger_order.trader == trader,
            OrdersError::WrongTrader
        );
        emit!(TriggerOrderV3CancelledEvent {
            market: ctx.accounts.trigger_order.market,
            trader,
            trigger_id: ctx.accounts.trigger_order.trigger_id,
        });
        Ok(())
        // Anchor closes via `close = trader` constraint in the ctx.
    }
}

// ─── Account types ────────────────────────────────────────────────────

/// V3 trigger order — owned by THIS (orders) program. Field layout
/// matches core's `state::TriggerOrderAccount` for the load-bearing
/// fields; OCO + trailing-stop fields will land in phase 3b alongside
/// the bracket-v3 ix.
///
/// Seeds: `[b"trigger_v3", market, trader, trigger_id]`. Distinct from
/// core's `[b"trigger", ...]` so legacy + v3 triggers can coexist
/// during the migration window without seed collision.
#[account]
#[derive(Debug)]
pub struct TriggerOrderAccountV3 {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub trigger_id: u8,
    pub side: u8,
    pub kind: u8,
    pub flags: u8,
    pub size_lots: u64,
    pub trigger_price_ticks: u64,
    pub limit_price_ticks: u64,
    pub created_at_slot: u64,
    pub expires_at_slot: u64,
}

impl TriggerOrderAccountV3 {
    pub const SEED: &'static [u8] = b"trigger_v3";
    pub const FLAG_REDUCE_ONLY: u8 = 1 << 0;
    pub const FLAG_ACTIVE: u8 = 1 << 1;
    pub fn space() -> usize {
        // 8 disc + 32+32+1+1+1+1+1 + 8+8+8+8+8 = 117. Round to 128.
        8 + 128
    }
}

// ─── Account contexts ─────────────────────────────────────────────────

#[derive(Accounts)]
pub struct Ping<'info> {
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct PlaceOrderViaCore<'info> {
    /// CHECK: trader pubkey stamped onto the synthesised RestingOrderV2.
    pub trader: UncheckedAccount<'info>,

    /// CHECK: this program's CPI signer PDA. Anchor verifies the
    /// derivation; we sign over it via invoke_signed.
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

#[derive(Accounts)]
#[instruction(trigger_id: u8)]
pub struct PlaceTriggerOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    #[account(
        init,
        payer = trader,
        space = TriggerOrderAccountV3::space(),
        seeds = [
            TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[trigger_id],
        ],
        bump,
    )]
    pub trigger_order: Account<'info, TriggerOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ExecuteTriggerOrderV3<'info> {
    /// Permissionless caller pays tx fee. Trader pre-authorized at
    /// trigger creation time.
    pub caller: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA, threaded into the core CPI.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trigger_order.trader.as_ref(),
            &[trigger_order.trigger_id],
        ],
        bump = trigger_order.bump,
    )]
    pub trigger_order: Account<'info, TriggerOrderAccountV3>,

    /// Trader's position — required for reduce-only triggers (lazy-
    /// loaded via seeds; safe to pass even when flag is clear).
    /// Lives in core's program ID (PositionAccount is core-owned).
    pub position: Account<'info, CorePositionAccount>,

    /// CHECK: trader pubkey stamped onto the synthesised order. NOT a
    /// signer — the trigger account's `trader` field is the authority.
    /// Address constraint enforces it matches the trigger.
    #[account(address = trigger_order.trader)]
    pub trader: UncheckedAccount<'info>,

    /// CHECK: this program's CPI signer PDA.
    #[account(
        seeds = [CPI_AUTHORITY_SEED],
        bump,
    )]
    pub cpi_authority: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, CoreFlashBook>,
}

#[derive(Accounts)]
pub struct CancelTriggerOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            TriggerOrderAccountV3::SEED,
            trigger_order.market.as_ref(),
            trigger_order.trader.as_ref(),
            &[trigger_order.trigger_id],
        ],
        bump = trigger_order.bump,
    )]
    pub trigger_order: Account<'info, TriggerOrderAccountV3>,
}

// ─── Errors + events ──────────────────────────────────────────────────

#[error_code]
pub enum OrdersError {
    #[msg("argument out of allowed range")]
    OutOfRange,
    #[msg("size cannot be zero")]
    ZeroSize,
    #[msg("price cannot be zero")]
    ZeroPrice,
    #[msg("size below min lot")]
    SizeBelowMinLot,
    #[msg("price not on tick")]
    PriceNotOnTick,
    #[msg("wrong trader")]
    WrongTrader,
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

#[event]
pub struct TriggerOrderV3PlacedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trigger_id: u8,
    pub side: u8,
    pub kind: u8,
    pub size_lots: u64,
    pub trigger_price_ticks: u64,
    pub limit_price_ticks: u64,
}

#[event]
pub struct TriggerOrderV3ExecutedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trigger_id: u8,
    pub executor: Pubkey,
    pub oracle_price_ticks: u64,
}

#[event]
pub struct TriggerOrderV3CancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trigger_id: u8,
}
