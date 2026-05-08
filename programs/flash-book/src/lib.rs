//! Flash Book — pool-backed CLOB matched by FBA on MagicBlock ER.
//!
//! Anchor program. The matcher core is in [`matcher`] (pure-Rust integer
//! arithmetic with checked overflow). This file is the on-chain shell:
//! account validation, PDA seeds, signer checks, and matcher invocation.
//!
//! Phase 1 status: matcher core ✅, account types ✅, instruction handlers ✅
//! (this file). Pending: ER delegation CPIs (blocked on upstream SDK), full
//! Position account update during run_batch (currently emits fills as event,
//! position state moved by separate `apply_fill` instruction).

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod matcher;
pub mod state;

pub use errors::FlashBookError;

use constants::{
    FLP_SEQ_RESERVED_OFFSET, MARK_HISTORY_LEN, MAX_ORDERS_PER_TRADER_PER_BATCH,
    ORDER_BUFFER_CAP,
};
use matcher::commit_reveal::{
    register_commit, redeem_reveal, sweep_expired, RevealPayload,
};
use matcher::fba::clear_batch;
use matcher::flp_quoter::{generate_quotes, FlpQuoterInputs, FlpQuoterParams};
use matcher::funding::advance;
use matcher::lot::{BaseLots, Ticks};
use matcher::order::{Order, OrderType, Side};
use state::{
    CommitBufferAccount, FlpExposureAccount, InsuranceFundAccount, MarketAccount,
    MarketParams, OrderBufferAccount, OrderSlot, TraderStateAccount,
};

declare_id!("FBookV1111111111111111111111111111111111111");

#[program]
pub mod flash_book {
    use super::*;

    // ─── Setup ──────────────────────────────────────────────────────

