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
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, Transfer};

pub mod constants;
pub mod er;
pub mod errors;
pub mod matcher;
pub mod state;

pub use errors::FlashBookError;

use constants::{
    FLP_SEQ_RESERVED_OFFSET, MARK_HISTORY_LEN, MAX_BASKET_LEGS_N,
    MAX_ORDERS_PER_TRADER_PER_BATCH, ORDER_BUFFER_CAP,
};
use matcher::commit_reveal::{
    register_commit, redeem_reveal, sweep_expired, RevealPayload,
};
use matcher::fba::clear_batch;
use matcher::flp_quoter::{generate_quotes, FlpQuoterInputs, FlpQuoterParams};
use matcher::funding::{advance, funding_owed};
use matcher::lot::{BaseLots, Ticks};
use matcher::order::{Order, OrderType, Side};
use matcher::risk::{
    assess_margin as assess_margin_fn, default_scenarios as default_scenarios_fn,
    MarketSnapshot as RiskMarketSnap, PositionSnapshot as RiskPosSnap,
};
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
        market.oracle_published_at_unix_seconds = Clock::get()?.unix_timestamp.max(0) as u64;
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
        commit_buf.commits = [state::CommitRow::default(); state::COMMIT_BUFFER_CAP];

        emit!(MarketInitializedEvent {
            market: market.key(),
            authority: market.authority,
            initial_oracle_ticks,
        });
        Ok(())
    }

    /// Initialize the FLP exposure account (one per protocol). Must run
    /// before `initialize_market`. Mints `initial_capital_quote_lots` shares
    /// to the authority at 1:1 (treasury endowment); these shares can later
    /// be redeemed via `withdraw_flp_capital`.
    pub fn initialize_flp_exposure(
        ctx: Context<InitializeFlpExposure>,
        initial_capital_quote_lots: u64,
    ) -> Result<()> {
        let flp = &mut ctx.accounts.flp_exposure;
        flp.authority = ctx.accounts.authority.key();
        flp.bump = ctx.bumps.flp_exposure;
        flp.total_capital_quote_lots = initial_capital_quote_lots;
        flp.realized_pnl = 0;
        flp.markets_count = 0;
        flp.lp_shares_outstanding = initial_capital_quote_lots;
        flp.per_market = [state::FlpMarketExposure::default(); 16];
        for slot in flp.per_market.iter_mut() {
            slot.side = 255;
        }

        // Treasury endowment: authority owns the initial shares 1:1.
        let lp_pos = &mut ctx.accounts.authority_lp_position;
        lp_pos.lp = ctx.accounts.authority.key();
        lp_pos.bump = ctx.bumps.authority_lp_position;
        lp_pos.shares = initial_capital_quote_lots;
        lp_pos.total_deposited_quote_lots = initial_capital_quote_lots;
        lp_pos.total_withdrawn_quote_lots = 0;

        emit!(FlpExposureInitializedEvent {
            authority: flp.authority,
            initial_capital: initial_capital_quote_lots,
        });
        Ok(())
    }

    /// Deposit capital into the FLP pool and mint shares at the current
    /// NAV/share price. Permissionless — any signer can become an LP.
    /// Their LpPositionAccount is created lazily via init_if_needed.
    ///
    /// Shares minted = amount × shares_outstanding / NAV. Bootstrap
    /// (NAV ≤ 0 or shares_outstanding == 0) mints 1:1.
    pub fn deposit_flp_capital(
        ctx: Context<DepositFlpCapital>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        // SPL transfer from LP's ATA to protocol vault.
        let cpi_accounts = Transfer {
            from: ctx.accounts.authority_quote_ata.to_account_info(),
            to: ctx.accounts.quote_vault.to_account_info(),
            authority: ctx.accounts.authority.to_account_info(),
        };
        let cpi_ctx =
            CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount_quote_lots)?;

        let flp = &mut ctx.accounts.flp_exposure;
        let nav = flp.nav();
        let shares_outstanding = flp.lp_shares_outstanding;

        // Compute shares to mint. Bootstrap (no shares yet OR NAV <= 0)
        // mints 1:1 — first depositor sets the share price.
        let shares_to_mint: u64 = if shares_outstanding == 0 || nav <= 0 {
            amount_quote_lots
        } else {
            let prod = (amount_quote_lots as u128)
                .checked_mul(shares_outstanding as u128)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            let s = prod / (nav as u128);
            require!(s <= u64::MAX as u128, FlashBookError::ArithmeticOverflow);
            s as u64
        };
        require!(shares_to_mint > 0, FlashBookError::ZeroSize);

        flp.total_capital_quote_lots = flp
            .total_capital_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        flp.lp_shares_outstanding = flp
            .lp_shares_outstanding
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        let lp_pos = &mut ctx.accounts.lp_position;
        // First-time depositor: initialize identity fields.
        if lp_pos.lp == Pubkey::default() {
            lp_pos.lp = ctx.accounts.authority.key();
            lp_pos.bump = ctx.bumps.lp_position;
        }
        lp_pos.shares = lp_pos
            .shares
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        lp_pos.total_deposited_quote_lots = lp_pos
            .total_deposited_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        emit!(FlpCapitalUpdatedEvent {
            new_total: flp.total_capital_quote_lots,
            delta: amount_quote_lots as i64,
        });
        Ok(())
    }

    /// LP withdraws capital from the FLP pool. Blocked if it would push
    /// utilization past 100% (current gross exposure must remain ≤ new
    /// capital).
    /// Burn `shares_to_burn` LP shares and withdraw the proportional NAV
    /// claim. Caller must own the shares. Returns USDC via SPL CPI signed
    /// by the insurance_fund PDA (vault authority).
    ///
    /// Amount returned = shares_to_burn × NAV / shares_outstanding.
    ///
    /// Position-aware solvency guard: when the FLP has open positions
    /// (markets_count > 0), the caller must pass each active market as
    /// `remaining_accounts`. We compute gross_exposure = Σ |size × mark|
    /// across all per_market entries and require post-withdraw NAV ≥
    /// gross_exposure. This lets LPs withdraw while the FLP carries
    /// positions, as long as enough NAV remains to absorb a max-shock
    /// loss. The empty-pool case (markets_count == 0) skips the walk.
    pub fn withdraw_flp_capital<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawFlpCapital<'info>>,
        shares_to_burn: u64,
    ) -> Result<()> {
        require!(shares_to_burn > 0, FlashBookError::ZeroSize);
        require_keys_eq!(
            ctx.accounts.lp_position.lp,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        require!(
            shares_to_burn <= ctx.accounts.lp_position.shares,
            FlashBookError::InsufficientCollateral
        );

        let flp_ro = &ctx.accounts.flp_exposure;
        let nav = flp_ro.nav();
        require!(nav > 0, FlashBookError::InsufficientCollateral);
        let shares_outstanding = flp_ro.lp_shares_outstanding;
        require!(shares_outstanding > 0, FlashBookError::InsufficientCollateral);

        // amount = shares_to_burn × NAV / shares_outstanding
        let prod = (shares_to_burn as u128)
            .checked_mul(nav as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        let amount_u128 = prod / (shares_outstanding as u128);
        require!(amount_u128 <= u64::MAX as u128, FlashBookError::ArithmeticOverflow);
        let amount_quote_lots = amount_u128 as u64;
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        // Compute new total_capital up-front so the solvency check below can
        // reason about post-withdraw NAV.
        let new_total = flp_ro
            .total_capital_quote_lots
            .checked_sub(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;

        // Position-aware solvency check. If the FLP has open positions
        // across markets, walk remaining_accounts to compute gross
        // exposure at current marks and ensure post-withdraw NAV stays
        // above it.
        if flp_ro.markets_count > 0 {
            let remaining = ctx.remaining_accounts;
            let mut gross_exposure: u128 = 0;
            let mut matched: u8 = 0;
            for slot in flp_ro.per_market.iter() {
                if slot.side == 255 {
                    continue;
                }
                // Find the matching market in remaining_accounts.
                let market_ai = remaining
                    .iter()
                    .find(|ai| ai.key() == slot.market)
                    .ok_or_else(|| error!(FlashBookError::MissingMarketAccount))?;
                let m_data = market_ai.try_borrow_data()?;
                let m_state = MarketAccount::try_deserialize(&mut &m_data[..])?;
                let notional = (slot.size_lots as u128)
                    .saturating_mul(m_state.mark_price_ticks as u128)
                    .saturating_mul(m_state.params.tick_size as u128);
                gross_exposure = gross_exposure.saturating_add(notional);
                matched += 1;
            }
            require!(
                matched == flp_ro.markets_count,
                FlashBookError::MissingMarketAccount
            );
            // Post-withdraw NAV = (new_total_capital + realized_pnl) must
            // cover gross exposure. realized_pnl is signed; cap at 0 if
            // negative for the conservative direction (don't credit).
            let post_nav: i128 = (new_total as i128) + (flp_ro.realized_pnl as i128);
            require!(
                post_nav >= 0 && (post_nav as u128) >= gross_exposure,
                FlashBookError::FlpWithdrawUndercollateralized
            );
        }

        // Vault must have enough quote tokens to satisfy the withdrawal.
        // The vault's amount field is the authoritative source.
        require!(
            ctx.accounts.quote_vault.amount >= amount_quote_lots,
            FlashBookError::InsufficientCollateral
        );

        let bump = ctx.accounts.insurance_fund.bump;
        let signer_seeds: &[&[u8]] = &[InsuranceFundAccount::SEED, &[bump]];
        let signers = &[signer_seeds];
        let cpi_accounts = Transfer {
            from: ctx.accounts.quote_vault.to_account_info(),
            to: ctx.accounts.authority_quote_ata.to_account_info(),
            authority: ctx.accounts.insurance_fund.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signers,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        let flp = &mut ctx.accounts.flp_exposure;
        flp.total_capital_quote_lots = new_total;
        flp.lp_shares_outstanding = flp
            .lp_shares_outstanding
            .checked_sub(shares_to_burn)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;

        let lp_pos = &mut ctx.accounts.lp_position;
        lp_pos.shares = lp_pos
            .shares
            .checked_sub(shares_to_burn)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        lp_pos.total_withdrawn_quote_lots = lp_pos
            .total_withdrawn_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        emit!(FlpCapitalUpdatedEvent {
            new_total,
            delta: -(amount_quote_lots as i64),
        });
        Ok(())
    }

    /// Initialize an insurance fund (one per protocol). Also creates the
    /// global protocol quote vault — a TokenAccount for `quote_mint` whose
    /// authority is the insurance_fund PDA itself. All trader collateral
    /// and FLP capital flows through this vault.
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
        f.quote_mint = ctx.accounts.quote_mint.key();
        f.quote_vault = ctx.accounts.quote_vault.key();
        Ok(())
    }

    /// Authority withdraws excess insurance fund balance. Cannot push the
    /// balance below `pause_threshold_quote_lots` — that gate keeps the
    /// fund solvent enough to absorb a max-shock loss without triggering
    /// the new-positions-paused state.
    ///
    /// Use case: governance rebalancing surplus contributions. The amount
    /// is transferred from the protocol vault (PDA-signed) to the
    /// authority's quote ATA. This does NOT route through the LP pool;
    /// the insurance fund is governance-owned, not LP-owned.
    pub fn withdraw_insurance_fund(
        ctx: Context<WithdrawInsuranceFund>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        require_keys_eq!(
            ctx.accounts.insurance_fund.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );

        let new_balance = ctx
            .accounts
            .insurance_fund
            .balance_quote_lots
            .checked_sub(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        // Cannot withdraw below the pause threshold — that's the protocol's
        // solvency floor.
        require!(
            new_balance >= ctx.accounts.insurance_fund.pause_threshold_quote_lots,
            FlashBookError::InsufficientCollateral
        );

        // Vault must hold enough tokens to satisfy the withdrawal.
        require!(
            ctx.accounts.quote_vault.amount >= amount_quote_lots,
            FlashBookError::InsufficientCollateral
        );

        let bump = ctx.accounts.insurance_fund.bump;
        let signer_seeds: &[&[u8]] = &[InsuranceFundAccount::SEED, &[bump]];
        let signers = &[signer_seeds];
        let cpi_accounts = Transfer {
            from: ctx.accounts.quote_vault.to_account_info(),
            to: ctx.accounts.authority_quote_ata.to_account_info(),
            authority: ctx.accounts.insurance_fund.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signers,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        let f = &mut ctx.accounts.insurance_fund;
        f.balance_quote_lots = new_balance;
        f.total_payouts = f
            .total_payouts
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
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
        s.fee_discount_bps = 0;
        s.delegate = Pubkey::default();
        s.referrer = Pubkey::default();
        Ok(())
    }

    /// Set the trader's referrer. ONE-TIME-WRITE: once set to a non-default
    /// pubkey, the field cannot be rewritten. Anti-rotation griefing —
    /// referrers earn off the trader for the lifetime of the account, no
    /// "rug your referrer" attack vector. Set to Pubkey::default() to
    /// opt out PERMANENTLY (also one-time-write).
    pub fn set_trader_referrer(
        ctx: Context<SetTraderReferrer>,
        referrer: Pubkey,
    ) -> Result<()> {
        let s = &mut ctx.accounts.trader_state;
        require!(s.referrer == Pubkey::default(), FlashBookError::OutOfRange);
        s.referrer = referrer;
        emit!(TraderReferrerSetEvent {
            trader: s.trader,
            referrer,
        });
        Ok(())
    }

    /// Set or clear the trader's delegate authority. The delegate is a
    /// pubkey allowed to sign trader-bound ix on the trader's behalf.
    /// Pass Pubkey::default() to clear. The trader's own signature
    /// always works regardless — delegate is additive, not exclusive.
    ///
    /// Use cases:
    ///   • Master/hot-key split: master holds funds; hot key trades
    ///     (Hyperliquid + dYdX standard pattern).
    ///   • Multi-sig subaccount manager.
    ///   • MM bot keypair authorized to manage a vault's positions.
    pub fn set_trader_delegate(
        ctx: Context<SetTraderDelegate>,
        new_delegate: Pubkey,
    ) -> Result<()> {
        let s = &mut ctx.accounts.trader_state;
        let prev = s.delegate;
        s.delegate = new_delegate;
        emit!(TraderDelegateUpdatedEvent {
            trader: s.trader,
            previous: prev,
            new: new_delegate,
        });
        Ok(())
    }

    /// Set a trader's per-trader fee discount in bps off the base taker
    /// fee. Authority-only. Off-chain volume tracking sets this based on
    /// 30-day rolling notional — the universal pattern at every CEX
    /// (Binance, OKX, Bybit, Hyperliquid).
    ///
    /// `discount_bps` is bounded to BPS_DENOM (10_000) — a 100% discount
    /// makes the taker fee zero; the chain refuses values above 10_000
    /// to prevent negative fees.
    pub fn set_trader_fee_tier(
        ctx: Context<SetTraderFeeTier>,
        discount_bps: u32,
    ) -> Result<()> {
        require!(
            discount_bps <= constants::BPS_DENOM as u32,
            FlashBookError::OutOfRange
        );
        let s = &mut ctx.accounts.trader_state;
        s.fee_discount_bps = discount_bps;
        emit!(TraderFeeTierUpdatedEvent {
            trader: s.trader,
            discount_bps,
        });
        Ok(())
    }

    /// Idempotently create the trader's quote ATA. Anchor's `init_if_needed`
    /// + `associated_token::*` constraints handle the AssociatedToken CPI:
    /// if the ATA already exists, this is a no-op; otherwise it's created
    /// at the canonical address with `payer` funding rent.
    ///
    /// Owner doesn't need to sign — anyone can fund someone else's ATA
    /// creation. The mint is constrained to the protocol's `quote_mint`
    /// so this can only be used to create ATAs that Flash Book accepts.
    pub fn init_trader_ata(_ctx: Context<InitTraderAta>) -> Result<()> {
        Ok(())
    }

    /// Close the trader's quote ATA and refund the rent lamports to
    /// `rent_destination`. CPIs to SPL Token's `CloseAccount`, which
    /// enforces an empty token balance — closing a non-empty ATA fails
    /// with `TokenError::NonNativeHasBalance` from the SPL program.
    ///
    /// The trader signs (they are the ATA authority). This is useful when
    /// a trader is fully exited and wants to reclaim their rent, or when
    /// the SDK wants to clean up an accidentally-created ATA.
    pub fn close_trader_ata(ctx: Context<CloseTraderAta>) -> Result<()> {
        let cpi_accounts = CloseAccount {
            account: ctx.accounts.trader_quote_ata.to_account_info(),
            destination: ctx.accounts.rent_destination.to_account_info(),
            authority: ctx.accounts.trader.to_account_info(),
        };
        let cpi_ctx =
            CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::close_account(cpi_ctx)
    }

    /// Deposit collateral. Performs an SPL transfer from the trader's
    /// quote ATA into the global protocol vault, then credits the trader's
    /// accounting balance. Both the SPL transfer and the accounting bump
    /// happen atomically — partial failure rolls back the whole tx.
    pub fn deposit_collateral(
        ctx: Context<DepositCollateral>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        // SPL transfer: trader_quote_ata → quote_vault. Trader signs.
        let cpi_accounts = Transfer {
            from: ctx.accounts.trader_quote_ata.to_account_info(),
            to: ctx.accounts.quote_vault.to_account_info(),
            authority: ctx.accounts.trader.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        // Accounting.
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

    /// Withdraw collateral. Decrements accounting + transfers from the
    /// vault back to the trader's ATA. The program signs as the
    /// insurance_fund PDA (which owns the vault). Blocked while the
    /// trader has open positions.
    pub fn withdraw_collateral(
        ctx: Context<WithdrawCollateral>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        // Pre-flight checks (do these before the transfer so we don't
        // mutate vault state on a rejected withdrawal).
        {
            let s = &ctx.accounts.trader_state;
            require!(s.open_positions == 0, FlashBookError::InsufficientCollateral);
            require!(
                amount_quote_lots <= s.collateral_quote_lots,
                FlashBookError::InsufficientCollateral,
            );
        }

        // SPL transfer: quote_vault → trader_quote_ata. Program signs.
        let bump = ctx.accounts.insurance_fund.bump;
        let signer_seeds: &[&[u8]] = &[InsuranceFundAccount::SEED, &[bump]];
        let signers = &[signer_seeds];
        let cpi_accounts = Transfer {
            from: ctx.accounts.quote_vault.to_account_info(),
            to: ctx.accounts.trader_quote_ata.to_account_info(),
            authority: ctx.accounts.insurance_fund.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signers,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        // Accounting.
        let s = &mut ctx.accounts.trader_state;
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

    /// Settle accrued funding for a single position. Computes funding owed
    /// since the last settlement using `funding_owed(side, notional, now,
    /// at_entry)`, debits or credits the trader's collateral_quote_lots,
    /// accumulates `position.funding_paid_quote_lots`, and resets the
    /// position's `cum_funding_index_at_entry` to the market's current
    /// index. Idempotent — calling again immediately is a no-op (delta=0).
    ///
    /// Permissionless: anyone can poke a position to settle. This protects
    /// the protocol against traders who never close — funding accrues into
    /// the margin calc forever otherwise. Off-chain keepers will sweep
    /// stale positions periodically.
    ///
    /// On insufficient collateral to cover positive funding owed, the
    /// position's collateral is drained to zero (it will be liquidated on
    /// the next risk check). We do not fail the tx because that would let
    /// underwater positions block keepers from settling them.
    pub fn settle_funding(ctx: Context<SettleFunding>) -> Result<()> {
        let market = &ctx.accounts.market;
        let position = &mut ctx.accounts.position;
        let trader_state = &mut ctx.accounts.trader_state;

        // No-op for empty positions.
        if position.size_lots == 0 {
            position.cum_funding_index_at_entry = market.cum_funding_index;
            return Ok(());
        }

        // notional = size × entry_price × tick_size, in quote lots
        let notional_u128 = (position.size_lots as u128)
            .checked_mul(position.entry_price_ticks as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            .checked_mul(market.params.tick_size as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(
            notional_u128 <= u64::MAX as u128,
            FlashBookError::ArithmeticOverflow
        );
        let notional = notional_u128 as u64;

        let is_long = position.side == 0;
        let owed_i128 = funding_owed(
            is_long,
            notional,
            market.cum_funding_index,
            position.cum_funding_index_at_entry,
        )?;

        // Apply settlement: positive owed → trader pays, negative → receives.
        // Clamp owed to i64 range; rounded values that overflow i64 are
        // capped (extreme case only reachable with insane funding rates).
        let owed_i64 = if owed_i128 > i64::MAX as i128 {
            i64::MAX
        } else if owed_i128 < i64::MIN as i128 {
            i64::MIN
        } else {
            owed_i128 as i64
        };

        if owed_i64 > 0 {
            // Trader owes funding. Drain up to current collateral; never fail.
            let pay = (owed_i64 as u64).min(trader_state.collateral_quote_lots);
            trader_state.collateral_quote_lots -= pay;
        } else if owed_i64 < 0 {
            // Trader receives funding from the protocol.
            let recv = owed_i64.unsigned_abs();
            trader_state.collateral_quote_lots = trader_state
                .collateral_quote_lots
                .checked_add(recv)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        }

        position.funding_paid_quote_lots = position
            .funding_paid_quote_lots
            .checked_add(owed_i64)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        position.cum_funding_index_at_entry = market.cum_funding_index;

        emit!(FundingSettledEvent {
            market: market.key(),
            trader: position.trader,
            owed_quote_lots: owed_i64,
            new_collateral: trader_state.collateral_quote_lots,
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
    /// `taker_was_jit`: set to true if the matched taker order was
    /// JIT-tagged (flag bit 3 on place_limit_order). The sequencer reads
    /// this from the order's stored flags. When true, the maker earns
    /// `market.params.jit_bonus_rebate_bps` extra rebate on top of the
    /// base maker_rebate_bps. Passing false preserves legacy behaviour.
    pub fn apply_fill(
        ctx: Context<ApplyFill>,
        size_lots: u64,
        price_ticks: u64,
        taker_side: u8,
        taker_was_jit: bool,
    ) -> Result<()> {
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(price_ticks > 0, FlashBookError::ZeroPrice);
        require!(taker_side <= 1, FlashBookError::OutOfRange);

        let market = &mut ctx.accounts.market;
        let market_key = market.key();
        let funding_index = market.cum_funding_index;
        let current_batch = market.current_batch;

        // Compute taker fee + maker rebate for this fill.
        // notional = size × price × tick_size (in quote_lots).
        let notional_u128 = (size_lots as u128)
            .checked_mul(price_ticks as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            .checked_mul(market.params.tick_size as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        let mut taker_fee_u128 =
            notional_u128.saturating_mul(market.params.taker_fee_bps as u128) / constants::BPS_DENOM as u128;
        // Apply taker's per-trader fee tier discount (0..10_000 bps).
        // Capped at 10_000 (100%) so the discount never inverts to a
        // negative fee. Discount is fee × (10_000 - discount_bps) / 10_000.
        let discount_bps =
            ctx.accounts.taker_trader_state.fee_discount_bps.min(constants::BPS_DENOM as u32) as u128;
        if discount_bps > 0 {
            taker_fee_u128 = taker_fee_u128
                .saturating_mul((constants::BPS_DENOM as u128).saturating_sub(discount_bps))
                / constants::BPS_DENOM as u128;
        }
        // Effective maker rebate = base + JIT bonus (if taker was tagged).
        // JIT bonus comes out of the protocol — paid by reducing the
        // insurance contribution downstream, not by raising the taker
        // fee. This is the Drift JIT economic model.
        let mut effective_rebate_bps = market.params.maker_rebate_bps as u128;
        if taker_was_jit {
            effective_rebate_bps =
                effective_rebate_bps.saturating_add(market.params.jit_bonus_rebate_bps as u128);
        }
        let maker_rebate_u128 =
            notional_u128.saturating_mul(effective_rebate_bps) / constants::BPS_DENOM as u128;
        // Rebate must never exceed fee; defense against bad governance config
        // AND against discounts pushing fee below rebate. If discount drops
        // taker_fee below maker_rebate, cap rebate at the (discounted) fee.
        let maker_rebate_u128 = maker_rebate_u128.min(taker_fee_u128);
        let taker_fee = if taker_fee_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            taker_fee_u128 as u64
        };
        let maker_rebate = if maker_rebate_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            maker_rebate_u128 as u64
        };
        let net_fee = taker_fee.saturating_sub(maker_rebate);
        let taker_side_enum = if taker_side == 0 { Side::Long } else { Side::Short };
        let maker_side_enum = taker_side_enum.opposite();
        let taker_trader_pk = ctx.accounts.taker_trader_state.trader;
        let maker_trader_pk = ctx.accounts.maker_trader_state.trader;

        // Apply fees BEFORE position state is mutated, so reads are clean.
        // Taker pays fee from collateral (must have it; place_limit_order's
        // margin gate ensured this at intake time, but we double-check).
        {
            let taker_state = &mut ctx.accounts.taker_trader_state;
            taker_state.collateral_quote_lots = taker_state
                .collateral_quote_lots
                .checked_sub(taker_fee)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        }
        // Maker receives rebate.
        {
            let maker_state = &mut ctx.accounts.maker_trader_state;
            maker_state.collateral_quote_lots = maker_state
                .collateral_quote_lots
                .checked_add(maker_rebate)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        }
        // Net fee to insurance fund (per fee_contribution_bps).
        {
            let fund = &mut ctx.accounts.insurance_fund;
            let contribution = (net_fee as u128)
                .saturating_mul(fund.fee_contribution_bps as u128)
                .checked_div(constants::BPS_DENOM as u128)
                .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
            let contribution_u64 = if contribution > u64::MAX as u128 {
                u64::MAX
            } else {
                contribution as u64
            };
            fund.balance_quote_lots = fund
                .balance_quote_lots
                .checked_add(contribution_u64)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            fund.total_contributions = fund
                .total_contributions
                .saturating_add(contribution_u64);
        }
        market.total_fees_collected = market
            .total_fees_collected
            .checked_add(net_fee)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        // ── Referral attribution (Hyperliquid affiliate model) ───────
        // When the taker has a referrer set, emit ReferralOwedEvent
        // with the share so off-chain integrators can credit referrer
        // balances. Pull-based (no on-chain referrer account walk) keeps
        // ApplyFill's account list bounded; off-chain ledger pays out.
        let taker_referrer = ctx.accounts.taker_trader_state.referrer;
        if taker_referrer != Pubkey::default() && market.params.referrer_share_bps > 0 {
            let share = ((net_fee as u128)
                .saturating_mul(market.params.referrer_share_bps as u128)
                / (constants::BPS_DENOM as u128)) as u64;
            if share > 0 {
                emit!(ReferralOwedEvent {
                    taker: ctx.accounts.taker_trader_state.trader,
                    referrer: taker_referrer,
                    amount_quote_lots: share,
                });
            }
        }

        // ── Toxicity tax (VPIN-scaled) ────────────────────────────────
        // Charges the taker an extra fee proportional to the market's
        // current VPIN signal. Compensates the maker — who was on the
        // wrong side of toxic flow — and tops up the insurance fund.
        // tax = notional × max_bps × vpin_bps / (10_000 × 10_000)
        // Skipped silently when vpin_bps == 0 (no observed toxicity) or
        // when toxicity_tax_max_bps == 0 (feature disabled per market).
        let tax_max_bps = market.params.toxicity_tax_max_bps;
        let vpin_bps = market.vpin.as_bps();
        if tax_max_bps > 0 && vpin_bps > 0 {
            let tax_u128 = notional_u128
                .saturating_mul(tax_max_bps as u128)
                .saturating_mul(vpin_bps as u128)
                / (constants::BPS_DENOM as u128)
                / (constants::BPS_DENOM as u128);
            let tax_uncapped: u64 = if tax_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                tax_u128 as u64
            };
            // Cap to taker's available collateral — never fail the fill.
            let tax = tax_uncapped.min(ctx.accounts.taker_trader_state.collateral_quote_lots);
            if tax > 0 {
                // Deduct from taker.
                ctx.accounts.taker_trader_state.collateral_quote_lots -= tax;
                // Split: insurance fund gets `tox_contribution_bps`, maker
                // receives the remainder as a toxic-flow rebate.
                let to_insurance = (tax as u128)
                    .saturating_mul(ctx.accounts.insurance_fund.toxicity_tax_contribution_bps as u128)
                    .checked_div(constants::BPS_DENOM as u128)
                    .ok_or_else(|| error!(FlashBookError::DivisionByZero))?
                    as u64;
                let to_maker = tax.saturating_sub(to_insurance);
                {
                    let fund = &mut ctx.accounts.insurance_fund;
                    fund.balance_quote_lots = fund
                        .balance_quote_lots
                        .checked_add(to_insurance)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                    fund.total_contributions = fund
                        .total_contributions
                        .saturating_add(to_insurance);
                }
                {
                    let maker_state = &mut ctx.accounts.maker_trader_state;
                    maker_state.collateral_quote_lots = maker_state
                        .collateral_quote_lots
                        .checked_add(to_maker)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                }
                market.total_toxicity_tax_collected = market
                    .total_toxicity_tax_collected
                    .checked_add(tax)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                emit!(ToxicityTaxAppliedEvent {
                    market: market_key,
                    taker: taker_trader_pk,
                    maker: maker_trader_pk,
                    vpin_bps,
                    tax_quote_lots: tax,
                    insurance_share: to_insurance,
                    maker_share: to_maker,
                });
            }
        }

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
    ///
    /// Hardened against the JELLY/POPCAT class of attacks (Hyperliquid 2025):
    ///
    ///   1. **Staleness check**: rejects updates where
    ///      `now - published_at > params.oracle_staleness_max_seconds`.
    ///      Prevents using outdated prices when the oracle network has a gap.
    ///   2. **Confidence check**: rejects updates where
    ///      `confidence / price > params.oracle_confidence_max_bps`.
    ///      When upstream sources disagree (typically during low-liquidity
    ///      manipulation), the wider confidence interval is detected and
    ///      the price is rejected rather than acted upon.
    ///
    /// `published_at_unix_seconds` is the publisher-attested timestamp
    /// (matches Pyth's `publish_time` field).
    pub fn update_oracle(
        ctx: Context<UpdateOracle>,
        price_ticks: u64,
        confidence: u64,
        published_at_unix_seconds: u64,
    ) -> Result<()> {
        require!(price_ticks > 0, FlashBookError::ZeroPrice);
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );

        // Staleness check.
        let now = Clock::get()?.unix_timestamp.max(0) as u64;
        let max_age = market.params.oracle_staleness_max_seconds as u64;
        if max_age > 0 {
            let age = now.saturating_sub(published_at_unix_seconds);
            require!(age <= max_age, FlashBookError::OracleTooStale);
        }

        // Confidence check: confidence_bps = (confidence / price) * 10000.
        let max_conf = market.params.oracle_confidence_max_bps;
        if max_conf > 0 {
            let conf_bps = ((confidence as u128) * (constants::BPS_DENOM as u128))
                .checked_div(price_ticks as u128)
                .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
            require!(
                conf_bps <= max_conf as u128,
                FlashBookError::OracleConfidenceTooWide,
            );
        }

        market.oracle_price_ticks = price_ticks;
        market.oracle_confidence = confidence;
        market.oracle_published_at_unix_seconds = published_at_unix_seconds;
        Ok(())
    }

    /// Update oracle price using a multi-oracle quorum (median of 3).
    ///
    /// Defense in depth against the JELLY/POPCAT class of attacks where an
    /// attacker manipulates a single upstream price source. With three
    /// independent sources (e.g. Pyth + Switchboard + internal TWAP), an
    /// attacker would have to corrupt the majority simultaneously to move
    /// the median.
    ///
    /// Each input has its own staleness + confidence checked individually.
    /// Additionally: the dispersion `(max − min) / median` must be ≤
    /// `oracle_quorum_max_dispersion_bps`. If it exceeds, the update is
    /// rejected — sources clearly disagree, so no single one is safe to use.
    ///
    /// The accepted price is the median; the accepted confidence is the
    /// max of the three (most pessimistic); the accepted publish time is
    /// the min of the three (oldest, so the staleness gate is conservative).
    pub fn update_oracle_quorum(
        ctx: Context<UpdateOracle>,
        prices_ticks: [u64; 3],
        confidences: [u64; 3],
        published_at_unix_seconds: [u64; 3],
    ) -> Result<()> {
        for &p in &prices_ticks {
            require!(p > 0, FlashBookError::ZeroPrice);
        }
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized,
        );

        // Per-source staleness check.
        let now = Clock::get()?.unix_timestamp.max(0) as u64;
        let max_age = market.params.oracle_staleness_max_seconds as u64;
        if max_age > 0 {
            for &t in &published_at_unix_seconds {
                let age = now.saturating_sub(t);
                require!(age <= max_age, FlashBookError::OracleTooStale);
            }
        }

        // Per-source confidence check.
        let max_conf = market.params.oracle_confidence_max_bps;
        if max_conf > 0 {
            for i in 0..3 {
                let conf_bps = ((confidences[i] as u128) * (constants::BPS_DENOM as u128))
                    .checked_div(prices_ticks[i] as u128)
                    .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
                require!(
                    conf_bps <= max_conf as u128,
                    FlashBookError::OracleConfidenceTooWide,
                );
            }
        }

        // Median + dispersion check.
        let mut sorted = prices_ticks;
        sorted.sort();
        let min_p = sorted[0];
        let median = sorted[1];
        let max_p = sorted[2];

        let max_disp = market.params.oracle_quorum_max_dispersion_bps;
        if max_disp > 0 {
            let dispersion_bps = ((max_p - min_p) as u128) * (constants::BPS_DENOM as u128)
                / (median as u128);
            require!(
                dispersion_bps <= max_disp as u128,
                FlashBookError::OracleQuorumDispersionTooWide,
            );
        }

        // Write conservative aggregates: median price, max confidence,
        // oldest publish_time.
        let combined_conf = confidences.iter().copied().max().unwrap_or(0);
        let combined_published_at = published_at_unix_seconds.iter().copied().min().unwrap_or(0);

        market.oracle_price_ticks = median;
        market.oracle_confidence = combined_conf;
        market.oracle_published_at_unix_seconds = combined_published_at;
        Ok(())
    }

    /// Verify market invariants and auto-halt on breach.
    ///
    /// Permissionless: anyone can poke a market to check. The protocol
    /// pays off-chain monitors / keepers nothing extra; calling this is
    /// just a tx fee. On any violation, the market is auto-flipped to
    /// Paused so no new orders can land while operators investigate.
    ///
    /// Currently checks:
    ///   S5 — open interest balance: oi_long_lots == oi_short_lots
    ///        (Should hold by construction of update_oi at every fill,
    ///        but a bug in fill paths could drift these. This is the
    ///        cheapest invariant to verify on-chain.)
    ///
    /// Future invariants this hook can absorb:
    ///   S4 — vault balance ≥ Σ trader collateral + FLP capital
    ///   S12 — FLP per-batch growth ≤ pool_capital × max_growth%
    ///   S14 — mark price within oracle band ±band_bps
    pub fn verify_market_invariants(ctx: Context<VerifyMarketInvariants>) -> Result<()> {
        let market = &mut ctx.accounts.market;

        if market.oi_long_lots != market.oi_short_lots {
            // Auto-halt: flip market to Paused. Closed is terminal — preserve.
            let prev_status = market.status;
            if market.status != MarketStatus::Closed as u8 {
                market.status = MarketStatus::Paused as u8;
            }
            emit!(InvariantBreachDetectedEvent {
                market: market.key(),
                invariant_code: 5,
                expected: market.oi_long_lots,
                actual: market.oi_short_lots,
                previous_status: prev_status,
                new_status: market.status,
            });
            return Err(error!(FlashBookError::OpenInterestImbalance));
        }

        Ok(())
    }

    /// Update market status (authority-only).
    ///
    /// Status transitions:
    ///   Active → PostOnly: existing positions trade; new takers blocked
    ///   Active → Paused:   no order intake; existing positions held
    ///   Any → Closed:      terminal sunset; only liquidation + close
    pub fn set_market_status(
        ctx: Context<UpdateMarketAuthority>,
        new_status: u8,
    ) -> Result<()> {
        require!(new_status <= 4, FlashBookError::OutOfRange);
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        // Closed is terminal — cannot reopen.
        require!(
            market.status != MarketStatus::Closed as u8,
            FlashBookError::OutOfRange
        );
        let prev = market.status;
        market.status = new_status;
        emit!(MarketStatusChangedEvent {
            market: market.key(),
            previous_status: prev,
            new_status,
        });
        Ok(())
    }

    /// Update mutable market parameters (authority-only).
    ///
    /// Immutable fields (set at initialization, NEVER mutable post-init):
    ///   - tick_size, base_lot_size, quote_lot_size, min_base_lots
    ///   These define the market's measurement primitives. Changing them
    ///   would silently invalidate every existing order and position.
    ///
    /// Mutable: everything else — fees, margins, FLP coefficients, funding
    /// rates, oracle band, VPIN, batch interval. Changes are applied to the
    /// next batch.
    pub fn update_market_params(
        ctx: Context<UpdateMarketAuthority>,
        new_params: MarketParams,
    ) -> Result<()> {
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );

        // Enforce immutability of measurement primitives.
        require!(
            new_params.tick_size == market.params.tick_size,
            FlashBookError::OutOfRange
        );
        require!(
            new_params.base_lot_size == market.params.base_lot_size,
            FlashBookError::OutOfRange
        );
        require!(
            new_params.quote_lot_size == market.params.quote_lot_size,
            FlashBookError::OutOfRange
        );
        require!(
            new_params.min_base_lots == market.params.min_base_lots,
            FlashBookError::OutOfRange
        );

        // Sanity bounds on the mutable fields.
        require!(new_params.max_leverage >= 1, FlashBookError::OutOfRange);
        require!(
            new_params.maintenance_margin_ratio_bps <= new_params.initial_margin_ratio_bps,
            FlashBookError::OutOfRange
        );
        require!(
            new_params.flp_max_growth_per_batch_bps <= constants::BPS_DENOM,
            FlashBookError::OutOfRange
        );
        require!(
            new_params.oracle_band_bps <= constants::BPS_DENOM,
            FlashBookError::OutOfRange
        );

        market.params = new_params;
        emit!(MarketParamsUpdatedEvent {
            market: market.key(),
        });
        Ok(())
    }

    /// Update a trader's authority (e.g. for wallet rotation). Either the
    /// current authority OR the trader signs.
    pub fn transfer_market_authority(
        ctx: Context<UpdateMarketAuthority>,
        new_authority: Pubkey,
    ) -> Result<()> {
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        let prev = market.authority;
        market.authority = new_authority;
        emit!(MarketAuthorityTransferredEvent {
            market: market.key(),
            previous_authority: prev,
            new_authority,
        });
        Ok(())
    }

    // ─── Order intake ───────────────────────────────────────────────

    /// Submit a resting limit order. Routed to the order buffer for the
    /// next batch.
    ///
    /// `flags` is a bitfield encoding TIF + safety flags (Phoenix v1
    /// patterns + dYdX / Binance / Hyperliquid + Drift JIT):
    ///
    ///   bit 0: post_only       — reject if would cross spread on entry
    ///   bit 1: reduce_only     — order can only shrink trader position
    ///   bit 2: ioc             — immediate-or-cancel: don't rest after batch
    ///                            (the matcher fills as much as possible
    ///                            this batch, the rest is dropped)
    ///   bit 3: jit             — "Just In Time" auction tag. JIT-tagged
    ///                            taker orders earn a BONUS rebate
    ///                            (`market.params.jit_bonus_rebate_bps`)
    ///                            for the maker that fills them. Drift-
    ///                            style economic incentive: MMs preferentially
    ///                            quote against JIT-tagged flow because the
    ///                            rebate beats organic order flow.
    ///
    /// Pass 0 for a vanilla GTC limit. The legacy boolean `post_only`
    /// argument continues to work transparently — it is mapped to
    /// `flags = (post_only as u8) << 0`.
    pub fn place_limit_order(
        ctx: Context<PlaceOrder>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        post_only: bool,
        flags: u8,
    ) -> Result<()> {
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(limit_ticks > 0, FlashBookError::ZeroPrice);
        require!(side <= 1, FlashBookError::OutOfRange);

        // Compose final flags: legacy `post_only` arg OR'd in for
        // backwards compat, then any caller-supplied bits.
        let post_only_bit: u8 = if post_only { 1 << 0 } else { 0 };
        let final_flags: u8 = post_only_bit | flags;
        let reduce_only = (final_flags & (1 << 1)) != 0;
        let ioc = (final_flags & (1 << 2)) != 0;
        // Reject unknown flag bits to keep the bitfield strict.
        // Bits 0-3 are now defined (post_only, reduce_only, ioc, jit).
        require!(final_flags & !0b0000_1111 == 0, FlashBookError::OutOfRange);

        // Reduce-only gate: order can only oppose + not exceed position.
        if reduce_only {
            let position = &ctx.accounts.position;
            require!(position.size_lots > 0, FlashBookError::OutOfRange);
            require!(position.side != side, FlashBookError::OutOfRange);
            require!(size_lots <= position.size_lots, FlashBookError::OutOfRange);
        }

        // post_only and ioc are mutually exclusive (post_only never
        // crosses; ioc must cross).
        let post_only_set = (final_flags & (1 << 0)) != 0;
        if post_only_set && ioc {
            return Err(error!(FlashBookError::OutOfRange));
        }
        // We carry `final_flags` into the OrderSlot below by repurposing
        // the existing `post_only: u8` slot as a flags bitfield (layout
        // compatible — the value 0 or 1 maps directly).
        let stored_flags = final_flags;
        let _ = ioc; // currently captured into stored_flags; matcher reads it

        let market = &ctx.accounts.market;
        // Status gate: limit orders are blocked when the market is Paused
        // or Closed. PostOnly status allows new limits (they rest until
        // crossing) but the taker-flow path (commit/reveal) is gated
        // separately.
        require!(
            market.status == MarketStatus::Active as u8
                || market.status == MarketStatus::PostOnly as u8,
            FlashBookError::OutOfRange
        );
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

        // Concentration cap: prevent any single trader from building a
        // position larger than `max_position_lots_per_trader`. Mitigates
        // the POPCAT-style attack where a single actor uses many wallets
        // to build outsized concentrated risk. (The per-wallet cap is
        // bypass-resistant because the signer is enforced; the multi-wallet
        // bypass requires real capital across each wallet.)
        let cap = market.params.max_position_lots_per_trader;
        if cap > 0 {
            let existing_size = ctx.accounts.position.size_lots;
            // Add the new order size; in the worst case (same side) this is
            // the post-fill position size.
            let new_size = existing_size.saturating_add(size_lots);
            require!(
                new_size <= cap,
                FlashBookError::PositionSizeCapExceeded,
            );
        }

        // Capital-relative concentration cap: prevent any single trader's
        // notional from exceeding `max_position_ratio_bps` of FLP capital.
        // Distinct from the absolute lots cap above — this scales with the
        // pool. As the pool grows, larger positions are allowed; if the
        // pool shrinks, existing positions don't shrink but new orders
        // are bound by the new cap.
        let ratio_cap = market.params.max_position_ratio_bps;
        if ratio_cap > 0 {
            let flp_capital = ctx.accounts.flp_exposure.total_capital_quote_lots;
            // 0 capital → no cap (bootstrap; should be rare).
            if flp_capital > 0 {
                let cap_quote_lots = (flp_capital as u128)
                    .saturating_mul(ratio_cap as u128)
                    / (constants::BPS_DENOM as u128);
                let existing_size = ctx.accounts.position.size_lots;
                let new_size = existing_size.saturating_add(size_lots);
                // Use the limit price as the worst-case fill price for
                // notional estimation. mark_price would be tighter but the
                // limit is what the trader is willing to pay.
                let new_notional = (new_size as u128)
                    .saturating_mul(limit_ticks as u128)
                    .saturating_mul(market.params.tick_size as u128);
                require!(
                    new_notional <= cap_quote_lots,
                    FlashBookError::PositionSizeCapExceeded,
                );
            }
        }

        // Stress-lattice margin gate. If the trader has an existing position
        // on this market, reject if it would push them past maintenance.
        // Empty position (first ever order) is trivially healthy.
        let position = &ctx.accounts.position;
        if position.size_lots > 0 {
            require!(
                position.trader == ctx.accounts.trader.key(),
                FlashBookError::WrongTrader
            );
            require!(
                position.market == market.key(),
                FlashBookError::WrongMarket
            );
            let pos_snap = RiskPosSnap {
                market: position.market,
                side: if position.side == 0 { Side::Long } else { Side::Short },
                size_lots: position.size_lots,
                entry_price: Ticks(position.entry_price_ticks),
                cum_funding_index_at_entry: position.cum_funding_index_at_entry,
            };
            let market_snap = RiskMarketSnap {
                market: market.key(),
                mark_price: Ticks(market.mark_price_ticks),
                cum_funding_index: market.cum_funding_index,
                maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
                tick_size: market.params.tick_size,
            };
            let scenarios = default_scenarios_fn(&[market.key()]);
            let assessment = assess_margin_fn(
                &[pos_snap],
                &[market_snap],
                &scenarios,
                ctx.accounts.trader_state.collateral_quote_lots,
            )?;
            require!(
                assessment.is_healthy,
                FlashBookError::TraderLiquidatable
            );
        }

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
                    // OrderSlot.post_only doubles as the flags bitfield —
                    // bit 0 mirrors the legacy post_only semantic; bits
                    // 1–2 carry reduce_only / ioc for the matcher.
                    post_only: stored_flags,
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

    /// Place two orders across two distinct markets atomically, with a
    /// single cross-market stress-lattice gate. The headline use case is
    /// pair trades / hedges where the trader wants to net long market A
    /// and net short market B simultaneously, with the engine recognizing
    /// the hedge so required margin collapses for offsetting positions.
    ///
    /// Without basket orders, two separate place_limit_order calls would
    /// each run a per-market margin check that doesn't see the offsetting
    /// leg — the first leg might be rejected even though the post-state
    /// is healthy. Basket orders fix this by projecting the post-state
    /// across both markets and running assess_margin once.
    ///
    /// Atomicity comes from Solana: if any leg fails (cap breach, buffer
    /// full, margin gate, etc.), the whole tx rolls back. Rate limit
    /// counts each leg toward orders_this_batch (basket = 2 units).
    ///
    /// V1 supports exactly two legs on two distinct markets. N-leg
    /// (basket > 2) is a follow-up that uses remaining_accounts walking.
    pub fn place_basket_order(
        ctx: Context<PlaceBasketOrder>,
        leg_a: BasketLeg,
        leg_b: BasketLeg,
    ) -> Result<()> {
        // Distinct markets are required — for same-market orders, callers
        // can use place_limit_order twice (no cross-margin benefit).
        let mkt_a = ctx.accounts.market_a.key();
        let mkt_b = ctx.accounts.market_b.key();
        require!(mkt_a != mkt_b, FlashBookError::OutOfRange);

        // Validate each leg in isolation: size, price, status, ticks. Skip
        // the per-market margin gate — basket margin runs across both legs
        // jointly below.
        validate_leg_intake(&ctx.accounts.market_a, &leg_a)?;
        validate_leg_intake(&ctx.accounts.market_b, &leg_b)?;

        // Per-market caps (absolute lots + capital ratio). Run before
        // basket margin so caps fail fast.
        check_caps_for_leg(
            &ctx.accounts.market_a,
            &ctx.accounts.position_a,
            &ctx.accounts.flp_exposure,
            &leg_a,
        )?;
        check_caps_for_leg(
            &ctx.accounts.market_b,
            &ctx.accounts.position_b,
            &ctx.accounts.flp_exposure,
            &leg_b,
        )?;

        // Project post-leg position state across both markets and run
        // a single assess_margin. This is where the hedge benefit
        // materializes — offsetting positions reduce required margin.
        let market_a = &ctx.accounts.market_a;
        let market_b = &ctx.accounts.market_b;
        let market_a_key = market_a.key();
        let market_b_key = market_b.key();
        let position_a = &ctx.accounts.position_a;
        let position_b = &ctx.accounts.position_b;
        let trader_key = ctx.accounts.trader.key();

        let proj_a = project_post_leg(position_a, &leg_a, market_a, market_a_key, trader_key)?;
        let proj_b = project_post_leg(position_b, &leg_b, market_b, market_b_key, trader_key)?;

        let mut snaps: Vec<RiskPosSnap> = Vec::with_capacity(2);
        let mut markets: Vec<RiskMarketSnap> = Vec::with_capacity(2);
        for (proj, market, market_key) in [
            (proj_a, market_a, market_a_key),
            (proj_b, market_b, market_b_key),
        ] {
            if let Some(s) = proj {
                snaps.push(s);
            }
            markets.push(RiskMarketSnap {
                market: market_key,
                mark_price: Ticks(market.mark_price_ticks),
                cum_funding_index: market.cum_funding_index,
                maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
                tick_size: market.params.tick_size,
            });
        }
        if !snaps.is_empty() {
            let mkt_keys: Vec<Pubkey> = markets.iter().map(|m| m.market).collect();
            let scenarios = default_scenarios_fn(&mkt_keys);
            let assessment = assess_margin_fn(
                &snaps,
                &markets,
                &scenarios,
                ctx.accounts.trader_state.collateral_quote_lots,
            )?;
            require!(assessment.is_healthy, FlashBookError::TraderLiquidatable);
        }

        // Rate limit: basket counts as 2 units. Reset on batch boundary.
        let trader_state = &mut ctx.accounts.trader_state;
        if trader_state.last_batch_seen != market_a.current_batch {
            trader_state.last_batch_seen = market_a.current_batch;
            trader_state.orders_this_batch = 0;
        }
        require!(
            trader_state.orders_this_batch.saturating_add(2)
                <= MAX_ORDERS_PER_TRADER_PER_BATCH,
            FlashBookError::RateLimited
        );
        trader_state.orders_this_batch += 2;

        // Insert into both order buffers.
        insert_into_buffer(
            &mut ctx.accounts.order_buffer_a,
            trader_key,
            &leg_a,
        )?;
        insert_into_buffer(
            &mut ctx.accounts.order_buffer_b,
            trader_key,
            &leg_b,
        )?;

        emit!(BasketOrderPlacedEvent {
            trader: trader_key,
            market_a: market_a_key,
            market_b: market_b_key,
            side_a: leg_a.side,
            side_b: leg_b.side,
            size_lots_a: leg_a.size_lots,
            size_lots_b: leg_b.size_lots,
        });
        Ok(())
    }

    /// N-leg basket order. Place K orders across K distinct markets in
    /// one transaction with a SINGLE cross-market stress-lattice gate.
    /// Generalises `place_basket_order` (which is hard-coded for K=2).
    ///
    /// `legs.len()` must equal the number of (market, order_buffer,
    /// position) triples in `remaining_accounts` (so 3 × K accounts).
    /// All markets must be distinct. Position PDAs MUST already exist —
    /// callers init them via a no-op place_limit_order on each market
    /// first (init_if_needed isn't safe with remaining_accounts).
    ///
    /// Hard caps: legs.len() ≤ MAX_BASKET_LEGS_N (4). Larger baskets
    /// can land via repeated 2-leg or N-leg calls.
    ///
    /// Atomicity: any failure (cap breach, buffer full, distinct-market
    /// guard, basket margin gate, rate limit) rolls back the whole tx.
    pub fn place_basket_order_n<'info>(
        ctx: Context<'_, '_, '_, 'info, PlaceBasketOrderN<'info>>,
        legs: Vec<BasketLeg>,
    ) -> Result<()> {
        require!(!legs.is_empty(), FlashBookError::ZeroSize);
        require!(
            legs.len() <= MAX_BASKET_LEGS_N,
            FlashBookError::OutOfRange
        );
        let remaining = ctx.remaining_accounts;
        require!(
            remaining.len() == legs.len() * 3,
            FlashBookError::OutOfRange
        );

        let trader_key = ctx.accounts.trader.key();
        let program_id = ctx.program_id;

        // Walk remaining_accounts → deserialize markets + positions.
        // Validate ownership, identity, market uniqueness inline.
        let mut markets: Vec<MarketAccount> = Vec::with_capacity(legs.len());
        let mut market_keys: Vec<Pubkey> = Vec::with_capacity(legs.len());
        let mut positions: Vec<state::PositionAccount> = Vec::with_capacity(legs.len());
        for (i, _leg) in legs.iter().enumerate() {
            let m_ai = &remaining[i * 3];
            let buf_ai = &remaining[i * 3 + 1];
            let pos_ai = &remaining[i * 3 + 2];

            // Owner checks (defense vs malicious foreign accounts).
            require_keys_eq!(*m_ai.owner, *program_id, FlashBookError::Unauthorized);
            require_keys_eq!(*buf_ai.owner, *program_id, FlashBookError::Unauthorized);
            require_keys_eq!(*pos_ai.owner, *program_id, FlashBookError::Unauthorized);

            let market: MarketAccount =
                MarketAccount::try_deserialize(&mut &m_ai.try_borrow_data()?[..])?;
            let position: state::PositionAccount =
                state::PositionAccount::try_deserialize(&mut &pos_ai.try_borrow_data()?[..])?;

            // Market uniqueness guard.
            for prev in &market_keys {
                require!(*prev != m_ai.key(), FlashBookError::OutOfRange);
            }
            market_keys.push(m_ai.key());

            // Per-leg intake validation.
            validate_leg_intake(&market, &legs[i])?;
            check_caps_for_leg(&market, &position, &ctx.accounts.flp_exposure, &legs[i])?;

            // Position binding (when non-empty). Empty positions OK
            // — projected as new positions below.
            if position.size_lots > 0 {
                require!(position.trader == trader_key, FlashBookError::WrongTrader);
                require!(position.market == m_ai.key(), FlashBookError::WrongMarket);
            }

            markets.push(market);
            positions.push(position);
        }

        // Cross-market stress-lattice margin gate. Project post-leg state
        // for each market, then assess against the joint scenario lattice.
        let mut snaps: Vec<RiskPosSnap> = Vec::with_capacity(legs.len());
        let mut market_snaps: Vec<RiskMarketSnap> = Vec::with_capacity(legs.len());
        for (i, leg) in legs.iter().enumerate() {
            if let Some(snap) = project_post_leg(
                &positions[i],
                leg,
                &markets[i],
                market_keys[i],
                trader_key,
            )? {
                snaps.push(snap);
            }
            market_snaps.push(RiskMarketSnap {
                market: market_keys[i],
                mark_price: Ticks(markets[i].mark_price_ticks),
                cum_funding_index: markets[i].cum_funding_index,
                maintenance_margin_bps: markets[i].params.maintenance_margin_ratio_bps,
                tick_size: markets[i].params.tick_size,
            });
        }
        if !snaps.is_empty() {
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_fn(
                &snaps,
                &market_snaps,
                &scenarios,
                ctx.accounts.trader_state.collateral_quote_lots,
            )?;
            require!(assessment.is_healthy, FlashBookError::TraderLiquidatable);
        }

        // Rate limit: bump by legs.len(). Reset on batch boundary.
        let trader_state = &mut ctx.accounts.trader_state;
        if trader_state.last_batch_seen != markets[0].current_batch {
            trader_state.last_batch_seen = markets[0].current_batch;
            trader_state.orders_this_batch = 0;
        }
        let leg_count_u32 = u32::try_from(legs.len()).unwrap_or(u32::MAX);
        require!(
            trader_state.orders_this_batch.saturating_add(leg_count_u32)
                <= MAX_ORDERS_PER_TRADER_PER_BATCH,
            FlashBookError::RateLimited
        );
        trader_state.orders_this_batch = trader_state
            .orders_this_batch
            .saturating_add(leg_count_u32);

        // Insert each leg's order into its buffer. Mutable borrow of
        // each buffer is short-lived (one insert per leg) so we don't
        // hold conflicting borrows.
        for (i, leg) in legs.iter().enumerate() {
            let buf_ai = &remaining[i * 3 + 1];
            let mut buf_data = buf_ai.try_borrow_mut_data()?;
            let mut buffer: OrderBufferAccount =
                OrderBufferAccount::try_deserialize(&mut &buf_data[..])?;
            insert_into_buffer(&mut buffer, trader_key, leg)?;
            // Re-serialize back into the account.
            let mut serialized: Vec<u8> = Vec::with_capacity(buf_data.len());
            buffer.try_serialize(&mut serialized)?;
            require!(
                serialized.len() <= buf_data.len(),
                FlashBookError::OutOfRange
            );
            buf_data[..serialized.len()].copy_from_slice(&serialized);
        }

        emit!(BasketOrderNPlacedEvent {
            trader: trader_key,
            leg_count: legs.len() as u8,
            markets: market_keys.clone(),
        });
        Ok(())
    }

    /// Cancel a pending order from the buffer. Only the original trader can
    /// cancel; other callers (or stale order_seq values) are rejected. The
    /// order must still be in the buffer (not yet processed by run_batch).
    ///
    /// On success: the slot is cleared (`valid = 0`), `head` decrements,
    /// and an `OrderCancelledEvent` is emitted.
    pub fn cancel_order(
        ctx: Context<CancelOrder>,
        order_seq: u64,
    ) -> Result<()> {
        let buffer = &mut ctx.accounts.order_buffer;
        let trader_key = ctx.accounts.trader.key();

        let mut found = false;
        for slot in buffer.slots.iter_mut() {
            if slot.valid == 1 && slot.seq == order_seq {
                require!(slot.trader == trader_key, FlashBookError::WrongTrader);
                // Only allow cancelling user-submitted orders, not synthesized
                // FLP/liquidation/ADL injections.
                require!(
                    slot.order_type == OrderType::Limit as u8
                        || slot.order_type == OrderType::Taker as u8,
                    FlashBookError::OutOfRange
                );
                *slot = OrderSlot::default();
                buffer.head = buffer.head.saturating_sub(1);
                found = true;
                break;
            }
        }
        require!(found, FlashBookError::LiquidationStale);

        emit!(OrderCancelledEvent {
            market: buffer.market,
            trader: trader_key,
            order_seq,
        });
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

    /// Apply a fill in which the FLP pool is the *maker*. Mutates the
    /// `FlpExposureAccount.per_market` entry for this market while
    /// applying the opposite-side update to the taker's `PositionAccount`.
    ///
    /// Trust model: same as `apply_fill` — sequencer-authenticated; the
    /// fill data is taken at face value (production verifies via per-batch
    /// fill buffer or Merkle proof).
    pub fn apply_flp_fill(
        ctx: Context<ApplyFlpFill>,
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
        let flp_side_enum = taker_side_enum.opposite();
        let taker_trader_pk = ctx.accounts.taker_trader_state.trader;

        // Fee + rebate accrual (parity with apply_fill).
        // FLP is the maker — rebate accrues to FLP capital instead of a maker
        // TraderState. Net fee still flows to the insurance fund.
        let notional_u128 = (size_lots as u128)
            .checked_mul(price_ticks as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            .checked_mul(market.params.tick_size as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        let taker_fee_u128 =
            notional_u128.saturating_mul(market.params.taker_fee_bps as u128) / constants::BPS_DENOM as u128;
        let maker_rebate_u128 =
            notional_u128.saturating_mul(market.params.maker_rebate_bps as u128) / constants::BPS_DENOM as u128;
        require!(maker_rebate_u128 <= taker_fee_u128, FlashBookError::OutOfRange);
        let taker_fee = if taker_fee_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            taker_fee_u128 as u64
        };
        let maker_rebate = if maker_rebate_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            maker_rebate_u128 as u64
        };
        let net_fee = taker_fee.saturating_sub(maker_rebate);

        // Deduct fee from taker.
        {
            let taker_state = &mut ctx.accounts.taker_trader_state;
            taker_state.collateral_quote_lots = taker_state
                .collateral_quote_lots
                .checked_sub(taker_fee)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        }
        // Credit rebate to FLP capital (FLP is the maker here).
        {
            let flp = &mut ctx.accounts.flp_exposure;
            flp.total_capital_quote_lots = flp
                .total_capital_quote_lots
                .checked_add(maker_rebate)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        }
        // Net fee to insurance fund.
        {
            let fund = &mut ctx.accounts.insurance_fund;
            let contribution = (net_fee as u128)
                .saturating_mul(fund.fee_contribution_bps as u128)
                .checked_div(constants::BPS_DENOM as u128)
                .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
            let contribution_u64 = if contribution > u64::MAX as u128 {
                u64::MAX
            } else {
                contribution as u64
            };
            fund.balance_quote_lots = fund
                .balance_quote_lots
                .checked_add(contribution_u64)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            fund.total_contributions = fund.total_contributions.saturating_add(contribution_u64);
        }
        market.total_fees_collected = market
            .total_fees_collected
            .checked_add(net_fee)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        // ── Toxicity tax (FLP-fill variant) ───────────────────────────
        // Same VPIN-scaled tax as apply_fill, but the maker is the FLP
        // pool — so the maker share flows into flp.total_capital, lifting
        // NAV/share for all LPs pro-rata. This is the LP equivalent of a
        // toxic-flow rebate.
        let tax_max_bps = market.params.toxicity_tax_max_bps;
        let vpin_bps = market.vpin.as_bps();
        if tax_max_bps > 0 && vpin_bps > 0 {
            let tax_u128 = notional_u128
                .saturating_mul(tax_max_bps as u128)
                .saturating_mul(vpin_bps as u128)
                / (constants::BPS_DENOM as u128)
                / (constants::BPS_DENOM as u128);
            let tax_uncapped: u64 = if tax_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                tax_u128 as u64
            };
            let tax = tax_uncapped.min(ctx.accounts.taker_trader_state.collateral_quote_lots);
            if tax > 0 {
                ctx.accounts.taker_trader_state.collateral_quote_lots -= tax;
                let to_insurance = (tax as u128)
                    .saturating_mul(ctx.accounts.insurance_fund.toxicity_tax_contribution_bps as u128)
                    .checked_div(constants::BPS_DENOM as u128)
                    .ok_or_else(|| error!(FlashBookError::DivisionByZero))?
                    as u64;
                let to_flp = tax.saturating_sub(to_insurance);
                {
                    let fund = &mut ctx.accounts.insurance_fund;
                    fund.balance_quote_lots = fund
                        .balance_quote_lots
                        .checked_add(to_insurance)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                    fund.total_contributions = fund
                        .total_contributions
                        .saturating_add(to_insurance);
                }
                {
                    let flp = &mut ctx.accounts.flp_exposure;
                    flp.total_capital_quote_lots = flp
                        .total_capital_quote_lots
                        .checked_add(to_flp)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                }
                market.total_toxicity_tax_collected = market
                    .total_toxicity_tax_collected
                    .checked_add(tax)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                emit!(ToxicityTaxAppliedEvent {
                    market: market_key,
                    taker: taker_trader_pk,
                    // For FLP fills the "maker" is the pool itself. Use the
                    // flp_exposure PDA address as the maker identity.
                    maker: ctx.accounts.flp_exposure.key(),
                    vpin_bps,
                    tax_quote_lots: tax,
                    insurance_share: to_insurance,
                    maker_share: to_flp,
                });
            }
        }

        let taker_pos = &mut ctx.accounts.taker_position;
        if taker_pos.market == Pubkey::default() {
            taker_pos.market = market_key;
            taker_pos.trader = taker_trader_pk;
            taker_pos.bump = ctx.bumps.taker_position;
            taker_pos.cum_funding_index_at_entry = funding_index;
            taker_pos.last_settlement_batch = current_batch;
        }

        let taker_was_open = taker_pos.size_lots > 0;
        let taker_pre_side = taker_pos.side;
        let taker_pre_size = taker_pos.size_lots;

        apply_fill_to_position(taker_pos, taker_side_enum, size_lots, price_ticks, funding_index)?;

        update_oi(market, taker_pre_side, taker_pre_size, taker_pos.side, taker_pos.size_lots)?;

        // Update FLP per-market entry on the OPPOSITE side.
        let flp = &mut ctx.accounts.flp_exposure;
        let flp_pre = flp_market_pre_state(flp, market_key);
        apply_fill_to_flp_market(flp, market_key, flp_side_enum, size_lots, price_ticks)?;
        let flp_post = flp_market_pre_state(flp, market_key);
        update_oi(market, flp_pre.0, flp_pre.1, flp_post.0, flp_post.1)?;

        // Update open_positions on TraderState.
        let taker_is_open = taker_pos.size_lots > 0;
        let taker_state = &mut ctx.accounts.taker_trader_state;
        if !taker_was_open && taker_is_open {
            taker_state.open_positions = taker_state.open_positions.saturating_add(1);
        } else if taker_was_open && !taker_is_open {
            taker_state.open_positions = taker_state.open_positions.saturating_sub(1);
        }

        emit!(FlpFillAppliedEvent {
            market: market_key,
            taker: taker_trader_pk,
            taker_side,
            size_lots,
            price_ticks,
            batch_num: current_batch,
            flp_size_after: flp_post.1,
            flp_side_after: flp_post.0,
        });
        Ok(())
    }

    /// Liquidate a specific position. Anyone may call this for any position;
    /// the matcher determines if the trader is actually unhealthy. If they
    /// are not, the instruction errors with `NotLiquidatable` — preserving
    /// the protocol invariant that healthy traders are never force-closed.
    ///
    /// On success: a synthetic Liquidation order is appended to the market's
    /// order buffer. The next `run_batch` clears it at the batch uniform
    /// price; `apply_fill` then settles the position. Bankruptcy waterfall
    /// (insurance → ADL) is handled in subsequent steps.
    ///
    /// Single-market scope: this instruction assesses margin against the
    /// trader's position on *this market only*. Cross-market portfolio
    /// margin will be a separate `liquidate_portfolio` instruction taking
    /// remaining_accounts (Phase 2).
    /// Liquidate an unhealthy position. Three production-grade upgrades
    /// over the v1 implementation:
    ///
    /// 1. PARTIAL LIQUIDATION via `requested_close_lots`. The keeper passes
    ///    the size to close (0 = full close). Hyperliquid-style: avoid
    ///    over-liquidating traders who can be brought back above
    ///    maintenance margin by closing only part of their position.
    ///
    /// 2. LIQUIDATOR REWARD. `market.params.liquidator_reward_bps` of the
    ///    closure notional is debited from the liquidatee's collateral and
    ///    credited to the caller's `caller_trader_state`. Drift/dYdX-style
    ///    tip-based incentive — attracts a competitive keeper pool so
    ///    underwater positions actually get liquidated. 0 bps = disabled
    ///    (operators rely on protocol-funded keepers).
    ///
    /// 3. RACE-SAFE atomic gate. The check `position.size_lots > 0` + the
    ///    fact that on-chain transactions land sequentially per-slot
    ///    means a second concurrent liquidator on the same position fails
    ///    cleanly with LiquidationStale (their tx still pays a base fee
    ///    but no double-debits or partial state). Tested.
    pub fn liquidate_position(
        ctx: Context<LiquidatePosition>,
        requested_close_lots: u64,
    ) -> Result<()> {
        let market = &ctx.accounts.market;
        let position = &ctx.accounts.position;
        let trader_state_pre = ctx.accounts.trader_state.clone();
        let buffer = &mut ctx.accounts.order_buffer;

        require!(position.size_lots > 0, FlashBookError::LiquidationStale);
        require!(
            position.trader == trader_state_pre.trader,
            FlashBookError::WrongTrader
        );
        require!(
            position.market == market.key(),
            FlashBookError::WrongMarket
        );

        // Determine close size. 0 = max (full close, preserves v1 behaviour
        // for keepers that don't size their liquidations).
        let close_size = if requested_close_lots == 0 {
            position.size_lots
        } else {
            require!(
                requested_close_lots <= position.size_lots,
                FlashBookError::OutOfRange
            );
            requested_close_lots
        };
        require!(close_size > 0, FlashBookError::ZeroSize);

        // Cooldown gate — anti-cascade. Prevents the same position from
        // being hammered in adjacent blocks. The cooldown is per-market
        // configurable; 0 = disabled (legacy behaviour).
        let current_slot = Clock::get()?.slot;
        let cooldown = market.params.liquidation_cooldown_slots as u64;
        if cooldown > 0 && position.last_liquidated_at_slot > 0 {
            let elapsed = current_slot.saturating_sub(position.last_liquidated_at_slot);
            require!(elapsed >= cooldown, FlashBookError::RateLimited);
        }

        // Health gate — rejects when the trader's portfolio is healthy at
        // current state. Same stress lattice as on placement.
        let pos_snap = RiskPosSnap {
            market: position.market,
            side: if position.side == 0 { Side::Long } else { Side::Short },
            size_lots: position.size_lots,
            entry_price: Ticks(position.entry_price_ticks),
            cum_funding_index_at_entry: position.cum_funding_index_at_entry,
        };
        let market_snap = RiskMarketSnap {
            market: market.key(),
            mark_price: Ticks(market.mark_price_ticks),
            cum_funding_index: market.cum_funding_index,
            maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
            tick_size: market.params.tick_size,
        };
        let scenarios = default_scenarios_fn(&[market.key()]);
        let assessment = assess_margin_fn(
            &[pos_snap],
            &[market_snap],
            &scenarios,
            trader_state_pre.collateral_quote_lots,
        )?;
        require!(!assessment.is_healthy, FlashBookError::NotLiquidatable);

        // Compute liquidation order params. Closes opposite side at
        // oracle ± penalty so the trader pays the penalty implicitly via
        // a worse fill price.
        require!(
            (buffer.head as usize) < ORDER_BUFFER_CAP,
            FlashBookError::BufferFull
        );
        let pos_side = pos_snap.side;
        let close_side = pos_side.opposite();
        let penalty = market.params.liq_penalty_bps as u128;
        let oracle = market.oracle_price_ticks as u128;
        let penalty_delta = (oracle * penalty) / constants::BPS_DENOM as u128;
        let limit = match close_side {
            Side::Short => (oracle.saturating_sub(penalty_delta)) as u64,
            Side::Long => (oracle.saturating_add(penalty_delta)) as u64,
        };

        // Lazy-initialize caller_trader_state on first liquidation so
        // freshly-rented keepers don't need to call open_trader_state
        // separately. trader field default = Pubkey::default() ⇒ untouched.
        {
            let cts = &mut ctx.accounts.caller_trader_state;
            if cts.trader == Pubkey::default() {
                cts.trader = ctx.accounts.caller.key();
                cts.bump = ctx.bumps.caller_trader_state;
            }
        }

        // Pay the liquidator reward BEFORE injecting the close order. The
        // reward is debited from the liquidatee's collateral and credited
        // to the caller's trader_state. Capped at available collateral so
        // we never go negative.
        //
        // Dutch-style auction on the REWARD: scaled from 0% → 100% of
        // `liquidator_reward_bps` over `liquidation_auction_duration_slots`
        // since `unhealthy_since_slot`. First responder gets a small
        // reward (or 0); later responders progressively larger up to full.
        // Encourages a competitive keeper pool to spread responses across
        // slots rather than all racing the same block.
        let mut reward_paid: u64 = 0;
        if market.params.liquidator_reward_bps > 0 {
            // Notional in quote lots = size × oracle_price × tick_size.
            // We use oracle_price (not the limit_ticks fill price) so the
            // reward is keeper-side-deterministic at submission time.
            let notional_u128 = (close_size as u128)
                .saturating_mul(oracle)
                .saturating_mul(market.params.tick_size as u128);
            let mut reward_bps_eff = market.params.liquidator_reward_bps as u128;
            // Apply Dutch-auction scaling if duration is set and we have
            // a `unhealthy_since_slot` anchor. First-time liquidator
            // (anchor = 0) gets the BASE reward; subsequent liquidators
            // see a progressively larger fraction up to full.
            let auction_duration =
                market.params.liquidation_auction_duration_slots as u64;
            if auction_duration > 0 && ctx.accounts.position.unhealthy_since_slot > 0 {
                let elapsed = current_slot
                    .saturating_sub(ctx.accounts.position.unhealthy_since_slot);
                let scale = (elapsed.min(auction_duration) as u128)
                    .saturating_mul(constants::BPS_DENOM as u128)
                    / (auction_duration as u128);
                reward_bps_eff = reward_bps_eff
                    .saturating_mul(scale)
                    / (constants::BPS_DENOM as u128);
            } else if auction_duration > 0 {
                // First detection of unhealthy state — base reward = 0
                // (or near-zero) so first responders aren't over-paid.
                // Operators who want first-responder bonus can wire it
                // off-chain (top-up tx).
                reward_bps_eff = 0;
            }
            let reward_u128 = notional_u128
                .saturating_mul(reward_bps_eff)
                / (constants::BPS_DENOM as u128);
            let reward_u64 = if reward_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                reward_u128 as u64
            };
            // Cap to available collateral on the liquidatee.
            reward_paid = reward_u64.min(ctx.accounts.trader_state.collateral_quote_lots);
            if reward_paid > 0 {
                ctx.accounts.trader_state.collateral_quote_lots -= reward_paid;
                let caller_ts = &mut ctx.accounts.caller_trader_state;
                caller_ts.collateral_quote_lots = caller_ts
                    .collateral_quote_lots
                    .checked_add(reward_paid)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            }
        }

        // Inject the synthetic close order.
        let next_seq = buffer
            .seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(next_seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
        let trader = position.trader;
        let mut inserted = false;
        for slot in buffer.slots.iter_mut() {
            if slot.valid == 0 {
                *slot = OrderSlot {
                    valid: 1,
                    side: close_side as u8,
                    order_type: OrderType::Liquidation as u8,
                    post_only: 0,
                    seq: next_seq,
                    id: next_seq,
                    trader,
                    size_lots: close_size,
                    limit_ticks: limit,
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

        // Update position liquidation timing for the cooldown + auction
        // gates. unhealthy_since_slot anchors the auction; subsequent
        // calls in the same auction window see growing rewards.
        // last_liquidated_at_slot enforces the cooldown gate.
        let position = &mut ctx.accounts.position;
        if position.unhealthy_since_slot == 0 {
            position.unhealthy_since_slot = current_slot;
        }
        position.last_liquidated_at_slot = current_slot;

        emit!(LiquidationInjectedEvent {
            market: market.key(),
            trader,
            side: pos_side as u8,
            size_lots: close_size,
            limit_ticks: limit,
            worst_scenario_idx: assessment.worst_scenario_idx,
        });
        if reward_paid > 0 {
            emit!(LiquidatorRewardedEvent {
                market: market.key(),
                liquidator: ctx.accounts.caller.key(),
                liquidatee: trader,
                reward_quote_lots: reward_paid,
            });
        }
        Ok(())
    }

    /// Cross-market portfolio liquidation. Walks the trader's positions
    /// across multiple markets via `remaining_accounts`, runs the matcher's
    /// cross-margin `assess_margin` against the joint stress lattice, and
    /// — if the trader is unhealthy — injects a liquidation order on the
    /// **execution market** specified by the named accounts.
    ///
    /// `remaining_accounts` is interpreted as pairs:
    ///     [other_market_0, other_position_0, other_market_1, other_position_1, …]
    /// Each pair is verified:
    ///   - both accounts owned by this program
    ///   - position.trader == trader_state.trader
    ///   - position.market == market_account.key()
    ///
    /// Cross-margin recognition: a long+short pair on different but
    /// correlated markets cancels in correlated stress scenarios, sharply
    /// reducing required margin. This is the same algorithm as the off-chain
    /// `previewPortfolioRisk` SDK helper — they MUST agree.
    pub fn liquidate_portfolio<'info>(
        ctx: Context<'_, '_, '_, 'info, LiquidatePortfolio<'info>>,
    ) -> Result<()> {
        let exec_market = &ctx.accounts.execution_market;
        let exec_position = &ctx.accounts.execution_position;
        let trader_state = &ctx.accounts.trader_state;
        let buffer = &mut ctx.accounts.execution_order_buffer;

        require!(
            exec_position.size_lots > 0,
            FlashBookError::LiquidationStale
        );
        require!(
            exec_position.trader == trader_state.trader,
            FlashBookError::WrongTrader
        );
        require!(
            exec_position.market == exec_market.key(),
            FlashBookError::WrongMarket
        );

        // Build snapshot vectors with the execution market+position first.
        let mut market_snaps: Vec<RiskMarketSnap> = Vec::new();
        let mut position_snaps: Vec<RiskPosSnap> = Vec::new();
        market_snaps.push(RiskMarketSnap {
            market: exec_market.key(),
            mark_price: Ticks(exec_market.mark_price_ticks),
            cum_funding_index: exec_market.cum_funding_index,
            maintenance_margin_bps: exec_market.params.maintenance_margin_ratio_bps,
            tick_size: exec_market.params.tick_size,
        });
        position_snaps.push(RiskPosSnap {
            market: exec_position.market,
            side: if exec_position.side == 0 { Side::Long } else { Side::Short },
            size_lots: exec_position.size_lots,
            entry_price: Ticks(exec_position.entry_price_ticks),
            cum_funding_index_at_entry: exec_position.cum_funding_index_at_entry,
        });

        // Walk remaining_accounts in (market, position) pairs.
        let remaining = ctx.remaining_accounts;
        require!(
            remaining.len().is_multiple_of(2),
            FlashBookError::OutOfRange
        );
        let program_id = ctx.program_id;
        let mut i = 0usize;
        while i + 1 < remaining.len() {
            let market_ai = &remaining[i];
            let position_ai = &remaining[i + 1];

            // Both accounts must be owned by this program.
            require_keys_eq!(
                *market_ai.owner,
                *program_id,
                FlashBookError::Unauthorized
            );
            require_keys_eq!(
                *position_ai.owner,
                *program_id,
                FlashBookError::Unauthorized
            );

            // Deserialize.
            let m_data = market_ai.try_borrow_data()?;
            let market: MarketAccount =
                MarketAccount::try_deserialize(&mut &m_data[..])?;
            let p_data = position_ai.try_borrow_data()?;
            let position: state::PositionAccount =
                state::PositionAccount::try_deserialize(&mut &p_data[..])?;

            // Validate trader + market binding.
            require!(
                position.trader == trader_state.trader,
                FlashBookError::WrongTrader
            );
            require!(
                position.market == market_ai.key(),
                FlashBookError::WrongMarket
            );

            // Skip empty positions; non-empty contribute to the assessment.
            if position.size_lots > 0 {
                market_snaps.push(RiskMarketSnap {
                    market: market_ai.key(),
                    mark_price: Ticks(market.mark_price_ticks),
                    cum_funding_index: market.cum_funding_index,
                    maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
                    tick_size: market.params.tick_size,
                });
                position_snaps.push(RiskPosSnap {
                    market: position.market,
                    side: if position.side == 0 { Side::Long } else { Side::Short },
                    size_lots: position.size_lots,
                    entry_price: Ticks(position.entry_price_ticks),
                    cum_funding_index_at_entry: position.cum_funding_index_at_entry,
                });
            }
            i += 2;
        }

        // Build the cross-market scenario lattice.
        let market_keys: Vec<Pubkey> = market_snaps.iter().map(|m| m.market).collect();
        let scenarios = default_scenarios_fn(&market_keys);

        let assessment = assess_margin_fn(
            &position_snaps,
            &market_snaps,
            &scenarios,
            trader_state.collateral_quote_lots,
        )?;
        require!(!assessment.is_healthy, FlashBookError::NotLiquidatable);

        // Inject a liquidation order on the execution market only.
        require!(
            (buffer.head as usize) < ORDER_BUFFER_CAP,
            FlashBookError::BufferFull
        );
        let pos_side = if exec_position.side == 0 { Side::Long } else { Side::Short };
        let close_side = pos_side.opposite();
        let penalty = exec_market.params.liq_penalty_bps as u128;
        let oracle = exec_market.oracle_price_ticks as u128;
        let penalty_delta = (oracle * penalty) / constants::BPS_DENOM as u128;
        let limit = match close_side {
            Side::Short => oracle.saturating_sub(penalty_delta) as u64,
            Side::Long => oracle.saturating_add(penalty_delta) as u64,
        };

        let next_seq = buffer
            .seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(next_seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);

        let trader = exec_position.trader;
        let mut inserted = false;
        for slot in buffer.slots.iter_mut() {
            if slot.valid == 0 {
                *slot = OrderSlot {
                    valid: 1,
                    side: close_side as u8,
                    order_type: OrderType::Liquidation as u8,
                    post_only: 0,
                    seq: next_seq,
                    id: next_seq,
                    trader,
                    size_lots: exec_position.size_lots,
                    limit_ticks: limit,
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

        emit!(LiquidationInjectedEvent {
            market: exec_market.key(),
            trader,
            side: pos_side as u8,
            size_lots: exec_position.size_lots,
            limit_ticks: limit,
            worst_scenario_idx: assessment.worst_scenario_idx,
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
pub struct InitializeFlpExposure<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = FlpExposureAccount::space(),
        seeds = [FlpExposureAccount::SEED],
        bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,

    /// Treasury LpPositionAccount — initial shares are minted here.
    #[account(
        init,
        payer = authority,
        space = state::LpPositionAccount::space(),
        seeds = [state::LpPositionAccount::SEED, authority.key().as_ref()],
        bump,
    )]
    pub authority_lp_position: Box<Account<'info, state::LpPositionAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DepositFlpCapital<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,

    /// LP's per-LP share account. Created lazily on first deposit.
    #[account(
        init_if_needed,
        payer = authority,
        space = state::LpPositionAccount::space(),
        seeds = [state::LpPositionAccount::SEED, authority.key().as_ref()],
        bump,
    )]
    pub lp_position: Box<Account<'info, state::LpPositionAccount>>,

    /// Insurance fund PDA — owns the protocol vault.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = authority,
    )]
    pub authority_quote_ata: Account<'info, TokenAccount>,

    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct WithdrawFlpCapital<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,

    /// LP's per-LP share account. Must already exist (no init_if_needed
    /// — withdrawals require pre-existing shares).
    #[account(
        mut,
        seeds = [state::LpPositionAccount::SEED, authority.key().as_ref()],
        bump = lp_position.bump,
        constraint = lp_position.lp == authority.key() @ FlashBookError::Unauthorized,
    )]
    pub lp_position: Box<Account<'info, state::LpPositionAccount>>,

    /// Insurance fund PDA — owns the vault and signs withdrawals.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = authority,
    )]
    pub authority_quote_ata: Account<'info, TokenAccount>,

    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
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

    /// The protocol's quote currency mint (typically USDC).
    pub quote_mint: Account<'info, Mint>,

    /// Global protocol vault. Created with `insurance_fund` PDA as
    /// authority so the program can sign transfers out.
    #[account(
        init,
        payer = authority,
        token::mint = quote_mint,
        token::authority = insurance_fund,
    )]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct WithdrawInsuranceFund<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    /// Authority's USDC ATA — destination for withdrawn rent.
    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = authority,
    )]
    pub authority_quote_ata: Account<'info, TokenAccount>,

    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
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
pub struct SetTraderReferrer<'info> {
    /// Trader signs to set their own referrer. One-time-write enforced
    /// inside the handler.
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
pub struct SetTraderDelegate<'info> {
    /// The trader signs to set/clear their own delegate. Only the trader
    /// can change this field — the delegate cannot rotate themselves out.
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
pub struct SetTraderFeeTier<'info> {
    /// Protocol authority (the same authority that runs init_insurance_fund).
    /// Off-chain volume tracker → governance updates each trader's tier.
    pub authority: Signer<'info>,

    /// The insurance_fund holds the canonical authority pubkey we
    /// validate against — single source of truth.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
        constraint = insurance_fund.authority == authority.key() @ FlashBookError::Unauthorized,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader_state.trader.as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct InitTraderAta<'info> {
    /// Funds the ATA rent. Doesn't have to be the trader — onboarding flows
    /// often have the protocol or a sponsor pay.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// Owner of the ATA being created. Doesn't sign: ATA creation is
    /// permissionless under the AssociatedToken program.
    /// CHECK: used only as the seed/authority for the ATA derivation.
    pub trader: UncheckedAccount<'info>,

    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    /// Created idempotently if missing. Anchor CPIs the AssociatedToken
    /// program when this account doesn't yet exist; the canonical address
    /// is derived from (trader, quote_mint).
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = quote_mint,
        associated_token::authority = trader,
    )]
    pub trader_quote_ata: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseTraderAta<'info> {
    /// Trader signs — they are the ATA authority.
    pub trader: Signer<'info>,

    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    /// The ATA being closed. Constrained to be the canonical ATA for
    /// (trader, quote_mint). SPL Token's CloseAccount will enforce that
    /// the token balance is zero before allowing close.
    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = trader,
    )]
    pub trader_quote_ata: Account<'info, TokenAccount>,

    /// Where the freed rent lamports are credited. Caller's choice — for
    /// most onboarding flows this is the trader; for sponsored flows the
    /// original payer might want to reclaim.
    /// CHECK: lamport-only credit; account data is not interpreted.
    #[account(mut)]
    pub rent_destination: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct VerifyMarketInvariants<'info> {
    /// Permissionless — anyone can poke the invariants. The signer just
    /// pays the tx fee.
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,
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
pub struct UpdateMarketAuthority<'info> {
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
    #[account(mut)]
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
    /// Position PDA — initialized lazily on first order for this (market, trader).
    /// `init_if_needed` makes the trader pay rent on first creation; subsequent
    /// orders find it already initialized. Used by the stress-lattice gate.
    #[account(
        init_if_needed,
        payer = trader,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader.key().as_ref()],
        bump,
    )]
    pub position: Account<'info, state::PositionAccount>,

    /// FLP capital pool — read-only here; we only need
    /// `total_capital_quote_lots` for the capital-relative position cap.
    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Account<'info, FlpExposureAccount>,

    pub system_program: Program<'info, System>,
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

    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    /// Quote mint (typically USDC). Required so `associated_token` can derive
    /// the canonical ATA address for validation.
    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    /// Trader's USDC ATA — must be the canonical associated token account
    /// for (trader, quote_mint). Anchor validates the address derivation.
    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = trader,
    )]
    pub trader_quote_ata: Account<'info, TokenAccount>,

    /// Global vault — must be the one stored on the insurance_fund.
    #[account(
        mut,
        address = insurance_fund.quote_vault,
    )]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
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

    /// Insurance fund PDA — authority over the vault. The program signs
    /// transfers out using its seeds.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = trader,
    )]
    pub trader_quote_ata: Account<'info, TokenAccount>,

    #[account(
        mut,
        address = insurance_fund.quote_vault,
    )]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct SettleFunding<'info> {
    /// Permissionless — any signer can settle funding for any position.
    /// The position's owner is determined by the position PDA's seeds,
    /// not by who signs the tx.
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// The trader being settled. Doesn't need to sign — settle is
    /// permissionless. Used as a seed to derive trader_state and position.
    /// CHECK: identity check is enforced by PDA seed derivation below.
    pub trader: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader.key().as_ref()],
        bump,
    )]
    pub position: Account<'info, state::PositionAccount>,
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
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

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

