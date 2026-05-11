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
pub mod hypertree;
pub mod matcher;
pub mod state;
pub mod state_v2;

pub use errors::FlashBookError;

use constants::{
    FLP_SEQ_RESERVED_OFFSET, MARK_HISTORY_LEN, MAX_BASKET_LEGS_N,
    MAX_ORDERS_PER_TRADER_PER_BATCH,
};
use matcher::commit_reveal::{
    register_commit, redeem_reveal, sweep_expired, RevealPayload,
};
use matcher::flp_quoter::{generate_quotes, FlpQuoterInputs, FlpQuoterParams};
use matcher::funding::funding_owed;
use matcher::lot::{BaseLots, Ticks};
use matcher::order::Side;
use matcher::risk::{
    assess_margin as assess_margin_fn, default_scenarios as default_scenarios_fn,
    MarketSnapshot as RiskMarketSnap, PositionSnapshot as RiskPosSnap,
};
use state::{
    CommitBufferAccount, FlpExposureAccount, InsuranceFundAccount, LeverageTier,
    MarketAccount, MarketLeverageTiersAccount, MarketParams, TraderStateAccount,
    MAX_LEVERAGE_TIERS,
};

declare_id!("HGP5GN7BHSt1geH1DxRwVGFg7g7ERU28Q2QEYf6KP24b");

#[program]
pub mod flash_book {
    use super::*;

    // ─── Setup ──────────────────────────────────────────────────────