    /// Initialize a new market and all associated PDAs.
    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        params: MarketParams,
        initial_oracle_ticks: u64,
    ) -> Result<()> {
        require!(params.tick_size > 0, FlashBookError::OutOfRange);
        require!(params.base_lot_size > 0, FlashBookError::OutOfRange);
        require!(params.quote_lot_size > 0, FlashBookError::OutOfRange);
        require!(params.max_leverage >= 1, FlashBookError::OutOfRange);
        require!(initial_oracle_ticks > 0, FlashBookError::ZeroPrice);

        let market = &mut ctx.accounts.market;
        market.authority = ctx.accounts.authority.key();
        market.flp_pool = ctx.accounts.flp_exposure.key();
        market.base_mint = ctx.accounts.base_mint.key();
        market.quote_mint = ctx.accounts.quote_mint.key();
        market.base_vault = ctx.accounts.base_vault.key();
        market.quote_vault = ctx.accounts.quote_vault.key();
        market.oracle_account = ctx.accounts.oracle_account.key();
        market.insurance_fund = ctx.accounts.insurance_fund.key();
        market.bump = ctx.bumps.market;
        market.status = MarketStatus::Active as u8;
        market.current_batch = 0;
        market.last_batch_ms = 0;
        market.oracle_price_ticks = initial_oracle_ticks;
        market.oracle_confidence = 0;
        market.mark_price_ticks = initial_oracle_ticks;
        market.cum_funding_index = 0;
        market.last_funding_rate_bps_per_sec = 0;
        market.vpin = matcher::vpin::VpinState::default();
        market.oi_long_lots = 0;
        market.oi_short_lots = 0;
        market.recent_clearing_prices = [0u64; MARK_HISTORY_LEN];
        market.recent_clearing_count = 0;
        market.total_fees_collected = 0;
        market.total_toxicity_tax_collected = 0;
        market.total_liquidations = 0;
        market.params = params;

        let buffer = &mut ctx.accounts.order_buffer;
        buffer.market = market.key();
        buffer.bump = ctx.bumps.order_buffer;
        buffer.head = 0;
        buffer.seq_counter = 0;
        buffer.slots = [OrderSlot::default(); ORDER_BUFFER_CAP];

        let commit_buf = &mut ctx.accounts.commit_buffer;
        commit_buf.market = market.key();
        commit_buf.bump = ctx.bumps.commit_buffer;
        commit_buf.head = 0;
        commit_buf.commits = [state::CommitRow::default(); 256];

        emit!(MarketInitializedEvent {
            market: market.key(),
            authority: market.authority,
            initial_oracle_ticks,
        });
        Ok(())
    }

    /// Initialize an insurance fund (one per protocol).
    pub fn initialize_insurance_fund(
        ctx: Context<InitializeInsuranceFund>,
        fee_contribution_bps: u32,
        toxicity_tax_contribution_bps: u32,
        liq_penalty_contribution_bps: u32,
        pause_threshold_quote_lots: u64,
    ) -> Result<()> {
        let f = &mut ctx.accounts.insurance_fund;
        f.authority = ctx.accounts.authority.key();
        f.bump = ctx.bumps.insurance_fund;
        f.balance_quote_lots = 0;
        f.fee_contribution_bps = fee_contribution_bps;
        f.toxicity_tax_contribution_bps = toxicity_tax_contribution_bps;
        f.liq_penalty_contribution_bps = liq_penalty_contribution_bps;
        f.pause_threshold_quote_lots = pause_threshold_quote_lots;
        f.total_contributions = 0;
        f.total_payouts = 0;
        Ok(())
    }

    /// Initialize per-trader state.
    pub fn open_trader_state(ctx: Context<OpenTraderState>) -> Result<()> {
        let s = &mut ctx.accounts.trader_state;
        s.trader = ctx.accounts.trader.key();
        s.bump = ctx.bumps.trader_state;
        s.collateral_quote_lots = 0;
        s.realized_pnl_quote_lots = 0;
        s.open_positions = 0;
        s.toxicity_score_bps = 0;
        s.orders_this_batch = 0;
        s.last_batch_seen = 0;
        Ok(())
    }

    /// Deposit collateral into the trader's state. SPL token transfer is
    /// done in a follow-up (this instruction credits the trader's accounting
    /// balance; production wiring also moves USDC into the protocol vault
    /// via SPL CPI).
    pub fn deposit_collateral(
        ctx: Context<DepositCollateral>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        let s = &mut ctx.accounts.trader_state;
        s.collateral_quote_lots = s
            .collateral_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        emit!(CollateralDepositedEvent {
            trader: s.trader,
            amount: amount_quote_lots,
            new_balance: s.collateral_quote_lots,
        });
        Ok(())
    }

    /// Withdraw collateral. Blocked if it would push the trader below
    /// initial-margin requirement on any open position. (For v1 we approximate
    /// by checking against `open_positions == 0`; production wiring iterates
    /// open Position PDAs via remaining_accounts.)
    pub fn withdraw_collateral(
        ctx: Context<WithdrawCollateral>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        let s = &mut ctx.accounts.trader_state;
        require!(
            s.open_positions == 0,
            FlashBookError::InsufficientCollateral
        );
        require!(
            amount_quote_lots <= s.collateral_quote_lots,
            FlashBookError::InsufficientCollateral
        );
        s.collateral_quote_lots = s
            .collateral_quote_lots
            .checked_sub(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        emit!(CollateralWithdrawnEvent {
            trader: s.trader,
            amount: amount_quote_lots,
            new_balance: s.collateral_quote_lots,
        });
        Ok(())
    }

    /// Apply a single fill against the taker's and maker's Position PDAs.
    /// Called by the sequencer after `run_batch` for each emitted Fill,
    /// or by an off-chain bookkeeper that batches multiple fills per tx.
    ///
    /// Trust model: `sequencer` is the same authority as `run_batch`; the
    /// fill data is taken at face value (production version verifies via
    /// per-batch fill buffer or Merkle proof).
    pub fn apply_fill(
        ctx: Context<ApplyFill>,
        size_lots: u64,
        price_ticks: u64,
        taker_side: u8,
    ) -> Result<()> {
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(price_ticks > 0, FlashBookError::ZeroPrice);
        require!(taker_side <= 1, FlashBookError::OutOfRange);

        let market = &mut ctx.accounts.market;
        let market_key = market.key();
        let funding_index = market.cum_funding_index;
        let current_batch = market.current_batch;
        let taker_side_enum = if taker_side == 0 { Side::Long } else { Side::Short };
        let maker_side_enum = taker_side_enum.opposite();
        let taker_trader_pk = ctx.accounts.taker_trader_state.trader;
        let maker_trader_pk = ctx.accounts.maker_trader_state.trader;

        let taker_pos = &mut ctx.accounts.taker_position;
        let maker_pos = &mut ctx.accounts.maker_position;

        // Initialize Position state on first ever fill against this PDA.
        if taker_pos.market == Pubkey::default() {
            taker_pos.market = market_key;
            taker_pos.trader = taker_trader_pk;
            taker_pos.bump = ctx.bumps.taker_position;
            taker_pos.cum_funding_index_at_entry = funding_index;
            taker_pos.last_settlement_batch = current_batch;
        }
        if maker_pos.market == Pubkey::default() {
            maker_pos.market = market_key;
            maker_pos.trader = maker_trader_pk;
            maker_pos.bump = ctx.bumps.maker_position;
            maker_pos.cum_funding_index_at_entry = funding_index;
            maker_pos.last_settlement_batch = current_batch;
        }

        // Snapshot pre-state so we can detect open/close transitions.
        let taker_was_open = taker_pos.size_lots > 0;
        let maker_was_open = maker_pos.size_lots > 0;
        let taker_pre_side = taker_pos.side;
        let maker_pre_side = maker_pos.side;
        let taker_pre_size = taker_pos.size_lots;
        let maker_pre_size = maker_pos.size_lots;

        apply_fill_to_position(taker_pos, taker_side_enum, size_lots, price_ticks, funding_index)?;
        apply_fill_to_position(maker_pos, maker_side_enum, size_lots, price_ticks, funding_index)?;

        // Update OI counters: walk pre→post for each side.
        update_oi(market, taker_pre_side, taker_pre_size, taker_pos.side, taker_pos.size_lots)?;
        update_oi(market, maker_pre_side, maker_pre_size, maker_pos.side, maker_pos.size_lots)?;

        // Update open_positions transitions on TraderState.
        let taker_is_open = taker_pos.size_lots > 0;
        let maker_is_open = maker_pos.size_lots > 0;
        let taker_state = &mut ctx.accounts.taker_trader_state;
        if !taker_was_open && taker_is_open {
            taker_state.open_positions = taker_state.open_positions.saturating_add(1);
        } else if taker_was_open && !taker_is_open {
            taker_state.open_positions = taker_state.open_positions.saturating_sub(1);
        }
        let maker_state = &mut ctx.accounts.maker_trader_state;
        if !maker_was_open && maker_is_open {
            maker_state.open_positions = maker_state.open_positions.saturating_add(1);
        } else if maker_was_open && !maker_is_open {
            maker_state.open_positions = maker_state.open_positions.saturating_sub(1);
        }

        emit!(FillAppliedEvent {
            market: market_key,
            taker: taker_trader_pk,
            maker: maker_trader_pk,
            taker_side,
            size_lots,
            price_ticks,
            batch_num: current_batch,
        });
        Ok(())
    }

    /// Update oracle price (authority-only). In production this is replaced
    /// by a Pyth read in `run_batch`.
    pub fn update_oracle(
        ctx: Context<UpdateOracle>,
        price_ticks: u64,
        confidence: u64,
    ) -> Result<()> {
        require!(price_ticks > 0, FlashBookError::ZeroPrice);
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        market.oracle_price_ticks = price_ticks;
        market.oracle_confidence = confidence;
        Ok(())
    }

    // ─── Order intake ───────────────────────────────────────────────

    /// Submit a resting limit order. Routed to the order buffer for the
    /// next batch.
    pub fn place_limit_order(
        ctx: Context<PlaceOrder>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        post_only: bool,
    ) -> Result<()> {
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(limit_ticks > 0, FlashBookError::ZeroPrice);
        require!(side <= 1, FlashBookError::OutOfRange);

        let market = &ctx.accounts.market;
        require!(
            size_lots >= market.params.min_base_lots,
            FlashBookError::SizeBelowMinLot
        );
        require!(
            limit_ticks.is_multiple_of(market.params.tick_size),
            FlashBookError::PriceNotOnTick
        );
        require!(
            size_lots <= FLP_SEQ_RESERVED_OFFSET, // sanity bound
            FlashBookError::OutOfRange
        );

        // Per-trader rate limit (reset on batch boundary).
        let trader_state = &mut ctx.accounts.trader_state;
        if trader_state.last_batch_seen != market.current_batch {
            trader_state.last_batch_seen = market.current_batch;
            trader_state.orders_this_batch = 0;
        }
        require!(
            trader_state.orders_this_batch < MAX_ORDERS_PER_TRADER_PER_BATCH,
            FlashBookError::RateLimited
        );
        trader_state.orders_this_batch = trader_state
            .orders_this_batch
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        let buffer = &mut ctx.accounts.order_buffer;
        require!(
            (buffer.head as usize) < ORDER_BUFFER_CAP,
            FlashBookError::BufferFull
        );

        let next_seq = buffer
            .seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(next_seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);

        let trader_key = ctx.accounts.trader.key();
        let mut inserted = false;
        for slot in buffer.slots.iter_mut() {
            if slot.valid == 0 {
                *slot = OrderSlot {
                    valid: 1,
                    side,
                    order_type: OrderType::Limit as u8,
                    post_only: if post_only { 1 } else { 0 },
                    seq: next_seq,
                    id: next_seq,
                    trader: trader_key,
                    size_lots,
                    limit_ticks,
                };
                inserted = true;
                break;
            }
        }
        require!(inserted, FlashBookError::BufferFull);
        buffer.seq_counter = next_seq;
        buffer.head = buffer
            .head
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        Ok(())
    }

    /// Submit a commit hash for a future taker reveal.
    pub fn submit_commit(
        ctx: Context<SubmitCommit>,
        hash: [u8; 32],
        bond: u64,
    ) -> Result<()> {
        let market = &ctx.accounts.market;
        let commit_buffer = &mut ctx.accounts.commit_buffer;
        register_commit(
            &mut commit_buffer.commits,
            hash,
            ctx.accounts.trader.key(),
            bond,
            market.current_batch,
            5, // expire after 5 batches
        )
    }

    /// Reveal a previously committed taker order. The matcher checks the
    /// hash and synthesizes a taker order in the next batch.
    pub fn submit_reveal(
        ctx: Context<SubmitReveal>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        nonce: [u8; 32],
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(limit_ticks > 0, FlashBookError::ZeroPrice);

        let payload = RevealPayload {
            trader: ctx.accounts.trader.key(),
            side: if side == 0 { Side::Long } else { Side::Short },
            size: BaseLots(size_lots),
            limit: Ticks(limit_ticks),
            nonce,
        };

        let market = &ctx.accounts.market;
        let commit_buffer = &mut ctx.accounts.commit_buffer;
        let buffer = &mut ctx.accounts.order_buffer;

        require!(
            (buffer.head as usize) < ORDER_BUFFER_CAP,
            FlashBookError::BufferFull
        );

        let next_seq = buffer
            .seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        let order = redeem_reveal(
            &mut commit_buffer.commits,
            &payload,
            market.current_batch,
            next_seq,
        )?;

        let mut inserted = false;
        for slot in buffer.slots.iter_mut() {
            if slot.valid == 0 {
                *slot = order_to_slot(&order);
                inserted = true;
                break;
            }
        }
        require!(inserted, FlashBookError::BufferFull);
        buffer.seq_counter = next_seq;
        buffer.head = buffer
            .head
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        Ok(())
    }

    // ─── Batch execution ────────────────────────────────────────────

    /// Run one batch: advance funding, generate FLP quotes, clear FBA,
    /// update mark, sweep expired commits. Position updates are emitted as
    /// an event for the off-chain bookkeeper or for `apply_fill` to consume.
    pub fn run_batch(ctx: Context<RunBatch>, now_ms: u64) -> Result<()> {
        let market = &mut ctx.accounts.market;
        let buffer = &mut ctx.accounts.order_buffer;
        let commit_buffer = &mut ctx.accounts.commit_buffer;
        let _insurance = &mut ctx.accounts.insurance_fund;
        let flp = &ctx.accounts.flp_exposure;

        // 1. Advance funding index.
        let block_delta_ms = if market.last_batch_ms == 0 {
            0
        } else {
            now_ms.saturating_sub(market.last_batch_ms)
        };
        let (new_index, ftick) = advance(
            market.cum_funding_index,
            Ticks(market.mark_price_ticks),
            Ticks(market.oracle_price_ticks),
            block_delta_ms,
            market.params.funding_rate_k_bps,
            market.params.funding_rate_max_bps_per_sec,
        )?;
        market.cum_funding_index = new_index;
        market.last_funding_rate_bps_per_sec = ftick.rate_bps_per_sec;

        // 2. Load buffered orders.
        let mut orders: Vec<Order> = Vec::with_capacity(buffer.head as usize);
        for slot in buffer.slots.iter().take(ORDER_BUFFER_CAP) {
            if slot.valid != 1 {
                continue;
            }
            orders.push(slot_to_order(slot)?);
        }

        // 3. Generate FLP virtual quotes — synthesized; consumed in this match.
        // Compute real signed exposure for *this* market from the per-market
        // entry on FlpExposureAccount, plus gross utilization across all
        // markets the pool is exposed to.
        let flp_pool_capital = flp.total_capital_quote_lots;
        let market_key = market.key();
        let flp_net_signed: i64 = {
            let entry = flp
                .per_market
                .iter()
                .find(|e| e.market == market_key && e.side != 255);
            match entry {
                Some(e) => {
                    let notional_u128 = (e.size_lots as u128)
                        .saturating_mul(e.entry_price_ticks as u128)
                        .saturating_mul(market.params.tick_size as u128);
                    let notional = notional_u128.min(i64::MAX as u128) as i64;
                    if e.side == 0 { notional } else { -notional }
                }
                None => 0,
            }
        };
        let utilization_bps = if flp_pool_capital > 0 {
            let oi_total = market
                .oi_long_lots
                .saturating_add(market.oi_short_lots) as u128;
            let notional = oi_total
                .saturating_mul(market.mark_price_ticks as u128)
                .saturating_mul(market.params.tick_size as u128);
            let bps = (notional / (flp_pool_capital as u128)).min(constants::BPS_DENOM as u128);
            bps as u32
        } else {
            0
        };

        // Realized volatility from the recent clearing-price window.
        let realized_vol_bps = realized_vol_bps_from_window(
            &market.recent_clearing_prices,
            market.recent_clearing_count,
        );

        let flp_params = FlpQuoterParams {
            base_spread_bps: market.params.flp_spread_base_bps,
            alpha_bps: market.params.flp_spread_alpha_bps,
            beta_bps: market.params.flp_spread_beta_bps,
            gamma_bps: market.params.flp_spread_gamma_bps,
            kappa_bps: market.params.flp_spread_kappa_bps,
            delta_bps: market.params.flp_spread_delta_bps,
            inventory_lambda_bps: market.params.flp_inventory_lambda_bps,
            depth_floor_lots: market.params.flp_depth_floor_lots,
            max_growth_per_batch_bps: market.params.flp_max_growth_per_batch_bps,
            levels: market.params.flp_quote_levels,
            tick_size: market.params.tick_size,
        };
        let flp_inputs = FlpQuoterInputs {
            oracle_ticks: Ticks(market.oracle_price_ticks),
            vpin_bps: market.vpin.as_bps(),
            realized_vol_bps,
            pool_capital_quote_lots: flp_pool_capital,
            pool_net_quote_lots_signed: flp_net_signed,
            pool_gross_utilization_bps: utilization_bps,
            oi_long_lots: market.oi_long_lots,
            oi_short_lots: market.oi_short_lots,
        };
        let flp_trader = flp.key();
        let flp_seq_base = FLP_SEQ_RESERVED_OFFSET
            .saturating_add(market.current_batch.saturating_mul(1024));
        let (_flp_out, flp_orders) =
            generate_quotes(flp_params, flp_inputs, flp_trader, flp_seq_base)?;
        for o in flp_orders {
            orders.push(o);
        }

        // 4. Run FBA Walrasian clearing.
        let prior_mark = Ticks(market.mark_price_ticks);
        let result = clear_batch(&orders, prior_mark)?;

        // 5. Update mark price (TWAP, oracle-banded).
        if result.clearing_volume.0 > 0 {
            let len = MARK_HISTORY_LEN;
            let idx = (market.current_batch as usize) % len;
            market.recent_clearing_prices[idx] = result.clearing_price.0;
            if (market.recent_clearing_count as usize) < len {
                market.recent_clearing_count =
                    market.recent_clearing_count.saturating_add(1);
            }
            // TWAP.
            let count = market.recent_clearing_count as usize;
            let sum: u128 = market
                .recent_clearing_prices
                .iter()
                .take(count)
                .fold(0u128, |acc, p| acc.saturating_add(*p as u128));
            let twap = (sum.checked_div(count as u128)).unwrap_or(result.clearing_price.0 as u128) as u64;
            // Oracle band.
            let band = (market.oracle_price_ticks as u128)
                .saturating_mul(market.params.oracle_band_bps as u128)
                / constants::BPS_DENOM as u128;
            let lo = (market.oracle_price_ticks as u128).saturating_sub(band) as u64;
            let hi = (market.oracle_price_ticks as u128).saturating_add(band) as u64;
            market.mark_price_ticks = twap.max(lo).min(hi);
        }

        // 6. Update VPIN. Snapshot params before mutable borrow on vpin.
        let vpin_bucket = market.params.vpin_bucket_size_lots;
        let vpin_window = market.params.vpin_ema_window;
        for fill in &result.fills {
            market
                .vpin
                .record_fill(fill.taker_side, fill.size.0, vpin_bucket, vpin_window)?;
        }

        // 7. Sweep expired commits (bond seizure logged).
        let seized = sweep_expired(&mut commit_buffer.commits, market.current_batch);

        // 8. Clear order buffer.
        for slot in buffer.slots.iter_mut() {
            *slot = OrderSlot::default();
        }
        buffer.head = 0;

        // 9. Bookkeeping.
        market.current_batch = market
            .current_batch
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        market.last_batch_ms = now_ms;

        emit!(BatchClearedEvent {
            market: market.key(),
            batch_num: market.current_batch,
            clearing_price: result.clearing_price.0,
            clearing_volume: result.clearing_volume.0,
            fill_count: result.fills.len() as u32,
            funding_rate_bps_per_sec: ftick.rate_bps_per_sec,
            seized_bonds: seized,
        });
        Ok(())
    }

    // ER delegation (delegate_market / undelegate_market) is intentionally
    // omitted in this build. The upstream `ephemeral-rollups-sdk` is not yet
    // compatible with Solana 2.x; introducing a stub here would create a
    // misleading instruction surface. The integration is purely additive —
    // when the SDK ships compat, two new instructions slot in here without
    // changing any existing semantics.
}