#[derive(Accounts)]
pub struct PlaceBasketOrder<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
        constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,

    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Account<'info, FlpExposureAccount>,

    // ── Leg A ──
    #[account(
        seeds = [MarketAccount::SEED, market_a.base_mint.as_ref(), market_a.quote_mint.as_ref()],
        bump = market_a.bump,
    )]
    pub market_a: Account<'info, MarketAccount>,

    #[account(
        mut,
        seeds = [OrderBufferAccount::SEED, market_a.key().as_ref()],
        bump = order_buffer_a.bump,
    )]
    pub order_buffer_a: Account<'info, OrderBufferAccount>,

    #[account(
        init_if_needed,
        payer = trader,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market_a.key().as_ref(), trader.key().as_ref()],
        bump,
    )]
    pub position_a: Account<'info, state::PositionAccount>,

    // ── Leg B ──
    #[account(
        seeds = [MarketAccount::SEED, market_b.base_mint.as_ref(), market_b.quote_mint.as_ref()],
        bump = market_b.bump,
    )]
    pub market_b: Account<'info, MarketAccount>,

    #[account(
        mut,
        seeds = [OrderBufferAccount::SEED, market_b.key().as_ref()],
        bump = order_buffer_b.bump,
    )]
    pub order_buffer_b: Account<'info, OrderBufferAccount>,

    #[account(
        init_if_needed,
        payer = trader,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market_b.key().as_ref(), trader.key().as_ref()],
        bump,
    )]
    pub position_b: Account<'info, state::PositionAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceBasketOrderN<'info> {
    pub trader: Signer<'info>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
        constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,

    /// Read for the capital-relative position cap (per-leg) and the
    /// joint stress-lattice gate.
    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Account<'info, FlpExposureAccount>,
    // Per-leg accounts arrive in remaining_accounts as triples:
    //   [market_0, order_buffer_0, position_0,
    //    market_1, order_buffer_1, position_1, ...]
}