    /// Initialize a new market and all associated PDAs (protocol-deployed).
    /// Uses the shared `initialize_market_inner` body; the only difference
    /// from `permissionless_initialize_market` is that `creator` is zeroed
    /// (no creator share) and params are not envelope-clamped.
    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        params: MarketParams,
        initial_oracle_ticks: u64,
    ) -> Result<()> {
        initialize_market_inner(ctx, params, initial_oracle_ticks, false)
    }

    /// Initialize the commit_buffer for an existing market. MUST be
    /// called after `initialize_market`. See `initialize_order_buffer`
    /// docstring for the split rationale.
    pub fn initialize_commit_buffer(
        ctx: Context<InitializeCommitBuffer>,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let commit_buf = &mut ctx.accounts.commit_buffer;
        commit_buf.market = market_key;
        commit_buf.bump = ctx.bumps.commit_buffer;
        commit_buf.head = 0;
        commit_buf.commits = [state::CommitRow::default(); state::COMMIT_BUFFER_CAP];
        Ok(())
    }

    /// Initialize the v2 hypertree-backed orderbook for a market.
    /// Allocates a fresh PDA at `[b"market_book", market]` of exactly
    /// MARKET_BOOK_TOTAL_BYTES (8264 B), stamps the v2 discriminator,
    /// and writes an empty header with all RBT root indices = NIL.
    ///
    /// This is the foundation for the wave-18 orderbook rewrite. The
    /// account is `UncheckedAccount` because the data layout
    /// (256-byte header + 8000-byte dynamic node array) is large
    /// enough that Anchor's `#[account(zero_copy)]` derive choke on
    /// the `[u8; 8000]` field. Manifest's exact pattern.
    ///
    /// This ix runs ALONGSIDE the legacy `initialize_order_buffer` for
    /// now; once the matcher is fully migrated (wave 18d-e), the
    /// legacy ix is deprecated and removed.
    pub fn init_market_book(ctx: Context<InitMarketBook>) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let base_mint = ctx.accounts.market.base_mint;
        let quote_mint = ctx.accounts.market.quote_mint;
        let bump = ctx.bumps.market_book;

        let space = state_v2::MARKET_BOOK_TOTAL_BYTES;
        let rent = Rent::get()?;
        let lamports = rent.minimum_balance(space);

        // CPI to System Program — create the PDA at the right seed
        // owned by our program. The PDA's seeds (`market_book` ‖ market)
        // make the address deterministic; the bump comes from Anchor's
        // constraint derivation in InitMarketBook below.
        let signer_seeds: &[&[u8]] =
            &[state_v2::MARKET_BOOK_SEED, market_key.as_ref(), &[bump]];
        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::create_account(
                &ctx.accounts.authority.key(),
                &ctx.accounts.market_book.key(),
                lamports,
                space as u64,
                ctx.program_id,
            ),
            &[
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.market_book.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[signer_seeds],
        )?;

        // Write the v2 discriminator + zero-init the header.
        let mut data = ctx.accounts.market_book.try_borrow_mut_data()?;
        state_v2::MarketBookHandle::write_disc_and_init_header(
            &mut data, bump, market_key, base_mint, quote_mint,
        )?;

        emit!(MarketBookInitializedEvent {
            market: market_key,
            market_book: ctx.accounts.market_book.key(),
            total_bytes: space as u32,
            data_bytes: state_v2::MARKET_BOOK_DATA_BYTES as u32,
        });
        Ok(())
    }

    /// Delegate the v2 hypertree market_book PDA to the MagicBlock ER.
    /// After this lands, the account state lives on the ER for sub-ms
    /// matcher access; only this program (via PDA signature) can
    /// undelegate it back to mainnet. The market account must also be
    /// delegated (see `delegate_market`) for `run_batch_v2` to mutate
    /// mark/funding/VPIN on the ER.
    ///
    /// `commit_frequency_ms` controls how often the ER auto-commits the
    /// state back to mainnet when the program isn't doing it explicitly.
    /// 0 disables auto-commit (only manual undelegate flushes state).
    /// Production target: 50–200 ms (matches the FBA cadence).
    ///
    /// `validator` pins the ER validator (None = permissionless selection).
    pub fn delegate_market_book(
        ctx: Context<DelegateMarketBook>,
        commit_frequency_ms: u32,
        validator: Option<Pubkey>,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let bump = ctx.bumps.market_book;

        // Defence-in-depth (er.rs SECURITY note): the market_book MUST
        // be owned by us before we sign a delegate over it. The seeds
        // constraint already implies this, but recheck explicitly.
        require_keys_eq!(
            *ctx.accounts.market_book.owner,
            *ctx.program_id,
            FlashBookError::Unauthorized
        );

        let seeds_for_args: Vec<Vec<u8>> = vec![
            state_v2::MARKET_BOOK_SEED.to_vec(),
            market_key.as_ref().to_vec(),
            vec![bump],
        ];
        let signer_seeds: &[&[u8]] = &[
            state_v2::MARKET_BOOK_SEED,
            market_key.as_ref(),
            &[bump],
        ];

        er::cpi_delegate(
            er::DelegateAccounts {
                payer: ctx.accounts.authority.to_account_info(),
                delegated_account: ctx.accounts.market_book.to_account_info(),
                owner_program: ctx.accounts.owner_program.to_account_info(),
                delegate_buffer: ctx.accounts.delegate_buffer.to_account_info(),
                delegation_record: ctx.accounts.delegation_record.to_account_info(),
                delegation_metadata: ctx.accounts.delegation_metadata.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                delegation_program: ctx.accounts.delegation_program.to_account_info(),
            },
            er::DelegateArgs {
                commit_frequency_ms,
                seeds: seeds_for_args,
                validator,
            },
            signer_seeds,
        )?;

        emit!(MarketBookDelegatedEvent {
            market: market_key,
            market_book: ctx.accounts.market_book.key(),
            commit_frequency_ms,
            validator: validator.unwrap_or_default(),
        });
        Ok(())
    }

    /// Undelegate the market_book PDA from the ER back to mainnet. After
    /// this lands the account state is authoritative on mainnet again
    /// and matcher ticks must run on mainnet (`run_batch_v2` callable
    /// directly, no ER bridging needed).
    ///
    /// Use during planned ER downtime, validator rotation, or to flush
    /// final state before a permanent shutdown of the ER instance.
    pub fn undelegate_market_book(ctx: Context<UndelegateMarketBook>) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let bump = ctx.bumps.market_book;
        let signer_seeds: &[&[u8]] = &[
            state_v2::MARKET_BOOK_SEED,
            market_key.as_ref(),
            &[bump],
        ];

        er::cpi_undelegate(
            er::UndelegateAccounts {
                payer: ctx.accounts.authority.to_account_info(),
                delegated_account: ctx.accounts.market_book.to_account_info(),
                owner_program: ctx.accounts.owner_program.to_account_info(),
                buffer: ctx.accounts.delegate_buffer.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                delegation_program: ctx.accounts.delegation_program.to_account_info(),
            },
            signer_seeds,
        )?;

        emit!(MarketBookUndelegatedEvent {
            market: market_key,
            market_book: ctx.accounts.market_book.key(),
        });
        Ok(())
    }

    /// Delegate the MarketAccount itself to the ER. Required for
    /// `run_batch_v2` to mutate `mark_price_ticks`, `cum_funding_index`,
    /// `last_funding_rate_bps_per_sec`, `vpin`, `current_batch`, and
    /// `last_batch_ms` on the ER. Pair this with `delegate_market_book`
    /// — both delegations must be live for the matcher tick to run on
    /// the ER.
    pub fn delegate_market(
        ctx: Context<DelegateMarket>,
        commit_frequency_ms: u32,
        validator: Option<Pubkey>,
    ) -> Result<()> {
        let base_mint = ctx.accounts.market.base_mint;
        let quote_mint = ctx.accounts.market.quote_mint;
        let bump = ctx.accounts.market.bump;

        require_keys_eq!(
            *ctx.accounts.market.to_account_info().owner,
            *ctx.program_id,
            FlashBookError::Unauthorized
        );

        let seeds_for_args: Vec<Vec<u8>> = vec![
            MarketAccount::SEED.to_vec(),
            base_mint.as_ref().to_vec(),
            quote_mint.as_ref().to_vec(),
            vec![bump],
        ];
        let signer_seeds: &[&[u8]] = &[
            MarketAccount::SEED,
            base_mint.as_ref(),
            quote_mint.as_ref(),
            &[bump],
        ];

        er::cpi_delegate(
            er::DelegateAccounts {
                payer: ctx.accounts.authority.to_account_info(),
                delegated_account: ctx.accounts.market.to_account_info(),
                owner_program: ctx.accounts.owner_program.to_account_info(),
                delegate_buffer: ctx.accounts.delegate_buffer.to_account_info(),
                delegation_record: ctx.accounts.delegation_record.to_account_info(),
                delegation_metadata: ctx.accounts.delegation_metadata.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                delegation_program: ctx.accounts.delegation_program.to_account_info(),
            },
            er::DelegateArgs {
                commit_frequency_ms,
                seeds: seeds_for_args,
                validator,
            },
            signer_seeds,
        )?;

        emit!(MarketDelegatedEvent {
            market: ctx.accounts.market.key(),
            commit_frequency_ms,
            validator: validator.unwrap_or_default(),
        });
        Ok(())
    }

    /// Undelegate the MarketAccount from the ER back to mainnet.
    pub fn undelegate_market(ctx: Context<UndelegateMarket>) -> Result<()> {
        let base_mint = ctx.accounts.market.base_mint;
        let quote_mint = ctx.accounts.market.quote_mint;
        let bump = ctx.accounts.market.bump;
        let signer_seeds: &[&[u8]] = &[
            MarketAccount::SEED,
            base_mint.as_ref(),
            quote_mint.as_ref(),
            &[bump],
        ];

        er::cpi_undelegate(
            er::UndelegateAccounts {
                payer: ctx.accounts.authority.to_account_info(),
                delegated_account: ctx.accounts.market.to_account_info(),
                owner_program: ctx.accounts.owner_program.to_account_info(),
                buffer: ctx.accounts.delegate_buffer.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                delegation_program: ctx.accounts.delegation_program.to_account_info(),
            },
            signer_seeds,
        )?;

        emit!(MarketUndelegatedEvent {
            market: ctx.accounts.market.key(),
        });
        Ok(())
    }

    /// V2 limit-order placement against the hypertree-backed orderbook.
    /// Runs ALONGSIDE the legacy `place_limit_order` for now — operators
    /// pick which book each market uses by calling `init_market_book`
    /// (v2) vs `initialize_order_buffer` (legacy).
    ///
    /// Validation mirrors the legacy ix's intake: status-active gate,
    /// min-base-lots, tick alignment, size cap. Then constructs a
    /// `RestingOrderV2` carrying the trader pubkey inline (free-funds
    /// indirection comes in wave 19) and inserts into the bids or asks
    /// RBT inside the `MarketBookHandle`.
    ///
    /// `flags` accepts the same bitfield as v1: bit0 post_only, bit1
    /// reduce_only, bit2 ioc, bit3 jit, bits 4-5 stp_mode.
    pub fn place_limit_order_v2(
        ctx: Context<PlaceLimitOrderV2>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        flags: u8,
        expires_at_slot: u64,
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(limit_ticks > 0, FlashBookError::ZeroPrice);
        // Reject unknown flag bits.
        require!(flags & !0b0011_1111 == 0, FlashBookError::OutOfRange);
        if expires_at_slot > 0 {
            let now = Clock::get()?.slot;
            require!(expires_at_slot > now, FlashBookError::OutOfRange);
        }

        let market = &ctx.accounts.market;
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
            limit_ticks % market.params.tick_size == 0,
            FlashBookError::PriceNotOnTick
        );

        // Per-market OI hard cap (mirror v1).
        let oi_cap = market.params.max_oi_base_lots;
        if oi_cap > 0 {
            let cur = if side == 0 { market.oi_long_lots } else { market.oi_short_lots };
            let projected = cur.saturating_add(size_lots);
            require!(projected <= oi_cap, FlashBookError::OpenInterestCapExceeded);
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();
        let now_slot = Clock::get()?.slot;

        // Borrow the market_book account data + load the handle.
        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        require!(
            handle.header.market_pubkey == market_key,
            FlashBookError::WrongMarket
        );

        // Allocate seq + build the resting order.
        let seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        handle.header.order_seq_counter = seq;

        let side_is_bid = side == 0;
        let order = state_v2::RestingOrderV2 {
            order_id: state_v2::encode_order_id(limit_ticks, seq, side_is_bid),
            seq,
            price_ticks: limit_ticks,
            size_lots,
            expires_at_slot,
            trader: trader_pk,
            last_valid_slot: now_slot as u32,
            side,
            order_type: 0, // 0 = limit (the only kind for now)
            flags,
            _pad: 0,
        };

        let inserted_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };

        emit!(OrderPlacedV2Event {
            market: market_key,
            trader: trader_pk,
            seq,
            side,
            price_ticks: limit_ticks,
            size_lots,
            node_index: inserted_idx,
            total_orders_after: handle.header.total_orders_active,
        });
        Ok(())
    }

    /// Wave 21 phase 2 — CPI variant of `place_limit_order_v2`. Called
    /// by wrapper programs (`flash-book-orders`, `-flp`, `-vaults`) via
    /// `invoke_signed` to place orders on behalf of traders WITHOUT
    /// the trader directly signing the core ix. The wrapper signs over
    /// its `[CPI_AUTHORITY_SEED]` PDA; core verifies the signer is one
    /// of the 3 expected derivations (anything else fails).
    ///
    /// Trust model: the wrapper has full authority over the trader's
    /// order placement decision via its OWN state (trigger account,
    /// vault deposit, etc.) — it already authenticated the trader's
    /// intent at trigger / deposit time. Core trusts the wrapper to
    /// inject an order with the trader's pubkey stamped on it.
    ///
    /// Pure parity with the trader-signed `place_limit_order_v2` for
    /// every validation gate (status, min lots, tick alignment, OI cap,
    /// expiry, flag-bit mask). Different ix entry, identical economic
    /// behavior.
    pub fn place_limit_order_v2_cpi(
        ctx: Context<PlaceLimitOrderV2Cpi>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        flags: u8,
        expires_at_slot: u64,
    ) -> Result<()> {
        // Wrapper-program signer check (~18K CU for the 3 PDA derivations).
        check_wave21_cpi_authority(&ctx.accounts.cpi_authority.key())?;
        let cpi_signer = ctx.accounts.cpi_authority.key();

        // Mirror place_limit_order_v2 intake validation.
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(limit_ticks > 0, FlashBookError::ZeroPrice);
        require!(flags & !0b0011_1111 == 0, FlashBookError::OutOfRange);
        let now_slot = Clock::get()?.slot;
        if expires_at_slot > 0 {
            require!(expires_at_slot > now_slot, FlashBookError::OutOfRange);
        }

        let market = &ctx.accounts.market;
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
            limit_ticks % market.params.tick_size == 0,
            FlashBookError::PriceNotOnTick
        );

        let oi_cap = market.params.max_oi_base_lots;
        if oi_cap > 0 {
            let cur = if side == 0 { market.oi_long_lots } else { market.oi_short_lots };
            let projected = cur.saturating_add(size_lots);
            require!(projected <= oi_cap, FlashBookError::OpenInterestCapExceeded);
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        require!(
            handle.header.market_pubkey == market_key,
            FlashBookError::WrongMarket
        );

        let seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        handle.header.order_seq_counter = seq;

        let side_is_bid = side == 0;
        let order = state_v2::RestingOrderV2 {
            order_id: state_v2::encode_order_id(limit_ticks, seq, side_is_bid),
            seq,
            price_ticks: limit_ticks,
            size_lots,
            expires_at_slot,
            trader: trader_pk,
            last_valid_slot: now_slot as u32,
            side,
            order_type: 0,
            flags,
            _pad: 0,
        };
        let inserted_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };

        emit!(OrderPlacedV2CpiEvent {
            market: market_key,
            trader: trader_pk,
            seq,
            side,
            price_ticks: limit_ticks,
            size_lots,
            node_index: inserted_idx,
            total_orders_after: handle.header.total_orders_active,
            cpi_authority: cpi_signer,
        });
        Ok(())
    }

    /// Wave 21 phase 8b/9b — inverse CPI release of quote collateral
    /// from the protocol's quote_vault to a user's ATA. Only callable
    /// by the 3 wrapper programs' CPI authority PDAs. Used by:
    ///
    ///   • flash-book-flp's `flp_withdraw_v3` — pays an LP burning
    ///     shares back into their ATA
    ///   • flash-book-vaults' `vault_withdraw_v3` — pays a vault
    ///     depositor burning shares back into their ATA
    ///
    /// Core signs the SPL transfer as `InsuranceFundAccount` (the
    /// vault's authority); the wrapper authenticates the WHO + HOW
    /// MUCH via its own state.
    ///
    /// Trust model: same as `place_limit_order_v2_cpi` — wrapper
    /// authority PDAs are whitelisted; once accepted, core trusts
    /// the wrapper's calculation of the release amount. This is
    /// safe because (a) the wrapper signed for a SPECIFIC trader
    /// (off-chain reconciliation against the wrapper's burn event
    /// catches misroutes); (b) total exposure is bounded by the
    /// wrapper's own balance accounting.
    pub fn cpi_release_collateral_to_user(
        ctx: Context<CpiReleaseCollateralToUser>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        // Wrapper signer check — same 3-PDA whitelist as
        // place_limit_order_v2_cpi (helper centralizes the gate).
        check_wave21_cpi_authority(&ctx.accounts.cpi_authority.key())?;
        let cpi_signer = ctx.accounts.cpi_authority.key();

        // SPL transfer signed by InsuranceFund PDA.
        let bump = ctx.accounts.insurance_fund.bump;
        let signer_seeds: &[&[u8]] = &[InsuranceFundAccount::SEED, &[bump]];
        let signers = &[signer_seeds];
        let cpi_accounts = Transfer {
            from: ctx.accounts.quote_vault.to_account_info(),
            to: ctx.accounts.user_quote_ata.to_account_info(),
            authority: ctx.accounts.insurance_fund.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signers,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        emit!(WrapperCollateralReleasedEvent {
            cpi_authority: cpi_signer,
            user: ctx.accounts.user_quote_ata.owner,
            amount: amount_quote_lots,
        });
        Ok(())
    }

    /// WAVE 22 / Phase 5 — vault-trading helpers (wrapper-authorized).
    /// All four ixs here are gated by the same 3-PDA whitelist
    /// (orders / flp / vaults program CPI authority) used by
    /// `place_limit_order_v2_cpi` and `cpi_release_collateral_to_user`.

    /// Bootstrap a `TraderStateAccount` for an arbitrary `trader_pk`
    /// (typically a wrapper-program PDA — e.g. a vault PDA in
    /// flash-book-vaults). Trader cannot sign for themselves because
    /// they are a PDA; this ix lets the wrapper open the account on
    /// their behalf, signed by the wrapper's CPI authority.
    pub fn cpi_open_trader_state_for_trader(
        ctx: Context<CpiOpenTraderStateForTrader>,
    ) -> Result<()> {
        check_wave21_cpi_authority(&ctx.accounts.cpi_authority.key())?;
        let s = &mut ctx.accounts.trader_state;
        s.trader = ctx.accounts.trader_owner.key();
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
        s.builder = Pubkey::default();
        s.builder_max_fee_share_bps = 0;
        s.volume_30d_quote_lots = 0;
        s.volume_window_start_slot = Clock::get()?.slot;
        emit!(WrapperTraderStateOpenedEvent {
            cpi_authority: ctx.accounts.cpi_authority.key(),
            trader: s.trader,
        });
        Ok(())
    }

    /// Credit a `TraderStateAccount.collateral_quote_lots` by `amount`.
    /// Wrapper-authorized — used by `vault_deposit_v3` after the SPL
    /// transfer lands in `quote_vault` so the vault PDA has trading
    /// capital recognized by core's margin gate.
    pub fn cpi_credit_collateral(
        ctx: Context<CpiCreditOrDebitCollateral>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        check_wave21_cpi_authority(&ctx.accounts.cpi_authority.key())?;
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        let s = &mut ctx.accounts.trader_state;
        s.collateral_quote_lots = s
            .collateral_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        emit!(WrapperCollateralCreditedEvent {
            cpi_authority: ctx.accounts.cpi_authority.key(),
            trader: s.trader,
            amount: amount_quote_lots,
            new_collateral: s.collateral_quote_lots,
        });
        Ok(())
    }

    /// Debit a `TraderStateAccount.collateral_quote_lots` by `amount`.
    /// Wrapper-authorized — used by `vault_withdraw_v3` BEFORE the
    /// inverse-CPI SPL release so the vault's core collateral matches
    /// what's been paid out.
    pub fn cpi_debit_collateral(
        ctx: Context<CpiCreditOrDebitCollateral>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        check_wave21_cpi_authority(&ctx.accounts.cpi_authority.key())?;
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        let s = &mut ctx.accounts.trader_state;
        s.collateral_quote_lots = s
            .collateral_quote_lots
            .checked_sub(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        emit!(WrapperCollateralDebitedEvent {
            cpi_authority: ctx.accounts.cpi_authority.key(),
            trader: s.trader,
            amount: amount_quote_lots,
            new_collateral: s.collateral_quote_lots,
        });
        Ok(())
    }

    /// Wave 22 / Phase 5 — wrapper-authorized cancel of a resting v2
    /// order. Counterpart to `place_limit_order_v2_cpi`. Used by
    /// `flash-book-vaults`' `vault_cancel_order_v3` so the strategist
    /// can manage vault-PDA orders.
    pub fn cancel_order_v2_cpi(
        ctx: Context<CancelOrderV2Cpi>,
        side: u8,
        order_id: u64,
    ) -> Result<()> {
        check_wave21_cpi_authority(&ctx.accounts.cpi_authority.key())?;
        require!(side <= 1, FlashBookError::OutOfRange);
        let trader_pk = ctx.accounts.trader.key();
        let market_key = ctx.accounts.market.key();

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        require!(
            handle.header.market_pubkey == market_key,
            FlashBookError::WrongMarket
        );

        let side_is_bid = side == 0;
        let idx = if side_is_bid {
            handle.lookup_bid_by_order_id(order_id)
        } else {
            handle.lookup_ask_by_order_id(order_id)
        };
        require!(
            idx != crate::hypertree::NIL,
            FlashBookError::LiquidationStale
        );

        let order_seq = {
            let order = handle.order_at(idx);
            require!(order.trader == trader_pk, FlashBookError::WrongTrader);
            order.seq
        };

        if side_is_bid {
            handle.remove_bid_node(idx);
        } else {
            handle.remove_ask_node(idx);
        }

        emit!(OrderCancelledV2Event {
            market: market_key,
            trader: trader_pk,
            order_seq,
            side,
            node_index: idx,
            total_orders_after: handle.header.total_orders_active,
        });
        Ok(())
    }

    /// V2 cancel: remove a resting order from the hypertree. Validates
    /// that the caller is the original trader (orders carry trader pubkey
    /// inline in wave 18 — wave 19 indirects through a seat). Refunds no
    /// SPL tokens — the v2 book has no escrow yet (wave 19's free-funds
    /// optimisation handles that).
    ///
    /// The off-chain SDK derives `order_id` via
    /// `encode_order_id(price_ticks, seq, side == 0)` from the
    /// `OrderPlacedV2Event` fields.
    pub fn cancel_order_v2(
        ctx: Context<CancelOrderV2>,
        side: u8,
        order_id: u64,
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        let trader_pk = ctx.accounts.trader.key();
        let market_key = ctx.accounts.market.key();

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        require!(
            handle.header.market_pubkey == market_key,
            FlashBookError::WrongMarket
        );

        let side_is_bid = side == 0;
        let idx = if side_is_bid {
            handle.lookup_bid_by_order_id(order_id)
        } else {
            handle.lookup_ask_by_order_id(order_id)
        };
        require!(
            idx != crate::hypertree::NIL,
            FlashBookError::LiquidationStale
        );

        // Ownership check — only the original trader can cancel.
        let order_seq = {
            let order = handle.order_at(idx);
            require!(order.trader == trader_pk, FlashBookError::WrongTrader);
            order.seq
        };

        if side_is_bid {
            handle.remove_bid_node(idx);
        } else {
            handle.remove_ask_node(idx);
        }

        emit!(OrderCancelledV2Event {
            market: market_key,
            trader: trader_pk,
            order_seq,
            side,
            node_index: idx,
            total_orders_after: handle.header.total_orders_active,
        });
        Ok(())
    }

    /// V2 read-side: emit the top-N levels of the hypertree-backed book
    /// as an event. Walks `for_each_bid_best_first` / `for_each_ask_best_first`
    /// and packs the first BOOK_DEPTH_LEVELS=4 of each side into a single
    /// `BookDepthV2Event`. Pure read — never mutates state.
    ///
    /// This is the wave-18e validation that the RBT iteration is correct:
    /// after a series of `place_limit_order_v2` calls, the bids-best-first
    /// walk must yield highest-priced bids first and asks-best-first walk
    /// must yield lowest-priced asks first. The wave-18f matcher consumes
    /// these same iterators when clearing crossed orders.
    pub fn view_book_depth_v2(ctx: Context<ViewBookDepthV2>) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let book_data = ctx.accounts.market_book.try_borrow_data()?;
        // Local Vec — hot path is fine since this is a view ix called
        // out-of-band (not on the matcher hot path).
        let mut book_data_owned = book_data.to_vec();
        let handle = state_v2::MarketBookHandle::from_account_data(&mut book_data_owned)?;
        require!(
            handle.header.market_pubkey == market_key,
            FlashBookError::WrongMarket
        );

        let mut bids: Vec<BookLevelV2> = Vec::with_capacity(BOOK_DEPTH_LEVELS);
        handle.for_each_bid_best_first(|_idx, order| {
            bids.push(BookLevelV2 {
                price_ticks: order.price_ticks,
                size_lots: order.size_lots,
                seq: order.seq,
                trader: order.trader,
            });
            bids.len() < BOOK_DEPTH_LEVELS
        });

        let mut asks: Vec<BookLevelV2> = Vec::with_capacity(BOOK_DEPTH_LEVELS);
        handle.for_each_ask_best_first(|_idx, order| {
            asks.push(BookLevelV2 {
                price_ticks: order.price_ticks,
                size_lots: order.size_lots,
                seq: order.seq,
                trader: order.trader,
            });
            asks.len() < BOOK_DEPTH_LEVELS
        });

        emit!(BookDepthV2Event {
            market: market_key,
            total_orders_active: handle.header.total_orders_active,
            bids,
            asks,
        });
        Ok(())
    }

    /// V2 matcher tick: run an FBA Walrasian clearing over the resting
    /// hypertree book, plus the full v1 bookkeeping suite, plus three
    /// improvements over Hyperliquid / Manifest / Phoenix:
    ///
    ///   1. **Vol-adaptive oracle band** — the `oracle_band_bps` cap on
    ///      the new mark widens proportionally with realized volatility,
    ///      so legitimate vol moves aren't clamped (HL uses fixed pct →
    ///      over-clamps during real moves; Phoenix has no band).
    ///   2. **VPIN-gated FLP pause** — when toxicity (VPIN) exceeds
    ///      `FLP_VPIN_PAUSE_THRESHOLD_BPS`, FLP virtual quotes are
    ///      skipped this batch. Manifest has no LP, Phoenix has no auto-
    ///      pause, HL has no LP-protection signal — this protects LP
    ///      capital from toxic flow at the matcher level (not just at
    ///      the per-fill toxicity-tax level).
    ///   3. **EMA-blended funding rate** — the per-batch dampened rate is
    ///      blended 50/50 with the prior `last_funding_rate_bps_per_sec`
    ///      via EMA, smoothing the funding curve (HL recomputes from
    ///      scratch each block — high microstructure noise).
    ///
    /// Off-chain settlement is identical to v1 — fills emit
    /// `FillAppliedEvent`-shaped `BatchClearedEvent`, the sequencer
    /// dispatches per-fill `apply_fill` / `apply_flp_fill` ixs.
    pub fn run_batch_v2(ctx: Context<RunBatchV2>, now_ms: u64) -> Result<()> {
        let market_key = ctx.accounts.market.key();

        // ─── Phase 0: walk the hypertree to harvest live orders ──────────
        let now_slot = Clock::get()?.slot;
        let max_per_side = MAX_BATCH_ORDERS_PER_SIDE_V2;
        // Note: do NOT use `Vec::with_capacity(2 * max_per_side)` here.
        // BPF heap is 32KB by default — 2 × 256 entries × ~80 bytes
        // would alloc 40KB upfront and OOM the matcher on small books.
        // Vec::new() defers allocation until push(); typical books
        // have far fewer than `max_per_side` orders so the actual
        // alloc stays well under the heap limit.
        let mut orders: Vec<matcher::order::Order> = Vec::new();
        let mut sources: Vec<(u64, hypertree::DataIndex, bool)> = Vec::new();
        {
            let book_data = ctx.accounts.market_book.try_borrow_data()?;
            let mut book_data_owned = book_data.to_vec();
            let handle =
                state_v2::MarketBookHandle::from_account_data(&mut book_data_owned)?;
            require!(
                handle.header.market_pubkey == market_key,
                FlashBookError::WrongMarket
            );
            handle.for_each_bid_best_first(|idx, o| {
                if orders.len() >= max_per_side
                    || (o.expires_at_slot > 0 && now_slot > o.expires_at_slot)
                {
                    return orders.len() < max_per_side;
                }
                orders.push(matcher::order::Order {
                    id: o.order_id,
                    trader: o.trader,
                    side: matcher::order::Side::Long,
                    order_type: order_type_byte_to_matcher(o.order_type),
                    size: matcher::lot::BaseLots(o.size_lots),
                    limit_price: matcher::lot::Ticks(o.price_ticks),
                    seq: o.seq,
                    post_only: (o.flags & 0b0000_0001) != 0,
                    stp_mode: matcher::order::StpMode::from_u8((o.flags >> 4) & 0b11),
                });
                sources.push((o.order_id, idx, true));
                true
            });
            let bids_loaded = orders.len();
            handle.for_each_ask_best_first(|idx, o| {
                if (orders.len() - bids_loaded) >= max_per_side
                    || (o.expires_at_slot > 0 && now_slot > o.expires_at_slot)
                {
                    return (orders.len() - bids_loaded) < max_per_side;
                }
                orders.push(matcher::order::Order {
                    id: o.order_id,
                    trader: o.trader,
                    side: matcher::order::Side::Short,
                    order_type: order_type_byte_to_matcher(o.order_type),
                    size: matcher::lot::BaseLots(o.size_lots),
                    limit_price: matcher::lot::Ticks(o.price_ticks),
                    seq: o.seq,
                    post_only: (o.flags & 0b0000_0001) != 0,
                    stp_mode: matcher::order::StpMode::from_u8((o.flags >> 4) & 0b11),
                });
                sources.push((o.order_id, idx, false));
                true
            });
        }

        let market = &mut ctx.accounts.market;
        let commit_buffer = &mut ctx.accounts.commit_buffer;
        let flp = &ctx.accounts.flp_exposure;

        // ─── Phase 1: advance funding (parity-port from v1 run_batch) ────
        let block_delta_ms = if market.last_batch_ms == 0 {
            0
        } else {
            now_ms.saturating_sub(market.last_batch_ms)
        };
        let mark_for_funding = if market.params.funding_premium_twap_window > 0 {
            let win = (market.params.funding_premium_twap_window as usize)
                .min(MARK_HISTORY_LEN)
                .min(market.recent_clearing_count as usize);
            if win == 0 {
                matcher::lot::Ticks(market.mark_price_ticks)
            } else {
                let mut sum: u128 = 0;
                let len = MARK_HISTORY_LEN;
                let newest_idx = if market.current_batch == 0 {
                    0
                } else {
                    (market.current_batch as usize - 1) % len
                };
                for k in 0..win {
                    let idx = (newest_idx + len - k) % len;
                    sum = sum.saturating_add(market.recent_clearing_prices[idx] as u128);
                }
                let avg = (sum / win as u128).min(u64::MAX as u128) as u64;
                if avg == 0 {
                    matcher::lot::Ticks(market.mark_price_ticks)
                } else {
                    matcher::lot::Ticks(avg)
                }
            }
        } else {
            matcher::lot::Ticks(market.mark_price_ticks)
        };
        let (raw_new_index, ftick) = matcher::funding::advance(
            market.cum_funding_index,
            mark_for_funding,
            matcher::lot::Ticks(market.oracle_price_ticks),
            block_delta_ms,
            market.params.funding_rate_k_bps,
            market.params.funding_rate_max_bps_per_sec,
        )?;

        // OI dampener (parity-port).
        let (new_index, dampened_rate) = if market.params.funding_oi_dampening {
            let total = (market.oi_long_lots as u128)
                .saturating_add(market.oi_short_lots as u128);
            let skew_bps: u128 = if total == 0 {
                0
            } else {
                let imbalance = if market.oi_long_lots >= market.oi_short_lots {
                    (market.oi_long_lots - market.oi_short_lots) as u128
                } else {
                    (market.oi_short_lots - market.oi_long_lots) as u128
                };
                ((imbalance.saturating_mul(constants::BPS_DENOM as u128)) / total)
                    .min(constants::BPS_DENOM as u128)
            };
            let index_delta = raw_new_index.saturating_sub(market.cum_funding_index);
            let scaled_delta = ((index_delta as i128).saturating_mul(skew_bps as i128))
                / (constants::BPS_DENOM as i128);
            let scaled_index = market.cum_funding_index.saturating_add(scaled_delta);
            let scaled_rate = ((ftick.rate_bps_per_sec as i128)
                .saturating_mul(skew_bps as i128))
                / (constants::BPS_DENOM as i128);
            (scaled_index, clamp_i128_to_i64(scaled_rate))
        } else {
            (raw_new_index, ftick.rate_bps_per_sec)
        };
        market.cum_funding_index = new_index;

        // SMARTER-THAN-HL #1: EMA-blend the dampened rate with the prior
        // posted rate. See `matcher::v2_bookkeeping::ema_blend_funding_rate`
        // for the why + math + edge cases.
        let blended_rate = matcher::v2_bookkeeping::ema_blend_funding_rate(
            market.last_funding_rate_bps_per_sec,
            dampened_rate,
            market.current_batch == 0,
        );
        market.last_funding_rate_bps_per_sec = blended_rate;

        // Per-period funding cap (parity-port).
        if market.params.funding_per_period_max_bps > 0
            && market.params.funding_period_seconds > 0
        {
            let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            if market.period_started_at_unix == 0
                || now_unix.saturating_sub(market.period_started_at_unix)
                    >= market.params.funding_period_seconds as u64
            {
                market.period_started_at_unix = now_unix;
                market.period_funding_paid_abs_bps = 0;
            }
            let raw_delta = new_index.saturating_sub(market.cum_funding_index);
            let abs_delta = raw_delta.unsigned_abs();
            let abs_bps_u128 = abs_delta
                .saturating_mul(constants::BPS_DENOM as u128)
                >> constants::FUNDING_INDEX_FRACTIONAL_BITS;
            let abs_bps = if abs_bps_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                abs_bps_u128 as u64
            };
            let cap = market.params.funding_per_period_max_bps as u64;
            let prior = market.period_funding_paid_abs_bps;
            let projected = prior.saturating_add(abs_bps);
            if projected > cap {
                let allowed = cap.saturating_sub(prior) as u128;
                let scale_den = abs_bps as u128;
                if scale_den != 0 {
                    let scaled_delta =
                        (raw_delta as i128).saturating_mul(allowed as i128) / scale_den as i128;
                    market.cum_funding_index =
                        market.cum_funding_index.saturating_add(scaled_delta);
                    market.period_funding_paid_abs_bps = cap;
                    let rate_scale = ((blended_rate as i128).saturating_mul(allowed as i128))
                        / scale_den.max(1) as i128;
                    market.last_funding_rate_bps_per_sec = clamp_i128_to_i64(rate_scale);
                    emit!(FundingPeriodCapHitEvent {
                        market: market_key,
                        period_started_at_unix: market.period_started_at_unix,
                        cap_bps: cap,
                        attenuated_rate_bps_per_sec: market.last_funding_rate_bps_per_sec,
                    });
                }
            } else {
                market.period_funding_paid_abs_bps = projected;
            }
        }

        // ─── Phase 2: SMARTER-THAN-MANIFEST #2 — VPIN-gated FLP ──────────
        // Skip FLP virtual quoting when current toxicity (VPIN) exceeds
        // the pause threshold. Protects LP capital from informed flow.
        let realized_vol_bps = realized_vol_bps_from_window(
            &market.recent_clearing_prices,
            market.recent_clearing_count,
        );
        let vpin_bps_now = market.vpin.as_bps();
        let flp_paused = vpin_bps_now >= FLP_VPIN_PAUSE_THRESHOLD_BPS;

        if !flp_paused {
            let flp_pool_capital = flp.total_capital_quote_lots;
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
                let oi_total =
                    market.oi_long_lots.saturating_add(market.oi_short_lots) as u128;
                let notional = oi_total
                    .saturating_mul(market.mark_price_ticks as u128)
                    .saturating_mul(market.params.tick_size as u128);
                ((notional / (flp_pool_capital as u128)).min(constants::BPS_DENOM as u128))
                    as u32
            } else {
                0
            };
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
                oracle_ticks: matcher::lot::Ticks(market.oracle_price_ticks),
                vpin_bps: vpin_bps_now,
                realized_vol_bps,
                pool_capital_quote_lots: flp_pool_capital,
                pool_net_quote_lots_signed: flp_net_signed,
                pool_gross_utilization_bps: utilization_bps,
                oi_long_lots: market.oi_long_lots,
                oi_short_lots: market.oi_short_lots,
            };
            let flp_trader = flp.key();
            let flp_seq_base =
                FLP_SEQ_RESERVED_OFFSET.saturating_add(market.current_batch.saturating_mul(1024));
            let (_flp_out, flp_orders) =
                generate_quotes(flp_params, flp_inputs, flp_trader, flp_seq_base)?;
            for o in flp_orders {
                orders.push(o);
            }
        }

        // ─── Phase 3: FBA Walrasian clearing ─────────────────────────────
        let prior_mark = matcher::lot::Ticks(market.mark_price_ticks);
        let result = matcher::fba::clear_batch(&orders, prior_mark)?;

        // ─── Sequencer feed — emit one event per fill so the off-chain
        //    sequencer can dispatch the matching `apply_fill` /
        //    `apply_flp_fill` ix on mainnet. The matcher itself only
        //    mutates orderbook state; trader/position state lives in
        //    core PDAs that the sequencer settles. Events are how we
        //    bridge the matcher tick (potentially on the ER) to the
        //    settlement ix (always on mainnet).
        for fill in &result.fills {
            emit!(BatchFillIntentEvent {
                market: market_key,
                taker: fill.taker_trader,
                maker: fill.maker_trader,
                taker_side: match fill.taker_side {
                    matcher::order::Side::Long => 0,
                    matcher::order::Side::Short => 1,
                },
                size_lots: fill.size.0,
                price_ticks: fill.price.0,
                taker_id: fill.taker_id,
                maker_id: fill.maker_id,
            });
        }

        // ─── Phase 4: apply fills back to the hypertree ──────────────────
        let mut consumed: Vec<(u64, hypertree::DataIndex, bool, u64)> =
            Vec::with_capacity(sources.len());
        for fill in &result.fills {
            for id in [fill.maker_id, fill.taker_id] {
                let found = sources.iter().find(|(oid, _, _)| *oid == id);
                let Some((src_id, src_idx, src_is_bid)) = found else {
                    continue;
                };
                if let Some(slot) = consumed
                    .iter_mut()
                    .find(|(cid, _, _, _)| *cid == *src_id)
                {
                    slot.3 = slot.3.saturating_add(fill.size.0);
                } else {
                    consumed.push((*src_id, *src_idx, *src_is_bid, fill.size.0));
                }
            }
        }
        if !consumed.is_empty() {
            let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
            let mut handle =
                state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
            for (_, idx, side_is_bid, total_filled) in &consumed {
                let new_size = handle.decrement_size_at(*idx, *total_filled);
                if new_size == 0 {
                    if *side_is_bid {
                        handle.remove_bid_node(*idx);
                    } else {
                        handle.remove_ask_node(*idx);
                    }
                }
            }
        }

        // ─── Phase 5: update mark price (TWAP + VOL-ADAPTIVE band + clamp)
        if result.clearing_volume.0 > 0 {
            let len = MARK_HISTORY_LEN;
            let idx = (market.current_batch as usize) % len;
            market.recent_clearing_prices[idx] = result.clearing_price.0;
            if (market.recent_clearing_count as usize) < len {
                market.recent_clearing_count =
                    market.recent_clearing_count.saturating_add(1);
            }
            let count = market.recent_clearing_count as usize;
            let sum: u128 = market
                .recent_clearing_prices
                .iter()
                .take(count)
                .fold(0u128, |acc, p| acc.saturating_add(*p as u128));
            let twap = sum
                .checked_div(count as u128)
                .unwrap_or(result.clearing_price.0 as u128) as u64;

            // SMARTER-THAN-HL #3: vol-adaptive oracle band. See
            // `matcher::v2_bookkeeping::vol_adaptive_band_bps` for the
            // multiplier curve, the 4× cap rationale, and unit tests.
            let adaptive_band_bps = matcher::v2_bookkeeping::vol_adaptive_band_bps(
                market.params.oracle_band_bps,
                realized_vol_bps,
            ) as u128;
            let band = (market.oracle_price_ticks as u128)
                .saturating_mul(adaptive_band_bps)
                / constants::BPS_DENOM as u128;
            let lo = (market.oracle_price_ticks as u128).saturating_sub(band) as u64;
            let hi = (market.oracle_price_ticks as u128)
                .saturating_add(band)
                .min(u64::MAX as u128) as u64;
            let banded = twap.max(lo).min(hi);

            // Mark-change sanity cap (parity-port).
            let prior = market.mark_price_ticks as u128;
            let cap_bps = market.params.mark_change_max_bps as u128;
            let new_mark = if cap_bps > 0 && prior > 0 {
                let cap_delta = prior.saturating_mul(cap_bps) / constants::BPS_DENOM as u128;
                let cap_lo = prior.saturating_sub(cap_delta) as u64;
                let cap_hi = prior
                    .saturating_add(cap_delta)
                    .min(u64::MAX as u128) as u64;
                let clamped = banded.max(cap_lo).min(cap_hi);
                if clamped != banded {
                    emit!(MarkChangeClampedEvent {
                        market: market_key,
                        batch_num: market.current_batch,
                        unclamped_mark_ticks: banded,
                        clamped_mark_ticks: clamped,
                        prior_mark_ticks: prior as u64,
                    });
                }
                clamped
            } else {
                banded
            };
            market.mark_price_ticks = new_mark;
        }

        // ─── Phase 6: update VPIN (parity-port) ──────────────────────────
        let vpin_bucket = market.params.vpin_bucket_size_lots;
        let vpin_window = market.params.vpin_ema_window;
        for fill in &result.fills {
            market
                .vpin
                .record_fill(fill.taker_side, fill.size.0, vpin_bucket, vpin_window)?;
        }

        // ─── Phase 7: sweep expired commit-bonds (parity-port) ───────────
        let seized = sweep_expired(&mut commit_buffer.commits, market.current_batch);

        // ─── Phase 8: bookkeeping + event ────────────────────────────────
        market.current_batch = market
            .current_batch
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        market.last_batch_ms = now_ms;

        emit!(BatchClearedEvent {
            market: market_key,
            batch_num: market.current_batch,
            clearing_price: result.clearing_price.0,
            clearing_volume: result.clearing_volume.0,
            fill_count: result.fills.len() as u32,
            funding_rate_bps_per_sec: market.last_funding_rate_bps_per_sec,
            seized_bonds: seized,
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
        s.builder = Pubkey::default();
        s.builder_max_fee_share_bps = 0;
        s.volume_30d_quote_lots = 0;
        s.volume_window_start_slot = Clock::get()?.slot;
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
    /// `discount_bps` is bounded to `MAX_FEE_DISCOUNT_BPS` = 12_000 (120%).
    /// Values up to 10_000 are a normal discount (down to zero fee);
    /// 10_000..12_000 enable HL/MM-pro top-tier NEGATIVE fees — the
    /// taker is *paid* for routing flow, with the rebate sourced from
    /// the protocol's own insurance contribution. Apply_fill clamps the
    /// rebate so the trader never extracts more than `max(rebate)` of
    /// notional, and the math respects the maker rebate priority.
    pub fn set_trader_fee_tier(
        ctx: Context<SetTraderFeeTier>,
        discount_bps: u32,
    ) -> Result<()> {
        require!(
            discount_bps <= constants::MAX_FEE_DISCOUNT_BPS,
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

    /// Set the per-position leverage cap. Trader (or delegate) signs.
    /// `cap` must be ∈ [1, market.params.max_leverage]. Pass 0 to clear
    /// (revert to using market default). Hyperliquid pattern: lets risk-
    /// conscious traders limit their effective leverage on a single
    /// position without affecting their other positions or the market's
    /// global cap. The cap is enforced at place_limit_order intake on
    /// the projected post-fill notional.
    ///
    /// Setting a tighter cap on a position that already exceeds it does
    /// NOT force a liquidation — the cap only applies to NEW orders that
    /// would grow the position. To reduce leverage on an existing
    /// position, the trader can add collateral or close part of it.
    pub fn set_position_leverage(
        ctx: Context<SetPositionLeverage>,
        cap: u32,
    ) -> Result<()> {
        let market = &ctx.accounts.market;
        if cap > 0 {
            require!(
                cap <= market.params.max_leverage,
                FlashBookError::LeverageExceeded
            );
        }
        let position = &mut ctx.accounts.position;
        let prev = position.leverage_cap;
        position.leverage_cap = cap;
        emit!(PositionLeverageUpdatedEvent {
            market: market.key(),
            trader: position.trader,
            previous_cap: prev,
            new_cap: cap,
        });
        Ok(())
    }

    /// Cross-margin sweep between two trader accounts under a common
    /// authority. The signer must be the delegate of BOTH source and
    /// destination trader_states. Moves `amount` quote-lots from
    /// source.collateral_quote_lots to dest.collateral_quote_lots
    /// atomically.
    ///
    /// Source can hold OPEN POSITIONS — caller passes [market, position]
    /// pairs in remaining_accounts (count must equal source.open_positions).
    /// Post-sweep margin is evaluated against the joint stress-lattice;
    /// rejects if the source would become unhealthy after the withdrawal.
    /// Source flat → pass `[]` for remaining_accounts (the legacy fast path).
    ///
    /// Cannot sweep to/from the same account.
    pub fn sweep_collateral<'info>(
        ctx: Context<'_, '_, 'info, 'info, SweepCollateral<'info>>,
        amount: u64,
    ) -> Result<()> {
        require!(amount > 0, FlashBookError::ZeroSize);
        let from = &ctx.accounts.from_state;
        let to = &ctx.accounts.to_state;
        require!(from.trader != to.trader, FlashBookError::OutOfRange);
        let signer = ctx.accounts.authority.key();
        require!(from.is_authorized(&signer), FlashBookError::Unauthorized);
        require!(to.is_authorized(&signer), FlashBookError::Unauthorized);

        // Position-aware margin gate. If source has open positions:
        //   1. Walk remaining_accounts as [market, position] pairs.
        //   2. Verify count matches from.open_positions exactly.
        //   3. Build snapshots and assess against the stress lattice
        //      using POST-sweep collateral (`from.collateral - amount`).
        //   4. Reject if not healthy.
        // No open positions → skip the walk (cheap fast path).
        if from.open_positions > 0 {
            let post_sweep_collateral = from
                .collateral_quote_lots
                .checked_sub(amount)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
            let expected = from.open_positions as usize;
            require!(
                ctx.remaining_accounts.len() == expected * 2,
                FlashBookError::OutOfRange
            );

            let mut snaps: Vec<RiskPosSnap> = Vec::with_capacity(expected);
            let mut market_snaps: Vec<RiskMarketSnap> = Vec::with_capacity(expected);
            let mut market_keys: Vec<Pubkey> = Vec::with_capacity(expected);
            for i in 0..expected {
                let market_ai = &ctx.remaining_accounts[i * 2];
                let position_ai = &ctx.remaining_accounts[i * 2 + 1];
                require!(market_ai.owner == ctx.program_id, FlashBookError::OutOfRange);
                require!(position_ai.owner == ctx.program_id, FlashBookError::OutOfRange);

                let market_data = market_ai.try_borrow_data()?;
                let market_acct: MarketAccount =
                    MarketAccount::try_deserialize(&mut &market_data[..])?;
                let position_data = position_ai.try_borrow_data()?;
                let position: state::PositionAccount =
                    state::PositionAccount::try_deserialize(&mut &position_data[..])?;
                require!(position.trader == from.trader, FlashBookError::WrongTrader);
                require!(position.market == market_ai.key(), FlashBookError::WrongMarket);

                snaps.push(RiskPosSnap {
                    market: position.market,
                    side: if position.side == 0 { Side::Long } else { Side::Short },
                    size_lots: position.size_lots,
                    entry_price: Ticks(position.entry_price_ticks),
                    cum_funding_index_at_entry: position.cum_funding_index_at_entry,
                });
                market_snaps.push(RiskMarketSnap {
                    market: market_ai.key(),
                    mark_price: Ticks(market_acct.mark_price_ticks),
                    cum_funding_index: market_acct.cum_funding_index,
                    maintenance_margin_bps: market_acct.params.maintenance_margin_ratio_bps,
                    tick_size: market_acct.params.tick_size,
                    concentration_threshold_lots: market_acct.params.concentration_threshold_lots,
                    concentration_extra_mmr_bps: market_acct.params.concentration_extra_mmr_bps,
                });
                market_keys.push(market_ai.key());
            }
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_fn(
                &snaps,
                &market_snaps,
                &scenarios,
                post_sweep_collateral,
            )?;
            require!(assessment.is_healthy, FlashBookError::TraderLiquidatable);
        }

        let from = &mut ctx.accounts.from_state;
        from.collateral_quote_lots = from
            .collateral_quote_lots
            .checked_sub(amount)
            .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        let to = &mut ctx.accounts.to_state;
        to.collateral_quote_lots = to
            .collateral_quote_lots
            .checked_add(amount)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        emit!(CollateralSweptEvent {
            authority: signer,
            from: ctx.accounts.from_state.trader,
            to: ctx.accounts.to_state.trader,
            amount_quote_lots: amount,
        });
        Ok(())
    }

    /// Crystallize the vault's performance fee. Strategist signs.
    /// If the current NAV/share exceeds the high-water mark, mints new
    /// shares to the strategist's vault_position equal to:
    ///   minted_shares = (gain_per_share × shares_outstanding × perf_fee_bps)
    ///                   / (current_nav_per_share × 10_000)
    /// and bumps the HWM to the post-mint NAV/share. If no gain (or
    /// bootstrap with HWM=0), simply anchors HWM at current NAV/share
    /// without minting.
    ///
    /// Vault must be FLAT (no open positions) so NAV is unambiguous.
    pub fn settle_vault_perf_fee(
        ctx: Context<SettleVaultPerfFee>,
    ) -> Result<()> {
        let vault = &ctx.accounts.vault;
        require!(
            ctx.accounts.strategist.key() == vault.strategist,
            FlashBookError::Unauthorized
        );
        let ts = &ctx.accounts.vault_trader_state;
        require!(ts.open_positions == 0, FlashBookError::SweepRequiresFlat);

        let shares_outstanding = vault.shares_outstanding;
        // No depositors yet → nothing to settle. Anchor HWM at unit price.
        if shares_outstanding == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = constants::USD_UNIT;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        let nav = ts.collateral_quote_lots as u128;
        // Current NAV per share, scaled by USD_UNIT for fixed-point precision.
        // nav_per_share_x6 = nav × USD_UNIT / shares_outstanding
        let nav_per_share_x6 = (nav.saturating_mul(constants::USD_UNIT as u128))
            / (shares_outstanding as u128);
        let nav_per_share_u64 = if nav_per_share_x6 > u64::MAX as u128 {
            u64::MAX
        } else {
            nav_per_share_x6 as u64
        };

        let prev_hwm = vault.hwm_nav_per_share_u64x6;
        // Bootstrap: first ever settle with HWM=0 → just anchor.
        if prev_hwm == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = nav_per_share_u64;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        require!(
            nav_per_share_u64 > prev_hwm,
            FlashBookError::VaultBelowHighWaterMark
        );
        let gain_per_share_x6 = (nav_per_share_u64 - prev_hwm) as u128;
        // Total gain = gain_per_share × shares_outstanding / USD_UNIT
        let total_gain = gain_per_share_x6
            .saturating_mul(shares_outstanding as u128)
            / (constants::USD_UNIT as u128);
        // Fee in quote-lots = total_gain × perf_fee_bps / 10_000
        let fee_quote_lots = total_gain
            .saturating_mul(vault.perf_fee_bps as u128)
            / (constants::BPS_DENOM as u128);
        // Convert fee to shares at current NAV/share:
        //   shares_to_mint = fee_quote_lots × shares_outstanding / nav_after_fee
        // We mint at PRE-fee NAV (standard convention in HWM vaults):
        //   shares_to_mint = fee_quote_lots × shares_outstanding / nav
        require!(nav > 0, FlashBookError::VaultNavNonPositive);
        let shares_to_mint_u128 = fee_quote_lots
            .saturating_mul(shares_outstanding as u128)
            / nav;
        let shares_to_mint = if shares_to_mint_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            shares_to_mint_u128 as u64
        };
        // No-op if rounding pushed it to zero (very small gain).
        if shares_to_mint == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = nav_per_share_u64;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        // Mint to strategist's vault_position.
        let sp = &mut ctx.accounts.strategist_position;
        if sp.depositor == Pubkey::default() {
            sp.depositor = ctx.accounts.strategist.key();
            sp.vault = ctx.accounts.vault.key();
            sp.bump = ctx.bumps.strategist_position;
        }
        sp.shares = sp
            .shares
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        let vault_key = ctx.accounts.vault.key();
        let v = &mut ctx.accounts.vault;
        v.shares_outstanding = v
            .shares_outstanding
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        v.total_perf_shares_minted = v
            .total_perf_shares_minted
            .saturating_add(shares_to_mint);
        // After mint, NAV/share is diluted; recompute and anchor HWM there
        // so the strategist starts the next epoch from the post-fee mark.
        let new_nav_per_share_x6 = (nav.saturating_mul(constants::USD_UNIT as u128))
            / (v.shares_outstanding as u128);
        v.hwm_nav_per_share_u64x6 = if new_nav_per_share_x6 > u64::MAX as u128 {
            u64::MAX
        } else {
            new_nav_per_share_x6 as u64
        };
        v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;

        emit!(VaultPerfFeeSettledEvent {
            vault: vault_key,
            strategist: v.strategist,
            shares_minted: shares_to_mint,
            new_hwm_per_share_u64x6: v.hwm_nav_per_share_u64x6,
        });
        Ok(())
    }

    /// Set or rotate the trader's builder pubkey + the maximum fee share
    /// (in bps of net fee) the trader authorizes the builder to collect.
    /// Pass Pubkey::default() to revoke. Hyperliquid builder-codes model:
    /// a third-party UI/wallet/aggregator routing flow earns a share of
    /// the protocol fee, capped by the user's approved max. Trader signs
    /// — neither the protocol authority nor the builder can install one
    /// unilaterally.
    ///
    /// `max_fee_share_bps` capped at BPS_DENOM (10_000 = 100% of net fee).
    /// The on-chain emit clamps `min(market.params.builder_share_bps,
    /// max_fee_share_bps)`.
    pub fn set_trader_builder(
        ctx: Context<SetTraderBuilder>,
        builder: Pubkey,
        max_fee_share_bps: u32,
    ) -> Result<()> {
        require!(
            max_fee_share_bps <= constants::BPS_DENOM as u32,
            FlashBookError::OutOfRange
        );
        let s = &mut ctx.accounts.trader_state;
        let prev = s.builder;
        s.builder = builder;
        s.builder_max_fee_share_bps = if builder == Pubkey::default() {
            0
        } else {
            max_fee_share_bps
        };
        emit!(TraderBuilderUpdatedEvent {
            trader: s.trader,
            previous: prev,
            new: builder,
            max_fee_share_bps: s.builder_max_fee_share_bps,
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

    /// Partial withdrawal that allows traders with open positions to
    /// pull out collateral above the safety floor. Hyperliquid pattern:
    /// post-withdrawal collateral must satisfy
    /// `remaining >= max(IM_required, WITHDRAWAL_FLOOR_BPS * notional)`
    /// where:
    ///   • IM_required is the standard initial-margin requirement under
    ///     the joint stress lattice (same engine as place_limit_order_v2
    ///     intake — `assess_margin_fn`)
    ///   • WITHDRAWAL_FLOOR_BPS = 1000 (10% of total notional) — HL's
    ///     anti-deposit-then-withdraw guard prevents a trader from
    ///     temporarily topping up to satisfy IM, placing a trade, then
    ///     immediately yanking the temporary collateral leaving only
    ///     enough for maintenance margin (a known footgun in v1
    ///     systems that gate only on IM)
    ///
    /// remaining_accounts layout: alternating (market, position) pairs
    /// for every market the trader has a non-zero position in. Identical
    /// to liquidate_portfolio_v2's walk pattern.
    ///
    /// This ix is ADDITIVE; the existing `withdraw_collateral` (which
    /// requires `open_positions == 0`) remains as the strict-safety
    /// path. A trader with no positions should prefer that ix —
    /// no remaining_accounts walk, smaller fee.
    pub fn partial_withdraw_collateral<'info>(
        ctx: Context<'_, '_, '_, 'info, PartialWithdrawCollateral<'info>>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        // Pre-flight: amount available.
        {
            let s = &ctx.accounts.trader_state;
            require!(
                amount_quote_lots <= s.collateral_quote_lots,
                FlashBookError::InsufficientCollateral,
            );
        }

        // Walk remaining_accounts in (market, position) pairs to build
        // the post-withdrawal margin snapshot.
        let trader_pk = ctx.accounts.trader_state.trader;
        let program_id = ctx.program_id;
        let remaining = ctx.remaining_accounts;
        require!(remaining.len() % 2 == 0, FlashBookError::OutOfRange);

        let mut snaps: Vec<RiskPosSnap> = Vec::new();
        let mut market_snaps: Vec<RiskMarketSnap> = Vec::new();
        let mut market_keys: Vec<Pubkey> = Vec::new();
        let mut total_notional_quote: u128 = 0;

        let mut i = 0usize;
        while i + 1 < remaining.len() {
            let m_ai = &remaining[i];
            let p_ai = &remaining[i + 1];
            require_keys_eq!(*m_ai.owner, *program_id, FlashBookError::Unauthorized);
            require_keys_eq!(*p_ai.owner, *program_id, FlashBookError::Unauthorized);

            let market: MarketAccount =
                MarketAccount::try_deserialize(&mut &m_ai.try_borrow_data()?[..])?;
            let position: state::PositionAccount =
                state::PositionAccount::try_deserialize(&mut &p_ai.try_borrow_data()?[..])?;
            require!(
                position.trader == trader_pk,
                FlashBookError::WrongTrader
            );
            require!(
                position.market == m_ai.key(),
                FlashBookError::WrongMarket
            );

            if position.size_lots > 0 {
                let notional = (position.size_lots as u128)
                    .saturating_mul(market.mark_price_ticks as u128)
                    .saturating_mul(market.params.tick_size as u128);
                total_notional_quote = total_notional_quote.saturating_add(notional);

                snaps.push(RiskPosSnap {
                    market: position.market,
                    side: if position.side == 0 { Side::Long } else { Side::Short },
                    size_lots: position.size_lots,
                    entry_price: Ticks(position.entry_price_ticks),
                    cum_funding_index_at_entry: position.cum_funding_index_at_entry,
                });
                market_snaps.push(RiskMarketSnap {
                    market: m_ai.key(),
                    mark_price: Ticks(market.mark_price_ticks),
                    cum_funding_index: market.cum_funding_index,
                    maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
                    tick_size: market.params.tick_size,
                    concentration_threshold_lots: market.params.concentration_threshold_lots,
                    concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
                });
                market_keys.push(m_ai.key());
            }
            i += 2;
        }

        // Pre-mutate snapshot of the post-withdrawal collateral.
        let post_collateral = ctx
            .accounts
            .trader_state
            .collateral_quote_lots
            .checked_sub(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;

        // Compute the safety floor.
        // (a) IM required under joint stress lattice.
        let im_required: u64 = if snaps.is_empty() {
            0
        } else {
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_fn(
                &snaps,
                &market_snaps,
                &scenarios,
                post_collateral,
            )?;
            assessment.required_quote_lots
        };

        // (b) HL withdrawal floor: 10% of total notional.
        let notional_floor_u128 = total_notional_quote
            .saturating_mul(WITHDRAWAL_FLOOR_BPS as u128)
            / (constants::BPS_DENOM as u128);
        let notional_floor = if notional_floor_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            notional_floor_u128 as u64
        };

        let floor = im_required.max(notional_floor);
        require!(
            post_collateral >= floor,
            FlashBookError::InsufficientCollateral
        );

        // SPL transfer (identical to withdraw_collateral).
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
        s.collateral_quote_lots = post_collateral;
        emit!(PartialCollateralWithdrawnEvent {
            trader: s.trader,
            amount: amount_quote_lots,
            new_balance: s.collateral_quote_lots,
            im_required,
            notional_floor,
            applied_floor: floor,
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

        // ── Wave 22 phase 2 — tier-resolved fees per trader ───────────
        // When the FeeTiersAccount is supplied, resolve each trader's
        // (maker_rebate_bps, taker_fee_bps) from their rolling-window
        // volume against the global tier table. Falls back to flat
        // market.params when no tier table is provided.
        //
        // Pre-fill volume is used so the trader doesn't get instant
        // promotion within their own promotion-trade. Post-fill tier
        // is computed at the bottom for the upgrade event.
        let (
            taker_maker_rebate_bps,
            taker_taker_fee_bps,
            maker_maker_rebate_bps,
            maker_taker_fee_bps,
            tier_pairs,
        ): (i32, u32, i32, u32, Vec<(u64, i32, u32)>) = if let Some(ft) = &ctx.accounts.fee_tiers {
            let pairs: Vec<(u64, i32, u32)> = ft.tiers[..ft.tier_count as usize]
                .iter()
                .map(|t| (t.min_volume_quote_lots, t.maker_rebate_bps, t.taker_fee_bps))
                .collect();
            let taker_volume = ctx.accounts.taker_trader_state.volume_30d_quote_lots;
            let maker_volume = ctx.accounts.maker_trader_state.volume_30d_quote_lots;
            let (tm, tt) = matcher::risk::resolve_fee_tier(
                market.params.maker_rebate_bps,
                market.params.taker_fee_bps,
                &pairs,
                taker_volume,
            );
            let (mm, mt) = matcher::risk::resolve_fee_tier(
                market.params.maker_rebate_bps,
                market.params.taker_fee_bps,
                &pairs,
                maker_volume,
            );
            (tm, tt, mm, mt, pairs)
        } else {
            (
                market.params.maker_rebate_bps,
                market.params.taker_fee_bps,
                market.params.maker_rebate_bps,
                market.params.taker_fee_bps,
                Vec::new(),
            )
        };
        // Capture pre-fill tier indices for upgrade-event detection.
        let pre_taker_tier_index = tier_index_for_volume(
            &tier_pairs,
            ctx.accounts.taker_trader_state.volume_30d_quote_lots,
        );
        let pre_maker_tier_index = tier_index_for_volume(
            &tier_pairs,
            ctx.accounts.maker_trader_state.volume_30d_quote_lots,
        );
        // Suppress unused-warning when no tier table is supplied.
        let _ = (taker_maker_rebate_bps, maker_taker_fee_bps);

        let base_taker_fee_u128 =
            notional_u128.saturating_mul(taker_taker_fee_bps as u128) / constants::BPS_DENOM as u128;
        // Apply taker's per-trader fee tier discount.
        //   discount ≤ 10_000 (100%) → standard discount, fee ≥ 0
        //   discount ∈ (10_000, 12_000] → NEGATIVE fee (rebate to taker)
        // Capped at MAX_FEE_DISCOUNT_BPS = 12_000 (120%). Negative fee
        // resolves to a credit on taker collateral; the rebate is sourced
        // from the protocol's insurance contribution downstream so it
        // can't push the insurance fund negative.
        let discount_bps_full = ctx.accounts.taker_trader_state.fee_discount_bps as u128;
        let discount_bps = discount_bps_full.min(constants::MAX_FEE_DISCOUNT_BPS as u128);
        let mut taker_fee_u128 = base_taker_fee_u128;
        let mut taker_negative_rebate_u128: u128 = 0;
        if discount_bps <= constants::BPS_DENOM as u128 {
            if discount_bps > 0 {
                taker_fee_u128 = base_taker_fee_u128
                    .saturating_mul((constants::BPS_DENOM as u128).saturating_sub(discount_bps))
                    / constants::BPS_DENOM as u128;
            }
        } else {
            // Negative-fee tier: fee = 0, rebate = base × (discount - 10_000) / 10_000
            let neg_bps = discount_bps - constants::BPS_DENOM as u128;
            taker_negative_rebate_u128 = base_taker_fee_u128.saturating_mul(neg_bps)
                / constants::BPS_DENOM as u128;
            taker_fee_u128 = 0;
        }
        // Effective maker rate = maker's TIER-RESOLVED maker_rebate_bps
        // (SIGNED) + JIT bonus (if taker was tagged). JIT bonus comes
        // out of the protocol — paid by reducing the insurance
        // contribution downstream. This is the Drift JIT economic
        // model.
        //
        // Sign semantics (wave 22 / negative-fee tiers):
        //   • effective_rebate_bps_signed > 0  → maker is PAID a rebate
        //   • effective_rebate_bps_signed < 0  → maker PAYS a fee
        //   • effective_rebate_bps_signed == 0 → no maker fee or rebate
        let mut effective_rebate_bps_signed: i128 = maker_maker_rebate_bps as i128;
        if taker_was_jit {
            effective_rebate_bps_signed = effective_rebate_bps_signed
                .saturating_add(market.params.jit_bonus_rebate_bps as i128);
        }
        // Split into rebate (positive bps) and maker_fee (negative bps).
        let (maker_rebate_u128, maker_fee_u128) = if effective_rebate_bps_signed >= 0 {
            let r = notional_u128
                .saturating_mul(effective_rebate_bps_signed as u128)
                / constants::BPS_DENOM as u128;
            (r, 0u128)
        } else {
            let f = notional_u128
                .saturating_mul((-effective_rebate_bps_signed) as u128)
                / constants::BPS_DENOM as u128;
            (0u128, f)
        };
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
        let maker_fee = if maker_fee_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            maker_fee_u128 as u64
        };
        // Net fee to insurance fund = taker_fee + maker_fee − maker_rebate.
        // (maker_fee and maker_rebate are mutually exclusive by the
        // sign split above.)
        let net_fee = taker_fee.saturating_add(maker_fee).saturating_sub(maker_rebate);
        let taker_side_enum = if taker_side == 0 { Side::Long } else { Side::Short };
        let maker_side_enum = taker_side_enum.opposite();
        let taker_trader_pk = ctx.accounts.taker_trader_state.trader;
        let maker_trader_pk = ctx.accounts.maker_trader_state.trader;

        // Apply fees BEFORE position state is mutated, so reads are clean.
        // Taker pays fee from collateral (must have it; place_limit_order's
        // margin gate ensured this at intake time, but we double-check).
        // For NEGATIVE-fee tier traders, taker_fee == 0 and we credit the
        // taker the rebate sourced from the protocol contribution.
        {
            let taker_state = &mut ctx.accounts.taker_trader_state;
            if taker_fee > 0 {
                taker_state.collateral_quote_lots = taker_state
                    .collateral_quote_lots
                    .checked_sub(taker_fee)
                    .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
            }
            if taker_negative_rebate_u128 > 0 {
                let neg_rebate_u64 = if taker_negative_rebate_u128 > u64::MAX as u128 {
                    u64::MAX
                } else {
                    taker_negative_rebate_u128 as u64
                };
                taker_state.collateral_quote_lots = taker_state
                    .collateral_quote_lots
                    .checked_add(neg_rebate_u64)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            }
        }
        // Maker receives rebate (positive `maker_rebate_bps` path) OR
        // pays a fee (negative path — wave 22 retail tier). Mutually
        // exclusive: at most one of `maker_rebate` / `maker_fee` is
        // non-zero per the sign split above.
        {
            let maker_state = &mut ctx.accounts.maker_trader_state;
            if maker_rebate > 0 {
                maker_state.collateral_quote_lots = maker_state
                    .collateral_quote_lots
                    .checked_add(maker_rebate)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            }
            if maker_fee > 0 {
                maker_state.collateral_quote_lots = maker_state
                    .collateral_quote_lots
                    .checked_sub(maker_fee)
                    .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
            }
        }
        // Net fee to insurance fund (per fee_contribution_bps).
        // For negative-fee tier the contribution is reduced by what we
        // paid out as taker rebate — protocol absorbs the cost from its
        // share, never from maker rebate or insurance balance.
        {
            let fund = &mut ctx.accounts.insurance_fund;
            let contribution = (net_fee as u128)
                .saturating_mul(fund.fee_contribution_bps as u128)
                .checked_div(constants::BPS_DENOM as u128)
                .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
            let contribution = contribution.saturating_sub(taker_negative_rebate_u128);
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

        // ── Builder code attribution (Hyperliquid builder-codes) ─────
        // When the taker has approved a builder, emit BuilderFeeOwedEvent
        // for off-chain accrual. Rate = min(market builder_share_bps,
        // trader-approved cap). Pull-based (no on-chain builder account
        // walk) keeps ApplyFill's account list bounded.
        let taker_builder = ctx.accounts.taker_trader_state.builder;
        let trader_builder_cap = ctx.accounts.taker_trader_state.builder_max_fee_share_bps;
        if taker_builder != Pubkey::default()
            && market.params.builder_share_bps > 0
            && trader_builder_cap > 0
        {
            let effective_bps =
                market.params.builder_share_bps.min(trader_builder_cap) as u128;
            let share =
                ((net_fee as u128).saturating_mul(effective_bps) / (constants::BPS_DENOM as u128)) as u64;
            if share > 0 {
                emit!(BuilderFeeOwedEvent {
                    taker: ctx.accounts.taker_trader_state.trader,
                    builder: taker_builder,
                    amount_quote_lots: share,
                });
            }
        }

        // ── HIP-3 creator share (permissionless market deployer) ─────
        // When the market was deployed permissionlessly, credit the
        // deployer with `creator_share_bps` of net fee. Pull-based
        // event — no on-chain creator account walk on the hot path.
        let market_creator = market.creator;
        if market_creator != Pubkey::default() && market.params.creator_share_bps > 0 {
            let share = ((net_fee as u128)
                .saturating_mul(market.params.creator_share_bps as u128)
                / (constants::BPS_DENOM as u128)) as u64;
            if share > 0 {
                emit!(CreatorFeeOwedEvent {
                    market: market_key,
                    creator: market_creator,
                    amount_quote_lots: share,
                });
            }
        }

        // ── Trading-rewards / points emit (Hyperliquid HYPE distrib) ─
        // Lightweight per-fill event with notional + side. Off-chain
        // accrual computes per-trader points (volume × multipliers).
        // No on-chain bookkeeping → no extra account writes; subgraphs
        // index these events cheaply. Emit only when the market opts in
        // (taker_fee_bps > 0 implies a real economic fill — skips $0
        // fee-tier wash trades).
        if market.params.taker_fee_bps > 0 {
            let notional_quote_lots = if notional_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                notional_u128 as u64
            };
            emit!(TradingRewardEligibleEvent {
                market: market_key,
                taker: taker_trader_pk,
                maker: maker_trader_pk,
                notional_quote_lots,
                taker_side,
            });

            // ── Wave 22 — credit rolling window volume for fee tiers ─
            // Both maker and taker get credited the full notional
            // (HL pattern — "30-day taker + maker volume"). Window
            // expiry resets first, so a fill after the window boundary
            // re-anchors at this fill's notional.
            let now_slot = Clock::get()?.slot;
            credit_volume_for_tier(
                &mut ctx.accounts.taker_trader_state,
                notional_quote_lots,
                now_slot,
            );
            credit_volume_for_tier(
                &mut ctx.accounts.maker_trader_state,
                notional_quote_lots,
                now_slot,
            );

            // ── Wave 22 phase 2 — emit TraderTierUpgradedEvent on
            //    tier boundary crossings (silent on no-change).
            if !tier_pairs.is_empty() {
                let post_taker_tier_index = tier_index_for_volume(
                    &tier_pairs,
                    ctx.accounts.taker_trader_state.volume_30d_quote_lots,
                );
                if post_taker_tier_index != pre_taker_tier_index {
                    emit!(TraderTierUpgradedEvent {
                        trader: taker_trader_pk,
                        previous_tier_index: pre_taker_tier_index,
                        new_tier_index: post_taker_tier_index,
                        volume_quote_lots: ctx
                            .accounts
                            .taker_trader_state
                            .volume_30d_quote_lots,
                    });
                }
                let post_maker_tier_index = tier_index_for_volume(
                    &tier_pairs,
                    ctx.accounts.maker_trader_state.volume_30d_quote_lots,
                );
                if post_maker_tier_index != pre_maker_tier_index {
                    emit!(TraderTierUpgradedEvent {
                        trader: maker_trader_pk,
                        previous_tier_index: pre_maker_tier_index,
                        new_tier_index: post_maker_tier_index,
                        volume_quote_lots: ctx
                            .accounts
                            .maker_trader_state
                            .volume_30d_quote_lots,
                    });
                }
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

        // ── Multi-threshold margin warning ──────────────────────────
        // Single-position equity-vs-MMR view (cheap, no portfolio walk):
        //   equity   = collateral + unrealized_pnl(pos, mark)
        //   required = position_notional × mmr_bps / 10_000
        // Emit on threshold crossings (250%/200%/125%) so off-chain UIs
        // can push pre-liquidation alerts. Hyperliquid pattern.
        for (pos, trader_pk, collateral) in [
            (
                &*ctx.accounts.taker_position,
                taker_trader_pk,
                ctx.accounts.taker_trader_state.collateral_quote_lots,
            ),
            (
                &*ctx.accounts.maker_position,
                maker_trader_pk,
                ctx.accounts.maker_trader_state.collateral_quote_lots,
            ),
        ] {
            if pos.size_lots == 0 {
                continue;
            }
            emit_margin_threshold_if_crossed(
                trader_pk,
                market_key,
                pos,
                market.mark_price_ticks,
                market.params.tick_size,
                market.params.maintenance_margin_ratio_bps,
                collateral,
            );
        }

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

    /// Initialize the per-market leverage-tier table — wave 20a.
    /// Hyperliquid pattern: per-asset MMR scales with notional. A trader
    /// holding $25M of BTC pays a higher MMR rate than one holding
    /// $100K of BTC.
    ///
    /// Authority-gated: only the market authority can init / update.
    /// `tiers` MUST be sorted ascending by `min_notional_quote_lots`,
    /// non-empty, length ≤ MAX_LEVERAGE_TIERS = 8. Each tier's
    /// `mmr_bps` MUST be ≥ market.params.maintenance_margin_bps
    /// (tiers can only INCREASE MMR vs the baseline).
    ///
    /// A market with no tiers PDA falls back to the existing 2-tier
    /// model (baseline + concentration_extra_mmr_bps).
    pub fn init_market_leverage_tiers(
        ctx: Context<InitMarketLeverageTiers>,
        tiers: Vec<LeverageTier>,
    ) -> Result<()> {
        validate_leverage_tiers(&ctx.accounts.market, &tiers)?;

        let acct = &mut ctx.accounts.leverage_tiers;
        acct.market = ctx.accounts.market.key();
        acct.bump = ctx.bumps.leverage_tiers;
        acct.tier_count = tiers.len() as u8;
        acct._pad0 = [0u8; 6];
        acct.tiers = [LeverageTier::default(); MAX_LEVERAGE_TIERS];
        for (i, t) in tiers.iter().enumerate() {
            acct.tiers[i] = LeverageTier {
                min_notional_quote_lots: t.min_notional_quote_lots,
                mmr_bps: t.mmr_bps,
                _pad: [0u8; 4],
            };
        }

        emit!(MarketLeverageTiersInitializedEvent {
            market: acct.market,
            tier_count: acct.tier_count,
        });
        Ok(())
    }

    /// Update an existing per-market leverage-tier table. Same
    /// validation as init. Authority-only.
    pub fn update_market_leverage_tiers(
        ctx: Context<UpdateMarketLeverageTiers>,
        tiers: Vec<LeverageTier>,
    ) -> Result<()> {
        validate_leverage_tiers(&ctx.accounts.market, &tiers)?;

        let acct = &mut ctx.accounts.leverage_tiers;
        acct.tier_count = tiers.len() as u8;
        acct.tiers = [LeverageTier::default(); MAX_LEVERAGE_TIERS];
        for (i, t) in tiers.iter().enumerate() {
            acct.tiers[i] = LeverageTier {
                min_notional_quote_lots: t.min_notional_quote_lots,
                mmr_bps: t.mmr_bps,
                _pad: [0u8; 4],
            };
        }

        emit!(MarketLeverageTiersUpdatedEvent {
            market: acct.market,
            tier_count: acct.tier_count,
        });
        Ok(())
    }

    // ─── Wave 22 — Multi-tier fee table (volume-based) ───────────────

    /// Initialize the protocol-wide fee tier table. Authority signs.
    /// Multi-tier volume-based fees — HL / Binance / dYdX standard.
    /// `volume_window_slots` = how many slots the rolling window covers
    /// (HL: 14d ≈ 3_024_000 slots @ 0.4s/slot). Tiers MUST be:
    ///   • non-empty, length ≤ MAX_FEE_TIERS = 10
    ///   • sorted ascending by `min_volume_quote_lots`
    ///   • first tier has `min_volume_quote_lots == 0` (default tier)
    ///   • monotone improving: each tier's `taker_fee_bps` ≤ prior
    ///     tier's, each tier's `maker_rebate_bps` ≥ prior tier's
    ///   • all bps within `MAX_FEE_TIER_BPS = 1_000` (10%) — guards
    ///     against an authority typo locking traders into 90%+ fees
    ///
    /// Markets without this account fall back to flat
    /// `MarketAccount.params.{maker_rebate_bps, taker_fee_bps}`.
    pub fn init_fee_tiers(
        ctx: Context<InitFeeTiers>,
        volume_window_slots: u64,
        tiers: Vec<state::FeeTier>,
    ) -> Result<()> {
        validate_fee_tiers(volume_window_slots, &tiers)?;

        let acct = &mut ctx.accounts.fee_tiers;
        acct.authority = ctx.accounts.authority.key();
        acct.bump = ctx.bumps.fee_tiers;
        acct.tier_count = tiers.len() as u8;
        acct._pad0 = [0u8; 6];
        acct.volume_window_slots = volume_window_slots;
        acct.tiers = [state::FeeTier::default(); state::MAX_FEE_TIERS];
        for (i, t) in tiers.iter().enumerate() {
            acct.tiers[i] = *t;
        }

        emit!(FeeTiersInitializedEvent {
            authority: acct.authority,
            tier_count: acct.tier_count,
            volume_window_slots,
        });
        Ok(())
    }

    /// Update the global fee tier table. Same validation as init.
    /// Authority-only.
    pub fn update_fee_tiers(
        ctx: Context<UpdateFeeTiers>,
        volume_window_slots: u64,
        tiers: Vec<state::FeeTier>,
    ) -> Result<()> {
        validate_fee_tiers(volume_window_slots, &tiers)?;
        require_keys_eq!(
            ctx.accounts.fee_tiers.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );

        let acct = &mut ctx.accounts.fee_tiers;
        acct.tier_count = tiers.len() as u8;
        acct.volume_window_slots = volume_window_slots;
        acct.tiers = [state::FeeTier::default(); state::MAX_FEE_TIERS];
        for (i, t) in tiers.iter().enumerate() {
            acct.tiers[i] = *t;
        }

        emit!(FeeTiersUpdatedEvent {
            authority: acct.authority,
            tier_count: acct.tier_count,
            volume_window_slots,
        });
        Ok(())
    }

    /// View ix — emit the trader's effective fee tier (maker rebate +
    /// taker fee bps) given their current rolling-window volume + the
    /// global tier table. UIs simulate the tx and read the event for
    /// display ("Your tier: VIP3 — 0.025% / 0.05%"). Permissionless.
    ///
    /// Window expiry semantics: if `now - volume_window_start_slot >
    /// volume_window_slots`, the trader's effective volume is treated
    /// as 0 (window has reset; they fall to tier 0 until the next fill
    /// re-anchors). This matches what the next apply_fill will see.
    pub fn view_trader_effective_tier(ctx: Context<ViewTraderEffectiveTier>) -> Result<()> {
        let s = &ctx.accounts.trader_state;
        let f = &ctx.accounts.fee_tiers;
        let now = Clock::get()?.slot;

        let effective_volume = if now.saturating_sub(s.volume_window_start_slot)
            > f.volume_window_slots
        {
            0
        } else {
            s.volume_30d_quote_lots
        };

        let pairs: Vec<(u64, i32, u32)> = f.tiers[..f.tier_count as usize]
            .iter()
            .map(|t| (t.min_volume_quote_lots, t.maker_rebate_bps, t.taker_fee_bps))
            .collect();
        let (maker_bps, taker_bps) =
            matcher::risk::resolve_fee_tier(0, 0, &pairs, effective_volume);

        // Identify which tier index won.
        let mut tier_index: u8 = 0;
        for (i, t) in f.tiers[..f.tier_count as usize].iter().enumerate() {
            if effective_volume >= t.min_volume_quote_lots {
                tier_index = i as u8;
            } else {
                break;
            }
        }

        emit!(TraderEffectiveTierEvent {
            trader: s.trader,
            tier_index,
            effective_volume_quote_lots: effective_volume,
            maker_rebate_bps: maker_bps,
            taker_fee_bps: taker_bps,
            window_expired: effective_volume == 0 && s.volume_30d_quote_lots > 0,
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

    /// V2 2-leg basket order against the hypertree-backed book. Pure
    /// parity port of `place_basket_order` — same distinct-market guard,
    /// same per-leg intake validation, same per-market caps, same joint
    /// stress-lattice margin gate, same rate limit. Only the injection
    /// target differs (per-market market_book PDA, not per-market
    /// order_buffer).
    pub fn place_basket_order_v2(
        ctx: Context<PlaceBasketOrderV2>,
        leg_a: BasketLeg,
        leg_b: BasketLeg,
    ) -> Result<()> {
        let mkt_a = ctx.accounts.market_a.key();
        let mkt_b = ctx.accounts.market_b.key();
        require!(mkt_a != mkt_b, FlashBookError::OutOfRange);

        validate_leg_intake(&ctx.accounts.market_a, &leg_a)?;
        validate_leg_intake(&ctx.accounts.market_b, &leg_b)?;
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

        let market_a = &ctx.accounts.market_a;
        let market_b = &ctx.accounts.market_b;
        let market_a_key = mkt_a;
        let market_b_key = mkt_b;
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
                concentration_threshold_lots: market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
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

        // Rate limit (parity).
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

        // V2 inject — into BOTH market_book PDAs.
        let now_slot = Clock::get()?.slot;
        let (seq_a, idx_a) = inject_leg_into_hypertree(
            &ctx.accounts.market_book_a,
            market_a_key,
            trader_key,
            &leg_a,
            now_slot,
        )?;
        let (seq_b, idx_b) = inject_leg_into_hypertree(
            &ctx.accounts.market_book_b,
            market_b_key,
            trader_key,
            &leg_b,
            now_slot,
        )?;

        emit!(BasketOrderPlacedV2Event {
            trader: trader_key,
            market_a: market_a_key,
            market_b: market_b_key,
            side_a: leg_a.side,
            side_b: leg_b.side,
            size_lots_a: leg_a.size_lots,
            size_lots_b: leg_b.size_lots,
            seq_a,
            seq_b,
            node_index_a: idx_a,
            node_index_b: idx_b,
        });
        Ok(())
    }

    /// V2 N-leg basket order. Pure parity port of `place_basket_order_n`
    /// — same K-distinct-markets guard, per-leg intake, caps, joint
    /// stress lattice, rate limit. remaining_accounts layout: triples
    /// of (market, market_book, position) per leg, so 3 × K accounts.
    pub fn place_basket_order_n_v2<'info>(
        ctx: Context<'_, '_, '_, 'info, PlaceBasketOrderNV2<'info>>,
        legs: Vec<BasketLeg>,
    ) -> Result<()> {
        require!(!legs.is_empty(), FlashBookError::ZeroSize);
        require!(legs.len() <= MAX_BASKET_LEGS_N, FlashBookError::OutOfRange);
        let remaining = ctx.remaining_accounts;
        require!(remaining.len() == legs.len() * 3, FlashBookError::OutOfRange);

        let trader_key = ctx.accounts.trader.key();
        let program_id = ctx.program_id;

        // Walk remaining_accounts in (market, market_book, position) triples.
        let mut markets: Vec<MarketAccount> = Vec::with_capacity(legs.len());
        let mut market_keys: Vec<Pubkey> = Vec::with_capacity(legs.len());
        let mut positions: Vec<state::PositionAccount> = Vec::with_capacity(legs.len());
        for (i, _leg) in legs.iter().enumerate() {
            let m_ai = &remaining[i * 3];
            let book_ai = &remaining[i * 3 + 1];
            let pos_ai = &remaining[i * 3 + 2];

            require_keys_eq!(*m_ai.owner, *program_id, FlashBookError::Unauthorized);
            // book_ai owner is also this program (PDA we own). Disc check
            // happens inside MarketBookHandle::from_account_data on inject.
            require_keys_eq!(*book_ai.owner, *program_id, FlashBookError::Unauthorized);
            require_keys_eq!(*pos_ai.owner, *program_id, FlashBookError::Unauthorized);

            let market: MarketAccount =
                MarketAccount::try_deserialize(&mut &m_ai.try_borrow_data()?[..])?;
            let position: state::PositionAccount =
                state::PositionAccount::try_deserialize(&mut &pos_ai.try_borrow_data()?[..])?;

            for prev in &market_keys {
                require!(*prev != m_ai.key(), FlashBookError::OutOfRange);
            }
            market_keys.push(m_ai.key());

            validate_leg_intake(&market, &legs[i])?;
            check_caps_for_leg(&market, &position, &ctx.accounts.flp_exposure, &legs[i])?;

            if position.size_lots > 0 {
                require!(position.trader == trader_key, FlashBookError::WrongTrader);
                require!(position.market == m_ai.key(), FlashBookError::WrongMarket);
            }

            markets.push(market);
            positions.push(position);
        }

        // Cross-market stress lattice (parity).
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
                concentration_threshold_lots: markets[i].params.concentration_threshold_lots,
                concentration_extra_mmr_bps: markets[i].params.concentration_extra_mmr_bps,
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

        // Rate limit (parity).
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

        // Inject each leg into its market_book.
        let now_slot = Clock::get()?.slot;
        for (i, leg) in legs.iter().enumerate() {
            let book_ai = &remaining[i * 3 + 1];
            inject_leg_into_hypertree_unchecked(
                book_ai,
                market_keys[i],
                trader_key,
                leg,
                now_slot,
            )?;
        }

        emit!(BasketOrderNPlacedV2Event {
            trader: trader_key,
            leg_count: legs.len() as u8,
            markets: market_keys.clone(),
        });
        Ok(())
    }

    /// V2: execute a trigger order against the hypertree-backed book.
    /// Same trigger semantics as v1 (kind, oracle compare, reduce-only,
    /// expiry, OCO partner deactivation) — only the order injection
    /// target differs: insert as a `RestingOrderV2` node in the bid or
    /// ask RBT instead of writing into the legacy flat buffer.
    ///
    /// Permissionless executor (any signer can fire a triggered trigger
    /// — trader pre-authorized by creating the trigger).
    pub fn execute_trigger_order_v2(
        ctx: Context<ExecuteTriggerOrderV2>,
    ) -> Result<()> {
        let trigger = &ctx.accounts.trigger_order;
        let market = &ctx.accounts.market;
        require!(
            trigger.flags & state::TriggerOrderAccount::FLAG_ACTIVE != 0,
            FlashBookError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if trigger.expires_at_slot > 0 {
            require!(trigger.expires_at_slot >= now, FlashBookError::OutOfRange);
        }

        let oracle = market.oracle_price_ticks;
        let fired = if trigger.kind == 0 {
            oracle <= trigger.trigger_price_ticks
        } else {
            oracle >= trigger.trigger_price_ticks
        };
        require!(fired, FlashBookError::OutOfRange);

        if trigger.flags & state::TriggerOrderAccount::FLAG_REDUCE_ONLY != 0 {
            let position = &ctx.accounts.position;
            require!(position.size_lots > 0, FlashBookError::OutOfRange);
            require!(position.side != trigger.side, FlashBookError::OutOfRange);
            require!(
                trigger.size_lots <= position.size_lots,
                FlashBookError::OutOfRange
            );
        }

        // V2 inject — into the hypertree.
        let market_key = market.key();
        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle =
            state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        require!(
            handle.header.market_pubkey == market_key,
            FlashBookError::WrongMarket
        );
        let next_seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(next_seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
        handle.header.order_seq_counter = next_seq;

        let side_is_bid = trigger.side == 0;
        let order = state_v2::RestingOrderV2 {
            order_id: state_v2::encode_order_id(
                trigger.limit_price_ticks,
                next_seq,
                side_is_bid,
            ),
            seq: next_seq,
            price_ticks: trigger.limit_price_ticks,
            size_lots: trigger.size_lots,
            expires_at_slot: 0,
            trader: trigger.trader,
            last_valid_slot: now as u32,
            side: trigger.side,
            order_type: 0, // limit
            flags: 0,
            _pad: 0,
        };
        let inserted_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };
        // Drop the borrow before re-borrowing for OCO partner read below.
        drop(book_data);

        // Mark trigger inactive (mirror of v1).
        let oco_pair_key = ctx.accounts.trigger_order.oco_pair;
        let trigger = &mut ctx.accounts.trigger_order;
        trigger.flags &= !state::TriggerOrderAccount::FLAG_ACTIVE;
        let exec_trader = trigger.trader;
        let exec_id = trigger.trigger_id;

        if oco_pair_key != Pubkey::default() {
            let oco_ai = ctx
                .remaining_accounts
                .iter()
                .find(|a| a.key() == oco_pair_key)
                .ok_or_else(|| error!(FlashBookError::OcoPairMismatch))?;
            require!(oco_ai.is_writable, FlashBookError::OcoPairMismatch);
            let mut data = oco_ai.try_borrow_mut_data()?;
            let mut partner: state::TriggerOrderAccount =
                state::TriggerOrderAccount::try_deserialize(&mut &data[..])?;
            require!(
                partner.oco_pair == ctx.accounts.trigger_order.key(),
                FlashBookError::OcoPairMismatch
            );
            partner.flags &= !state::TriggerOrderAccount::FLAG_ACTIVE;
            let mut cursor = &mut data[..];
            partner.try_serialize(&mut cursor)?;
        }

        emit!(TriggerOrderExecutedV2Event {
            market: market_key,
            trader: exec_trader,
            trigger_id: exec_id,
            executor: ctx.accounts.caller.key(),
            oracle_price_ticks: oracle,
            order_seq: next_seq,
            node_index: inserted_idx,
        });
        Ok(())
    }

    /// Cancel a trigger order. Trader signs; account is closed and rent
    /// returned to the trader. Works whether the trigger has already fired
    /// (active=0) or not (active=1). If this trigger participates in an
    /// OCO bracket, the partner is also marked inactive (passed via
    /// remaining_accounts) so it can't fire orphaned.
    pub fn cancel_trigger_order(ctx: Context<CancelTriggerOrder>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.trigger_order.trader == trader,
            FlashBookError::WrongTrader
        );
        let oco_pair_key = ctx.accounts.trigger_order.oco_pair;
        if oco_pair_key != Pubkey::default() {
            // OCO partner deactivation is best-effort: if the trader
            // explicitly cancels just one leg without passing the partner,
            // we accept it (the partner remains placed but the trader
            // can cancel it independently). When the partner IS passed,
            // we deactivate it.
            if let Some(oco_ai) = ctx.remaining_accounts.iter().find(|a| a.key() == oco_pair_key) {
                require!(oco_ai.is_writable, FlashBookError::OcoPairMismatch);
                let mut data = oco_ai.try_borrow_mut_data()?;
                let mut partner: state::TriggerOrderAccount =
                    state::TriggerOrderAccount::try_deserialize(&mut &data[..])?;
                require!(partner.oco_pair == ctx.accounts.trigger_order.key(),
                    FlashBookError::OcoPairMismatch);
                partner.flags &= !state::TriggerOrderAccount::FLAG_ACTIVE;
                // Clear the link too so a subsequent cancel of the partner
                // doesn't try to walk the now-closed account.
                partner.oco_pair = Pubkey::default();
                let mut cursor = &mut data[..];
                partner.try_serialize(&mut cursor)?;
            }
        }
        emit!(TriggerOrderCancelledEvent {
            market: ctx.accounts.trigger_order.market,
            trader,
            trigger_id: ctx.accounts.trigger_order.trigger_id,
        });
        Ok(())
        // Account closure is handled by Anchor's `close = trader` constraint.
    }

    /// Ratchet a trailing-stop trigger order — permissionless. Reads the
    /// current oracle and updates the trigger's anchor + price if the
    /// oracle has moved in the trader's favour. Hyperliquid trailing-stop
    /// pattern, generalised: works for both sides + both trigger kinds.
    ///
    /// Math (offset = trailing_offset_bps × oracle / 10_000):
    ///   • kind=0 (fire on ≤): SL for a long position. Best = MAX oracle.
    ///     If oracle > anchor: anchor ← oracle; trigger ← anchor − offset.
    ///   • kind=1 (fire on ≥): SL for a short position. Best = MIN oracle.
    ///     If oracle < anchor (or anchor==0): anchor ← oracle;
    ///     trigger ← anchor + offset.
    ///
    /// Tick-aligns the new trigger price (rounds toward the more
    /// conservative side: kind=0 floors, kind=1 ceils so the trigger
    /// is never less protective than intended).
    ///
    /// Rejects when the trigger isn't trailing (offset == 0) or already
    /// inactive. Idempotent — calling on a "no-progress" oracle is a
    /// no-op (no events emitted).
    pub fn update_trailing_stop(ctx: Context<UpdateTrailingStop>) -> Result<()> {
        let trigger = &ctx.accounts.trigger_order;
        let market = &ctx.accounts.market;
        require!(trigger.trailing_offset_bps > 0, FlashBookError::OutOfRange);
        require!(
            trigger.flags & state::TriggerOrderAccount::FLAG_ACTIVE != 0,
            FlashBookError::OutOfRange
        );

        let oracle = market.oracle_price_ticks;
        require!(oracle > 0, FlashBookError::ZeroPrice);
        let tick_size = market.params.tick_size;
        require!(tick_size > 0, FlashBookError::ZeroPrice);
        let offset_bps = trigger.trailing_offset_bps as u128;
        let offset_ticks: u128 = (oracle as u128).saturating_mul(offset_bps)
            / constants::BPS_DENOM as u128;

        let prev_anchor = trigger.trailing_anchor_ticks;
        let (new_anchor, raw_new_trigger): (u64, i128) = if trigger.kind == 0 {
            // Long-side SL: anchor = max oracle. Ratchet up only.
            if prev_anchor != 0 && oracle <= prev_anchor {
                return Ok(()); // no progress
            }
            let new_trigger = (oracle as i128) - (offset_ticks as i128);
            (oracle, new_trigger)
        } else {
            // Short-side SL: anchor = min oracle. Ratchet down only.
            if prev_anchor != 0 && oracle >= prev_anchor {
                return Ok(()); // no progress
            }
            let new_trigger = (oracle as i128) + (offset_ticks as i128);
            (oracle, new_trigger)
        };

        // Tick alignment with conservative rounding (don't make the
        // trigger MORE aggressive than offset_bps would allow).
        let new_trigger_clamped = if raw_new_trigger < tick_size as i128 {
            tick_size as i128
        } else {
            raw_new_trigger
        };
        let new_trigger_unsigned = new_trigger_clamped as u128;
        let aligned: u64 = if trigger.kind == 0 {
            // Floor to nearest tick (more protective: trigger fires SOONER if
            // oracle drops; conservative for an SL on a long).
            let floored = (new_trigger_unsigned / tick_size as u128) * tick_size as u128;
            // But floor would make trigger LOWER → less protective. Use ceil
            // to keep the SL tighter (fires EARLIER on a drop).
            let ceiled = floored.saturating_add(if new_trigger_unsigned % tick_size as u128 != 0 {
                tick_size as u128
            } else { 0 });
            if ceiled > u64::MAX as u128 { u64::MAX } else { ceiled as u64 }
        } else {
            // Floor — keeps trigger LOWER for a short-side SL (fires earlier
            // on a rally).
            let floored = (new_trigger_unsigned / tick_size as u128) * tick_size as u128;
            if floored > u64::MAX as u128 { u64::MAX } else { floored as u64 }
        };

        // Bail if alignment didn't actually change the trigger (oracle
        // moved within one tick).
        if aligned == trigger.trigger_price_ticks && new_anchor == prev_anchor {
            return Ok(());
        }

        let trigger_id = trigger.trigger_id;
        let trader = trigger.trader;
        let market_key = market.key();
        let prev_trigger_price = trigger.trigger_price_ticks;
        let trigger = &mut ctx.accounts.trigger_order;
        trigger.trailing_anchor_ticks = new_anchor;
        trigger.trigger_price_ticks = aligned;

        emit!(TrailingStopRatchetedEvent {
            market: market_key,
            trader,
            trigger_id,
            previous_trigger_price_ticks: prev_trigger_price,
            new_trigger_price_ticks: aligned,
            anchor_ticks: new_anchor,
        });
        Ok(())
    }

    /// V2: execute one TWAP slice against the hypertree-backed book.
    /// Same scheduling semantics as v1 (FLAG_ACTIVE check, end_slot
    /// expiry, slot_interval gating, slice sizing with min_base_lots
    /// floor, parent depletion deactivation); only the order injection
    /// target differs (hypertree, not v1 buffer).
    ///
    /// Permissionless caller — pre-authorized by trader at TWAP creation.
    pub fn execute_twap_slice_v2(ctx: Context<ExecuteTwapSliceV2>) -> Result<()> {
        let twap = &ctx.accounts.twap_order;
        let market = &ctx.accounts.market;
        require!(
            twap.flags & state::TwapOrderAccount::FLAG_ACTIVE != 0,
            FlashBookError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if twap.end_slot > 0 {
            require!(twap.end_slot >= now, FlashBookError::OutOfRange);
        }
        require!(
            now >= twap.last_slice_at_slot.saturating_add(twap.slot_interval),
            FlashBookError::OutOfRange
        );

        let remaining = twap
            .total_size_lots
            .checked_sub(twap.size_executed_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(remaining > 0, FlashBookError::OutOfRange);
        let slice_size = core::cmp::min(twap.slice_size_lots, remaining);
        require!(
            slice_size >= market.params.min_base_lots || slice_size == remaining,
            FlashBookError::SizeBelowMinLot
        );

        // V2 inject — into the hypertree.
        let market_key = market.key();
        let twap_trader = twap.trader;
        let twap_side = twap.side;
        let twap_limit_ticks = twap.limit_price_ticks;
        let inserted_idx;
        let next_seq;
        {
            let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
            let mut handle =
                state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
            require!(
                handle.header.market_pubkey == market_key,
                FlashBookError::WrongMarket
            );
            next_seq = handle
                .header
                .order_seq_counter
                .checked_add(1)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            require!(
                next_seq < FLP_SEQ_RESERVED_OFFSET,
                FlashBookError::OutOfRange
            );
            handle.header.order_seq_counter = next_seq;

            let side_is_bid = twap_side == 0;
            let order = state_v2::RestingOrderV2 {
                order_id: state_v2::encode_order_id(
                    twap_limit_ticks,
                    next_seq,
                    side_is_bid,
                ),
                seq: next_seq,
                price_ticks: twap_limit_ticks,
                size_lots: slice_size,
                expires_at_slot: 0,
                trader: twap_trader,
                last_valid_slot: now as u32,
                side: twap_side,
                order_type: 0, // limit
                flags: 0,
                _pad: 0,
            };
            inserted_idx = if side_is_bid {
                handle.insert_bid(order)?
            } else {
                handle.insert_ask(order)?
            };
        }

        // Update TWAP scheduling state (mirror v1).
        let twap = &mut ctx.accounts.twap_order;
        twap.size_executed_lots = twap
            .size_executed_lots
            .checked_add(slice_size)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        twap.last_slice_at_slot = now;
        if twap.size_executed_lots >= twap.total_size_lots {
            twap.flags &= !state::TwapOrderAccount::FLAG_ACTIVE;
        }

        emit!(TwapSliceExecutedV2Event {
            market: market_key,
            trader: twap_trader,
            twap_id: twap.twap_id,
            executor: ctx.accounts.caller.key(),
            slice_size_lots: slice_size,
            cumulative_executed_lots: twap.size_executed_lots,
            order_seq: next_seq,
            node_index: inserted_idx,
        });
        Ok(())
    }

    /// Cancel a TWAP order — trader signs, account is closed, rent
    /// returned. Works whether fully executed or partial.
    pub fn cancel_twap_order(ctx: Context<CancelTwapOrder>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.twap_order.trader == trader,
            FlashBookError::WrongTrader
        );
        emit!(TwapOrderCancelledEvent {
            market: ctx.accounts.twap_order.market,
            trader,
            twap_id: ctx.accounts.twap_order.twap_id,
            unfilled_lots: ctx
                .accounts
                .twap_order
                .total_size_lots
                .saturating_sub(ctx.accounts.twap_order.size_executed_lots),
        });
        Ok(())
    }

    /// V2: replenish an iceberg's visible chunk against the hypertree-
    /// backed book. Same iceberg semantics as v1 (FLAG_ACTIVE, expiry,
    /// "still_resting" probe to avoid double-displaying, displayed-size
    /// chunking, residual-tail allowance below min_base_lots,
    /// auto-deactivate at zero remaining); only the order injection
    /// target differs (hypertree, not v1 buffer).
    ///
    /// Lookup mechanics: the v1 buffer scan is replaced by an O(log n)
    /// `lookup_bid/ask_by_order_id` against the hypertree using the
    /// child's encoded order_id (recomputed from iceberg.limit_ticks +
    /// iceberg.child_order_seq + side). NIL = fully consumed → replenish.
    pub fn replenish_iceberg_v2(ctx: Context<ReplenishIcebergV2>) -> Result<()> {
        let iceberg = &ctx.accounts.iceberg_order;
        let market = &ctx.accounts.market;
        require!(
            iceberg.flags & state::IcebergOrderAccount::FLAG_ACTIVE != 0,
            FlashBookError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if iceberg.expires_at_slot > 0 {
            require!(iceberg.expires_at_slot >= now, FlashBookError::OutOfRange);
        }
        require!(iceberg.remaining_lots > 0, FlashBookError::OutOfRange);

        let market_key = market.key();
        let side_is_bid = iceberg.side == 0;
        let chunk = iceberg.displayed_size_lots.min(iceberg.remaining_lots);
        let trader_pk = iceberg.trader;
        let limit_ticks = iceberg.limit_ticks;
        let expires_at_slot = iceberg.expires_at_slot;
        let prior_child_seq = iceberg.child_order_seq;
        let iceberg_side = iceberg.side;

        let inserted_idx;
        let next_seq;
        {
            let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
            let mut handle =
                state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
            require!(
                handle.header.market_pubkey == market_key,
                FlashBookError::WrongMarket
            );

            // Probe: is the prior child still resting? If so, no-op
            // (lazy poll-friendly; same UX as v1). prior_child_seq == 0
            // means "no prior child yet" (first replenish on a fresh
            // iceberg) — skip probe.
            if prior_child_seq != 0 {
                let prior_id = state_v2::encode_order_id(
                    limit_ticks,
                    prior_child_seq,
                    side_is_bid,
                );
                let prior_idx = if side_is_bid {
                    handle.lookup_bid_by_order_id(prior_id)
                } else {
                    handle.lookup_ask_by_order_id(prior_id)
                };
                if prior_idx != crate::hypertree::NIL {
                    return Ok(());
                }
            }

            next_seq = handle
                .header
                .order_seq_counter
                .checked_add(1)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            require!(
                next_seq < FLP_SEQ_RESERVED_OFFSET,
                FlashBookError::OutOfRange
            );
            handle.header.order_seq_counter = next_seq;

            let order = state_v2::RestingOrderV2 {
                order_id: state_v2::encode_order_id(limit_ticks, next_seq, side_is_bid),
                seq: next_seq,
                price_ticks: limit_ticks,
                size_lots: chunk,
                expires_at_slot,
                trader: trader_pk,
                last_valid_slot: now as u32,
                side: iceberg_side,
                order_type: 0, // limit
                flags: 0,
                _pad: 0,
            };
            inserted_idx = if side_is_bid {
                handle.insert_bid(order)?
            } else {
                handle.insert_ask(order)?
            };
        }

        let iceberg = &mut ctx.accounts.iceberg_order;
        iceberg.remaining_lots = iceberg.remaining_lots.saturating_sub(chunk);
        iceberg.child_order_seq = next_seq;
        if iceberg.remaining_lots == 0 {
            iceberg.flags &= !state::IcebergOrderAccount::FLAG_ACTIVE;
        }

        emit!(IcebergReplenishedV2Event {
            market: market_key,
            trader: trader_pk,
            iceberg_id: iceberg.iceberg_id,
            executor: ctx.accounts.caller.key(),
            chunk_size_lots: chunk,
            remaining_lots: iceberg.remaining_lots,
            new_child_seq: next_seq,
            node_index: inserted_idx,
        });
        Ok(())
    }

    /// V2: cancel an iceberg order against the hypertree-backed book.
    /// Best-effort child removal: if the current child is still resting,
    /// look it up via O(log n) RBT search (vs v1's O(n) buffer scan)
    /// and remove via handle.remove_*_node. If already filled, no-op.
    /// Closes the iceberg account (rent returned via Anchor's `close`
    /// constraint).
    pub fn cancel_iceberg_v2(ctx: Context<CancelIcebergV2>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.iceberg_order.trader == trader,
            FlashBookError::WrongTrader
        );

        let iceberg = &ctx.accounts.iceberg_order;
        let child_seq = iceberg.child_order_seq;
        let limit_ticks = iceberg.limit_ticks;
        let side_is_bid = iceberg.side == 0;
        let market_key = iceberg.market;

        if child_seq != 0 {
            let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
            let mut handle =
                state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
            require!(
                handle.header.market_pubkey == market_key,
                FlashBookError::WrongMarket
            );
            let child_id =
                state_v2::encode_order_id(limit_ticks, child_seq, side_is_bid);
            let child_idx = if side_is_bid {
                handle.lookup_bid_by_order_id(child_id)
            } else {
                handle.lookup_ask_by_order_id(child_id)
            };
            if child_idx != crate::hypertree::NIL {
                // Belt-and-suspenders: ensure the resting node really is
                // this trader's (defends against the unlikely case that
                // an attacker happened to land at the same encoded id).
                let resting = handle.order_at(child_idx);
                if resting.trader == trader {
                    if side_is_bid {
                        handle.remove_bid_node(child_idx);
                    } else {
                        handle.remove_ask_node(child_idx);
                    }
                }
            }
        }

        let unfilled = ctx
            .accounts
            .iceberg_order
            .remaining_lots
            .saturating_add(ctx.accounts.iceberg_order.displayed_size_lots);
        emit!(IcebergCancelledEvent {
            market: market_key,
            trader,
            iceberg_id: ctx.accounts.iceberg_order.iceberg_id,
            unfilled_lots: unfilled.min(ctx.accounts.iceberg_order.total_size_lots),
        });
        Ok(())
    }

    /// View ix: compute the predicted funding rate (no state change).
    /// Emits `PredictedFundingEvent` so callers can read the result via
    /// tx simulation logs without paying for an actual on-chain mutation.
    /// Useful for UIs that need to display "next funding payment" before
    /// the rate crystallises in `settle_funding`.
    ///
    /// Math is identical to `matcher::funding::advance`'s rate
    /// computation: premium = (mark - oracle) * 10_000 / oracle; rate
    /// = clamp(K * premium, ±rate_max_bps_per_sec).
    pub fn view_predicted_funding(ctx: Context<ViewMarket>) -> Result<()> {
        let m = &ctx.accounts.market;
        let oracle = m.oracle_price_ticks as i128;
        let mark = m.mark_price_ticks as i128;
        let (rate_bps_per_sec, premium_bps): (i64, i64) = if oracle == 0 {
            (0, 0)
        } else {
            let premium_i128 = (mark - oracle)
                .checked_mul(constants::BPS_DENOM as i128)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
                .checked_div(oracle)
                .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
            let premium = clamp_i128_to_i64(premium_i128);
            let raw = (m.params.funding_rate_k_bps as i128)
                .checked_mul(premium as i128)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
                / (constants::BPS_DENOM as i128);
            let max = m.params.funding_rate_max_bps_per_sec as i128;
            let rate_clamped = if raw > max { max } else if raw < -max { -max } else { raw };
            (rate_clamped as i64, premium)
        };
        emit!(PredictedFundingEvent {
            market: m.key(),
            mark_price_ticks: m.mark_price_ticks,
            oracle_price_ticks: m.oracle_price_ticks,
            premium_bps,
            rate_bps_per_sec,
            current_cum_index: m.cum_funding_index,
        });
        Ok(())
    }

    /// View ix: snapshot the FLP quoter's would-be next-batch quote
    /// ladder (no state change). Runs the same generate_quotes
    /// computation as `run_batch` but doesn't mutate the OrderBuffer
    /// or any other state. Emits `QuoteLadderSnapshotEvent` carrying
    /// the top-N levels (each side) for off-chain UI rendering.
    ///
    /// Levels are interleaved bid/ask in seq order: [bid0, ask0,
    /// bid1, ask1, ...] — same emission order as the matcher would
    /// see. `levels_emitted` is bounded by `params.flp_quote_levels`.
    pub fn view_quote_ladder(ctx: Context<ViewMarket>) -> Result<()> {
        let market = &ctx.accounts.market;
        let flp = &ctx.accounts.flp_exposure;

        // Mirror run_batch's setup of FlpQuoterParams + Inputs.
        let market_key = market.key();
        let flp_pool_capital = flp.total_capital_quote_lots;
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
        let mut gross_used: u128 = 0;
        for e in flp.per_market.iter() {
            if e.side == 255 { continue; }
            let n = (e.size_lots as u128)
                .saturating_mul(e.entry_price_ticks as u128)
                .saturating_mul(market.params.tick_size as u128);
            gross_used = gross_used.saturating_add(n);
        }
        let utilization_bps: u32 = if flp_pool_capital == 0 {
            0
        } else {
            ((gross_used.saturating_mul(constants::BPS_DENOM as u128))
                / (flp_pool_capital as u128))
                .min(u32::MAX as u128) as u32
        };

        let flp_params = matcher::flp_quoter::FlpQuoterParams {
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
        let realized_vol_bps = realized_vol_bps_from_window(
            &market.recent_clearing_prices,
            market.recent_clearing_count,
        );
        let flp_inputs = matcher::flp_quoter::FlpQuoterInputs {
            oracle_ticks: matcher::lot::Ticks(market.oracle_price_ticks),
            vpin_bps: market.vpin.as_bps(),
            realized_vol_bps,
            pool_capital_quote_lots: flp_pool_capital,
            pool_net_quote_lots_signed: flp_net_signed,
            pool_gross_utilization_bps: utilization_bps,
            oi_long_lots: market.oi_long_lots,
            oi_short_lots: market.oi_short_lots,
        };
        let (out, _orders) = matcher::flp_quoter::generate_quotes(
            flp_params,
            flp_inputs,
            flp.key(),
            0,
        )?;
        // Top-level summary. Per-level array would balloon the event;
        // off-chain consumers can re-run generate_quotes with the same
        // inputs (deterministic) for the full ladder if needed.
        let top_bid = out.bids.first().map(|(p, _)| p.0).unwrap_or(0);
        let top_ask = out.asks.first().map(|(p, _)| p.0).unwrap_or(0);
        let top_bid_size = out.bids.first().map(|(_, s)| s.0).unwrap_or(0);
        let top_ask_size = out.asks.first().map(|(_, s)| s.0).unwrap_or(0);
        emit!(QuoteLadderSnapshotEvent {
            market: market_key,
            fair_value_ticks: out.fair_value.0,
            skew_bps: out.skew_bps,
            top_bid_ticks: top_bid,
            top_ask_ticks: top_ask,
            top_bid_size_lots: top_bid_size,
            top_ask_size_lots: top_ask_size,
            level_count: out.bids.len().min(u8::MAX as usize) as u8,
        });
        Ok(())
    }

    /// View ix: cross-market portfolio risk for a trader. Walks the
    /// trader's open positions via remaining_accounts ([market, position]
    /// pairs, count must match `trader_state.open_positions`), runs the
    /// stress-lattice assess, and emits `PortfolioRiskEvent` with
    /// (collateral, unrealized_pnl, equity, required_margin, health_ratio_bps,
    /// largest_position_market, largest_position_notional, worst_scenario_idx).
    ///
    /// SDK callers simulate the tx and read the event from logs without
    /// paying for a state mutation. Single-call portfolio risk for UIs
    /// that previously had to fetch every position + market separately
    /// and run `previewPortfolioRisk` client-side.
    ///
    /// Pass `[]` for remaining_accounts if the trader has no open
    /// positions (returns equity == collateral, required == 0,
    /// health_ratio_bps == u32::MAX).
    pub fn view_portfolio_risk<'info>(
        ctx: Context<'_, '_, 'info, 'info, ViewPortfolioRisk<'info>>,
    ) -> Result<()> {
        let trader_state = &ctx.accounts.trader_state;
        let open = trader_state.open_positions as usize;
        require!(
            ctx.remaining_accounts.len() == open * 2,
            FlashBookError::OutOfRange
        );

        let mut snaps: Vec<RiskPosSnap> = Vec::with_capacity(open);
        let mut market_snaps: Vec<RiskMarketSnap> = Vec::with_capacity(open);
        let mut market_keys: Vec<Pubkey> = Vec::with_capacity(open);
        let mut largest_notional: u128 = 0;
        let mut largest_market = Pubkey::default();
        let mut unrealized_total: i128 = 0;

        for i in 0..open {
            let market_ai = &ctx.remaining_accounts[i * 2];
            let position_ai = &ctx.remaining_accounts[i * 2 + 1];
            require!(market_ai.owner == ctx.program_id, FlashBookError::OutOfRange);
            require!(position_ai.owner == ctx.program_id, FlashBookError::OutOfRange);

            let market_data = market_ai.try_borrow_data()?;
            let market_acct: MarketAccount =
                MarketAccount::try_deserialize(&mut &market_data[..])?;
            let position_data = position_ai.try_borrow_data()?;
            let position: state::PositionAccount =
                state::PositionAccount::try_deserialize(&mut &position_data[..])?;
            require!(
                position.trader == trader_state.trader,
                FlashBookError::WrongTrader
            );
            require!(
                position.market == market_ai.key(),
                FlashBookError::WrongMarket
            );

            // Track largest by notional at mark.
            let notional = (position.size_lots as u128)
                .saturating_mul(market_acct.mark_price_ticks as u128)
                .saturating_mul(market_acct.params.tick_size as u128);
            if notional > largest_notional {
                largest_notional = notional;
                largest_market = market_ai.key();
            }

            // Unrealized PnL at mark.
            if position.size_lots > 0 && market_acct.mark_price_ticks > 0 {
                let pnl_per_lot_ticks =
                    (market_acct.mark_price_ticks as i128) - (position.entry_price_ticks as i128);
                let sign: i128 = if position.side == 0 { 1 } else { -1 };
                let upnl = sign
                    .saturating_mul(position.size_lots as i128)
                    .saturating_mul(pnl_per_lot_ticks)
                    .saturating_mul(market_acct.params.tick_size as i128);
                unrealized_total = unrealized_total.saturating_add(upnl);
            }

            snaps.push(RiskPosSnap {
                market: position.market,
                side: if position.side == 0 { Side::Long } else { Side::Short },
                size_lots: position.size_lots,
                entry_price: Ticks(position.entry_price_ticks),
                cum_funding_index_at_entry: position.cum_funding_index_at_entry,
            });
            market_snaps.push(RiskMarketSnap {
                market: market_ai.key(),
                mark_price: Ticks(market_acct.mark_price_ticks),
                cum_funding_index: market_acct.cum_funding_index,
                maintenance_margin_bps: market_acct.params.maintenance_margin_ratio_bps,
                tick_size: market_acct.params.tick_size,
                concentration_threshold_lots: market_acct.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market_acct.params.concentration_extra_mmr_bps,
            });
            market_keys.push(market_ai.key());
        }

        let (required, equity_signed, worst_idx) = if open == 0 {
            (0u64, trader_state.collateral_quote_lots as i128, 0u32)
        } else {
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_fn(
                &snaps,
                &market_snaps,
                &scenarios,
                trader_state.collateral_quote_lots,
            )?;
            (assessment.required_quote_lots, assessment.equity_quote_lots_signed, assessment.worst_scenario_idx)
        };

        // Health ratio = equity / required, clamped to u32. u32::MAX = no required (healthy).
        let health_ratio_bps: u32 = if required == 0 {
            u32::MAX
        } else if equity_signed <= 0 {
            0
        } else {
            let ratio = (equity_signed as u128).saturating_mul(constants::BPS_DENOM as u128)
                / (required as u128);
            if ratio > u32::MAX as u128 { u32::MAX } else { ratio as u32 }
        };
        let largest_notional_u64 = if largest_notional > u64::MAX as u128 {
            u64::MAX
        } else {
            largest_notional as u64
        };

        emit!(PortfolioRiskEvent {
            trader: trader_state.trader,
            collateral_quote_lots: trader_state.collateral_quote_lots,
            unrealized_pnl_quote_lots: clamp_i128_to_i64(unrealized_total),
            equity_quote_lots: clamp_i128_to_i64(equity_signed),
            required_margin_quote_lots: required,
            health_ratio_bps,
            largest_position_market: largest_market,
            largest_position_notional_quote_lots: largest_notional_u64,
            open_positions: trader_state.open_positions,
            worst_scenario_idx: worst_idx,
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

    /// V2 reveal: redeem a previously-submitted commit and inject the
    /// revealed taker order into the hypertree-backed book. Same
    /// commit-reveal cryptography as v1 (`redeem_reveal` validates the
    /// hash + bond against the commit_buffer); only the inject target
    /// differs (hypertree, not v1 buffer).
    ///
    /// The revealed order is inserted with order_type byte = 1 (Taker)
    /// so the matcher's FIFO mapping promotes it AHEAD of resting limits
    /// at the same price tier — same priority semantics as v1.
    pub fn submit_reveal_v2(
        ctx: Context<SubmitRevealV2>,
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

        let market_key = ctx.accounts.market.key();
        let current_batch = ctx.accounts.market.current_batch;
        let commit_buffer = &mut ctx.accounts.commit_buffer;
        let trader_pk = ctx.accounts.trader.key();
        let now_slot = Clock::get()?.slot;

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle =
            state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        require!(
            handle.header.market_pubkey == market_key,
            FlashBookError::WrongMarket
        );

        // Bump seq + redeem the reveal (validates the hash + clears the commit).
        let next_seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(
            next_seq < FLP_SEQ_RESERVED_OFFSET,
            FlashBookError::OutOfRange
        );
        let _matcher_order = redeem_reveal(
            &mut commit_buffer.commits,
            &payload,
            current_batch,
            next_seq,
        )?;
        handle.header.order_seq_counter = next_seq;

        // Build + insert the resting order. order_type = 1 (Taker) so
        // the matcher's mapping (`order_type_byte_to_matcher`) promotes
        // it to OrderType::Taker FIFO priority.
        let side_is_bid = side == 0;
        let order = state_v2::RestingOrderV2 {
            order_id: state_v2::encode_order_id(limit_ticks, next_seq, side_is_bid),
            seq: next_seq,
            price_ticks: limit_ticks,
            size_lots,
            expires_at_slot: 0,
            trader: trader_pk,
            last_valid_slot: now_slot as u32,
            side,
            order_type: 1, // Taker
            flags: 0,
            _pad: 0,
        };
        let inserted_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };

        emit!(RevealAppliedV2Event {
            market: market_key,
            trader: trader_pk,
            side,
            size_lots,
            limit_ticks,
            order_seq: next_seq,
            node_index: inserted_idx,
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

        // ── Wave 22 phase 2 (FLP path) — tier-resolved taker fee ─────
        // When the FeeTiersAccount is supplied, use the taker's
        // tier-resolved taker_fee_bps. Pre-fill volume is used so the
        // current trade doesn't promote itself.
        let (taker_taker_fee_bps, fee_tier_pairs): (u32, Vec<(u64, i32, u32)>) =
            if let Some(ft) = &ctx.accounts.fee_tiers {
                let pairs: Vec<(u64, i32, u32)> = ft.tiers[..ft.tier_count as usize]
                    .iter()
                    .map(|t| (t.min_volume_quote_lots, t.maker_rebate_bps, t.taker_fee_bps))
                    .collect();
                let taker_volume = ctx.accounts.taker_trader_state.volume_30d_quote_lots;
                let (_unused_maker, tt) = matcher::risk::resolve_fee_tier(
                    market.params.maker_rebate_bps,
                    market.params.taker_fee_bps,
                    &pairs,
                    taker_volume,
                );
                (tt, pairs)
            } else {
                (market.params.taker_fee_bps, Vec::new())
            };
        let pre_taker_tier_index = tier_index_for_volume(
            &fee_tier_pairs,
            ctx.accounts.taker_trader_state.volume_30d_quote_lots,
        );

        let taker_fee_u128 =
            notional_u128.saturating_mul(taker_taker_fee_bps as u128) / constants::BPS_DENOM as u128;
        // FLP-as-maker case: ignore negative maker_rebate_bps (the
        // protocol cannot charge itself a fee). Tier-tier semantics
        // for retail makers don't apply when the maker IS the protocol.
        let maker_rebate_pos_bps = market.params.maker_rebate_bps.max(0) as u128;
        let maker_rebate_u128 =
            notional_u128.saturating_mul(maker_rebate_pos_bps) / constants::BPS_DENOM as u128;
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

        // ── Wave 22 phase 2 (FLP path) — credit taker's rolling volume
        //    + emit tier-upgrade event on boundary crossings.
        // FLP fills only credit the TAKER (no human maker).
        if market.params.taker_fee_bps > 0 {
            let notional_quote_lots = if notional_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                notional_u128 as u64
            };
            let now_slot = Clock::get()?.slot;
            credit_volume_for_tier(
                &mut ctx.accounts.taker_trader_state,
                notional_quote_lots,
                now_slot,
            );
            if !fee_tier_pairs.is_empty() {
                let post_taker_tier_index = tier_index_for_volume(
                    &fee_tier_pairs,
                    ctx.accounts.taker_trader_state.volume_30d_quote_lots,
                );
                if post_taker_tier_index != pre_taker_tier_index {
                    emit!(TraderTierUpgradedEvent {
                        trader: taker_trader_pk,
                        previous_tier_index: pre_taker_tier_index,
                        new_tier_index: post_taker_tier_index,
                        volume_quote_lots: ctx
                            .accounts
                            .taker_trader_state
                            .volume_30d_quote_lots,
                    });
                }
            }
        }
        Ok(())
    }

    /// V2: liquidate an unhealthy position by injecting a synthetic
    /// close order into the hypertree-backed book.
    ///
    /// PURE PARITY PORT of v1's `liquidate_position` — same cooldown
    /// gate, same stress-lattice health check, same Dutch-auction
    /// reward curve, same `oracle ± penalty_bps` limit pricing, same
    /// position timing updates. Only the order injection target
    /// differs: hypertree, not v1 buffer. The order_type byte is set
    /// to 3 (Liquidation) so the matcher's FIFO mapping (mirror of
    /// v1's `slot_to_order`) places it AHEAD of regular limits at
    /// the same price tier — same priority semantics as v1.
    pub fn liquidate_position_v2(
        ctx: Context<LiquidatePositionV2>,
        requested_close_lots: u64,
    ) -> Result<()> {
        let market = &ctx.accounts.market;
        let position = &ctx.accounts.position;
        let trader_state_pre = ctx.accounts.trader_state.clone();

        require!(position.size_lots > 0, FlashBookError::LiquidationStale);
        require!(
            position.trader == trader_state_pre.trader,
            FlashBookError::WrongTrader
        );
        require!(
            position.market == market.key(),
            FlashBookError::WrongMarket
        );

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

        let current_slot = Clock::get()?.slot;
        let cooldown = market.params.liquidation_cooldown_slots as u64;
        if cooldown > 0 && position.last_liquidated_at_slot > 0 {
            let elapsed = current_slot.saturating_sub(position.last_liquidated_at_slot);
            require!(elapsed >= cooldown, FlashBookError::RateLimited);
        }

        // Health gate (parity-port).
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
            concentration_threshold_lots: market.params.concentration_threshold_lots,
            concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
        };
        let scenarios = default_scenarios_fn(&[market.key()]);
        let assessment = assess_margin_fn(
            &[pos_snap],
            &[market_snap],
            &scenarios,
            trader_state_pre.collateral_quote_lots,
        )?;
        require!(!assessment.is_healthy, FlashBookError::NotLiquidatable);

        let pos_side = pos_snap.side;
        let close_side = pos_side.opposite();
        let penalty = market.params.liq_penalty_bps as u128;
        let oracle = market.oracle_price_ticks as u128;
        let penalty_delta = (oracle * penalty) / constants::BPS_DENOM as u128;
        let limit = match close_side {
            Side::Short => (oracle.saturating_sub(penalty_delta)) as u64,
            Side::Long => (oracle.saturating_add(penalty_delta)) as u64,
        };

        // Lazy-init caller_trader_state (parity-port).
        {
            let cts = &mut ctx.accounts.caller_trader_state;
            if cts.trader == Pubkey::default() {
                cts.trader = ctx.accounts.caller.key();
                cts.bump = ctx.bumps.caller_trader_state;
            }
        }

        let market_key = market.key();

        // Dutch-auction reward (parity-port from v1).
        let mut reward_paid: u64 = 0;
        if market.params.liquidator_reward_bps > 0 {
            let notional_u128 = (close_size as u128)
                .saturating_mul(oracle)
                .saturating_mul(market.params.tick_size as u128);
            let mut reward_bps_eff = market.params.liquidator_reward_bps as u128;
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

        // V2 inject — into the hypertree, with order_type = Liquidation (3).
        let trader = position.trader;
        let close_side_u8 = close_side as u8;
        let inserted_idx;
        let next_seq;
        {
            let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
            let mut handle =
                state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
            next_seq = handle
                .header
                .order_seq_counter
                .checked_add(1)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            require!(
                next_seq < FLP_SEQ_RESERVED_OFFSET,
                FlashBookError::OutOfRange
            );
            handle.header.order_seq_counter = next_seq;

            let side_is_bid = close_side_u8 == 0;
            let order = state_v2::RestingOrderV2 {
                order_id: state_v2::encode_order_id(limit, next_seq, side_is_bid),
                seq: next_seq,
                price_ticks: limit,
                size_lots: close_size,
                expires_at_slot: 0,
                trader,
                last_valid_slot: current_slot as u32,
                side: close_side_u8,
                order_type: 3, // 3 = Liquidation (matcher promotes priority)
                flags: 0,
                _pad: 0,
            };
            inserted_idx = if side_is_bid {
                handle.insert_bid(order)?
            } else {
                handle.insert_ask(order)?
            };
        }

        let position = &mut ctx.accounts.position;
        if position.unhealthy_since_slot == 0 {
            position.unhealthy_since_slot = current_slot;
        }
        position.last_liquidated_at_slot = current_slot;

        emit!(LiquidationInjectedV2Event {
            market: market_key,
            trader,
            side: pos_side as u8,
            size_lots: close_size,
            limit_ticks: limit,
            worst_scenario_idx: assessment.worst_scenario_idx,
            order_seq: next_seq,
            node_index: inserted_idx,
        });
        if reward_paid > 0 {
            emit!(LiquidatorRewardedEvent {
                market: market_key,
                liquidator: ctx.accounts.caller.key(),
                liquidatee: trader,
                reward_quote_lots: reward_paid,
            });
        }
        Ok(())
    }

    /// Post a slashable HIP-3 deployer bond on a market. The bond signals
    /// the depositor's commitment to the market's solvency; if the
    /// market goes bad (oracle stale, mass insolvent liqs, etc.),
    /// protocol governance can `slash_market_bond` to transfer up to
    /// the full bond into the insurance fund.
    ///
    /// Caller signs and is the depositor (each bond is per (market,
    /// depositor) pair — anyone can post bond on any market). SPL
    /// transfer from depositor's quote ATA to the protocol vault. The
    /// MarketBondAccount tracks the depositor's claim.
    ///
    /// Posting bond does NOT change creator share or market params; it's
    /// a purely additive commitment signal. Off-chain UIs can rank
    /// markets by total bond amount (sum across all bond accounts for
    /// a market).
    pub fn post_market_bond(
        ctx: Context<PostMarketBond>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        // SPL transfer to protocol vault (same vault as collateral / FLP).
        let cpi_accounts = Transfer {
            from: ctx.accounts.depositor_quote_ata.to_account_info(),
            to: ctx.accounts.quote_vault.to_account_info(),
            authority: ctx.accounts.depositor.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
        let bond = &mut ctx.accounts.market_bond;
        // Lazy init on first post.
        if bond.depositor == Pubkey::default() {
            bond.market = ctx.accounts.market.key();
            bond.depositor = ctx.accounts.depositor.key();
            bond.bump = ctx.bumps.market_bond;
            bond._pad0 = [0u8; 7];
            bond.deposited_at_unix = now_unix;
            bond.total_slashed_quote_lots = 0;
        }
        bond.amount_quote_lots = bond
            .amount_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        // Adding more bond cancels any pending unbond request — the
        // depositor is signalling they want to keep the bond live.
        bond.unbond_request_at_unix = 0;

        emit!(MarketBondPostedEvent {
            market: bond.market,
            depositor: bond.depositor,
            amount_quote_lots,
            new_total_quote_lots: bond.amount_quote_lots,
        });
        Ok(())
    }

    /// Request to unbond. Sets the unbond timestamp; bond cannot be
    /// claimed until BOND_UNBOND_DELAY_SECONDS elapses. The delay
    /// prevents withdraw-before-slash races. Re-posting bond cancels
    /// the request (signals continued commitment).
    pub fn request_unbond_market_bond(
        ctx: Context<UnbondMarketBondAuth>,
    ) -> Result<()> {
        let bond = &mut ctx.accounts.market_bond;
        require!(bond.amount_quote_lots > 0, FlashBookError::BondTooSmall);
        let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
        bond.unbond_request_at_unix = now_unix;
        emit!(MarketBondUnbondRequestedEvent {
            market: bond.market,
            depositor: bond.depositor,
            requested_at_unix: now_unix,
            claimable_at_unix: now_unix.saturating_add(constants::BOND_UNBOND_DELAY_SECONDS),
        });
        Ok(())
    }

    /// Claim unbonded bond. Requires `unbond_request_at_unix > 0` AND
    /// `now ≥ unbond_request_at_unix + BOND_UNBOND_DELAY_SECONDS`.
    /// Transfers the FULL outstanding amount back to depositor's ATA;
    /// MarketBondAccount stays open with amount = 0 (re-postable).
    pub fn claim_unbonded_market_bond(
        ctx: Context<ClaimUnbondedMarketBond>,
    ) -> Result<()> {
        let bond = &ctx.accounts.market_bond;
        require!(bond.amount_quote_lots > 0, FlashBookError::BondTooSmall);
        require!(
            bond.unbond_request_at_unix > 0,
            FlashBookError::BondUnbondingDelay
        );
        let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
        let claimable_at = bond
            .unbond_request_at_unix
            .saturating_add(constants::BOND_UNBOND_DELAY_SECONDS);
        require!(now_unix >= claimable_at, FlashBookError::BondUnbondingDelay);

        let amount = bond.amount_quote_lots;

        // SPL transfer back: insurance_fund PDA signs.
        let bump = ctx.accounts.insurance_fund.bump;
        let seeds: &[&[u8]] = &[InsuranceFundAccount::SEED, &[bump]];
        let signer = &[seeds];
        let cpi_accounts = Transfer {
            from: ctx.accounts.quote_vault.to_account_info(),
            to: ctx.accounts.depositor_quote_ata.to_account_info(),
            authority: ctx.accounts.insurance_fund.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer,
        );
        token::transfer(cpi_ctx, amount)?;

        let bond = &mut ctx.accounts.market_bond;
        bond.amount_quote_lots = 0;
        bond.unbond_request_at_unix = 0;

        emit!(MarketBondClaimedEvent {
            market: bond.market,
            depositor: bond.depositor,
            amount_quote_lots: amount,
        });
        Ok(())
    }

    /// Slash a deployer bond. Protocol authority signs (single source of
    /// truth: insurance_fund.authority). Transfers `amount` quote-lots
    /// from the bond into the insurance fund's balance.
    /// Funds physically stay in the same vault — only the accounting
    /// changes (decrement bond, increment insurance balance).
    ///
    /// Slash conditions are enforced off-chain by governance + monitors
    /// (oracle staleness, insolvency events). On-chain we trust the
    /// authority — same trust model as `withdraw_insurance_fund`.
    pub fn slash_market_bond(
        ctx: Context<SlashMarketBond>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        let bond = &mut ctx.accounts.market_bond;
        require!(
            amount_quote_lots <= bond.amount_quote_lots,
            FlashBookError::BondTooSmall
        );
        bond.amount_quote_lots = bond.amount_quote_lots.saturating_sub(amount_quote_lots);
        bond.total_slashed_quote_lots = bond
            .total_slashed_quote_lots
            .saturating_add(amount_quote_lots);
        // Cancel any pending unbond — slashing is a state change that
        // resets the unbond clock.
        bond.unbond_request_at_unix = 0;

        let fund = &mut ctx.accounts.insurance_fund;
        fund.balance_quote_lots = fund
            .balance_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        fund.total_contributions = fund
            .total_contributions
            .saturating_add(amount_quote_lots);

        emit!(MarketBondSlashedEvent {
            market: bond.market,
            depositor: bond.depositor,
            amount_quote_lots,
            remaining_bond_quote_lots: bond.amount_quote_lots,
        });
        Ok(())
    }

    /// AUTO-DELEVERAGE — the safety primitive of last resort. When the
    /// insurance fund falls below `pause_threshold_quote_lots`, a sick
    /// position cannot be safely liquidated through the normal flow
    /// (the bankruptcy gap exceeds insurance buffer). ADL force-closes
    /// the highest-ranked profitable counter-trader at the BANKRUPTCY
    /// PRICE of the underwater position — they realize their PnL up to
    /// the bankruptcy price (less than market would give them) and the
    /// gap between bp and mark is what would have come from insurance.
    ///
    /// Permissionless. Caller passes:
    ///   • underwater (position + trader_state)
    ///   • counter_position + counter_trader_state (chosen off-chain by
    ///     ranking unrealized_pnl × leverage, highest first)
    ///   • close_size_lots (≤ min(underwater.size, counter.size))
    ///
    /// Eligibility checks (all enforced on-chain):
    ///   1. insurance_fund.balance < pause_threshold (ADL trigger)
    ///   2. underwater is actually unhealthy (margin assessment)
    ///   3. counter.side != underwater.side
    ///   4. counter has positive PnL at the BANKRUPTCY price (fair: we
    ///      never force a counter-trader into a loss they wouldn't have
    ///      had anyway)
    ///
    /// Action:
    ///   • close `close_size` lots from BOTH at the bankruptcy price
    ///   • underwater realizes loss (collateral debited pro-rata)
    ///   • counter realizes profit at bp (less than mark — that's the
    ///     "give up", which absorbs the loss insurance would have eaten)
    ///   • update OI counters
    ///   • emit AutoDeleveragedEvent
    ///
    /// Off-chain ranking is incentivised separately (operators run a
    /// sorted-by-pnl-leverage queue keeper). On-chain we trust the
    /// caller's ranking but enforce eligibility — invalid ranking just
    /// rejects.
    pub fn auto_deleverage(
        ctx: Context<AutoDeleverage>,
        close_size_lots: u64,
    ) -> Result<()> {
        require!(close_size_lots > 0, FlashBookError::ZeroSize);

        let market = &ctx.accounts.market;
        let underwater = &ctx.accounts.underwater_position;
        let counter = &ctx.accounts.counter_position;

        // Sanity: positions on this market, opposite sides, both have size.
        require!(underwater.market == market.key(), FlashBookError::WrongMarket);
        require!(counter.market == market.key(), FlashBookError::WrongMarket);
        require!(underwater.size_lots > 0, FlashBookError::LiquidationStale);
        require!(counter.size_lots > 0, FlashBookError::LiquidationStale);
        require!(underwater.side != counter.side, FlashBookError::OutOfRange);
        require!(
            close_size_lots <= underwater.size_lots,
            FlashBookError::OutOfRange
        );
        require!(
            close_size_lots <= counter.size_lots,
            FlashBookError::OutOfRange
        );

        // Trader-state alignment.
        require!(
            ctx.accounts.underwater_trader_state.trader == underwater.trader,
            FlashBookError::WrongTrader
        );
        require!(
            ctx.accounts.counter_trader_state.trader == counter.trader,
            FlashBookError::WrongTrader
        );
        // Cannot ADL yourself.
        require!(
            underwater.trader != counter.trader,
            FlashBookError::OutOfRange
        );

        // Trigger gate: insurance fund must be below the pause threshold
        // for ADL to be admissible. Above the threshold, normal
        // liquidation is the right path (insurance can absorb the gap).
        let fund = &ctx.accounts.insurance_fund;
        require!(
            fund.balance_quote_lots < fund.pause_threshold_quote_lots,
            FlashBookError::AdlNotEligible
        );

        // Underwater health check — same stress lattice as liquidate_position.
        let pos_snap = RiskPosSnap {
            market: underwater.market,
            side: if underwater.side == 0 { Side::Long } else { Side::Short },
            size_lots: underwater.size_lots,
            entry_price: Ticks(underwater.entry_price_ticks),
            cum_funding_index_at_entry: underwater.cum_funding_index_at_entry,
        };
        let market_snap = RiskMarketSnap {
            market: market.key(),
            mark_price: Ticks(market.mark_price_ticks),
            cum_funding_index: market.cum_funding_index,
            maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
            tick_size: market.params.tick_size,
                concentration_threshold_lots: market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
        };
        let scenarios = default_scenarios_fn(&[market.key()]);
        let assessment = assess_margin_fn(
            &[pos_snap],
            &[market_snap],
            &scenarios,
            ctx.accounts.underwater_trader_state.collateral_quote_lots,
        )?;
        require!(!assessment.is_healthy, FlashBookError::NotLiquidatable);

        // Compute bankruptcy price: the mark at which underwater equity
        // is exactly zero. For long: bp = entry - C / (S × tick).
        //                  short: bp = entry + C / (S × tick).
        // We compute in u128 ticks to avoid overflow at large notional.
        let tick_size = market.params.tick_size as u128;
        require!(tick_size > 0, FlashBookError::ZeroPrice);
        let collateral = ctx.accounts.underwater_trader_state.collateral_quote_lots as u128;
        let entry = underwater.entry_price_ticks as u128;
        let size = underwater.size_lots as u128;
        let denom = size.checked_mul(tick_size)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        require!(denom > 0, FlashBookError::ZeroPrice);
        let collateral_per_lot_ticks = collateral / denom;
        let bp_u128: u128 = if underwater.side == 0 {
            // long: bp = entry - C/(S*tick); clamp to 1 if collateral overshoots
            entry.saturating_sub(collateral_per_lot_ticks).max(1)
        } else {
            // short: bp = entry + C/(S*tick)
            entry.checked_add(collateral_per_lot_ticks)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
        };
        let bp_ticks = if bp_u128 > u64::MAX as u128 { u64::MAX } else { bp_u128 as u64 };

        // Counter-eligibility: counter must have POSITIVE PnL at bp.
        //   Long counter:  pnl = (bp - entry_c) × close × tick > 0  → bp > entry_c
        //   Short counter: pnl = (entry_c - bp) × close × tick > 0  → bp < entry_c
        let counter_entry = counter.entry_price_ticks;
        if counter.side == 0 {
            require!(bp_ticks > counter_entry, FlashBookError::AdlNotEligible);
        } else {
            require!(bp_ticks < counter_entry, FlashBookError::AdlNotEligible);
        }

        // ── Settle PnL ──
        // Underwater realizes loss = collateral × close/size (proportional
        // collateral wiped). Equivalent to: (bp - entry) × close × tick × sign.
        // We compute via collateral fraction to avoid floating drift.
        let loss_quote_lots_u128 = collateral.saturating_mul(close_size_lots as u128) / size;
        let loss_quote_lots = if loss_quote_lots_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            loss_quote_lots_u128 as u64
        };

        // Counter realizes positive PnL at bp.
        // |pnl| = |bp - entry_c| × close × tick
        let counter_gain_per_lot = if counter.side == 0 {
            (bp_ticks as u128).saturating_sub(counter_entry as u128)
        } else {
            (counter_entry as u128).saturating_sub(bp_ticks as u128)
        };
        let counter_gain_u128 = counter_gain_per_lot
            .saturating_mul(close_size_lots as u128)
            .saturating_mul(tick_size);
        let counter_gain = if counter_gain_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            counter_gain_u128 as u64
        };

        // Apply to TraderStates.
        let market_key = market.key();
        let uw_trader = underwater.trader;
        let ct_trader = counter.trader;
        {
            let uts = &mut ctx.accounts.underwater_trader_state;
            uts.collateral_quote_lots = uts.collateral_quote_lots.saturating_sub(loss_quote_lots);
            uts.realized_pnl_quote_lots = uts
                .realized_pnl_quote_lots
                .saturating_sub(loss_quote_lots as i64);
        }
        {
            let cts = &mut ctx.accounts.counter_trader_state;
            cts.collateral_quote_lots = cts
                .collateral_quote_lots
                .checked_add(counter_gain)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            cts.realized_pnl_quote_lots = cts
                .realized_pnl_quote_lots
                .saturating_add(counter_gain as i64);
        }

        // Reduce both positions by close_size. If a side closes to zero,
        // decrement open_positions on that trader_state.
        let uw_was_open = underwater.size_lots > 0;
        let ct_was_open = counter.size_lots > 0;
        let uw_pre_side = underwater.side;
        let ct_pre_side = counter.side;
        let uw_pre_size = underwater.size_lots;
        let ct_pre_size = counter.size_lots;

        {
            let uw = &mut ctx.accounts.underwater_position;
            uw.size_lots = uw.size_lots.saturating_sub(close_size_lots);
            if uw.size_lots == 0 {
                // Reset settlement anchors on close.
                uw.entry_price_ticks = 0;
                uw.unhealthy_since_slot = 0;
                uw.last_liquidated_at_slot = 0;
            }
        }
        {
            let ct = &mut ctx.accounts.counter_position;
            ct.size_lots = ct.size_lots.saturating_sub(close_size_lots);
            if ct.size_lots == 0 {
                ct.entry_price_ticks = 0;
            }
        }

        // OI updates: walk pre→post for each side.
        let uw_post_side = ctx.accounts.underwater_position.side;
        let uw_post_size = ctx.accounts.underwater_position.size_lots;
        let ct_post_side = ctx.accounts.counter_position.side;
        let ct_post_size = ctx.accounts.counter_position.size_lots;
        let market = &mut ctx.accounts.market;
        update_oi(market, uw_pre_side, uw_pre_size, uw_post_side, uw_post_size)?;
        update_oi(market, ct_pre_side, ct_pre_size, ct_post_side, ct_post_size)?;

        // open_positions transitions on TraderStates.
        if uw_was_open && uw_post_size == 0 {
            let uts = &mut ctx.accounts.underwater_trader_state;
            uts.open_positions = uts.open_positions.saturating_sub(1);
        }
        if ct_was_open && ct_post_size == 0 {
            let cts = &mut ctx.accounts.counter_trader_state;
            cts.open_positions = cts.open_positions.saturating_sub(1);
        }
        // Bookkeeping for monitoring.
        market.total_liquidations = market.total_liquidations.saturating_add(1);

        emit!(AutoDeleveragedEvent {
            market: market_key,
            underwater_trader: uw_trader,
            counter_trader: ct_trader,
            close_size_lots,
            bankruptcy_price_ticks: bp_ticks,
            counter_gain_quote_lots: counter_gain,
            executor: ctx.accounts.caller.key(),
        });
        Ok(())
    }

    /// V2: cross-market portfolio liquidation against the hypertree-backed
    /// book. Pure parity port of v1's `liquidate_portfolio`:
    ///   • Walks remaining_accounts in (market, position) pairs
    ///   • Verifies owner program + trader/market binding for each pair
    ///   • Builds the joint cross-market scenario lattice
    ///   • Stress-checks via assess_margin_fn
    ///   • Injects synthetic close on EXECUTION market only
    ///
    /// Only the injection target differs (hypertree, not v1 buffer). The
    /// order_type byte is set to 3 (Liquidation) so wave 19e's matcher
    /// mapping promotes it to FIFO liquidation priority.
    pub fn liquidate_portfolio_v2<'info>(
        ctx: Context<'_, '_, '_, 'info, LiquidatePortfolioV2<'info>>,
    ) -> Result<()> {
        let exec_market = &ctx.accounts.execution_market;
        let exec_position = &ctx.accounts.execution_position;
        let trader_state = &ctx.accounts.trader_state;

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
            concentration_threshold_lots: exec_market.params.concentration_threshold_lots,
            concentration_extra_mmr_bps: exec_market.params.concentration_extra_mmr_bps,
        });
        position_snaps.push(RiskPosSnap {
            market: exec_position.market,
            side: if exec_position.side == 0 { Side::Long } else { Side::Short },
            size_lots: exec_position.size_lots,
            entry_price: Ticks(exec_position.entry_price_ticks),
            cum_funding_index_at_entry: exec_position.cum_funding_index_at_entry,
        });

        let remaining = ctx.remaining_accounts;
        require!(remaining.len() % 2 == 0, FlashBookError::OutOfRange);
        let program_id = ctx.program_id;
        let mut i = 0usize;
        while i + 1 < remaining.len() {
            let market_ai = &remaining[i];
            let position_ai = &remaining[i + 1];
            require_keys_eq!(*market_ai.owner, *program_id, FlashBookError::Unauthorized);
            require_keys_eq!(*position_ai.owner, *program_id, FlashBookError::Unauthorized);

            let m_data = market_ai.try_borrow_data()?;
            let market: MarketAccount =
                MarketAccount::try_deserialize(&mut &m_data[..])?;
            let p_data = position_ai.try_borrow_data()?;
            let position: state::PositionAccount =
                state::PositionAccount::try_deserialize(&mut &p_data[..])?;
            require!(
                position.trader == trader_state.trader,
                FlashBookError::WrongTrader
            );
            require!(
                position.market == market_ai.key(),
                FlashBookError::WrongMarket
            );

            if position.size_lots > 0 {
                market_snaps.push(RiskMarketSnap {
                    market: market_ai.key(),
                    mark_price: Ticks(market.mark_price_ticks),
                    cum_funding_index: market.cum_funding_index,
                    maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
                    tick_size: market.params.tick_size,
                    concentration_threshold_lots: market.params.concentration_threshold_lots,
                    concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
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

        let market_keys: Vec<Pubkey> = market_snaps.iter().map(|m| m.market).collect();
        let scenarios = default_scenarios_fn(&market_keys);
        let assessment = assess_margin_fn(
            &position_snaps,
            &market_snaps,
            &scenarios,
            trader_state.collateral_quote_lots,
        )?;
        require!(!assessment.is_healthy, FlashBookError::NotLiquidatable);

        // Inject liquidation order on the EXECUTION market's hypertree.
        let pos_side = if exec_position.side == 0 { Side::Long } else { Side::Short };
        let close_side = pos_side.opposite();
        let penalty = exec_market.params.liq_penalty_bps as u128;
        let oracle = exec_market.oracle_price_ticks as u128;
        let penalty_delta = (oracle * penalty) / constants::BPS_DENOM as u128;
        let limit = match close_side {
            Side::Short => oracle.saturating_sub(penalty_delta) as u64,
            Side::Long => oracle.saturating_add(penalty_delta) as u64,
        };

        let trader = exec_position.trader;
        let close_size = exec_position.size_lots;
        let close_side_u8 = close_side as u8;
        let market_key = exec_market.key();
        let now_slot = Clock::get()?.slot;
        let inserted_idx;
        let next_seq;
        {
            let mut book_data = ctx.accounts.execution_market_book.try_borrow_mut_data()?;
            let mut handle =
                state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
            require!(
                handle.header.market_pubkey == market_key,
                FlashBookError::WrongMarket
            );
            next_seq = handle
                .header
                .order_seq_counter
                .checked_add(1)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            require!(
                next_seq < FLP_SEQ_RESERVED_OFFSET,
                FlashBookError::OutOfRange
            );
            handle.header.order_seq_counter = next_seq;

            let side_is_bid = close_side_u8 == 0;
            let order = state_v2::RestingOrderV2 {
                order_id: state_v2::encode_order_id(limit, next_seq, side_is_bid),
                seq: next_seq,
                price_ticks: limit,
                size_lots: close_size,
                expires_at_slot: 0,
                trader,
                last_valid_slot: now_slot as u32,
                side: close_side_u8,
                order_type: 3, // Liquidation
                flags: 0,
                _pad: 0,
            };
            inserted_idx = if side_is_bid {
                handle.insert_bid(order)?
            } else {
                handle.insert_ask(order)?
            };
        }

        emit!(LiquidationInjectedV2Event {
            market: market_key,
            trader,
            side: pos_side as u8,
            size_lots: close_size,
            limit_ticks: limit,
            worst_scenario_idx: assessment.worst_scenario_idx,
            order_seq: next_seq,
            node_index: inserted_idx,
        });
        Ok(())
    }

    /// Delegate the commit_buffer PDA to the MagicBlock ER. Required for
    /// run_batch_v2 on the ER to call sweep_expired() against the commit
    /// buffer (which mutates expired bonds). Pair with delegate_market_book
    /// + delegate_market — all three must be live for full ER tick.
    pub fn delegate_commit_buffer(
        ctx: Context<DelegateCommitBuffer>,
        commit_frequency_ms: u32,
        validator: Option<Pubkey>,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let bump = ctx.accounts.commit_buffer.bump;

        require_keys_eq!(
            *ctx.accounts.commit_buffer.to_account_info().owner,
            *ctx.program_id,
            FlashBookError::Unauthorized
        );

        let seeds_for_args: Vec<Vec<u8>> = vec![
            CommitBufferAccount::SEED.to_vec(),
            market_key.as_ref().to_vec(),
            vec![bump],
        ];
        let signer_seeds: &[&[u8]] = &[
            CommitBufferAccount::SEED,
            market_key.as_ref(),
            &[bump],
        ];

        er::cpi_delegate(
            er::DelegateAccounts {
                payer: ctx.accounts.authority.to_account_info(),
                delegated_account: ctx.accounts.commit_buffer.to_account_info(),
                owner_program: ctx.accounts.owner_program.to_account_info(),
                delegate_buffer: ctx.accounts.delegate_buffer.to_account_info(),
                delegation_record: ctx.accounts.delegation_record.to_account_info(),
                delegation_metadata: ctx.accounts.delegation_metadata.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                delegation_program: ctx.accounts.delegation_program.to_account_info(),
            },
            er::DelegateArgs {
                commit_frequency_ms,
                seeds: seeds_for_args,
                validator,
            },
            signer_seeds,
        )?;

        emit!(CommitBufferDelegatedEvent {
            market: market_key,
            commit_buffer: ctx.accounts.commit_buffer.key(),
            commit_frequency_ms,
            validator: validator.unwrap_or_default(),
        });
        Ok(())
    }

    /// Undelegate the commit_buffer PDA from the ER back to mainnet.
    pub fn undelegate_commit_buffer(
        ctx: Context<UndelegateCommitBuffer>,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let bump = ctx.accounts.commit_buffer.bump;
        let signer_seeds: &[&[u8]] = &[
            CommitBufferAccount::SEED,
            market_key.as_ref(),
            &[bump],
        ];

        er::cpi_undelegate(
            er::UndelegateAccounts {
                payer: ctx.accounts.authority.to_account_info(),
                delegated_account: ctx.accounts.commit_buffer.to_account_info(),
                owner_program: ctx.accounts.owner_program.to_account_info(),
                buffer: ctx.accounts.delegate_buffer.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                delegation_program: ctx.accounts.delegation_program.to_account_info(),
            },
            signer_seeds,
        )?;

        emit!(CommitBufferUndelegatedEvent {
            market: market_key,
            commit_buffer: ctx.accounts.commit_buffer.key(),
        });
        Ok(())
    }

    // ER delegation lifecycle ixs (delegate_market_book, undelegate_market_book,
    // delegate_market, undelegate_market, delegate_commit_buffer,
    // undelegate_commit_buffer) ship in waves 19b + 19g via in-house CPI
    // wrappers in `src/er.rs`. flp_exposure is intentionally NOT
    // ER-delegatable: it's a singleton, so delegating it would bottleneck
    // ALL markets to a single ER instance. Per-market FLP exposure is
    // queued for wave 21 (modular wrapper programs).
    //
    // Historical note: in-house CPI sidesteps the upstream
    // `ephemeral-rollups-sdk` Solana 2.x compat issue. When upstream lands
    // 2.1-compatible release we can swap to it drop-in via the Delegate /
    // Undelegate ix builders in `src/er.rs`.

    /// HIP-3 / permissionless market deployment. ANY signer can call this
    /// to deploy a new market — no protocol authority gating. The caller
    /// becomes BOTH the market authority AND the creator (earns
    /// `creator_share_bps` of net fee on every fill, forever). Params
    /// are clamped to a SAFE ENVELOPE that prevents griefing the FLP /
    /// users with hostile economics:
    ///
    ///   • max_leverage capped at MAX_PERMISSIONLESS_LEVERAGE (20×)
    ///   • taker_fee_bps clamped to [10, 200] (0.1% – 2%)
    ///   • maker_rebate_bps ≤ taker_fee_bps × 80%
    ///   • maintenance_margin_ratio_bps ≥ 200 (2% floor)
    ///   • initial_margin_ratio_bps ≥ maintenance × 2
    ///   • liq_penalty_bps ∈ [50, 500]
    ///   • creator_share_bps ≤ MAX_PERMISSIONLESS_CREATOR_SHARE_BPS (3000)
    ///   • builder_share_bps ≤ 1500, referrer_share_bps ≤ 2500
    ///   • max_position_lots_per_trader > 0 (must be set; no unlimited)
    ///   • max_position_ratio_bps > 0 and ≤ 100 (≤ 1% of FLP per trader)
    ///   • toxicity_tax_max_bps ≤ 50 (anti-griefing taxes capped)
    ///
    /// Anything outside the envelope rejects with OutOfRange. Inside, the
    /// market deploys with creator = caller, authority = caller. The
    /// caller can later call update_market_params to tune within the
    /// envelope (the envelope is re-applied on every update).
    ///
    /// No bond is required in v1 — the safe envelope alone gates
    /// griefing. A future version may add a slashable HYPE-style bond
    /// to back the FLP exposure on the new market.
    pub fn permissionless_initialize_market(
        ctx: Context<InitializeMarket>,
        params: MarketParams,
        initial_oracle_ticks: u64,
    ) -> Result<()> {
        let clamped = clamp_permissionless_params(&params)?;
        // Reuse the canonical init logic — same state-init invariants —
        // and then patch the creator field after.
        initialize_market_inner(ctx, clamped, initial_oracle_ticks, true)
    }
}

/// Permissionless market params validation. Returns an error if the
/// params are outside the safe envelope; returns the clamped params on
/// success. Clamps fields silently where it can (e.g. taker_fee 5 →
/// MIN_PERMISSIONLESS_TAKER_FEE_BPS); rejects where ambiguity would be
/// dangerous (e.g. max_leverage > cap).
fn clamp_permissionless_params(p: &MarketParams) -> Result<MarketParams> {
    const MAX_PERMISSIONLESS_LEVERAGE: u32 = 20;
    const MIN_PERMISSIONLESS_TAKER_FEE_BPS: u32 = 10;
    const MAX_PERMISSIONLESS_TAKER_FEE_BPS: u32 = 200;
    const MIN_PERMISSIONLESS_MAINT_MARGIN_BPS: u32 = 200;
    const MIN_PERMISSIONLESS_LIQ_PENALTY_BPS: u32 = 50;
    const MAX_PERMISSIONLESS_LIQ_PENALTY_BPS: u32 = 500;
    const MAX_PERMISSIONLESS_CREATOR_SHARE_BPS: u32 = 3_000;
    const MAX_PERMISSIONLESS_BUILDER_SHARE_BPS: u32 = 1_500;
    const MAX_PERMISSIONLESS_REFERRER_SHARE_BPS: u32 = 2_500;
    const MAX_PERMISSIONLESS_POS_RATIO_BPS: u32 = 100;
    const MAX_PERMISSIONLESS_TOXICITY_TAX_BPS: u32 = 50;

    require!(
        p.max_leverage >= 1 && p.max_leverage <= MAX_PERMISSIONLESS_LEVERAGE,
        FlashBookError::OutOfRange
    );
    require!(
        p.taker_fee_bps >= MIN_PERMISSIONLESS_TAKER_FEE_BPS
            && p.taker_fee_bps <= MAX_PERMISSIONLESS_TAKER_FEE_BPS,
        FlashBookError::OutOfRange
    );
    // maker_rebate_bps is SIGNED (i32). Permissionless markets cap the
    // POSITIVE rebate at 80% of taker fee (insurance fund must net >0
    // on every fill). Negative values (= maker fee) are bounded by
    // MAX_FEE_TIER_BPS in absolute terms — large maker fees would
    // discourage MM and harm liquidity, so we reuse the same cap.
    let max_maker_rebate = (p.taker_fee_bps as u64).saturating_mul(80) / 100;
    if p.maker_rebate_bps >= 0 {
        require!(
            (p.maker_rebate_bps as u64) <= max_maker_rebate,
            FlashBookError::OutOfRange
        );
    } else {
        require!(
            ((-p.maker_rebate_bps) as u32) <= constants::MAX_FEE_TIER_BPS,
            FlashBookError::OutOfRange
        );
    }
    require!(
        p.maintenance_margin_ratio_bps >= MIN_PERMISSIONLESS_MAINT_MARGIN_BPS,
        FlashBookError::OutOfRange
    );
    require!(
        p.initial_margin_ratio_bps
            >= p.maintenance_margin_ratio_bps.saturating_mul(2),
        FlashBookError::OutOfRange
    );
    require!(
        p.liq_penalty_bps >= MIN_PERMISSIONLESS_LIQ_PENALTY_BPS
            && p.liq_penalty_bps <= MAX_PERMISSIONLESS_LIQ_PENALTY_BPS,
        FlashBookError::OutOfRange
    );
    require!(
        p.creator_share_bps <= MAX_PERMISSIONLESS_CREATOR_SHARE_BPS,
        FlashBookError::OutOfRange
    );
    require!(
        p.builder_share_bps <= MAX_PERMISSIONLESS_BUILDER_SHARE_BPS,
        FlashBookError::OutOfRange
    );
    require!(
        p.referrer_share_bps <= MAX_PERMISSIONLESS_REFERRER_SHARE_BPS,
        FlashBookError::OutOfRange
    );
    require!(
        p.max_position_lots_per_trader > 0,
        FlashBookError::OutOfRange
    );
    require!(
        p.max_position_ratio_bps > 0
            && p.max_position_ratio_bps <= MAX_PERMISSIONLESS_POS_RATIO_BPS,
        FlashBookError::OutOfRange
    );
    require!(
        p.toxicity_tax_max_bps <= MAX_PERMISSIONLESS_TOXICITY_TAX_BPS,
        FlashBookError::OutOfRange
    );
    Ok(p.clone())
}

/// Shared init body for `initialize_market` and
/// `permissionless_initialize_market`. The single difference is whether
/// `market.creator` is set to the signer (permissionless) or zeroed
/// (protocol).
fn initialize_market_inner(
    ctx: Context<InitializeMarket>,
    params: MarketParams,
    initial_oracle_ticks: u64,
    is_permissionless: bool,
) -> Result<()> {
    require!(params.tick_size > 0, FlashBookError::OutOfRange);
    require!(params.base_lot_size > 0, FlashBookError::OutOfRange);
    require!(params.quote_lot_size > 0, FlashBookError::OutOfRange);
    require!(params.max_leverage >= 1, FlashBookError::OutOfRange);
    require!(initial_oracle_ticks > 0, FlashBookError::ZeroPrice);

    let market = &mut ctx.accounts.market;
    market.authority = ctx.accounts.authority.key();
    market.creator = if is_permissionless {
        ctx.accounts.authority.key()
    } else {
        Pubkey::default()
    };
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
    market.period_started_at_unix = 0;
    market.period_funding_paid_abs_bps = 0;
    market.params = params;

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
    if is_permissionless {
        emit!(PermissionlessMarketDeployedEvent {
            market: market.key(),
            creator: market.creator,
            creator_share_bps: market.params.creator_share_bps,
            is_pre_launch: market.params.is_pre_launch,
        });
    }
    Ok(())
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

    /// commit_buffer init alongside the market; commit-reveal needs it.
    /// (v1 also init'd order_buffer here; deleted in wave 19i since the
    /// hypertree-backed market_book PDA is initialized via init_market_book.)
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
pub struct PlaceLimitOrderV2<'info> {
    pub trader: Signer<'info>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: PDA at the market_book seed; we own + validate the
    /// 8-byte custom disc inside the handler via `MarketBookHandle::
    /// from_account_data`. Mut because we write the new resting order
    /// node + update the header indices.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CpiReleaseCollateralToUser<'info> {
    /// CPI authority — must be one of the 3 wrapper-program PDAs.
    pub cpi_authority: Signer<'info>,

    /// InsuranceFund PDA — owns the vault, signs the SPL transfer out.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    /// Source vault (program-owned, InsuranceFund authority).
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// Destination — user's ATA. Mut because we credit it.
    #[account(mut)]
    pub user_quote_ata: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct CpiOpenTraderStateForTrader<'info> {
    /// Wrapper CPI authority (one of the 3 wave-21 PDAs).
    pub cpi_authority: Signer<'info>,

    /// CHECK: pubkey that the new TraderState will be seeded by. For
    /// vault trading this is the vault PDA. The TraderState's
    /// `trader` field is set to this. Used as the seed only — no
    /// signing required (the wrapper is the authority).
    pub trader_owner: UncheckedAccount<'info>,

    /// The wrapper PAYS for rent here. Use the cpi_authority pubkey
    /// directly is awkward (PDA can't pay rent without lamport
    /// transfer); instead, pass a separate fee-payer signer.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = TraderStateAccount::space(),
        seeds = [TraderStateAccount::SEED, trader_owner.key().as_ref()],
        bump,
    )]
    pub trader_state: Box<Account<'info, TraderStateAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CpiCreditOrDebitCollateral<'info> {
    pub cpi_authority: Signer<'info>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader_state.trader.as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Box<Account<'info, TraderStateAccount>>,
}

#[derive(Accounts)]
pub struct CancelOrderV2Cpi<'info> {
    pub cpi_authority: Signer<'info>,

    /// CHECK: trader pubkey carried on the resting order. Used to
    /// validate ownership.
    pub trader: UncheckedAccount<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: hypertree PDA; disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct PlaceLimitOrderV2Cpi<'info> {
    /// CPI authority — must be the `[CPI_AUTHORITY_SEED]` PDA of one
    /// of the 3 whitelisted wrapper programs (orders / flp / vaults).
    /// Wrapper signs over this PDA via `invoke_signed`. Core's handler
    /// computes the 3 expected derivations and verifies the signer
    /// matches one of them.
    pub cpi_authority: Signer<'info>,

    /// CHECK: trader pubkey to stamp on the resulting RestingOrderV2.
    /// NOT a signer — the wrapper authorized the trader at trigger /
    /// vault-deposit time via its own state. Core trusts the wrapper
    /// to inject the right trader pubkey here.
    pub trader: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: PDA at the market_book seed; disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ViewBookDepthV2<'info> {
    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: read-only view of the market_book PDA. Disc validation
    /// happens inside the handler via `MarketBookHandle::from_account_data`.
    #[account(
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CancelOrderV2<'info> {
    pub trader: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: PDA at the market_book seed; disc validated inside handler.
    /// Mut because we remove a node + update header indices + free-list.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct RunBatchV2<'info> {
    pub sequencer: Signer<'info>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: PDA at the market_book seed; disc validated inside handler.
    /// Mut because the matcher mutates filled-order sizes + removes
    /// fully-filled nodes back to the free-list.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    /// Mut so commit-buffer expired-bond sweep can clear stale entries.
    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,

    /// Read-only — feeds FLP virtual quote generation. The pool's NAV
    /// is mutated by `apply_flp_fill` (separate ix), not by run_batch.
    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,
}

#[derive(Accounts)]
pub struct DelegateMarketBook<'info> {
    /// Pays for the delegation buffer + delegation record allocation.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: PDA we own; signed via seeds for the delegate CPI. Anchor
    /// verifies the seeds + bump match. Inside the handler we additionally
    /// verify .owner == this program (defence-in-depth — er.rs SECURITY note).
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    /// CHECK: this program's account info (passed as `owner_program` to the
    /// MagicBlock delegation program). Verified inside cpi_delegate via
    /// the program ID match. Constraint pins it to this crate's program ID.
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,

    /// CHECK: PDA under owner_program at [b"buffer", market_book]. The
    /// MagicBlock delegation program initialises this; we don't preallocate.
    #[account(mut)]
    pub delegate_buffer: UncheckedAccount<'info>,

    /// CHECK: PDA under DELEGATION_PROGRAM_ID at [b"delegation", market_book].
    /// Initialised by the delegation program.
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,

    /// CHECK: PDA under DELEGATION_PROGRAM_ID at [b"delegation-metadata", market_book].
    /// Initialised by the delegation program.
    #[account(mut)]
    pub delegation_metadata: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: MagicBlock delegation program. Address pinned to the
    /// canonical DELEGATION_PROGRAM_ID; cpi_delegate also rechecks.
    #[account(address = er::DELEGATION_PROGRAM_ID)]
    pub delegation_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct UndelegateMarketBook<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: PDA we own; signed via seeds for undelegate CPI.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    /// CHECK: this program's account info.
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,

    /// CHECK: same buffer PDA from delegate (it carries the committed state).
    #[account(mut)]
    pub delegate_buffer: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: MagicBlock delegation program.
    #[account(address = er::DELEGATION_PROGRAM_ID)]
    pub delegation_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct DelegateMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// The market account itself becomes a delegated account; mut so we
    /// can sign over it via seeds (PDA-as-signer for invoke_signed).
    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: this program's account info.
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,

    /// CHECK: PDA under owner_program at [b"buffer", market].
    #[account(mut)]
    pub delegate_buffer: UncheckedAccount<'info>,

    /// CHECK: PDA under DELEGATION_PROGRAM_ID at [b"delegation", market].
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,

    /// CHECK: PDA under DELEGATION_PROGRAM_ID at [b"delegation-metadata", market].
    #[account(mut)]
    pub delegation_metadata: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: MagicBlock delegation program.
    #[account(address = er::DELEGATION_PROGRAM_ID)]
    pub delegation_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct UndelegateMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: this program's account info.
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,

    /// CHECK: same buffer PDA from delegate.
    #[account(mut)]
    pub delegate_buffer: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: MagicBlock delegation program.
    #[account(address = er::DELEGATION_PROGRAM_ID)]
    pub delegation_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct InitMarketBook<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: PDA we own; allocated via SystemProgram CPI in the
    /// handler. Anchor's `init` constraint can't size this (8264 B
    /// with the [u8; 8000] field), and `#[account(zero_copy)]` rejects
    /// large array fields in its derive expansion. Manifest's pattern.
    /// Owner check happens implicitly via the seeds derivation.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeCommitBuffer<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        init,
        payer = authority,
        space = CommitBufferAccount::space(),
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,

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
pub struct SetPositionLeverage<'info> {
    /// Trader OR delegate may sign — TraderStateAccount.is_authorized
    /// gates the action via the trader_state.delegate slot. Trader is
    /// the authority of record (position.trader); delegate is allowed
    /// for hot-key UX patterns.
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    #[account(
        seeds = [TraderStateAccount::SEED, position.trader.as_ref()],
        bump = trader_state.bump,
        constraint = trader_state.is_authorized(&authority.key()) @ FlashBookError::Unauthorized,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), position.trader.as_ref()],
        bump = position.bump,
    )]
    pub position: Account<'info, state::PositionAccount>,
}

#[derive(Accounts)]
pub struct SweepCollateral<'info> {
    /// Master authority (trader OR delegate of BOTH source and destination
    /// trader_states). Authorization is checked against each via
    /// `is_authorized` in the handler.
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, from_state.trader.as_ref()],
        bump = from_state.bump,
    )]
    pub from_state: Account<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, to_state.trader.as_ref()],
        bump = to_state.bump,
    )]
    pub to_state: Account<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct SettleVaultPerfFee<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        mut,
        seeds = [
            state::VaultAccount::SEED,
            strategist.key().as_ref(),
            &[vault.vault_id],
        ],
        bump = vault.bump,
        constraint = vault.strategist == strategist.key() @ FlashBookError::Unauthorized,
    )]
    pub vault: Box<Account<'info, state::VaultAccount>>,

    #[account(
        seeds = [TraderStateAccount::SEED, vault.key().as_ref()],
        bump = vault_trader_state.bump,
        constraint = vault_trader_state.key() == vault.trader_state @ FlashBookError::OutOfRange,
    )]
    pub vault_trader_state: Box<Account<'info, TraderStateAccount>>,

    /// Strategist's vault_position — minted into on settle. Created
    /// lazily on first non-zero settlement.
    #[account(
        init_if_needed,
        payer = strategist,
        space = state::VaultPositionAccount::space(),
        seeds = [
            state::VaultPositionAccount::SEED,
            vault.key().as_ref(),
            strategist.key().as_ref(),
        ],
        bump,
    )]
    pub strategist_position: Box<Account<'info, state::VaultPositionAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetTraderBuilder<'info> {
    /// Trader signs — the user is the only one who can install or rotate
    /// the builder for their account. Protocol authority does NOT have
    /// this power (otherwise builders could be installed against the
    /// user's will).
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
pub struct InitMarketLeverageTiers<'info> {
    #[account(mut, address = market.authority)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    #[account(
        init,
        payer = authority,
        space = MarketLeverageTiersAccount::space(),
        seeds = [MarketLeverageTiersAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub leverage_tiers: Account<'info, MarketLeverageTiersAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateMarketLeverageTiers<'info> {
    #[account(address = market.authority)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    #[account(
        mut,
        seeds = [MarketLeverageTiersAccount::SEED, market.key().as_ref()],
        bump = leverage_tiers.bump,
    )]
    pub leverage_tiers: Account<'info, MarketLeverageTiersAccount>,
}

