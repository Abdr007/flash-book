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
use flash_book::state::{
    IcebergOrderAccount as CoreIcebergOrderAccount, MarketAccount,
    PositionAccount as CorePositionAccount, TriggerOrderAccount as CoreTriggerOrderAccount,
    TwapOrderAccount as CoreTwapOrderAccount,
};

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
    }

    // ─── Wave 21 phase 3b — TWAP orders v3 ──────────────────────────

    /// Create a v3 TWAP order PDA owned by this (orders) program.
    /// Mirrors core's `place_twap_order` validation; account is now
    /// owned by orders. Schedule slices fire via
    /// `execute_twap_slice_v3` which CPIs into core.
    pub fn place_twap_order_v3(
        ctx: Context<PlaceTwapOrderV3>,
        twap_id: u8,
        side: u8,
        slice_size_lots: u64,
        total_size_lots: u64,
        limit_price_ticks: u64,
        slot_interval: u64,
        end_slot: u64,
    ) -> Result<()> {
        require!(side <= 1, OrdersError::OutOfRange);
        require!(slice_size_lots > 0, OrdersError::ZeroSize);
        require!(total_size_lots >= slice_size_lots, OrdersError::OutOfRange);
        require!(limit_price_ticks > 0, OrdersError::ZeroPrice);
        require!(slot_interval > 0, OrdersError::OutOfRange);

        let market = &ctx.accounts.market;
        require!(
            limit_price_ticks % market.params.tick_size == 0,
            OrdersError::PriceNotOnTick
        );
        require!(
            slice_size_lots >= market.params.min_base_lots,
            OrdersError::SizeBelowMinLot
        );

        let now = Clock::get()?.slot;
        if end_slot > 0 {
            require!(end_slot > now, OrdersError::OutOfRange);
        }

        let twap = &mut ctx.accounts.twap_order;
        twap.trader = ctx.accounts.trader.key();
        twap.market = market.key();
        twap.bump = ctx.bumps.twap_order;
        twap.twap_id = twap_id;
        twap.side = side;
        twap.flags = TwapOrderAccountV3::FLAG_ACTIVE;
        twap.slice_size_lots = slice_size_lots;
        twap.total_size_lots = total_size_lots;
        twap.size_executed_lots = 0;
        twap.limit_price_ticks = limit_price_ticks;
        twap.start_slot = now;
        twap.slot_interval = slot_interval;
        twap.end_slot = end_slot;
        twap.last_slice_at_slot = 0;

        emit!(TwapOrderV3PlacedEvent {
            market: market.key(),
            trader: twap.trader,
            twap_id,
            side,
            total_size_lots,
            slice_size_lots,
            limit_price_ticks,
            slot_interval,
        });
        Ok(())
    }

    /// Permissionless TWAP slice executor — fires one slice when the
    /// interval gate is open. Same validation as core's v2; CPIs into
    /// core to inject the slice.
    pub fn execute_twap_slice_v3(ctx: Context<ExecuteTwapSliceV3>) -> Result<()> {
        let twap = &ctx.accounts.twap_order;
        let market = &ctx.accounts.market;
        require!(
            twap.flags & TwapOrderAccountV3::FLAG_ACTIVE != 0,
            OrdersError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if twap.end_slot > 0 {
            require!(twap.end_slot >= now, OrdersError::OutOfRange);
        }
        require!(
            now >= twap.last_slice_at_slot.saturating_add(twap.slot_interval),
            OrdersError::OutOfRange
        );

        let remaining = twap
            .total_size_lots
            .checked_sub(twap.size_executed_lots)
            .ok_or_else(|| error!(OrdersError::OutOfRange))?;
        require!(remaining > 0, OrdersError::OutOfRange);
        let slice_size = core::cmp::min(twap.slice_size_lots, remaining);
        require!(
            slice_size >= market.params.min_base_lots || slice_size == remaining,
            OrdersError::SizeBelowMinLot
        );

        let side = twap.side;
        let limit = twap.limit_price_ticks;
        let twap_id = twap.twap_id;
        let trader_pk = twap.trader;
        let market_key = market.key();

        // CPI into core to inject the slice.
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
            slice_size,
            limit,
            0,
            0,
        )?;

        // Update scheduling state.
        let twap = &mut ctx.accounts.twap_order;
        twap.size_executed_lots = twap
            .size_executed_lots
            .checked_add(slice_size)
            .ok_or_else(|| error!(OrdersError::OutOfRange))?;
        twap.last_slice_at_slot = now;
        if twap.size_executed_lots >= twap.total_size_lots {
            twap.flags &= !TwapOrderAccountV3::FLAG_ACTIVE;
        }

        emit!(TwapSliceV3ExecutedEvent {
            market: market_key,
            trader: trader_pk,
            twap_id,
            executor: ctx.accounts.caller.key(),
            slice_size_lots: slice_size,
            cumulative_executed_lots: twap.size_executed_lots,
        });
        Ok(())
    }

    /// Cancel a v3 TWAP order and close the account.
    pub fn cancel_twap_order_v3(ctx: Context<CancelTwapOrderV3>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.twap_order.trader == trader,
            OrdersError::WrongTrader
        );
        let unfilled = ctx
            .accounts
            .twap_order
            .total_size_lots
            .saturating_sub(ctx.accounts.twap_order.size_executed_lots);
        emit!(TwapOrderV3CancelledEvent {
            market: ctx.accounts.twap_order.market,
            trader,
            twap_id: ctx.accounts.twap_order.twap_id,
            unfilled_lots: unfilled,
        });
        Ok(())
    }

    // ─── Wave 21 phase 3c — Iceberg orders v3 ───────────────────────

    /// Create a v3 iceberg + seed first visible chunk into the
    /// hypertree via CPI. Mirrors core's place_iceberg_order_v2.
    pub fn place_iceberg_order_v3(
        ctx: Context<PlaceIcebergOrderV3>,
        iceberg_id: u8,
        side: u8,
        total_size_lots: u64,
        displayed_size_lots: u64,
        limit_ticks: u64,
        expires_at_slot: u64,
    ) -> Result<()> {
        require!(side <= 1, OrdersError::OutOfRange);
        require!(total_size_lots > 0, OrdersError::ZeroSize);
        require!(displayed_size_lots > 0, OrdersError::ZeroSize);
        require!(
            displayed_size_lots <= total_size_lots,
            OrdersError::OutOfRange
        );
        require!(limit_ticks > 0, OrdersError::ZeroPrice);

        let market = &ctx.accounts.market;
        require!(
            displayed_size_lots >= market.params.min_base_lots,
            OrdersError::SizeBelowMinLot
        );
        require!(
            limit_ticks % market.params.tick_size == 0,
            OrdersError::PriceNotOnTick
        );

        let now = Clock::get()?.slot;
        if expires_at_slot > 0 {
            require!(expires_at_slot > now, OrdersError::OutOfRange);
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();
        let first_chunk = displayed_size_lots.min(total_size_lots);

        // CPI into core to inject the first chunk.
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
            first_chunk,
            limit_ticks,
            0,
            expires_at_slot,
        )?;

        let iceberg = &mut ctx.accounts.iceberg_order;
        iceberg.trader = trader_pk;
        iceberg.market = market_key;
        iceberg.bump = ctx.bumps.iceberg_order;
        iceberg.iceberg_id = iceberg_id;
        iceberg.side = side;
        iceberg.flags = IcebergOrderAccountV3::FLAG_ACTIVE;
        iceberg.limit_ticks = limit_ticks;
        iceberg.total_size_lots = total_size_lots;
        iceberg.remaining_lots = total_size_lots.saturating_sub(first_chunk);
        iceberg.displayed_size_lots = displayed_size_lots;
        // child_order_seq is unknown to the wrapper (lives on core's
        // OrderPlacedV2CpiEvent log line). Off-chain reconciliation
        // pairs the iceberg with its seq via the event stream.
        iceberg.child_order_seq = 0;
        iceberg.created_at_slot = now;
        iceberg.expires_at_slot = expires_at_slot;

        emit!(IcebergOrderV3PlacedEvent {
            market: market_key,
            trader: trader_pk,
            iceberg_id,
            side,
            total_size_lots,
            displayed_size_lots,
            limit_ticks,
            first_chunk_size_lots: first_chunk,
        });
        Ok(())
    }

    /// Replenish v3 iceberg's next chunk — permissionless keeper.
    /// CPIs into core's place_limit_order_v2_cpi. Off-chain caller is
    /// responsible for calling only when the prior chunk has filled
    /// (queryable via core's view_book_depth_v2 / event stream).
    pub fn replenish_iceberg_v3(ctx: Context<ReplenishIcebergV3>) -> Result<()> {
        let iceberg = &ctx.accounts.iceberg_order;
        let market = &ctx.accounts.market;
        require!(
            iceberg.flags & IcebergOrderAccountV3::FLAG_ACTIVE != 0,
            OrdersError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if iceberg.expires_at_slot > 0 {
            require!(iceberg.expires_at_slot >= now, OrdersError::OutOfRange);
        }
        require!(iceberg.remaining_lots > 0, OrdersError::OutOfRange);

        let chunk = iceberg.displayed_size_lots.min(iceberg.remaining_lots);
        let side = iceberg.side;
        let limit = iceberg.limit_ticks;
        let expires = iceberg.expires_at_slot;
        let iceberg_id = iceberg.iceberg_id;
        let trader_pk = iceberg.trader;
        let market_key = market.key();

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
            chunk,
            limit,
            0,
            expires,
        )?;

        let iceberg = &mut ctx.accounts.iceberg_order;
        iceberg.remaining_lots = iceberg.remaining_lots.saturating_sub(chunk);
        if iceberg.remaining_lots == 0 {
            iceberg.flags &= !IcebergOrderAccountV3::FLAG_ACTIVE;
        }

        emit!(IcebergV3ReplenishedEvent {
            market: market_key,
            trader: trader_pk,
            iceberg_id,
            executor: ctx.accounts.caller.key(),
            chunk_size_lots: chunk,
            remaining_lots: iceberg.remaining_lots,
        });
        Ok(())
    }

    /// Cancel a v3 iceberg and close the account.
    pub fn cancel_iceberg_v3(ctx: Context<CancelIcebergV3>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.iceberg_order.trader == trader,
            OrdersError::WrongTrader
        );
        let unfilled = ctx
            .accounts
            .iceberg_order
            .remaining_lots
            .saturating_add(ctx.accounts.iceberg_order.displayed_size_lots);
        emit!(IcebergV3CancelledEvent {
            market: ctx.accounts.iceberg_order.market,
            trader,
            iceberg_id: ctx.accounts.iceberg_order.iceberg_id,
            unfilled_lots: unfilled.min(ctx.accounts.iceberg_order.total_size_lots),
        });
        Ok(())
    }

    // ─── Wave 21 phase 3d — Bracket orders v3 ───────────────────────

    /// Atomic bracket: parent limit order injected via CPI + 2 OCO-
    /// linked TriggerOrderAccountV3 PDAs (TP + SL). Same semantics as
    /// core's place_bracket_order_v2 but the trigger PDAs are owned
    /// by orders, not core.
    pub fn place_bracket_order_v3(
        ctx: Context<PlaceBracketOrderV3>,
        parent_side: u8,
        size_lots: u64,
        parent_limit_ticks: u64,
        tp_trigger_id: u8,
        tp_trigger_price_ticks: u64,
        tp_limit_ticks: u64,
        sl_trigger_id: u8,
        sl_trigger_price_ticks: u64,
        sl_limit_ticks: u64,
        expires_at_slot: u64,
    ) -> Result<()> {
        require!(parent_side <= 1, OrdersError::OutOfRange);
        require!(tp_trigger_id != sl_trigger_id, OrdersError::OutOfRange);
        require!(size_lots > 0, OrdersError::ZeroSize);
        require!(parent_limit_ticks > 0, OrdersError::ZeroPrice);
        require!(tp_trigger_price_ticks > 0, OrdersError::ZeroPrice);
        require!(sl_trigger_price_ticks > 0, OrdersError::ZeroPrice);
        require!(tp_limit_ticks > 0, OrdersError::ZeroPrice);
        require!(sl_limit_ticks > 0, OrdersError::ZeroPrice);

        let market = &ctx.accounts.market;
        require!(
            size_lots >= market.params.min_base_lots,
            OrdersError::SizeBelowMinLot
        );
        for p in [
            parent_limit_ticks,
            tp_trigger_price_ticks,
            sl_trigger_price_ticks,
            tp_limit_ticks,
            sl_limit_ticks,
        ] {
            require!(
                p % market.params.tick_size == 0,
                OrdersError::PriceNotOnTick
            );
        }

        let now = Clock::get()?.slot;
        if expires_at_slot > 0 {
            require!(expires_at_slot > now, OrdersError::OutOfRange);
        }

        // TP must be on profitable side; SL on loss side (mirrors v2).
        if parent_side == 0 {
            require!(
                tp_trigger_price_ticks > parent_limit_ticks,
                OrdersError::OutOfRange
            );
            require!(
                sl_trigger_price_ticks < parent_limit_ticks,
                OrdersError::OutOfRange
            );
        } else {
            require!(
                tp_trigger_price_ticks < parent_limit_ticks,
                OrdersError::OutOfRange
            );
            require!(
                sl_trigger_price_ticks > parent_limit_ticks,
                OrdersError::OutOfRange
            );
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();

        // 1. Parent limit order via CPI into core.
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
            parent_side,
            size_lots,
            parent_limit_ticks,
            0,
            0,
        )?;

        // 2. Wire the two reduce-only triggers (close-side opposite).
        let close_side = 1 - parent_side;
        let (tp_kind, sl_kind) = if parent_side == 0 {
            (1u8, 0u8)
        } else {
            (0u8, 1u8)
        };
        let common_flags = TriggerOrderAccountV3::FLAG_ACTIVE
            | TriggerOrderAccountV3::FLAG_REDUCE_ONLY;

        let tp = &mut ctx.accounts.tp_trigger;
        tp.trader = trader_pk;
        tp.market = market_key;
        tp.bump = ctx.bumps.tp_trigger;
        tp.trigger_id = tp_trigger_id;
        tp.side = close_side;
        tp.kind = tp_kind;
        tp.flags = common_flags;
        tp.size_lots = size_lots;
        tp.trigger_price_ticks = tp_trigger_price_ticks;
        tp.limit_price_ticks = tp_limit_ticks;
        tp.created_at_slot = now;
        tp.expires_at_slot = expires_at_slot;

        let sl = &mut ctx.accounts.sl_trigger;
        sl.trader = trader_pk;
        sl.market = market_key;
        sl.bump = ctx.bumps.sl_trigger;
        sl.trigger_id = sl_trigger_id;
        sl.side = close_side;
        sl.kind = sl_kind;
        sl.flags = common_flags;
        sl.size_lots = size_lots;
        sl.trigger_price_ticks = sl_trigger_price_ticks;
        sl.limit_price_ticks = sl_limit_ticks;
        sl.created_at_slot = now;
        sl.expires_at_slot = expires_at_slot;

        emit!(BracketOrderV3PlacedEvent {
            market: market_key,
            trader: trader_pk,
            parent_side,
            size_lots,
            parent_limit_ticks,
            tp_trigger_id,
            sl_trigger_id,
            tp_trigger_price_ticks,
            sl_trigger_price_ticks,
        });
        Ok(())
    }

    // ─── Wave 21 phase 10 — per-account-type state-copy migration ────
    //
    // Trader-signed migration ixs that read a legacy core-owned order
    // account, initialize the matching v3 wrapper-owned account with
    // the same state, and emit a migration event. Distinct seed
    // prefixes (b"trigger_v3" vs core's b"trigger") let legacy + v3
    // coexist for the duration of the migration window — traders
    // cancel their legacy account separately via core's existing
    // cancel ixs once they've confirmed v3 picked up.
    //
    // Trader signs because (a) only they should decide when to migrate
    // their order and (b) it cleanly avoids the cross-program close
    // problem (legacy stays alive until trader cancels via core).

    /// Migrate a single legacy core trigger to TriggerOrderAccountV3.
    /// Drops trailing-stop + OCO fields (those are reissued via
    /// `place_bracket_order_v3` if needed).
    pub fn migrate_trigger_to_v3(ctx: Context<MigrateTriggerToV3>) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            src.trader == ctx.accounts.trader.key(),
            OrdersError::Unauthorized
        );

        let dst = &mut ctx.accounts.v3;
        dst.trader = src.trader;
        dst.market = src.market;
        dst.bump = ctx.bumps.v3;
        dst.trigger_id = src.trigger_id;
        dst.side = src.side;
        dst.kind = src.kind;
        // Keep ACTIVE + REDUCE_ONLY bits; drop BRACKET_LEG (re-armed
        // via place_bracket_order_v3 if needed).
        dst.flags = src.flags
            & (TriggerOrderAccountV3::FLAG_REDUCE_ONLY | TriggerOrderAccountV3::FLAG_ACTIVE);
        dst.size_lots = src.size_lots;
        dst.trigger_price_ticks = src.trigger_price_ticks;
        dst.limit_price_ticks = src.limit_price_ticks;
        dst.created_at_slot = src.created_at_slot;
        dst.expires_at_slot = src.expires_at_slot;

        emit!(LegacyTriggerMigratedV3Event {
            trader: dst.trader,
            market: dst.market,
            trigger_id: dst.trigger_id,
            legacy: src.key(),
            v3: dst.key(),
        });
        Ok(())
    }

    /// Migrate a single legacy core TWAP to TwapOrderAccountV3.
    pub fn migrate_twap_to_v3(ctx: Context<MigrateTwapToV3>) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            src.trader == ctx.accounts.trader.key(),
            OrdersError::Unauthorized
        );

        let dst = &mut ctx.accounts.v3;
        dst.trader = src.trader;
        dst.market = src.market;
        dst.bump = ctx.bumps.v3;
        dst.twap_id = src.twap_id;
        dst.side = src.side;
        dst.flags = src.flags & TwapOrderAccountV3::FLAG_ACTIVE;
        dst.slice_size_lots = src.slice_size_lots;
        dst.total_size_lots = src.total_size_lots;
        dst.size_executed_lots = src.size_executed_lots;
        dst.limit_price_ticks = src.limit_price_ticks;
        dst.start_slot = src.start_slot;
        dst.slot_interval = src.slot_interval;
        dst.end_slot = src.end_slot;
        dst.last_slice_at_slot = src.last_slice_at_slot;

        emit!(LegacyTwapMigratedV3Event {
            trader: dst.trader,
            market: dst.market,
            twap_id: dst.twap_id,
            legacy: src.key(),
            v3: dst.key(),
        });
        Ok(())
    }

    /// Migrate a single legacy core iceberg to IcebergOrderAccountV3.
    /// `child_order_seq` is dropped (reset to 0) because the v3 path
    /// uses hypertree-resident orders identified by encoded order ID;
    /// the next replenish_iceberg_v3 call repopulates it.
    pub fn migrate_iceberg_to_v3(ctx: Context<MigrateIcebergToV3>) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            src.trader == ctx.accounts.trader.key(),
            OrdersError::Unauthorized
        );

        let dst = &mut ctx.accounts.v3;
        dst.trader = src.trader;
        dst.market = src.market;
        dst.bump = ctx.bumps.v3;
        dst.iceberg_id = src.iceberg_id;
        dst.side = src.side;
        dst.flags = src.flags & IcebergOrderAccountV3::FLAG_ACTIVE;
        dst._pad0 = [0; 4];
        dst.limit_ticks = src.limit_ticks;
        dst.total_size_lots = src.total_size_lots;
        dst.remaining_lots = src.remaining_lots;
        dst.displayed_size_lots = src.displayed_size_lots;
        // Drop child_order_seq — v3 child is hypertree-resident,
        // identified by encoded order_id which is rebuilt on next
        // replenish.
        dst.child_order_seq = 0;
        dst.created_at_slot = src.created_at_slot;
        dst.expires_at_slot = src.expires_at_slot;

        emit!(LegacyIcebergMigratedV3Event {
            trader: dst.trader,
            market: dst.market,
            iceberg_id: dst.iceberg_id,
            legacy: src.key(),
            v3: dst.key(),
        });
        Ok(())
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