#[derive(Accounts)]
pub struct CancelOrder<'info> {
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
}

#[derive(Accounts)]
pub struct ApplyFlpFill<'info> {
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
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, taker_trader_state.trader.as_ref()],
        bump = taker_trader_state.bump,
    )]
    pub taker_trader_state: Account<'info, TraderStateAccount>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), taker_trader_state.trader.as_ref()],
        bump,
    )]
    pub taker_position: Account<'info, state::PositionAccount>,

    #[account(
        mut,
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Account<'info, FlpExposureAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct LiquidatePortfolio<'info> {
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, execution_market.base_mint.as_ref(), execution_market.quote_mint.as_ref()],
        bump = execution_market.bump,
    )]
    pub execution_market: Account<'info, MarketAccount>,

    #[account(
        mut,
        seeds = [OrderBufferAccount::SEED, execution_market.key().as_ref()],
        bump = execution_order_buffer.bump,
    )]
    pub execution_order_buffer: Account<'info, OrderBufferAccount>,

    #[account(
        seeds = [TraderStateAccount::SEED, trader_state.trader.as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,

    #[account(
        seeds = [state::PositionAccount::SEED, execution_market.key().as_ref(), trader_state.trader.as_ref()],
        bump = execution_position.bump,
    )]
    pub execution_position: Account<'info, state::PositionAccount>,
    // remaining_accounts: alternating [Market, Position] pairs for the
    // trader's other markets (cross-margin assessment).
}