// ─── Wave 22 — Multi-tier fee table ix accounts ──────────────────────

#[derive(Accounts)]
pub struct InitFeeTiers<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = state::FeeTiersAccount::space(),
        seeds = [state::FeeTiersAccount::SEED],
        bump,
    )]
    pub fee_tiers: Account<'info, state::FeeTiersAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateFeeTiers<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [state::FeeTiersAccount::SEED],
        bump = fee_tiers.bump,
    )]
    pub fee_tiers: Account<'info, state::FeeTiersAccount>,
}

#[derive(Accounts)]
pub struct ViewTraderEffectiveTier<'info> {
    /// CHECK: trader pubkey — used as the trader_state seed only.
    pub trader: UncheckedAccount<'info>,

    #[account(
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,

    #[account(
        seeds = [state::FeeTiersAccount::SEED],
        bump = fee_tiers.bump,
    )]
    pub fee_tiers: Account<'info, state::FeeTiersAccount>,
}

#[derive(Accounts)]
pub struct SubmitCommit<'info> {
    pub trader: Signer<'info>,
    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,
    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,
}

#[derive(Accounts)]
pub struct SubmitRevealV2<'info> {
    pub trader: Signer<'info>,
    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,
    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,
    /// CHECK: hypertree PDA; disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
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
pub struct PartialWithdrawCollateral<'info> {
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
    // remaining_accounts: alternating (market, position) pairs for every
    // market the trader has a non-zero position in. Walked inside the
    // handler to compute total notional + IM required for the floor check.
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
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        mut,
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, taker_trader_state.trader.as_ref()],
        bump = taker_trader_state.bump,
    )]
    pub taker_trader_state: Box<Account<'info, TraderStateAccount>>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, maker_trader_state.trader.as_ref()],
        bump = maker_trader_state.bump,
    )]
    pub maker_trader_state: Box<Account<'info, TraderStateAccount>>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), taker_trader_state.trader.as_ref()],
        bump,
    )]
    pub taker_position: Box<Account<'info, state::PositionAccount>>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), maker_trader_state.trader.as_ref()],
        bump,
    )]
    pub maker_position: Box<Account<'info, state::PositionAccount>>,

    /// WAVE 22 phase 2: optional global fee-tier table. When supplied,
    /// per-trader maker rebate / taker fee bps are resolved from this
    /// account against the trader's `volume_30d_quote_lots` (HL/Binance
    /// pattern). When omitted (None), apply_fill falls back to the flat
    /// `market.params.{maker_rebate_bps, taker_fee_bps}` per the
    /// pre-tier behavior. Singleton PDA at `[b"fee_tiers"]`.
    pub fee_tiers: Option<Box<Account<'info, state::FeeTiersAccount>>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceBasketOrderV2<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.bump,
        constraint = trader_state.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: Box<Account<'info, TraderStateAccount>>,

    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,

    // ── Leg A ──
    #[account(
        seeds = [MarketAccount::SEED, market_a.base_mint.as_ref(), market_a.quote_mint.as_ref()],
        bump = market_a.bump,
    )]
    pub market_a: Box<Account<'info, MarketAccount>>,

    /// CHECK: hypertree PDA for leg A; disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market_a.key().as_ref()],
        bump,
    )]
    pub market_book_a: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = trader,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market_a.key().as_ref(), trader.key().as_ref()],
        bump,
    )]
    pub position_a: Box<Account<'info, state::PositionAccount>>,

    // ── Leg B ──
    #[account(
        seeds = [MarketAccount::SEED, market_b.base_mint.as_ref(), market_b.quote_mint.as_ref()],
        bump = market_b.bump,
    )]
    pub market_b: Box<Account<'info, MarketAccount>>,

    /// CHECK: hypertree PDA for leg B; disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market_b.key().as_ref()],
        bump,
    )]
    pub market_book_b: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = trader,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market_b.key().as_ref(), trader.key().as_ref()],
        bump,
    )]
    pub position_b: Box<Account<'info, state::PositionAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceBasketOrderNV2<'info> {
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
    // Per-leg accounts arrive in remaining_accounts as triples:
    //   [market_0, market_book_0, position_0,
    //    market_1, market_book_1, position_1, ...]
}