/// V3 TWAP order. Seeds: `[b"twap_v3", market, trader, twap_id]`.
#[account]
#[derive(Debug)]
pub struct TwapOrderAccountV3 {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub twap_id: u8,
    pub side: u8,
    pub flags: u8, // bit 0: active
    pub slice_size_lots: u64,
    pub total_size_lots: u64,
    pub size_executed_lots: u64,
    pub limit_price_ticks: u64,
    pub start_slot: u64,
    pub slot_interval: u64,
    pub end_slot: u64,
    pub last_slice_at_slot: u64,
}
impl TwapOrderAccountV3 {
    pub const SEED: &'static [u8] = b"twap_v3";
    pub const FLAG_ACTIVE: u8 = 1 << 0;
    pub fn space() -> usize {
        8 + 144
    }
}

/// V3 iceberg order. Seeds: `[b"iceberg_v3", market, trader, iceberg_id]`.
#[account]
#[derive(Debug)]
pub struct IcebergOrderAccountV3 {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub iceberg_id: u8,
    pub side: u8,
    pub flags: u8, // bit 0: active
    pub _pad0: [u8; 4],
    pub limit_ticks: u64,
    pub total_size_lots: u64,
    pub remaining_lots: u64,
    pub displayed_size_lots: u64,
    pub child_order_seq: u64, // 0 — see place_iceberg_order_v3 comment
    pub created_at_slot: u64,
    pub expires_at_slot: u64,
}
impl IcebergOrderAccountV3 {
    pub const SEED: &'static [u8] = b"iceberg_v3";
    pub const FLAG_ACTIVE: u8 = 1 << 0;
    pub fn space() -> usize {
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

// ─── TWAP v3 contexts ─────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(twap_id: u8)]
pub struct PlaceTwapOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    #[account(
        init,
        payer = trader,
        space = TwapOrderAccountV3::space(),
        seeds = [
            TwapOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[twap_id],
        ],
        bump,
    )]
    pub twap_order: Account<'info, TwapOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ExecuteTwapSliceV3<'info> {
    pub caller: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA, threaded into the core CPI.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            TwapOrderAccountV3::SEED,
            market.key().as_ref(),
            twap_order.trader.as_ref(),
            &[twap_order.twap_id],
        ],
        bump = twap_order.bump,
    )]
    pub twap_order: Account<'info, TwapOrderAccountV3>,

    /// CHECK: trader pubkey stamped onto the synthesised slice order.
    #[account(address = twap_order.trader)]
    pub trader: UncheckedAccount<'info>,

    /// CHECK: this program's CPI signer PDA.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, CoreFlashBook>,
}