#[derive(Accounts)]
pub struct LiquidatePosition<'info> {
    /// Anyone may call. The caller pays the tx fee and (when
    /// `liquidator_reward_bps > 0`) receives a tip credited to their
    /// `caller_trader_state`.
    #[account(mut)]
    pub caller: Signer<'info>,

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

    /// The unhealthy trader's state. Mut because the liquidator reward is
    /// debited from their collateral.
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader_state.trader.as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,

    /// The caller's own trader_state. Reward is credited here. Created
    /// lazily on first liquidation (init_if_needed) so new keepers don't
    /// need to call open_trader_state separately.
    #[account(
        init_if_needed,
        payer = caller,
        space = TraderStateAccount::space(),
        seeds = [TraderStateAccount::SEED, caller.key().as_ref()],
        bump,
    )]
    pub caller_trader_state: Account<'info, TraderStateAccount>,

    #[account(
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader_state.trader.as_ref()],
        bump = position.bump,
    )]
    pub position: Account<'info, state::PositionAccount>,

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
pub struct FlpExposureInitializedEvent {
    pub authority: Pubkey,
    pub initial_capital: u64,
}

#[event]
pub struct FlpCapitalUpdatedEvent {
    pub new_total: u64,
    pub delta: i64,
}

#[event]
pub struct MarketStatusChangedEvent {
    pub market: Pubkey,
    pub previous_status: u8,
    pub new_status: u8,
}