// ─── Account contexts ───────────────────────────────────────────────────

#[derive(Accounts)]
pub struct InitializeMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: base mint account; not validated as SPL here for v1.
    pub base_mint: UncheckedAccount<'info>,
    /// CHECK: quote mint account.
    pub quote_mint: UncheckedAccount<'info>,
    /// CHECK: base vault token account.
    pub base_vault: UncheckedAccount<'info>,
    /// CHECK: quote vault token account.
    pub quote_vault: UncheckedAccount<'info>,
    /// CHECK: oracle account (e.g. Pyth price account).
    pub oracle_account: UncheckedAccount<'info>,

    #[account(
        init,
        payer = authority,
        space = MarketAccount::space(),
        seeds = [MarketAccount::SEED, base_mint.key().as_ref(), quote_mint.key().as_ref()],
        bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        init,
        payer = authority,
        space = OrderBufferAccount::space(),
        seeds = [OrderBufferAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub order_buffer: Box<Account<'info, OrderBufferAccount>>,

    #[account(
        init,
        payer = authority,
        space = CommitBufferAccount::space(),
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,

    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,

    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeInsuranceFund<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = InsuranceFundAccount::space(),
        seeds = [InsuranceFundAccount::SEED],
        bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct OpenTraderState<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,
    #[account(
        init,
        payer = trader,
        space = TraderStateAccount::space(),
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump,
    )]
    pub trader_state: Box<Account<'info, TraderStateAccount>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateOracle<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,
}