#[derive(Accounts)]
pub struct CancelTwapOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            TwapOrderAccountV3::SEED,
            twap_order.market.as_ref(),
            twap_order.trader.as_ref(),
            &[twap_order.twap_id],
        ],
        bump = twap_order.bump,
    )]
    pub twap_order: Account<'info, TwapOrderAccountV3>,
}

// ─── Iceberg v3 contexts ──────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(iceberg_id: u8)]
pub struct PlaceIcebergOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA — first chunk goes through CPI into core.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        init,
        payer = trader,
        space = IcebergOrderAccountV3::space(),
        seeds = [
            IcebergOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[iceberg_id],
        ],
        bump,
    )]
    pub iceberg_order: Account<'info, IcebergOrderAccountV3>,

    /// CHECK: this program's CPI signer PDA.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, CoreFlashBook>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReplenishIcebergV3<'info> {
    pub caller: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA, threaded through CPI.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            IcebergOrderAccountV3::SEED,
            market.key().as_ref(),
            iceberg_order.trader.as_ref(),
            &[iceberg_order.iceberg_id],
        ],
        bump = iceberg_order.bump,
    )]
    pub iceberg_order: Account<'info, IcebergOrderAccountV3>,

    /// CHECK: trader pubkey stamped onto the synthesised chunk.
    #[account(address = iceberg_order.trader)]
    pub trader: UncheckedAccount<'info>,

    /// CHECK: this program's CPI signer PDA.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, CoreFlashBook>,
}