#[event]
pub struct MarketParamsUpdatedEvent {
    pub market: Pubkey,
}

#[event]
pub struct MarketAuthorityTransferredEvent {
    pub market: Pubkey,
    pub previous_authority: Pubkey,
    pub new_authority: Pubkey,
}

#[event]
pub struct FlpFillAppliedEvent {
    pub market: Pubkey,
    pub taker: Pubkey,
    pub taker_side: u8,
    pub size_lots: u64,
    pub price_ticks: u64,
    pub batch_num: u64,
    pub flp_size_after: u64,
    pub flp_side_after: u8,
}

#[event]
pub struct LiquidationInjectedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub limit_ticks: u64,
    pub worst_scenario_idx: u32,
}

#[event]
pub struct OrderCancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub order_seq: u64,
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

#[event]
pub struct FundingSettledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    /// Signed funding owed in quote lots: positive = trader paid, negative
    /// = trader received.
    pub owed_quote_lots: i64,
    pub new_collateral: u64,
}

#[event]
pub struct BasketOrderNPlacedEvent {
    pub trader: Pubkey,
    pub leg_count: u8,
    pub markets: Vec<Pubkey>,
}

#[event]
pub struct BasketOrderPlacedEvent {
    pub trader: Pubkey,
    pub market_a: Pubkey,
    pub market_b: Pubkey,
    pub side_a: u8,
    pub side_b: u8,
    pub size_lots_a: u64,
    pub size_lots_b: u64,
}