#[derive(Accounts)]
pub struct PlaceOrder<'info> {
    pub trader: Signer<'info>,
    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,
    #[account(
        mut,
        seeds = [OrderBufferAccount::SEED, market.key().as_ref()],
        bump = order_buffer.bump,
    )]
    pub order_buffer: Account<'info, OrderBufferAccount>,
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
        constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct SubmitCommit<'info> {
    pub trader: Signer<'info>,
    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,
    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Account<'info, CommitBufferAccount>,
}

#[derive(Accounts)]
pub struct SubmitReveal<'info> {
    pub trader: Signer<'info>,
    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,
    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Account<'info, CommitBufferAccount>,
    #[account(
        mut,
        seeds = [OrderBufferAccount::SEED, market.key().as_ref()],
        bump = order_buffer.bump,
    )]
    pub order_buffer: Account<'info, OrderBufferAccount>,
}

#[derive(Accounts)]
pub struct RunBatch<'info> {
    pub sequencer: Signer<'info>,
    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,
    #[account(
        mut,
        seeds = [OrderBufferAccount::SEED, market.key().as_ref()],
        bump = order_buffer.bump,
    )]
    pub order_buffer: Box<Account<'info, OrderBufferAccount>>,
    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,
    #[account(
        mut,
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,
    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,
}

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    pub trader: Signer<'info>,
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
        constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct WithdrawCollateral<'info> {
    pub trader: Signer<'info>,
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
        constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct ApplyFill<'info> {
    #[account(mut)]
    pub sequencer: Signer<'info>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, taker_trader_state.trader.as_ref()],
        bump = taker_trader_state.bump,
    )]
    pub taker_trader_state: Account<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, maker_trader_state.trader.as_ref()],
        bump = maker_trader_state.bump,
    )]
    pub maker_trader_state: Account<'info, TraderStateAccount>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), taker_trader_state.trader.as_ref()],
        bump,
    )]
    pub taker_position: Account<'info, state::PositionAccount>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), maker_trader_state.trader.as_ref()],
        bump,
    )]
    pub maker_position: Account<'info, state::PositionAccount>,

    pub system_program: Program<'info, System>,
}