#[derive(Accounts)]
pub struct ExecuteTriggerOrderV2<'info> {
    /// Permissionless caller pays tx fee.
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: PDA at the market_book seed; disc validated inside handler
    /// via `MarketBookHandle::from_account_data`. Mut because the trigger
    /// inserts a new resting order.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            state::TriggerOrderAccount::SEED,
            market.key().as_ref(),
            trigger_order.trader.as_ref(),
            &[trigger_order.trigger_id],
        ],
        bump = trigger_order.bump,
    )]
    pub trigger_order: Account<'info, state::TriggerOrderAccount>,

    /// Trader's position — required when reduce_only flag is set.
    /// Same lazy-load pattern as v1.
    #[account(
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trigger_order.trader.as_ref()],
        bump,
    )]
    pub position: Account<'info, state::PositionAccount>,
}

#[derive(Accounts)]
pub struct UpdateTrailingStop<'info> {
    /// Permissionless. Caller pays tx fee. Production deployments wire
    /// this to a per-market keeper that reads oracle ticks and calls
    /// the ix when the favourable-direction move ≥ 1 tick.
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    #[account(
        mut,
        seeds = [
            state::TriggerOrderAccount::SEED,
            market.key().as_ref(),
            trigger_order.trader.as_ref(),
            &[trigger_order.trigger_id],
        ],
        bump = trigger_order.bump,
    )]
    pub trigger_order: Account<'info, state::TriggerOrderAccount>,
}