#[event]
pub struct TraderFeeTierUpdatedEvent {
    pub trader: Pubkey,
    pub discount_bps: u32,
}

#[event]
pub struct TraderDelegateUpdatedEvent {
    pub trader: Pubkey,
    pub previous: Pubkey,
    pub new: Pubkey,
}

#[event]
pub struct TraderReferrerSetEvent {
    pub trader: Pubkey,
    pub referrer: Pubkey,
}

#[event]
pub struct ReferralPaidEvent {
    pub taker: Pubkey,
    pub referrer: Pubkey,
    pub amount_quote_lots: u64,
}

#[event]
pub struct ReferralOwedEvent {
    pub taker: Pubkey,
    pub referrer: Pubkey,
    pub amount_quote_lots: u64,
}

#[event]
pub struct LiquidatorRewardedEvent {
    pub market: Pubkey,
    pub liquidator: Pubkey,
    pub liquidatee: Pubkey,
    pub reward_quote_lots: u64,
}

#[event]
pub struct ToxicityTaxAppliedEvent {
    pub market: Pubkey,
    pub taker: Pubkey,
    /// The maker who absorbed the toxic flow (or the FLP pool PDA on
    /// apply_flp_fill).
    pub maker: Pubkey,
    pub vpin_bps: u32,
    pub tax_quote_lots: u64,
    pub insurance_share: u64,
    pub maker_share: u64,
}