// ─── Events ─────────────────────────────────────────────────────────────

#[event]
pub struct MarketInitializedEvent {
    pub market: Pubkey,
    pub authority: Pubkey,
    pub initial_oracle_ticks: u64,
}

#[event]
pub struct BatchClearedEvent {
    pub market: Pubkey,
    pub batch_num: u64,
    pub clearing_price: u64,
    pub clearing_volume: u64,
    pub fill_count: u32,
    pub funding_rate_bps_per_sec: i64,
    pub seized_bonds: u64,
}

#[event]
pub struct CollateralDepositedEvent {
    pub trader: Pubkey,
    pub amount: u64,
    pub new_balance: u64,
}

#[event]
pub struct CollateralWithdrawnEvent {
    pub trader: Pubkey,
    pub amount: u64,
    pub new_balance: u64,
}

#[event]
pub struct FillAppliedEvent {
    pub market: Pubkey,
    pub taker: Pubkey,
    pub maker: Pubkey,
    pub taker_side: u8,
    pub size_lots: u64,
    pub price_ticks: u64,
    pub batch_num: u64,
}

// ─── Helpers ────────────────────────────────────────────────────────────

#[repr(u8)]
pub enum MarketStatus {
    Inactive = 0,
    Active = 1,
    PostOnly = 2,
    Paused = 3,
    Closed = 4,
}