#[derive(Accounts)]
pub struct CancelTriggerOrder<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            state::TriggerOrderAccount::SEED,
            trigger_order.market.as_ref(),
            trigger_order.trader.as_ref(),
            &[trigger_order.trigger_id],
        ],
        bump = trigger_order.bump,
    )]
    pub trigger_order: Account<'info, state::TriggerOrderAccount>,
    // OCO partner (if linked) is passed via remaining_accounts. Optional.
}

#[derive(Accounts)]
pub struct ExecuteTwapSliceV2<'info> {
    /// Permissionless caller pays tx fee.
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: PDA at the market_book seed; disc validated inside handler
    /// via `MarketBookHandle::from_account_data`. Mut because the slice
    /// inserts a new resting order.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            state::TwapOrderAccount::SEED,
            market.key().as_ref(),
            twap_order.trader.as_ref(),
            &[twap_order.twap_id],
        ],
        bump = twap_order.bump,
    )]
    pub twap_order: Account<'info, state::TwapOrderAccount>,
}

#[derive(Accounts)]
pub struct CancelTwapOrder<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            state::TwapOrderAccount::SEED,
            twap_order.market.as_ref(),
            twap_order.trader.as_ref(),
            &[twap_order.twap_id],
        ],
        bump = twap_order.bump,
    )]
    pub twap_order: Account<'info, state::TwapOrderAccount>,
}