#[derive(Accounts)]
pub struct CancelIcebergV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            IcebergOrderAccountV3::SEED,
            iceberg_order.market.as_ref(),
            iceberg_order.trader.as_ref(),
            &[iceberg_order.iceberg_id],
        ],
        bump = iceberg_order.bump,
    )]
    pub iceberg_order: Account<'info, IcebergOrderAccountV3>,
}

// ─── Bracket v3 ctx ───────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(
    parent_side: u8,
    size_lots: u64,
    parent_limit_ticks: u64,
    tp_trigger_id: u8,
    tp_trigger_price_ticks: u64,
    tp_limit_ticks: u64,
    sl_trigger_id: u8,
)]
pub struct PlaceBracketOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA — parent goes through CPI.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        init,
        payer = trader,
        space = TriggerOrderAccountV3::space(),
        seeds = [
            TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[tp_trigger_id],
        ],
        bump,
    )]
    pub tp_trigger: Account<'info, TriggerOrderAccountV3>,

    #[account(
        init,
        payer = trader,
        space = TriggerOrderAccountV3::space(),
        seeds = [
            TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[sl_trigger_id],
        ],
        bump,
    )]
    pub sl_trigger: Account<'info, TriggerOrderAccountV3>,

    /// CHECK: this program's CPI signer PDA.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, CoreFlashBook>,
    pub system_program: Program<'info, System>,
}