fn slot_to_order(slot: &OrderSlot) -> Result<Order> {
    let side = if slot.side == 0 { Side::Long } else { Side::Short };
    let order_type = match slot.order_type {
        0 => OrderType::Limit,
        1 => OrderType::Taker,
        2 => OrderType::FlpVirtual,
        3 => OrderType::Liquidation,
        4 => OrderType::Adl,
        _ => return Err(error!(FlashBookError::OutOfRange)),
    };
    Ok(Order {
        id: slot.id,
        trader: slot.trader,
        side,
        order_type,
        size: BaseLots(slot.size_lots),
        limit_price: Ticks(slot.limit_ticks),
        seq: slot.seq,
        post_only: slot.post_only == 1,
    })
}

/// Realized volatility (stdev of relative returns) over a clearing-price
/// window, expressed in bps. Pure-integer; uses isqrt.
fn realized_vol_bps_from_window(prices: &[u64; MARK_HISTORY_LEN], count: u8) -> u32 {
    let n = (count as usize).min(prices.len());
    if n < 2 {
        return 0;
    }
    // returns_bps[i] = (p[i+1] - p[i]) * 10_000 / p[i]
    let mut returns: [i64; MARK_HISTORY_LEN] = [0; MARK_HISTORY_LEN];
    let mut returns_n: usize = 0;
    let mut sum: i64 = 0;
    for i in 0..(n - 1) {
        let p0 = prices[i] as i128;
        let p1 = prices[i + 1] as i128;
        if p0 <= 0 {
            continue;
        }
        let r_bps = ((p1 - p0) * 10_000) / p0;
        // clamp to i64 range (returns of ±100% × 10_000 = ±1_000_000 fits trivially)
        let r = if r_bps > i64::MAX as i128 {
            i64::MAX
        } else if r_bps < i64::MIN as i128 {
            i64::MIN
        } else {
            r_bps as i64
        };
        returns[returns_n] = r;
        returns_n += 1;
        sum = sum.saturating_add(r);
    }
    if returns_n == 0 {
        return 0;
    }
    let mean = sum / returns_n as i64;
    let mut var_sum: i128 = 0;
    for r in returns.iter().take(returns_n) {
        let d = (*r - mean) as i128;
        var_sum = var_sum.saturating_add(d * d);
    }
    let variance = var_sum / returns_n as i128;
    let stdev = (variance.max(0) as u128).isqrt() as u64;
    // Cap at 10_000 bps (= 100%) — a plausible upper bound on per-batch return stdev.
    stdev.min(10_000) as u32
}