#[derive(Accounts)]
pub struct ReplenishIcebergV2<'info> {
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: PDA at the market_book seed; disc validated inside handler
    /// via `MarketBookHandle::from_account_data`. Mut because we both
    /// READ (probe by order_id for "still resting" check) and WRITE
    /// (insert next chunk).
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            state::IcebergOrderAccount::SEED,
            market.key().as_ref(),
            iceberg_order.trader.as_ref(),
            &[iceberg_order.iceberg_id],
        ],
        bump = iceberg_order.bump,
    )]
    pub iceberg_order: Account<'info, state::IcebergOrderAccount>,
}

#[derive(Accounts)]
pub struct CancelIcebergV2<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    /// CHECK: PDA at market_book seed; disc validated inside handler.
    /// Mut because the active child (if still resting) is removed.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, iceberg_order.market.as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            state::IcebergOrderAccount::SEED,
            iceberg_order.market.as_ref(),
            iceberg_order.trader.as_ref(),
            &[iceberg_order.iceberg_id],
        ],
        bump = iceberg_order.bump,
    )]
    pub iceberg_order: Account<'info, state::IcebergOrderAccount>,
}

#[derive(Accounts)]
pub struct ViewPortfolioRisk<'info> {
    #[account(
        seeds = [TraderStateAccount::SEED, trader_state.trader.as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Account<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct ViewMarket<'info> {
    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,
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
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        mut,
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, taker_trader_state.trader.as_ref()],
        bump = taker_trader_state.bump,
    )]
    pub taker_trader_state: Box<Account<'info, TraderStateAccount>>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), taker_trader_state.trader.as_ref()],
        bump,
    )]
    pub taker_position: Box<Account<'info, state::PositionAccount>>,

    #[account(
        mut,
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,

    /// WAVE 22 phase 2 (FLP path): optional global fee-tier table.
    /// When supplied, the TAKER's `taker_fee_bps` is resolved per
    /// their rolling-window volume. FLP-side maker rebate stays flat
    /// (FLP IS the protocol — tier-tier semantics don't apply on the
    /// maker side here). When omitted, falls back to flat
    /// `market.params.taker_fee_bps`.
    pub fee_tiers: Option<Box<Account<'info, state::FeeTiersAccount>>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct LiquidatePortfolioV2<'info> {
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, execution_market.base_mint.as_ref(), execution_market.quote_mint.as_ref()],
        bump = execution_market.bump,
    )]
    pub execution_market: Account<'info, MarketAccount>,

    /// CHECK: hypertree PDA for the execution market; disc validated inside.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, execution_market.key().as_ref()],
        bump,
    )]
    pub execution_market_book: UncheckedAccount<'info>,

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
pub struct DelegateCommitBuffer<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,

    /// CHECK: this program's account info.
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,

    /// CHECK: PDA under owner_program at [b"buffer", commit_buffer].
    #[account(mut)]
    pub delegate_buffer: UncheckedAccount<'info>,

    /// CHECK: PDA under DELEGATION_PROGRAM_ID.
    #[account(mut)]
    pub delegation_record: UncheckedAccount<'info>,

    /// CHECK: PDA under DELEGATION_PROGRAM_ID.
    #[account(mut)]
    pub delegation_metadata: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: MagicBlock delegation program.
    #[account(address = er::DELEGATION_PROGRAM_ID)]
    pub delegation_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct UndelegateCommitBuffer<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        mut,
        seeds = [CommitBufferAccount::SEED, market.key().as_ref()],
        bump = commit_buffer.bump,
    )]
    pub commit_buffer: Box<Account<'info, CommitBufferAccount>>,

    /// CHECK: this program's account info.
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,

    /// CHECK: same buffer PDA from delegate.
    #[account(mut)]
    pub delegate_buffer: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: MagicBlock delegation program.
    #[account(address = er::DELEGATION_PROGRAM_ID)]
    pub delegation_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct LiquidatePositionV2<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: PDA at market_book seed; disc validated inside handler.
    /// Mut because the synthetic close order is inserted into the
    /// hypertree.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader_state.trader.as_ref()],
        bump = trader_state.bump,
    )]
    pub trader_state: Box<Account<'info, TraderStateAccount>>,

    #[account(
        init_if_needed,
        payer = caller,
        space = TraderStateAccount::space(),
        seeds = [TraderStateAccount::SEED, caller.key().as_ref()],
        bump,
    )]
    pub caller_trader_state: Box<Account<'info, TraderStateAccount>>,

    /// MUT — wave 19e fixes a v1 latent bug: v1's LiquidatePosition has
    /// this WITHOUT `mut`, so writes to `unhealthy_since_slot` /
    /// `last_liquidated_at_slot` silently don't persist. Anchor doesn't
    /// serialize back accounts not declared mut. v2 correctly marks it.
    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader_state.trader.as_ref()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, state::PositionAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PostMarketBond<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = state::MarketBondAccount::space(),
        seeds = [
            state::MarketBondAccount::SEED,
            market.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump,
    )]
    pub market_bond: Box<Account<'info, state::MarketBondAccount>>,

    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = depositor,
    )]
    pub depositor_quote_ata: Box<Account<'info, TokenAccount>>,

    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UnbondMarketBondAuth<'info> {
    /// Depositor signs — only the bond owner can request unbond.
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [
            state::MarketBondAccount::SEED,
            market_bond.market.as_ref(),
            depositor.key().as_ref(),
        ],
        bump = market_bond.bump,
        constraint = market_bond.depositor == depositor.key() @ FlashBookError::Unauthorized,
    )]
    pub market_bond: Account<'info, state::MarketBondAccount>,
}