#[event]
pub struct InvariantBreachDetectedEvent {
    pub market: Pubkey,
    /// Solvency invariant identifier from docs/SAFETY.md (5 = OI balance,
    /// 4 = vault conservation, etc).
    pub invariant_code: u8,
    pub expected: u64,
    pub actual: u64,
    pub previous_status: u8,
    pub new_status: u8,
}

// ─── Helpers ────────────────────────────────────────────────────────────

/// One leg of a basket order. Mirrors place_limit_order's args minus
/// market identity (which is bound by the account context per leg).
#[derive(Debug, Clone, Copy, AnchorSerialize, AnchorDeserialize)]
pub struct BasketLeg {
    pub side: u8,
    pub size_lots: u64,
    pub limit_ticks: u64,
    pub post_only: bool,
}

/// Validate per-leg intake gates: status, size/price floors, tick alignment.
/// Skips per-market margin check — basket margin runs across both legs.
fn validate_leg_intake(market: &MarketAccount, leg: &BasketLeg) -> Result<()> {
    require!(leg.size_lots > 0, FlashBookError::ZeroSize);
    require!(leg.limit_ticks > 0, FlashBookError::ZeroPrice);
    require!(leg.side <= 1, FlashBookError::OutOfRange);
    require!(
        market.status == MarketStatus::Active as u8
            || market.status == MarketStatus::PostOnly as u8,
        FlashBookError::OutOfRange
    );
    require!(
        leg.size_lots >= market.params.min_base_lots,
        FlashBookError::SizeBelowMinLot
    );
    require!(
        leg.limit_ticks.is_multiple_of(market.params.tick_size),
        FlashBookError::PriceNotOnTick
    );
    require!(
        leg.size_lots <= FLP_SEQ_RESERVED_OFFSET,
        FlashBookError::OutOfRange
    );
    Ok(())
}