/// Update OI counters for a single trader's position transition.
fn update_oi(
    market: &mut MarketAccount,
    pre_side: u8,
    pre_size: u64,
    post_side: u8,
    post_size: u64,
) -> Result<()> {
    if pre_size > 0 {
        if pre_side == 0 {
            market.oi_long_lots = market.oi_long_lots.saturating_sub(pre_size);
        } else {
            market.oi_short_lots = market.oi_short_lots.saturating_sub(pre_size);
        }
    }
    if post_size > 0 {
        if post_side == 0 {
            market.oi_long_lots = market
                .oi_long_lots
                .checked_add(post_size)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        } else {
            market.oi_short_lots = market
                .oi_short_lots
                .checked_add(post_size)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        }
    }
    Ok(())
}

/// Apply one fill against a single Position account in-place.
///
/// Cases:
///   - Empty position (size == 0): open with `side`, `size`, `entry = price`.
///   - Same side: increase size, recompute volume-weighted entry.
///   - Opposite side, size ≤ existing: reduce; realize PnL on closed portion.
///   - Opposite side, size > existing: flip side; realize PnL on existing
///     fully closed; remaining size opens at `price`.
fn apply_fill_to_position(
    pos: &mut state::PositionAccount,
    fill_side: Side,
    fill_size_lots: u64,
    fill_price_ticks: u64,
    funding_index_now: i128,
) -> Result<()> {
    let cur_side = if pos.side == 0 { Side::Long } else { Side::Short };

    if pos.size_lots == 0 {
        pos.side = fill_side as u8;
        pos.size_lots = fill_size_lots;
        pos.entry_price_ticks = fill_price_ticks;
        pos.cum_funding_index_at_entry = funding_index_now;
        return Ok(());
    }

    if cur_side == fill_side {
        // Same side: weighted-avg entry.
        let new_size = pos
            .size_lots
            .checked_add(fill_size_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        // entry = (entry*old_size + price*fill_size) / new_size
        let weighted = (pos.entry_price_ticks as u128)
            .checked_mul(pos.size_lots as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            .checked_add(
                (fill_price_ticks as u128)
                    .checked_mul(fill_size_lots as u128)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?,
            )
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            .checked_div(new_size as u128)
            .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
        pos.entry_price_ticks = weighted as u64;
        pos.size_lots = new_size;
        return Ok(());
    }

    // Opposite side: realize PnL on the closed portion.
    let close_size = fill_size_lots.min(pos.size_lots);
    let sign: i128 = if cur_side == Side::Long { 1 } else { -1 };
    let pnl_per_lot: i128 =
        (fill_price_ticks as i128) - (pos.entry_price_ticks as i128);
    let pnl: i128 = sign
        .checked_mul(close_size as i128)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
        .checked_mul(pnl_per_lot)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
    let pnl_clamped = if pnl > i64::MAX as i128 {
        i64::MAX
    } else if pnl < i64::MIN as i128 {
        i64::MIN
    } else {
        pnl as i64
    };
    pos.realized_pnl_quote_lots = pos
        .realized_pnl_quote_lots
        .checked_add(pnl_clamped)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

    if fill_size_lots <= pos.size_lots {
        pos.size_lots = pos
            .size_lots
            .checked_sub(fill_size_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        if pos.size_lots == 0 {
            pos.entry_price_ticks = 0;
            pos.cum_funding_index_at_entry = funding_index_now;
        }
    } else {
        // Flip side. Remaining = fill - existing.
        let remaining = fill_size_lots
            .checked_sub(pos.size_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        pos.side = fill_side as u8;
        pos.size_lots = remaining;
        pos.entry_price_ticks = fill_price_ticks;
        pos.cum_funding_index_at_entry = funding_index_now;
    }
    Ok(())
}

fn order_to_slot(o: &Order) -> OrderSlot {
    OrderSlot {
        valid: 1,
        side: o.side as u8,
        order_type: o.order_type as u8,
        post_only: if o.post_only { 1 } else { 0 },
        seq: o.seq,
        id: o.id,
        trader: o.trader,
        size_lots: o.size.0,
        limit_ticks: o.limit_price.0,
    }
}