#[derive(Accounts)]
pub struct ClaimUnbondedMarketBond<'info> {
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [
            state::MarketBondAccount::SEED,
            market_bond.market.as_ref(),
            depositor.key().as_ref(),
        ],
        bump = market_bond.bump,
        constraint = market_bond.depositor == depositor.key() @ FlashBookError::Unauthorized,
    )]
    pub market_bond: Account<'info, state::MarketBondAccount>,

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
        associated_token::authority = depositor,
    )]
    pub depositor_quote_ata: Account<'info, TokenAccount>,

    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct SlashMarketBond<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
        constraint = insurance_fund.authority == authority.key() @ FlashBookError::Unauthorized,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(
        mut,
        seeds = [
            state::MarketBondAccount::SEED,
            market_bond.market.as_ref(),
            market_bond.depositor.as_ref(),
        ],
        bump = market_bond.bump,
    )]
    pub market_bond: Account<'info, state::MarketBondAccount>,
}

#[derive(Accounts)]
pub struct AutoDeleverage<'info> {
    /// Anyone may call. ADL keepers compete off-chain on the (pnl ×
    /// leverage) ranking; first valid call wins.
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// Insurance fund — read-only here, used to verify ADL trigger gate
    /// (balance < pause threshold).
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, underwater_trader_state.trader.as_ref()],
        bump = underwater_trader_state.bump,
    )]
    pub underwater_trader_state: Box<Account<'info, TraderStateAccount>>,

    #[account(
        mut,
        seeds = [
            state::PositionAccount::SEED,
            market.key().as_ref(),
            underwater_trader_state.trader.as_ref(),
        ],
        bump = underwater_position.bump,
    )]
    pub underwater_position: Box<Account<'info, state::PositionAccount>>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, counter_trader_state.trader.as_ref()],
        bump = counter_trader_state.bump,
    )]
    pub counter_trader_state: Box<Account<'info, TraderStateAccount>>,

    #[account(
        mut,
        seeds = [
            state::PositionAccount::SEED,
            market.key().as_ref(),
            counter_trader_state.trader.as_ref(),
        ],
        bump = counter_position.bump,
    )]
    pub counter_position: Box<Account<'info, state::PositionAccount>>,
}

// ─── Events ─────────────────────────────────────────────────────────────

#[event]
pub struct MarketBookInitializedEvent {
    pub market: Pubkey,
    pub market_book: Pubkey,
    pub total_bytes: u32,
    pub data_bytes: u32,
}

#[event]
pub struct MarketBookDelegatedEvent {
    pub market: Pubkey,
    pub market_book: Pubkey,
    pub commit_frequency_ms: u32,
    /// Pinned ER validator pubkey, or default Pubkey if permissionless.
    pub validator: Pubkey,
}

#[event]
pub struct CommitBufferDelegatedEvent {
    pub market: Pubkey,
    pub commit_buffer: Pubkey,
    pub commit_frequency_ms: u32,
    pub validator: Pubkey,
}

#[event]
pub struct CommitBufferUndelegatedEvent {
    pub market: Pubkey,
    pub commit_buffer: Pubkey,
}

#[event]
pub struct MarketBookUndelegatedEvent {
    pub market: Pubkey,
    pub market_book: Pubkey,
}

#[event]
pub struct MarketDelegatedEvent {
    pub market: Pubkey,
    pub commit_frequency_ms: u32,
    pub validator: Pubkey,
}

#[event]
pub struct MarketUndelegatedEvent {
    pub market: Pubkey,
}

#[event]
pub struct OrderPlacedV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub seq: u64,
    pub side: u8,
    pub price_ticks: u64,
    pub size_lots: u64,
    pub node_index: u32,
    pub total_orders_after: u32,
}

/// Sequencer feed — emitted by `run_batch_v2` for every cleared fill.
/// The off-chain sequencer subscribes to these and dispatches the
/// matching `apply_fill` / `apply_flp_fill` ix on mainnet. Carries
/// the full per-fill payload + the order IDs (so the sequencer can
/// dedup against its own outbox).
///
/// FLP detection: when `maker == FLP_VIRTUAL_TRADER` (= Pubkey::default
/// — FLP makers don't carry a real trader pubkey), the sequencer
/// dispatches `apply_flp_fill` instead of `apply_fill`.
#[event]
pub struct BatchFillIntentEvent {
    pub market: Pubkey,
    pub taker: Pubkey,
    pub maker: Pubkey,
    pub taker_side: u8,
    pub size_lots: u64,
    pub price_ticks: u64,
    pub taker_id: u64,
    pub maker_id: u64,
}

/// Wave 21 phase 8b/9b: emitted when a wrapper program's CPI releases
/// collateral from the protocol vault to a user. Carries the
/// cpi_authority pubkey (so off-chain reconciliation can attribute
/// the release to a specific wrapper program).
#[event]
pub struct WrapperCollateralReleasedEvent {
    pub cpi_authority: Pubkey,
    pub user: Pubkey,
    pub amount: u64,
}

/// Wave 22 phase 5 — wrapper bootstrapped a TraderState for a non-
/// signing trader (typically a vault PDA).
#[event]
pub struct WrapperTraderStateOpenedEvent {
    pub cpi_authority: Pubkey,
    pub trader: Pubkey,
}

/// Wave 22 phase 5 — wrapper credited collateral to a trader's
/// TraderState (vault deposit path).
#[event]
pub struct WrapperCollateralCreditedEvent {
    pub cpi_authority: Pubkey,
    pub trader: Pubkey,
    pub amount: u64,
    pub new_collateral: u64,
}

/// Wave 22 phase 5 — wrapper debited collateral from a trader's
/// TraderState (vault withdraw path).
#[event]
pub struct WrapperCollateralDebitedEvent {
    pub cpi_authority: Pubkey,
    pub trader: Pubkey,
    pub amount: u64,
    pub new_collateral: u64,
}

/// Wave 21 phase 2: emitted when a wrapper program's CPI lands an order
/// via `place_limit_order_v2_cpi`. Same payload as OrderPlacedV2Event
/// PLUS the cpi_authority that signed (so off-chain reconciliation can
/// attribute the order to a specific wrapper program).
#[event]
pub struct OrderPlacedV2CpiEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub seq: u64,
    pub side: u8,
    pub price_ticks: u64,
    pub size_lots: u64,
    pub node_index: u32,
    pub total_orders_after: u32,
    pub cpi_authority: Pubkey,
}

/// How many price levels per side `view_book_depth_v2` returns.
/// Capped to keep the event log payload bounded; off-chain depth
/// reconstruction watches `OrderPlacedV2Event` + `OrderCancelledV2Event`
/// for the long tail.
pub const BOOK_DEPTH_LEVELS: usize = 4;

/// Per-side ceiling on orders fed into a single `run_batch_v2` clearing.
/// Wave 22 phase 6 refactored the matcher's clearing-price search from
/// O(N²) to O(N log N) (single sort + monotone two-pointer walk over
/// candidate prices), lifting the safe cap from 64 to 256 per side
/// (~5K CU per fill at the larger N, well inside the BPF budget).
pub const MAX_BATCH_ORDERS_PER_SIDE_V2: usize = 256;