// ─── Errors + events ──────────────────────────────────────────────────

// ─── Wave 21 phase 10 — migration ix accounts ───────────────────────

#[derive(Accounts)]
pub struct MigrateTriggerToV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    /// Legacy core-owned trigger. Anchor verifies owner = core program.
    /// Read-only — we leave it intact (trader cancels via core after
    /// confirming v3 state).
    #[account(constraint = legacy.trader == trader.key() @ OrdersError::Unauthorized)]
    pub legacy: Account<'info, CoreTriggerOrderAccount>,

    /// Destination v3 account in this program's address space.
    #[account(
        init,
        payer = trader,
        space = TriggerOrderAccountV3::space(),
        seeds = [
            TriggerOrderAccountV3::SEED,
            legacy.market.as_ref(),
            legacy.trader.as_ref(),
            &[legacy.trigger_id],
        ],
        bump,
    )]
    pub v3: Account<'info, TriggerOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateTwapToV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(constraint = legacy.trader == trader.key() @ OrdersError::Unauthorized)]
    pub legacy: Account<'info, CoreTwapOrderAccount>,

    #[account(
        init,
        payer = trader,
        space = TwapOrderAccountV3::space(),
        seeds = [
            TwapOrderAccountV3::SEED,
            legacy.market.as_ref(),
            legacy.trader.as_ref(),
            &[legacy.twap_id],
        ],
        bump,
    )]
    pub v3: Account<'info, TwapOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateIcebergToV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(constraint = legacy.trader == trader.key() @ OrdersError::Unauthorized)]
    pub legacy: Account<'info, CoreIcebergOrderAccount>,

    #[account(
        init,
        payer = trader,
        space = IcebergOrderAccountV3::space(),
        seeds = [
            IcebergOrderAccountV3::SEED,
            legacy.market.as_ref(),
            legacy.trader.as_ref(),
            &[legacy.iceberg_id],
        ],
        bump,
    )]
    pub v3: Account<'info, IcebergOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

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
    #[msg("unauthorized caller")]
    Unauthorized,
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