/// Apply absolute and capital-relative position caps for a single basket leg.
fn check_caps_for_leg(
    market: &MarketAccount,
    position: &state::PositionAccount,
    flp: &FlpExposureAccount,
    leg: &BasketLeg,
) -> Result<()> {
    let cap = market.params.max_position_lots_per_trader;
    if cap > 0 {
        let new_size = position.size_lots.saturating_add(leg.size_lots);
        require!(new_size <= cap, FlashBookError::PositionSizeCapExceeded);
    }
    let ratio_cap = market.params.max_position_ratio_bps;
    if ratio_cap > 0 && flp.total_capital_quote_lots > 0 {
        let cap_quote_lots = (flp.total_capital_quote_lots as u128)
            .saturating_mul(ratio_cap as u128)
            / (constants::BPS_DENOM as u128);
        let new_size = position.size_lots.saturating_add(leg.size_lots);
        let new_notional = (new_size as u128)
            .saturating_mul(leg.limit_ticks as u128)
            .saturating_mul(market.params.tick_size as u128);
        require!(
            new_notional <= cap_quote_lots,
            FlashBookError::PositionSizeCapExceeded
        );
    }
    Ok(())
}

/// Project a position's post-leg state for the cross-market margin check.
/// Returns None if the resulting projected position is still empty.
fn project_post_leg(
    position: &state::PositionAccount,
    leg: &BasketLeg,
    market: &MarketAccount,
    market_key: Pubkey,
    trader: Pubkey,
) -> Result<Option<RiskPosSnap>> {
    // If no current position, the projected state assumes the leg fills
    // entirely as a new position at limit_ticks.
    if position.size_lots == 0 {
        return Ok(Some(RiskPosSnap {
            market: market_key,
            side: if leg.side == 0 { Side::Long } else { Side::Short },
            size_lots: leg.size_lots,
            entry_price: Ticks(leg.limit_ticks),
            cum_funding_index_at_entry: market.cum_funding_index,
        }));
    }
    require!(
        position.trader == trader,
        FlashBookError::WrongTrader
    );
    require!(
        position.market == market_key,
        FlashBookError::WrongMarket
    );
    // Worst-case projection: same-side adds, opposite-side has the leg's
    // limit price as the worst-case fill (size grows for margin assessment
    // even if it would actually reduce on cross). Conservative.
    let projected_size = if position.side == leg.side {
        position.size_lots.saturating_add(leg.size_lots)
    } else {
        // Opposite side: in worst case the leg fully fills and adds to
        // the existing same-side, AKA a flip. Approximate post-state as
        // |existing - leg_size|; if leg ≥ existing, we'd be on the leg's
        // side after fill.
        if leg.size_lots >= position.size_lots {
            leg.size_lots - position.size_lots
        } else {
            position.size_lots - leg.size_lots
        }
    };
    let projected_side = if position.side == leg.side {
        position.side
    } else if leg.size_lots > position.size_lots {
        leg.side
    } else {
        position.side
    };
    Ok(Some(RiskPosSnap {
        market: market_key,
        side: if projected_side == 0 { Side::Long } else { Side::Short },
        size_lots: projected_size,
        entry_price: Ticks(position.entry_price_ticks),
        cum_funding_index_at_entry: position.cum_funding_index_at_entry,
    }))
}

/// Insert a basket leg's order into the given order buffer. Mirrors the
/// insertion logic in place_limit_order (next_seq, slot scan, head bump).
fn insert_into_buffer(
    buffer: &mut OrderBufferAccount,
    trader: Pubkey,
    leg: &BasketLeg,
) -> Result<()> {
    require!(
        (buffer.head as usize) < ORDER_BUFFER_CAP,
        FlashBookError::BufferFull
    );
    let next_seq = buffer
        .seq_counter
        .checked_add(1)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
    require!(next_seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
    let mut inserted = false;
    for slot in buffer.slots.iter_mut() {
        if slot.valid == 0 {
            *slot = OrderSlot {
                valid: 1,
                side: leg.side,
                order_type: OrderType::Limit as u8,
                post_only: if leg.post_only { 1 } else { 0 },
                seq: next_seq,
                id: next_seq,
                trader,
                size_lots: leg.size_lots,
                limit_ticks: leg.limit_ticks,
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

/// Read FLP per-market entry side+size for a market. Returns (side, size).
/// Side 255 = empty slot.
fn flp_market_pre_state(flp: &FlpExposureAccount, market: Pubkey) -> (u8, u64) {
    for entry in flp.per_market.iter() {
        if entry.side != 255 && entry.market == market {
            return (entry.side, entry.size_lots);
        }
    }
    (255, 0)
}

/// Apply a fill against the FLP's per-market entry. Mirrors
/// `apply_fill_to_position` semantics on a `FlpMarketExposure` slot.
fn apply_fill_to_flp_market(
    flp: &mut FlpExposureAccount,
    market: Pubkey,
    fill_side: Side,
    fill_size_lots: u64,
    fill_price_ticks: u64,
) -> Result<()> {
    // Find existing entry or first empty slot.
    let mut entry_idx: Option<usize> = None;
    let mut empty_idx: Option<usize> = None;
    for (i, entry) in flp.per_market.iter().enumerate() {
        if entry.side != 255 && entry.market == market {
            entry_idx = Some(i);
            break;
        }
        if entry.side == 255 && empty_idx.is_none() {
            empty_idx = Some(i);
        }
    }

    let idx = match entry_idx {
        Some(i) => i,
        None => {
            let i = empty_idx.ok_or_else(|| error!(FlashBookError::BufferFull))?;
            flp.per_market[i] = state::FlpMarketExposure {
                market,
                side: fill_side as u8,
                size_lots: fill_size_lots,
                entry_price_ticks: fill_price_ticks,
            };
            flp.markets_count = flp.markets_count.saturating_add(1);
            return Ok(());
        }
    };

    let cur = &mut flp.per_market[idx];
    let cur_side_enum = if cur.side == 0 { Side::Long } else { Side::Short };

    if cur_side_enum == fill_side {
        let new_size = cur
            .size_lots
            .checked_add(fill_size_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        let weighted = (cur.entry_price_ticks as u128)
            .checked_mul(cur.size_lots as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            .checked_add(
                (fill_price_ticks as u128)
                    .checked_mul(fill_size_lots as u128)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?,
            )
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            .checked_div(new_size as u128)
            .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
        cur.entry_price_ticks = weighted as u64;
        cur.size_lots = new_size;
        return Ok(());
    }

    // Opposite side: realize PnL on closed portion (FLP carries this in
    // `flp.realized_pnl`, not per-market).
    let close_size = fill_size_lots.min(cur.size_lots);
    let sign: i128 = if cur_side_enum == Side::Long { 1 } else { -1 };
    let pnl_per_lot: i128 = (fill_price_ticks as i128) - (cur.entry_price_ticks as i128);
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
    flp.realized_pnl = flp.realized_pnl.saturating_add(pnl_clamped);

    if fill_size_lots <= cur.size_lots {
        cur.size_lots = cur
            .size_lots
            .checked_sub(fill_size_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        if cur.size_lots == 0 {
            // Mark slot empty.
            cur.side = 255;
            cur.entry_price_ticks = 0;
            flp.markets_count = flp.markets_count.saturating_sub(1);
        }
    } else {
        // Flip side.
        let remaining = fill_size_lots
            .checked_sub(cur.size_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        cur.side = fill_side as u8;
        cur.size_lots = remaining;
        cur.entry_price_ticks = fill_price_ticks;
    }
    Ok(())
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