/// Wave 21 phase 2 — sister-program IDs that core trusts as CPI callers.
/// Matches `programs/flash-book-orders/src/lib.rs::declare_id!`,
/// `flash-book-flp::declare_id!`, `flash-book-vaults::declare_id!`.
/// Reading this list = reading the trust boundary; if a wrapper isn't
/// listed here, its CPI into core's `*_cpi` ixs will be rejected.
pub const WAVE21_ORDERS_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("2RpeanTHjLtMDbbHNguxzvitGnJasSYwwNUtM2Gse9H5");
pub const WAVE21_FLP_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("eTJb5VHJ3vwAoPWZAcMJP7ArAS5HNpyWDG5JshVyK1M");
pub const WAVE21_VAULTS_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt");

/// PDA seed each wrapper program uses to sign CPI calls into core.
/// Wrapper signs over `[CPI_AUTHORITY_SEED]` with its own program ID,
/// producing a unique authority pubkey per wrapper. Core's `*_cpi`
/// ixs check `cpi_authority.key()` against the 3 derived addresses.
pub const CPI_AUTHORITY_SEED: &[u8] = b"cpi_authority";

/// HL withdrawal floor — wave 20b. When a trader pulls collateral from
/// `partial_withdraw_collateral` with positions still open, the remaining
/// collateral must satisfy `>= max(IM_required, WITHDRAWAL_FLOOR_BPS ×
/// total_notional)`. 1000 bps = 10% — Hyperliquid's exact value. Defends
/// against deposit-then-withdraw attacks where a trader briefly tops up
/// to satisfy IM, places a trade, then yanks the temporary collateral
/// leaving only enough for maintenance margin.
pub const WITHDRAWAL_FLOOR_BPS: u32 = 1000;

/// VPIN level (bps of toxicity probability) at or above which `run_batch_v2`
/// SKIPS FLP virtual-quote generation for the current batch. Protects LP
/// capital from informed flow at the matcher level (the per-fill toxicity
/// tax is a downstream defence; this is upstream — don't even quote).
/// 7000 bps = 70%: a high-confidence "this batch is being adversarially
/// selected" signal. Below 70% LP keeps quoting normally; the spread
/// widens via the existing VPIN-scaled `kappa_bps` term in the FLP quoter.
pub const FLP_VPIN_PAUSE_THRESHOLD_BPS: u32 = 7000;

#[event]
pub struct OrderCancelledV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub order_seq: u64,
    pub side: u8,
    pub node_index: u32,
    pub total_orders_after: u32,
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize)]
pub struct BookLevelV2 {
    pub price_ticks: u64,
    pub size_lots: u64,
    pub seq: u64,
    pub trader: Pubkey,
}

#[event]
pub struct BookDepthV2Event {
    pub market: Pubkey,
    pub total_orders_active: u32,
    pub bids: Vec<BookLevelV2>,
    pub asks: Vec<BookLevelV2>,
}

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
pub struct PartialCollateralWithdrawnEvent {
    pub trader: Pubkey,
    pub amount: u64,
    pub new_balance: u64,
    /// IM required at post-withdrawal collateral (the IM-floor input).
    pub im_required: u64,
    /// 10% × total_notional (the notional-floor input).
    pub notional_floor: u64,
    /// The active floor = max(im_required, notional_floor). Post-
    /// withdrawal collateral must be ≥ this.
    pub applied_floor: u64,
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
pub struct MarketLeverageTiersInitializedEvent {
    pub market: Pubkey,
    pub tier_count: u8,
}

#[event]
pub struct MarketLeverageTiersUpdatedEvent {
    pub market: Pubkey,
    pub tier_count: u8,
}

// ─── Wave 22 — Multi-tier fee table events ───────────────────────────

#[event]
pub struct FeeTiersInitializedEvent {
    pub authority: Pubkey,
    pub tier_count: u8,
    pub volume_window_slots: u64,
}

#[event]
pub struct FeeTiersUpdatedEvent {
    pub authority: Pubkey,
    pub tier_count: u8,
    pub volume_window_slots: u64,
}

#[event]
pub struct TraderEffectiveTierEvent {
    pub trader: Pubkey,
    pub tier_index: u8,
    pub effective_volume_quote_lots: u64,
    /// SIGNED — positive = maker rebate, negative = maker fee
    /// (wave 22 negative-fee semantics).
    pub maker_rebate_bps: i32,
    pub taker_fee_bps: u32,
    /// True iff the trader's `volume_window_start_slot` has aged past
    /// `volume_window_slots` AND `volume_30d_quote_lots > 0`. UIs
    /// surface "Tier resets on next trade" copy.
    pub window_expired: bool,
}

/// WAVE 22 — emitted by `apply_fill` whenever a trader crosses a tier
/// boundary (maker OR taker). Off-chain UIs surface "🎉 You upgraded
/// to VIP3!" pushes. Silent when no tier change.
#[event]
pub struct TraderTierUpgradedEvent {
    pub trader: Pubkey,
    pub previous_tier_index: u8,
    pub new_tier_index: u8,
    pub volume_quote_lots: u64,
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
pub struct LiquidationInjectedV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub limit_ticks: u64,
    pub worst_scenario_idx: u32,
    /// Sequence assigned to the synthesized close order.
    pub order_seq: u64,
    /// Hypertree node index of the inserted RestingOrderV2.
    pub node_index: u32,
}

#[event]
pub struct IcebergReplenishedV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub iceberg_id: u8,
    pub executor: Pubkey,
    pub chunk_size_lots: u64,
    pub remaining_lots: u64,
    pub new_child_seq: u64,
    /// Hypertree node index of the inserted child RestingOrderV2.
    pub node_index: u32,
}

#[event]
pub struct IcebergCancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub iceberg_id: u8,
    pub unfilled_lots: u64,
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
pub struct BasketOrderPlacedV2Event {
    pub trader: Pubkey,
    pub market_a: Pubkey,
    pub market_b: Pubkey,
    pub side_a: u8,
    pub side_b: u8,
    pub size_lots_a: u64,
    pub size_lots_b: u64,
    pub seq_a: u64,
    pub seq_b: u64,
    pub node_index_a: u32,
    pub node_index_b: u32,
}

#[event]
pub struct BasketOrderNPlacedV2Event {
    pub trader: Pubkey,
    pub leg_count: u8,
    pub markets: Vec<Pubkey>,
}

#[event]
pub struct RevealAppliedV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub limit_ticks: u64,
    pub order_seq: u64,
    pub node_index: u32,
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
pub struct BuilderFeeOwedEvent {
    pub taker: Pubkey,
    pub builder: Pubkey,
    pub amount_quote_lots: u64,
}

/// HIP-3 deployer share. Off-chain ledger credits the market creator
/// with `amount_quote_lots` per fill. Pull-based — no creator account
/// is touched in the apply_fill hot path.
#[event]
pub struct CreatorFeeOwedEvent {
    pub market: Pubkey,
    pub creator: Pubkey,
    pub amount_quote_lots: u64,
}

/// Emitted by `permissionless_initialize_market` so off-chain indexers
/// can distinguish HIP-3 deployments from protocol-curated markets.
#[event]
pub struct PermissionlessMarketDeployedEvent {
    pub market: Pubkey,
    pub creator: Pubkey,
    pub creator_share_bps: u32,
    pub is_pre_launch: bool,
}

/// Per-fill trading-rewards eligibility event. Off-chain accrual
/// computes per-trader points (notional × multipliers × time-windows).
/// Hyperliquid HYPE-distribution model — minimal on-chain footprint
/// (one event per fill, no extra writes) so emissions can be indexed
/// at zero on-chain cost.
#[event]
pub struct TradingRewardEligibleEvent {
    pub market: Pubkey,
    pub taker: Pubkey,
    pub maker: Pubkey,
    pub notional_quote_lots: u64,
    pub taker_side: u8,
}

#[event]
pub struct VaultPerfFeeSettledEvent {
    pub vault: Pubkey,
    pub strategist: Pubkey,
    pub shares_minted: u64,
    pub new_hwm_per_share_u64x6: u64,
}

#[event]
pub struct PositionLeverageUpdatedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub previous_cap: u32,
    pub new_cap: u32,
}

#[event]
pub struct CollateralSweptEvent {
    pub authority: Pubkey,
    pub from: Pubkey,
    pub to: Pubkey,
    pub amount_quote_lots: u64,
}

#[event]
pub struct TraderBuilderUpdatedEvent {
    pub trader: Pubkey,
    pub previous: Pubkey,
    pub new: Pubkey,
    pub max_fee_share_bps: u32,
}

/// Multi-threshold margin warning. Emitted when a trader's account-level
/// margin ratio crosses an alert threshold (75% → caution, 50% → warn,
/// 25% → critical) so off-chain UIs can push notifications BEFORE
/// liquidation. Hyperliquid pattern: gives users runway to add collateral
/// or de-risk instead of being surprised by an MMR breach.
#[event]
pub struct MarginThresholdCrossedEvent {
    pub trader: Pubkey,
    pub market: Pubkey,
    /// 0 = caution (75% of MMR headroom), 1 = warn (50%), 2 = critical (25%)
    pub level: u8,
    /// Position equity / required margin, in bps (e.g. 12_500 = 125%).
    pub equity_to_mmr_bps: u32,
}

#[event]
pub struct TriggerOrderCancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trigger_id: u8,
}

#[event]
pub struct TriggerOrderExecutedV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trigger_id: u8,
    pub executor: Pubkey,
    pub oracle_price_ticks: u64,
    /// Sequence assigned to the synthesized resting order.
    pub order_seq: u64,
    /// Hypertree node index of the inserted RestingOrderV2.
    pub node_index: u32,
}

/// View ix output: cross-market portfolio risk for a trader.
/// Emitted by `view_portfolio_risk` so SDK callers can fetch a
/// trader's full risk snapshot in one tx-simulate. Equivalent to
/// running `previewPortfolioRisk` client-side but authoritative
/// (uses the same on-chain stress-lattice as liquidations do).
#[event]
pub struct PortfolioRiskEvent {
    pub trader: Pubkey,
    pub collateral_quote_lots: u64,
    pub unrealized_pnl_quote_lots: i64,
    pub equity_quote_lots: i64,
    pub required_margin_quote_lots: u64,
    pub health_ratio_bps: u32,
    pub largest_position_market: Pubkey,
    pub largest_position_notional_quote_lots: u64,
    pub open_positions: u8,
    pub worst_scenario_idx: u32,
}

/// View ix output: predicted next-batch funding rate. Emitted by
/// `view_predicted_funding` so SDK callers (via tx simulation) can read
/// the rate from logs without paying for an actual on-chain mutation.
/// Off-chain UIs use this to display "next funding payment" before the
/// rate crystallises in `settle_funding`.
#[event]
pub struct PredictedFundingEvent {
    pub market: Pubkey,
    pub mark_price_ticks: u64,
    pub oracle_price_ticks: u64,
    pub premium_bps: i64,
    pub rate_bps_per_sec: i64,
    pub current_cum_index: i128,
}

/// View ix output: snapshot of the FLP quoter's would-be next-batch
/// quote ladder. The ladder is deterministic given (market state, FLP
/// state, oracle) — off-chain consumers can re-run `generate_quotes`
/// with the same inputs for the full per-level array; this emit only
/// carries the top-level summary to keep the log compact.
#[event]
pub struct QuoteLadderSnapshotEvent {
    pub market: Pubkey,
    pub fair_value_ticks: u64,
    pub skew_bps: i32,
    pub top_bid_ticks: u64,
    pub top_ask_ticks: u64,
    pub top_bid_size_lots: u64,
    pub top_ask_size_lots: u64,
    pub level_count: u8,
}

/// Emitted when funding hits the per-period cap and is scaled to fit.
/// Off-chain monitors page operators on repeated emissions (sign that
/// the cap is too tight or that the market is in extended one-way
/// funding stress).
#[event]
pub struct FundingPeriodCapHitEvent {
    pub market: Pubkey,
    pub period_started_at_unix: u64,
    pub cap_bps: u64,
    pub attenuated_rate_bps_per_sec: i64,
}

/// Anti-flash-crash event. Emitted when the post-batch mark would have
/// moved further than `params.mark_change_max_bps` and was clamped.
/// Off-chain monitors page operators on repeated emissions (a sign
/// either liquidity has thinned dramatically or the cap is too tight).
#[event]
pub struct MarkChangeClampedEvent {
    pub market: Pubkey,
    pub batch_num: u64,
    pub unclamped_mark_ticks: u64,
    pub clamped_mark_ticks: u64,
    pub prior_mark_ticks: u64,
}

#[event]
pub struct TrailingStopRatchetedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trigger_id: u8,
    pub previous_trigger_price_ticks: u64,
    pub new_trigger_price_ticks: u64,
    pub anchor_ticks: u64,
}

#[event]
pub struct TwapOrderCancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub twap_id: u8,
    pub unfilled_lots: u64,
}

#[event]
pub struct TwapSliceExecutedV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub twap_id: u8,
    pub executor: Pubkey,
    pub slice_size_lots: u64,
    pub cumulative_executed_lots: u64,
    /// Sequence assigned to the synthesized resting slice.
    pub order_seq: u64,
    /// Hypertree node index of the inserted RestingOrderV2.
    pub node_index: u32,
}

#[event]
pub struct MarketBondPostedEvent {
    pub market: Pubkey,
    pub depositor: Pubkey,
    pub amount_quote_lots: u64,
    pub new_total_quote_lots: u64,
}

#[event]
pub struct MarketBondUnbondRequestedEvent {
    pub market: Pubkey,
    pub depositor: Pubkey,
    pub requested_at_unix: u64,
    pub claimable_at_unix: u64,
}

#[event]
pub struct MarketBondClaimedEvent {
    pub market: Pubkey,
    pub depositor: Pubkey,
    pub amount_quote_lots: u64,
}

#[event]
pub struct MarketBondSlashedEvent {
    pub market: Pubkey,
    pub depositor: Pubkey,
    pub amount_quote_lots: u64,
    pub remaining_bond_quote_lots: u64,
}

#[event]
pub struct AutoDeleveragedEvent {
    pub market: Pubkey,
    pub underwater_trader: Pubkey,
    pub counter_trader: Pubkey,
    pub close_size_lots: u64,
    pub bankruptcy_price_ticks: u64,
    pub counter_gain_quote_lots: u64,
    pub executor: Pubkey,
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
        leg.limit_ticks % market.params.tick_size == 0,
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

#[repr(u8)]
pub enum MarketStatus {
    Inactive = 0,
    Active = 1,
    PostOnly = 2,
    Paused = 3,
    Closed = 4,
}

/// Inject one basket leg into a hypertree-backed market_book PDA.
/// Used by `place_basket_order_v2`. Returns (assigned_seq, node_index)
/// for the inserted RestingOrderV2.
///
/// The market_book account is passed as `UncheckedAccount` (typed in the
/// caller's ctx). This helper takes a borrow of its data, writes the
/// new RestingOrderV2 via the hypertree handle, and returns.
fn inject_leg_into_hypertree(
    market_book: &UncheckedAccount<'_>,
    market_key: Pubkey,
    trader_key: Pubkey,
    leg: &BasketLeg,
    now_slot: u64,
) -> Result<(u64, hypertree::DataIndex)> {
    let mut book_data = market_book.try_borrow_mut_data()?;
    let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
    require!(
        handle.header.market_pubkey == market_key,
        FlashBookError::WrongMarket
    );
    let next_seq = handle
        .header
        .order_seq_counter
        .checked_add(1)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
    require!(next_seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
    handle.header.order_seq_counter = next_seq;

    let side_is_bid = leg.side == 0;
    let order = state_v2::RestingOrderV2 {
        order_id: state_v2::encode_order_id(leg.limit_ticks, next_seq, side_is_bid),
        seq: next_seq,
        price_ticks: leg.limit_ticks,
        size_lots: leg.size_lots,
        expires_at_slot: 0,
        trader: trader_key,
        last_valid_slot: now_slot as u32,
        side: leg.side,
        order_type: 0, // limit
        flags: if leg.post_only { 0b0000_0001 } else { 0 },
        _pad: 0,
    };
    let idx = if side_is_bid {
        handle.insert_bid(order)?
    } else {
        handle.insert_ask(order)?
    };
    Ok((next_seq, idx))
}

/// AccountInfo variant for `place_basket_order_n_v2`, where leg market_books
/// arrive via `remaining_accounts` (untyped). Same insertion contract.
fn inject_leg_into_hypertree_unchecked(
    market_book_ai: &AccountInfo<'_>,
    market_key: Pubkey,
    trader_key: Pubkey,
    leg: &BasketLeg,
    now_slot: u64,
) -> Result<()> {
    let mut book_data = market_book_ai.try_borrow_mut_data()?;
    let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
    require!(
        handle.header.market_pubkey == market_key,
        FlashBookError::WrongMarket
    );
    let next_seq = handle
        .header
        .order_seq_counter
        .checked_add(1)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
    require!(next_seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
    handle.header.order_seq_counter = next_seq;

    let side_is_bid = leg.side == 0;
    let order = state_v2::RestingOrderV2 {
        order_id: state_v2::encode_order_id(leg.limit_ticks, next_seq, side_is_bid),
        seq: next_seq,
        price_ticks: leg.limit_ticks,
        size_lots: leg.size_lots,
        expires_at_slot: 0,
        trader: trader_key,
        last_valid_slot: now_slot as u32,
        side: leg.side,
        order_type: 0,
        flags: if leg.post_only { 0b0000_0001 } else { 0 },
        _pad: 0,
    };
    if side_is_bid {
        handle.insert_bid(order)?;
    } else {
        handle.insert_ask(order)?;
    }
    Ok(())
}

/// Map a `RestingOrderV2.order_type` byte to the matcher's OrderType.
/// Matches v1's `slot_to_order` mapping so v2 orders fed into the same
/// FBA clearing logic get the same FIFO priority weighting (limits
/// behind takers, takers behind liquidations, ADL highest).
///
/// Unknown bytes default to Limit — defensive; an attacker writing a
/// junk value in the order_type byte gets the lowest-priority bucket.
/// Validate a leverage-tier table for `init_market_leverage_tiers` /
/// `update_market_leverage_tiers`. Tiers must be:
///   • non-empty and length ≤ MAX_LEVERAGE_TIERS
///   • sorted ascending by min_notional_quote_lots (with strict increase)
///   • each tier mmr_bps ≥ market.params.maintenance_margin_ratio_bps
///     (tiers can only INCREASE MMR vs the baseline)
///   • mmr_bps ≤ BPS_DENOM (sanity)
///
/// First tier may have min_notional = 0 (becomes the new baseline).
fn validate_leverage_tiers(market: &MarketAccount, tiers: &[LeverageTier]) -> Result<()> {
    require!(!tiers.is_empty(), FlashBookError::ZeroSize);
    require!(
        tiers.len() <= MAX_LEVERAGE_TIERS,
        FlashBookError::OutOfRange
    );
    let base_mmr = market.params.maintenance_margin_ratio_bps;
    let mut prev_min: Option<u64> = None;
    for t in tiers {
        require!(
            t.mmr_bps >= base_mmr,
            FlashBookError::OutOfRange
        );
        require!(
            t.mmr_bps <= constants::BPS_DENOM as u32,
            FlashBookError::OutOfRange
        );
        if let Some(prev) = prev_min {
            require!(
                t.min_notional_quote_lots > prev,
                FlashBookError::OutOfRange
            );
        }
        prev_min = Some(t.min_notional_quote_lots);
    }
    Ok(())
}

/// WAVE 22: validate a fee-tier table for `init_fee_tiers /
/// update_fee_tiers`. Enforces:
///   • Non-empty + ≤ MAX_FEE_TIERS
///   • Sorted ascending by `min_volume_quote_lots`
///   • Tier 0 has `min_volume == 0` (default tier required)
///   • Monotone improving: taker fee ↘, maker rebate ↗ as volume rises
///   • All bps within MAX_FEE_TIER_BPS
///   • volume_window_slots is non-zero (would otherwise treat every
///     fill as a window expiry → permanent reset)
fn validate_fee_tiers(volume_window_slots: u64, tiers: &[state::FeeTier]) -> Result<()> {
    require!(volume_window_slots > 0, FlashBookError::OutOfRange);
    require!(!tiers.is_empty(), FlashBookError::OutOfRange);
    require!(tiers.len() <= state::MAX_FEE_TIERS, FlashBookError::OutOfRange);
    require!(
        tiers[0].min_volume_quote_lots == 0,
        FlashBookError::OutOfRange
    );

    let mut prev_min: Option<u64> = None;
    let mut prev_taker: Option<u32> = None;
    let mut prev_maker: Option<i32> = None;
    for t in tiers {
        require!(
            t.taker_fee_bps <= constants::MAX_FEE_TIER_BPS,
            FlashBookError::OutOfRange
        );
        // Maker rebate is SIGNED (i32). Cap by absolute value — same
        // typo guard as taker fee, applied to either sign of rebate.
        require!(
            t.maker_rebate_bps.unsigned_abs() <= constants::MAX_FEE_TIER_BPS,
            FlashBookError::OutOfRange
        );
        if let Some(p) = prev_min {
            require!(
                t.min_volume_quote_lots > p,
                FlashBookError::OutOfRange
            );
        }
        if let Some(pt) = prev_taker {
            require!(t.taker_fee_bps <= pt, FlashBookError::OutOfRange);
        }
        if let Some(pm) = prev_maker {
            // Monotone non-decreasing — higher tier never has WORSE
            // maker treatment than a lower tier (signed comparison
            // means -10 < 0 < +5, so retail → MM progression works).
            require!(t.maker_rebate_bps >= pm, FlashBookError::OutOfRange);
        }
        prev_min = Some(t.min_volume_quote_lots);
        prev_taker = Some(t.taker_fee_bps);
        prev_maker = Some(t.maker_rebate_bps);
    }
    Ok(())
}

/// WAVE 22 phase 5 — wrapper-program CPI authority whitelist check.
/// Returns Ok if `cpi_signer` matches the `[CPI_AUTHORITY_SEED]` PDA
/// of one of the 3 allowed wrapper programs (orders / flp / vaults).
/// Single source of truth for the wrapper-authorized ix gate.
fn check_wave21_cpi_authority(cpi_signer: &Pubkey) -> Result<()> {
    let (orders_authority, _) =
        Pubkey::find_program_address(&[CPI_AUTHORITY_SEED], &WAVE21_ORDERS_PROGRAM_ID);
    let (flp_authority, _) =
        Pubkey::find_program_address(&[CPI_AUTHORITY_SEED], &WAVE21_FLP_PROGRAM_ID);
    let (vaults_authority, _) =
        Pubkey::find_program_address(&[CPI_AUTHORITY_SEED], &WAVE21_VAULTS_PROGRAM_ID);
    require!(
        *cpi_signer == orders_authority
            || *cpi_signer == flp_authority
            || *cpi_signer == vaults_authority,
        FlashBookError::Unauthorized
    );
    Ok(())
}

/// WAVE 22 phase 2 — pure helper that returns the index of the highest
/// tier the trader's `volume` qualifies for (0-indexed). Empty pairs →
/// returns 0 (no tier change ever fires). Used to detect tier-upgrade
/// boundary crossings inside `apply_fill` so we can emit a
/// `TraderTierUpgradedEvent` only on actual change.
fn tier_index_for_volume(pairs: &[(u64, i32, u32)], volume: u64) -> u8 {
    let mut idx: u8 = 0;
    for (i, (min_vol, _, _)) in pairs.iter().enumerate() {
        if volume >= *min_vol {
            idx = i as u8;
        } else {
            break;
        }
    }
    idx
}

/// WAVE 22 — credit a trader's rolling-window volume + re-anchor the
/// window if it has expired (or was never seeded). Called on every
/// economic fill from `apply_fill` for both maker and taker.
///
/// Window-expiry semantics: when `now - window_start >
/// DEFAULT_VOLUME_WINDOW_SLOTS`, we RESET volume to 0 and re-anchor
/// the window at `now` BEFORE crediting the new fill's notional. This
/// matches HL's "rolling window resets, current trade starts the new
/// window" pattern. `view_trader_effective_tier` mirrors this — for
/// reads it honors the authority-configured window from FeeTiersAccount
/// (which can be tighter or wider than the default).
fn credit_volume_for_tier(state: &mut TraderStateAccount, notional_quote_lots: u64, now_slot: u64) {
    let elapsed = now_slot.saturating_sub(state.volume_window_start_slot);
    if state.volume_window_start_slot == 0 || elapsed > constants::DEFAULT_VOLUME_WINDOW_SLOTS {
        state.volume_30d_quote_lots = 0;
        state.volume_window_start_slot = now_slot;
    }
    state.volume_30d_quote_lots = state
        .volume_30d_quote_lots
        .saturating_add(notional_quote_lots);
}

pub fn order_type_byte_to_matcher(b: u8) -> matcher::order::OrderType {
    use matcher::order::OrderType;
    match b {
        0 => OrderType::Limit,
        1 => OrderType::Taker,
        2 => OrderType::FlpVirtual,
        3 => OrderType::Liquidation,
        4 => OrderType::Adl,
        _ => OrderType::Limit,
    }
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
/// Single-position margin-threshold check. Cheap (no portfolio walk):
/// computes equity = collateral + unrealized_pnl(pos, mark) and required
/// = pos_notional × mmr_bps / 10_000, then emits a
/// MarginThresholdCrossedEvent on threshold crossings (250%, 200%, 125%).
/// Off-chain UIs subscribe and push pre-liquidation alerts. Hyperliquid
/// pattern. Silent when required == 0 or numbers don't fit.
fn emit_margin_threshold_if_crossed(
    trader: Pubkey,
    market: Pubkey,
    pos: &state::PositionAccount,
    mark_ticks: u64,
    tick_size: u64,
    mmr_bps: u32,
    collateral_quote_lots: u64,
) {
    if pos.size_lots == 0 || mark_ticks == 0 || mmr_bps == 0 {
        return;
    }
    let notional_u128 = (pos.size_lots as u128)
        .saturating_mul(mark_ticks as u128)
        .saturating_mul(tick_size as u128);
    let required = notional_u128.saturating_mul(mmr_bps as u128) / constants::BPS_DENOM as u128;
    if required == 0 {
        return;
    }
    // Unrealized PnL: (mark - entry) × size × ±1
    let pnl_per_lot_ticks = (mark_ticks as i128) - (pos.entry_price_ticks as i128);
    let sign: i128 = if pos.side == 0 { 1 } else { -1 };
    let unrealized = sign
        .saturating_mul(pos.size_lots as i128)
        .saturating_mul(pnl_per_lot_ticks)
        .saturating_mul(tick_size as i128);
    let equity_signed = (collateral_quote_lots as i128).saturating_add(unrealized);
    if equity_signed <= 0 {
        emit!(MarginThresholdCrossedEvent {
            trader,
            market,
            level: 2,
            equity_to_mmr_bps: 0,
        });
        return;
    }
    let ratio_bps_u128 = (equity_signed as u128).saturating_mul(constants::BPS_DENOM as u128) / required;
    let ratio_bps: u32 = if ratio_bps_u128 > u32::MAX as u128 {
        u32::MAX
    } else {
        ratio_bps_u128 as u32
    };
    // Threshold ladder. Higher ratio = healthier. Critical first wins.
    let level: Option<u8> = if ratio_bps < 12_500 {
        Some(2) // critical (< 125% of MMR)
    } else if ratio_bps < 20_000 {
        Some(1) // warn (< 200%)
    } else if ratio_bps < 25_000 {
        Some(0) // caution (< 250%)
    } else {
        None
    };
    if let Some(level) = level {
        emit!(MarginThresholdCrossedEvent {
            trader,
            market,
            level,
            equity_to_mmr_bps: ratio_bps,
        });
    }
}

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

fn clamp_i128_to_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 { i64::MAX }
    else if v < i64::MIN as i128 { i64::MIN }
    else { v as i64 }
}