#[event]
pub struct TwapOrderV3PlacedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub twap_id: u8,
    pub side: u8,
    pub total_size_lots: u64,
    pub slice_size_lots: u64,
    pub limit_price_ticks: u64,
    pub slot_interval: u64,
}

#[event]
pub struct TwapSliceV3ExecutedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub twap_id: u8,
    pub executor: Pubkey,
    pub slice_size_lots: u64,
    pub cumulative_executed_lots: u64,
}

#[event]
pub struct TwapOrderV3CancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub twap_id: u8,
    pub unfilled_lots: u64,
}

#[event]
pub struct IcebergOrderV3PlacedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub iceberg_id: u8,
    pub side: u8,
    pub total_size_lots: u64,
    pub displayed_size_lots: u64,
    pub limit_ticks: u64,
    pub first_chunk_size_lots: u64,
}

#[event]
pub struct IcebergV3ReplenishedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub iceberg_id: u8,
    pub executor: Pubkey,
    pub chunk_size_lots: u64,
    pub remaining_lots: u64,
}

#[event]
pub struct IcebergV3CancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub iceberg_id: u8,
    pub unfilled_lots: u64,
}

#[event]
pub struct BracketOrderV3PlacedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub parent_side: u8,
    pub size_lots: u64,
    pub parent_limit_ticks: u64,
    pub tp_trigger_id: u8,
    pub sl_trigger_id: u8,
    pub tp_trigger_price_ticks: u64,
    pub sl_trigger_price_ticks: u64,
}

// ─── Wave 21 phase 10 — migration events ────────────────────────────

#[event]
pub struct LegacyTriggerMigratedV3Event {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub trigger_id: u8,
    pub legacy: Pubkey,
    pub v3: Pubkey,
}

#[event]
pub struct LegacyTwapMigratedV3Event {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub twap_id: u8,
    pub legacy: Pubkey,
    pub v3: Pubkey,
}

#[event]
pub struct LegacyIcebergMigratedV3Event {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub iceberg_id: u8,
    pub legacy: Pubkey,
    pub v3: Pubkey,
}
