//! Flash Book — pool-backed CLOB on MagicBlock ER.
//!
//! Anchor program. The matcher core is in [`matcher`] (pure-Rust integer
//! arithmetic with checked overflow). This file is the on-chain shell:
//! account validation, PDA seeds, signer checks, and matcher invocation.
//!
//! Phase 1 status: matcher core ✅, account types ✅, instruction handlers ✅
//! (this file). Pending: ER delegation CPIs (blocked on upstream SDK).
//! Position state is moved by the `apply_fill` instruction in response to
//! taker fills emitted by `place_taker_order_v2`.

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
pub mod state_v3;

pub use errors::FlashBookError;

use constants::{
    FLP_SEQ_RESERVED_OFFSET, MARK_HISTORY_LEN, MAX_BASKET_LEGS_N,
    MAX_ORDERS_PER_TRADER_PER_BATCH,
};
use matcher::funding::funding_owed;
use matcher::lot::Ticks;
use matcher::order::Side;
use matcher::risk::{
    assess_margin as assess_margin_fn, assess_margin_unified as assess_margin_unified_fn,
    default_scenarios as default_scenarios_fn, MarketSnapshot as RiskMarketSnap,
    PositionSnapshot as RiskPosSnap,
};
use state::{
    FlpExposureAccount, InsuranceFundAccount, LeverageTier,
    MarketAccount, MarketLeverageTiersAccount, MarketParams, TraderStateAccount,
    MAX_LEVERAGE_TIERS,
};

declare_id!("5VqBguVaSj8PH6BTk9X5s3nJCHRqAkZfB7G7Bjenzcq");

#[cfg(not(feature = "no-entrypoint"))]
solana_security_txt::security_txt! {
    name: "Flash Book",
    project_url: "https://github.com/Abdr007/flash-book",
    contacts: "link:https://github.com/Abdr007/flash-book/issues,link:https://github.com/Abdr007/flash-book/security/advisories/new",
    policy: "https://github.com/Abdr007/flash-book/blob/main/SECURITY.md",
    preferred_languages: "en",
    source_code: "https://github.com/Abdr007/flash-book",
    source_revision: env!("CARGO_PKG_VERSION"),
    source_release: env!("CARGO_PKG_VERSION"),
    auditors: "Internal audit only — see https://github.com/Abdr007/flash-book/blob/main/docs/AUDIT.md. External audit not yet engaged.",
    acknowledgements: "Risk-engine design derived from Percolator (aeyakovenko/percolator) and GMX V2 (gmx-io/gmx-synthetics). See docs/RESEARCH_NOTES.md for attribution."
}

#[program]
pub mod flash_book {
    use super::*;

    // ─── Setup ──────────────────────────────────────────────────────

    /// Initialize a new market and all associated PDAs. AUTHORITY-GATED:
    /// only the protocol authority can deploy markets — Flash Book does
    /// not support permissionless market creation. The `creator` field
    /// on the market account is zeroed (no creator share / HIP-3-style
    /// fee split is paid out unless the authority later patches in a
    /// curated creator via a future migration ix).
    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        params: MarketParams,
        initial_oracle_ticks: u64,
    ) -> Result<()> {
        initialize_market_inner(ctx, params, initial_oracle_ticks)
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

    /// Grow an existing v2 `market_book` PDA in place by `additional_nodes`
    /// hypertree slots — this is what breaks the 100-node init cap. Only the
    /// market authority may call it (same gate as `init_market_book`).
    ///
    /// Solana caps one realloc at `MAX_PERMITTED_DATA_INCREASE` (10 KiB ≈ 106
    /// nodes), so growing a book all the way to `MAX_NODES_EXPANDED` takes
    /// several calls. The authority tops up rent for the new bytes so the
    /// account stays rent-exempt.
    ///
    /// MUST run on the BASE LAYER with the book UNdelegated: reallocating an
    /// ER-delegated account would desync the delegation record. The owner
    /// check below rejects a delegated account (its owner is the delegation
    /// program, not us).
    pub fn expand_market_book(
        ctx: Context<ExpandMarketBook>,
        additional_nodes: u32,
    ) -> Result<()> {
        require!(additional_nodes > 0, FlashBookError::OutOfRange);

        let book_ai = ctx.accounts.market_book.to_account_info();

        // Defence-in-depth: the PDA must still be owned by us. A delegated
        // book is owned by the delegation program — realloc would be illegal
        // and would corrupt the delegation record.
        require_keys_eq!(
            *book_ai.owner,
            *ctx.program_id,
            FlashBookError::Unauthorized
        );

        let additional_bytes = (additional_nodes as usize)
            .checked_mul(state_v2::NODE_TOTAL_BYTES)
            .ok_or(error!(FlashBookError::ArithmeticOverflow))?;
        require!(
            additional_bytes
                <= anchor_lang::solana_program::entrypoint::MAX_PERMITTED_DATA_INCREASE,
            FlashBookError::OutOfRange
        );

        let old_len = book_ai.data_len();
        let new_len = old_len
            .checked_add(additional_bytes)
            .ok_or(error!(FlashBookError::ArithmeticOverflow))?;
        require!(
            new_len <= state_v2::MARKET_BOOK_MAX_TOTAL_BYTES,
            FlashBookError::OutOfRange
        );

        // Top up rent so the larger account stays rent-exempt.
        let rent = Rent::get()?;
        let new_minimum = rent.minimum_balance(new_len);
        let cur_lamports = book_ai.lamports();
        if new_minimum > cur_lamports {
            let topup = new_minimum.saturating_sub(cur_lamports);
            anchor_lang::solana_program::program::invoke(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.authority.key(),
                    &book_ai.key(),
                    topup,
                ),
                &[
                    ctx.accounts.authority.to_account_info(),
                    book_ai.clone(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
        }

        // Grow in place; zero-init the appended tail so the bump allocator
        // and free-list see clean memory.
        book_ai.realloc(new_len, true)?;

        // Sanity: the grown account must still parse as a valid book (disc +
        // node-aligned size). Cheap, and catches a botched realloc loudly.
        {
            let mut data = book_ai.try_borrow_mut_data()?;
            let _ = state_v2::MarketBookHandle::from_account_data(&mut data)?;
        }

        emit!(MarketBookExpandedEvent {
            market: ctx.accounts.market.key(),
            market_book: book_ai.key(),
            old_bytes: old_len as u32,
            new_bytes: new_len as u32,
            max_nodes: ((new_len - state_v2::MARKET_BOOK_PREFIX_BYTES)
                / state_v2::NODE_TOTAL_BYTES) as u32,
        });
        Ok(())
    }

    /// Delegate the v2 hypertree market_book PDA to the MagicBlock ER.
    /// After this lands, the account state lives on the ER for sub-ms
    /// matcher access; only this program (via PDA signature) can
    /// undelegate it back to mainnet. The market account must also be
    /// delegated (see `delegate_market`) so the matcher running on the
    /// ER can mutate mark/funding/VPIN state in lockstep with the book.
    ///
    /// `commit_frequency_ms` controls how often the ER auto-commits the
    /// state back to mainnet when the program isn't doing it explicitly.
    /// 0 disables auto-commit (only manual undelegate flushes state).
    /// Production target: 50–200 ms — short enough that liquidations on
    /// mainnet read a near-live mark; long enough not to spam commits.
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
    /// and `place_taker_order_v2` / `place_limit_order_v2` execute on
    /// mainnet directly.
    ///
    /// Use during planned ER downtime, validator rotation, or to flush
    /// final state before a permanent shutdown of the ER instance.
    /// Operators should call `settle_mark` shortly after undelegate to
    /// resync the mainnet mark with the live oracle — `liquidate_position_v2`
    /// will reject calls with a stale oracle (see oracle freshness gate).
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

    /// Delegate the MarketAccount itself to the ER. The matcher running
    /// on the ER mutates `mark_price_ticks`, `cum_funding_index`,
    /// `last_funding_rate_bps_per_sec`, `vpin`, `current_batch`, and
    /// `last_batch_ms` in lockstep with the order book. Pair this with
    /// `delegate_market_book` — both delegations must be live for the
    /// matcher to run on the ER without forking state between layers.
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

    // ─── ER lifecycle: commit + undelegation callback (audit ER-2) ───────
    // These complete the MagicBlock settlement loop that bare Delegate/
    // Undelegate cannot do alone. `commit_*` run ON THE ER (keeper-driven)
    // and CPI the Magic program; `process_undelegation` is the BASE-LAYER
    // callback the delegation program invokes to reopen the PDA + copy the
    // committed state back. NOTE: end-to-end behaviour requires a live
    // MagicBlock ER (a devnet lifecycle test) — these compile and integrate
    // into dispatch, but the commit/undelegate flow itself is ER-gated.

    /// ON THE ER: snapshot the delegated `market_book` back to L1 while staying
    /// delegated (`ScheduleCommit` Magic Action). Keeper-driven (permissionless,
    /// like Phoenix-ER — the validator/magic program enforce commit semantics).
    pub fn commit_market_book(ctx: Context<CommitMarketBook>) -> Result<()> {
        er::cpi_commit(
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.magic_context,
            &ctx.accounts.magic_program,
            &[ctx.accounts.market_book.to_account_info()],
            false,
        )
    }

    /// ON THE ER: snapshot final state AND queue undelegation
    /// (`ScheduleCommitAndUndelegate`). After this lands, the delegation
    /// program calls back into `process_undelegation` on base to finalize.
    pub fn commit_and_undelegate_market_book(ctx: Context<CommitMarketBook>) -> Result<()> {
        er::cpi_commit(
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.magic_context,
            &ctx.accounts.magic_program,
            &[ctx.accounts.market_book.to_account_info()],
            true,
        )
    }

    /// BASE-LAYER callback invoked by the MagicBlock delegation program to
    /// finalize undelegation. Its Anchor discriminator is EXACTLY
    /// `sha256("global:process_undelegation")[..8]` = the bytes the delegation
    /// program sends, so this is an ordinary Anchor ix (no entrypoint fallback).
    /// Re-creates the delegated PDA program-owned and copies the committed
    /// buffer back (`er::process_external_undelegate`).
    pub fn process_undelegation(
        ctx: Context<ProcessUndelegation>,
        account_seeds: Vec<Vec<u8>>,
    ) -> Result<()> {
        er::process_external_undelegate(
            &ctx.accounts.delegated_account,
            &ctx.accounts.buffer,
            &ctx.accounts.payer,
            &ctx.accounts.system_program,
            account_seeds,
        )
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
        sub_index: u8,
    ) -> Result<()> {
        // ── Hot-path validation (collapsed). Single Clock::get(), one
        // bounded match per family of inputs. Branch order matches
        // empirical frequency: malformed inputs rare → fast-path through.
        let market = &ctx.accounts.market;
        let now_slot = Clock::get()?.slot;

        // Fast input guards (most-common-pass first).
        if side > 1
            || size_lots == 0
            || limit_ticks == 0
            || (flags & !0b0111_1111) != 0
        {
            return err!(FlashBookError::OutOfRange);
        }
        if expires_at_slot != 0 && expires_at_slot <= now_slot {
            return err!(FlashBookError::OutOfRange);
        }

        // Market-state guards.
        let p = &market.params;
        if market.status != MarketStatus::Active as u8
            && market.status != MarketStatus::PostOnly as u8
        {
            return err!(FlashBookError::OutOfRange);
        }
        if size_lots < p.min_base_lots {
            return err!(FlashBookError::SizeBelowMinLot);
        }
        if limit_ticks % p.tick_size != 0 {
            return err!(FlashBookError::PriceNotOnTick);
        }

        // Per-market OI hard cap (cold for most markets — oi_cap == 0).
        let oi_cap = p.max_oi_base_lots;
        if oi_cap > 0 {
            let cur = if side == 0 { market.oi_long_lots } else { market.oi_short_lots };
            if cur.saturating_add(size_lots) > oi_cap {
                return err!(FlashBookError::OpenInterestCapExceeded);
            }
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();

        // Borrow the market_book account data + load the handle.
        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        if handle.header.market_pubkey != market_key {
            return err!(FlashBookError::WrongMarket);
        }

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
            last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
            side,
            order_type: 0, // 0 = limit (the only kind for now)
            flags,
            // Phase 2e — written from the ix param so ApplyFill can
            // resolve the correct TraderState (main or sub) when this
            // order matches as a maker. 0 = main (default).
            sub_index,
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

    /// Immediate CLOB-style placement — walks the opposite-side book at
    /// the maker's resting price (strict price-time priority) and matches
    /// against every crossable resting order until the taker is filled or
    /// `limit_ticks` is exhausted.
    ///
    /// Walk semantics:
    ///   • Buy order: iterate asks best-first; while ask.price ≤ limit,
    ///     match `min(remaining, ask.size)` at `ask.price`. Stop when
    ///     next ask doesn't cross OR taker fully consumed.
    ///   • Sell order: symmetric over bids (best = highest price).
    ///
    /// All matches are accumulated into a single tail `FillBatchEvent`
    /// carrying `Vec<FillEntry>` — the off-chain sequencer iterates
    /// those entries and dispatches `apply_fill` / `apply_flp_fill` per
    /// row. Maker orders are decremented in place on partial fill and
    /// removed on full consumption. Any residual taker size becomes a
    /// resting limit at `limit_ticks` so the order stays live (unless
    /// the IOC or FOK flag is set).
    pub fn place_taker_order_v2(
        ctx: Context<PlaceLimitOrderV2>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        flags: u8,
        expires_at_slot: u64,
        sub_index: u8,
    ) -> Result<()> {
        // Hot-path validation: collapsed branches, single Clock::get().
        let market = &ctx.accounts.market;
        let now_slot = Clock::get()?.slot;

        if side > 1
            || size_lots == 0
            || limit_ticks == 0
            || (flags & !0b0111_1111) != 0
        {
            return err!(FlashBookError::OutOfRange);
        }
        if expires_at_slot != 0 && expires_at_slot <= now_slot {
            return err!(FlashBookError::OutOfRange);
        }

        let p = &market.params;
        if market.status != MarketStatus::Active as u8
            && market.status != MarketStatus::PostOnly as u8
        {
            return err!(FlashBookError::OutOfRange);
        }
        if size_lots < p.min_base_lots {
            return err!(FlashBookError::SizeBelowMinLot);
        }
        if limit_ticks % p.tick_size != 0 {
            return err!(FlashBookError::PriceNotOnTick);
        }

        // Per-market OI hard cap — mirror the limit path (MATCH-H1). Without
        // this guard a taker (filled portion + resting residual) bypasses the
        // per-market open-interest cap entirely. Conservative: bounds the full
        // taker size against the side's current OI, same as place_limit.
        let oi_cap = p.max_oi_base_lots;
        if oi_cap > 0 {
            let cur = if side == 0 { market.oi_long_lots } else { market.oi_short_lots };
            if cur.saturating_add(size_lots) > oi_cap {
                return err!(FlashBookError::OpenInterestCapExceeded);
            }
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        if handle.header.market_pubkey != market_key {
            return err!(FlashBookError::WrongMarket);
        }

        let taker_seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        handle.header.order_seq_counter = taker_seq;

        let side_is_bid = side == 0;
        let taker_order_id = state_v2::encode_order_id(limit_ticks, taker_seq, side_is_bid);

        // Advanced flags (Phoenix / Manifest / HL parity):
        //   bit 0  POST_ONLY    — reject if any matches (caller wants rest)
        //   bit 1  REDUCE_ONLY  — enforced upstream by margin ix
        //   bit 2  IOC          — cancel residual after walk (no rest)
        //   bit 3  JIT          — Drift-style JIT bonus
        //   bits 4-5 STP_MODE   — self-trade prevention mode
        //   bit 6  FOK          — fill-or-kill (revert if any residual)
        const FLAG_POST_ONLY: u8 = 1 << 0;
        const FLAG_IOC: u8 = 1 << 2;
        const FLAG_FOK: u8 = 1 << 6;
        // STP modes (2 bits at positions 4-5):
        const STP_SKIP: u8 = 0b00;           // skip own resting orders (default)
        const STP_CANCEL_OLDEST: u8 = 0b01;  // cancel the resting maker, keep walking
        const STP_CANCEL_BOTH: u8 = 0b10;    // reject the entire taker order
        let post_only = flags & FLAG_POST_ONLY != 0;
        let ioc = flags & FLAG_IOC != 0;
        let fok = flags & FLAG_FOK != 0;
        let stp_mode = (flags >> 4) & 0b11;

        // ── Phase 1: walk opposite side, collect matchable orders ───
        // Mid-iteration mutation is unsafe (borrow conflicts), so we
        // collect (idx, maker_id, fill_size, price, maker_trader) for
        // matches and DataIndex for STP_CANCEL_OLDEST removals, then
        // apply mutations after. Cap traversal at MAX_BATCH per side
        // to bound CU on a single ix.
        let mut remaining = size_lots;
        // Pre-size to the typical walk depth (most takers fill in <16
        // levels). Skips ~3 doubling reallocs on the hot path; the
        // worst-case walk is bounded by MAX_BATCH_ORDERS_PER_SIDE_V2
        // and any overflow spills onto the heap via Vec's grow path.
        // Stack-allocated fixed arrays aren't viable here: at 256-cap
        // × 60 B per tuple the array exceeds BPF's 4 KB stack frame.
        const TYPICAL_WALK_DEPTH: usize = 16;
        // (data_idx, maker_order_id, fill_size_lots, fill_price_ticks,
        //  maker_pubkey, maker_sub_index) — Phase 2e adds maker_sub_index
        // so the emitted FillEntry carries it for the sequencer.
        let mut matches: Vec<(hypertree::DataIndex, u64, u64, u64, Pubkey, u8)> =
            Vec::with_capacity(TYPICAL_WALK_DEPTH);
        let mut stp_cancels: Vec<hypertree::DataIndex> = Vec::new();
        let mut stp_aborted = false;
        let walk_limit = MAX_BATCH_ORDERS_PER_SIDE_V2;

        // Always walk to detect crossings. post_only check happens
        // AFTER — if matches were found, the order would cross, so
        // reject. Without this walk, post_only can't detect would-
        // cross conditions.
        if side_is_bid {
            handle.for_each_ask_best_first(|idx, ask| {
                if matches.len() >= walk_limit { return false; }
                if remaining == 0 { return false; }
                if ask.price_ticks > limit_ticks { return false; }
                if ask.expires_at_slot > 0 && now_slot > ask.expires_at_slot { return true; }
                if ask.trader == trader_pk {
                    match stp_mode {
                        STP_CANCEL_OLDEST => {
                            stp_cancels.push(idx);
                            return true;
                        }
                        STP_CANCEL_BOTH => {
                            stp_aborted = true;
                            return false;
                        }
                        STP_SKIP | _ => return true, // skip self-match, keep walking
                    }
                }
                let fill = ask.size_lots.min(remaining);
                matches.push((idx, ask.order_id, fill, ask.price_ticks, ask.trader, ask.sub_index));
                remaining -= fill;
                true
            });
        } else {
            handle.for_each_bid_best_first(|idx, bid| {
                if matches.len() >= walk_limit { return false; }
                if remaining == 0 { return false; }
                if bid.price_ticks < limit_ticks { return false; }
                if bid.expires_at_slot > 0 && now_slot > bid.expires_at_slot { return true; }
                if bid.trader == trader_pk {
                    match stp_mode {
                        STP_CANCEL_OLDEST => {
                            stp_cancels.push(idx);
                            return true;
                        }
                        STP_CANCEL_BOTH => {
                            stp_aborted = true;
                            return false;
                        }
                        _ => return true,
                    }
                }
                let fill = bid.size_lots.min(remaining);
                matches.push((idx, bid.order_id, fill, bid.price_ticks, bid.trader, bid.sub_index));
                remaining -= fill;
                true
            });
        }

        // STP_CANCEL_BOTH: a self-match was found and the caller asked
        // us to reject the whole taker order.
        if stp_aborted {
            return err!(FlashBookError::SelfTrade);
        }

        // STP_CANCEL_OLDEST: cancel each self-matched resting order before
        // applying the (non-self) matches. Best-cache stays consistent —
        // each remove updates the cached best via the successor-capture
        // path in remove_*_node.
        for idx in &stp_cancels {
            if side_is_bid {
                handle.remove_ask_node(*idx);
            } else {
                handle.remove_bid_node(*idx);
            }
        }

        // post_only: if walk found ANY match, the order would cross —
        // reject (caller asked for guaranteed-maker semantics).
        if post_only && !matches.is_empty() {
            return err!(FlashBookError::PostOnlyWouldCross);
        }

        // FOK: require the entire order to be filled; abort if not.
        if fok && remaining > 0 {
            return err!(FlashBookError::FillOrKillNotFilled);
        }

        // ── Phase 2: apply each match (decrement or remove maker) ───
        // Per-fill emit replaced with one batched FillBatchEvent at end —
        // saves ~200 CU per fill (Borsh + sol_log_data overhead amortized
        // over the whole walk instead of paid per-step).
        let mut fills: Vec<FillEntry> = Vec::with_capacity(matches.len());
        for (maker_idx, maker_id, fill_size, fill_price, maker_trader, maker_sub_idx) in &matches {
            let new_size = handle.decrement_size_at(*maker_idx, *fill_size)?;
            if new_size == 0 {
                if side_is_bid {
                    handle.remove_ask_node(*maker_idx);
                } else {
                    handle.remove_bid_node(*maker_idx);
                }
            }
            fills.push(FillEntry {
                maker: *maker_trader,
                size_lots: *fill_size,
                price_ticks: *fill_price,
                maker_id: *maker_id,
                maker_sub_index: *maker_sub_idx,
            });
        }
        if !fills.is_empty() {
            emit!(FillBatchEvent {
                market: market_key,
                taker: trader_pk,
                taker_side: side,
                taker_id: taker_order_id,
                fills,
                taker_sub_index: sub_index,
            });
        }

        // ── Phase 3: residual handling — IOC vs rest-as-limit ───────
        // IOC: cancel residual (do not insert). Phoenix / Manifest /
        // Binance IOC semantics. post_only-with-no-match falls
        // through here to insert as pure resting limit.
        let mut inserted_idx: hypertree::DataIndex = hypertree::NIL;
        // MATCH-H2: only rest a residual that meets the per-market minimum lot
        // size. A sub-min remainder is dropped (IOC-style, inserted_idx stays
        // NIL) rather than resting as a dust order that violates the
        // `min_base_lots` invariant the rest of the system assumes.
        if remaining > 0 && !ioc && remaining >= p.min_base_lots {
            let order = state_v2::RestingOrderV2 {
                order_id: taker_order_id,
                seq: taker_seq,
                price_ticks: limit_ticks,
                size_lots: remaining,
                expires_at_slot,
                trader: trader_pk,
                last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
                side,
                order_type: 0,
                flags,
                // Residual taker rests as a limit order — carry the
                // taker's sub_index so subsequent matches route to the
                // right TraderState.
                sub_index,
            };
            inserted_idx = if side_is_bid {
                handle.insert_bid(order)?
            } else {
                handle.insert_ask(order)?
            };
        }

        emit!(TakerOrderClearedEvent {
            market: market_key,
            taker: trader_pk,
            side,
            taker_size_lots: size_lots,
            filled_lots: size_lots - remaining,
            residual_resting_lots: remaining,
            match_count: matches.len() as u32,
            taker_order_id,
            residual_node_index: inserted_idx,
        });
        Ok(())
    }

    // ─── REMOVED: wrapper-program CPI ixs (place_limit_order_v2_cpi,
    // cpi_release_collateral_to_user, cpi_open_trader_state_for_trader,
    // cpi_credit_collateral, cpi_debit_collateral, cancel_order_v2_cpi)
    // were the cross-program CPI gate used by the now-merged wrapper
    // programs. With monolithic flash_book the wrapper-authority gate is
    // gone — V3 ixs operate directly.


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
        if side > 1 {
            return err!(FlashBookError::OutOfRange);
        }
        let trader_pk = ctx.accounts.trader.key();
        let market_key = ctx.accounts.market.key();

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        if handle.header.market_pubkey != market_key {
            return err!(FlashBookError::WrongMarket);
        }

        let side_is_bid = side == 0;
        let idx = if side_is_bid {
            handle.lookup_bid_by_order_id(order_id)
        } else {
            handle.lookup_ask_by_order_id(order_id)
        };
        if idx == crate::hypertree::NIL {
            return err!(FlashBookError::LiquidationStale);
        }

        // Ownership check — only the original trader can cancel.
        let order_seq = {
            let order = handle.order_at(idx);
            if order.trader != trader_pk {
                return err!(FlashBookError::WrongTrader);
            }
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

    /// V2 cancel-all: remove every resting order owned by the caller in
    /// this market. Walks both bid and ask RBTs, collecting indices of
    /// orders whose `trader == caller`, then removes each in two
    /// passes (read-only walk → mutating removes). The walk is bounded
    /// by `MAX_CANCELS_PER_IX_V2 = 24`: a single tx that triggers more
    /// cancels would risk the BPF CU budget. Traders with many resting
    /// orders simply call this ix repeatedly until `cancelled_count == 0`.
    ///
    /// Returns silently when the trader has no resting orders — useful
    /// as a "best-effort cleanup" the bot can call defensively.
    pub fn cancel_all_v2(
        ctx: Context<CancelOrderV2>,
    ) -> Result<()> {
        let trader_pk = ctx.accounts.trader.key();
        let market_key = ctx.accounts.market.key();

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        if handle.header.market_pubkey != market_key {
            return err!(FlashBookError::WrongMarket);
        }

        // Two passes per side because the walk closure borrows &handle
        // immutably; removal needs &mut handle. Collect first, mutate after.
        let mut bid_idxs: Vec<hypertree::DataIndex> = Vec::with_capacity(8);
        let mut ask_idxs: Vec<hypertree::DataIndex> = Vec::with_capacity(8);
        let cap = MAX_CANCELS_PER_IX_V2;

        handle.for_each_bid_best_first(|idx, bid| {
            if bid_idxs.len() + ask_idxs.len() >= cap { return false; }
            if bid.trader == trader_pk {
                bid_idxs.push(idx);
            }
            true
        });
        handle.for_each_ask_best_first(|idx, ask| {
            if bid_idxs.len() + ask_idxs.len() >= cap { return false; }
            if ask.trader == trader_pk {
                ask_idxs.push(idx);
            }
            true
        });

        let cancelled_count = (bid_idxs.len() + ask_idxs.len()) as u32;
        for idx in bid_idxs.iter() {
            handle.remove_bid_node(*idx);
        }
        for idx in ask_idxs.iter() {
            handle.remove_ask_node(*idx);
        }

        emit!(BulkOrderCancelledV2Event {
            market: market_key,
            trader: trader_pk,
            cancelled_count,
            total_orders_after: handle.header.total_orders_active,
        });
        Ok(())
    }

    /// V2 modify: atomic cancel + place in one ix. Cheaper than two
    /// separate txs (one signature, one set of account-load costs)
    /// and preserves the trader's intent across the cancel window —
    /// no chance of being filled between the cancel and the replacement.
    ///
    /// Validates the new params against the same gates as
    /// `place_limit_order_v2` (status, min_lot, tick alignment, OI cap)
    /// BEFORE removing the old order so a malformed modify doesn't
    /// silently drop the original. The new order receives a fresh
    /// `seq` from the header's monotonic counter; off-chain consumers
    /// must re-derive the order_id from the emitted `OrderModifiedV2Event`.
    pub fn modify_order_v2(
        ctx: Context<PlaceLimitOrderV2>,
        side: u8,
        old_order_id: u64,
        new_size_lots: u64,
        new_limit_ticks: u64,
        new_flags: u8,
        new_expires_at_slot: u64,
    ) -> Result<()> {
        let market = &ctx.accounts.market;
        let now_slot = Clock::get()?.slot;

        // Fast input guards — same as place_limit_order_v2.
        if side > 1
            || new_size_lots == 0
            || new_limit_ticks == 0
            || (new_flags & !0b0111_1111) != 0
        {
            return err!(FlashBookError::OutOfRange);
        }
        if new_expires_at_slot != 0 && new_expires_at_slot <= now_slot {
            return err!(FlashBookError::OutOfRange);
        }

        // Market-state guards.
        let p = &market.params;
        if market.status != MarketStatus::Active as u8
            && market.status != MarketStatus::PostOnly as u8
        {
            return err!(FlashBookError::OutOfRange);
        }
        if new_size_lots < p.min_base_lots {
            return err!(FlashBookError::SizeBelowMinLot);
        }
        if new_limit_ticks % p.tick_size != 0 {
            return err!(FlashBookError::PriceNotOnTick);
        }

        // Per-market OI hard cap. Resting orders don't actually move OI
        // (only fills do), so this check is conservative — guards against
        // a market that places a huge limit hoping to manipulate the cap
        // gate when a fill eventually lands.
        let oi_cap = p.max_oi_base_lots;
        if oi_cap > 0 {
            let cur = if side == 0 { market.oi_long_lots } else { market.oi_short_lots };
            if cur.saturating_add(new_size_lots) > oi_cap {
                return err!(FlashBookError::OpenInterestCapExceeded);
            }
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();

        let mut book_data = ctx.accounts.market_book.try_borrow_mut_data()?;
        let mut handle = state_v2::MarketBookHandle::from_account_data(&mut book_data)?;
        if handle.header.market_pubkey != market_key {
            return err!(FlashBookError::WrongMarket);
        }

        // ── Phase 1: locate + validate ownership of the old order ────
        let side_is_bid = side == 0;
        let old_idx = if side_is_bid {
            handle.lookup_bid_by_order_id(old_order_id)
        } else {
            handle.lookup_ask_by_order_id(old_order_id)
        };
        if old_idx == crate::hypertree::NIL {
            return err!(FlashBookError::LiquidationStale);
        }
        let (old_seq, old_sub_index) = {
            let order = handle.order_at(old_idx);
            if order.trader != trader_pk {
                return err!(FlashBookError::WrongTrader);
            }
            (order.seq, order.sub_index)
        };

        // ── Phase 2: remove the old order ─────────────────────────────
        if side_is_bid {
            handle.remove_bid_node(old_idx);
        } else {
            handle.remove_ask_node(old_idx);
        }

        // ── Phase 3: allocate a new seq + build + insert the new order
        let new_seq = handle
            .header
            .order_seq_counter
            .checked_add(1)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        handle.header.order_seq_counter = new_seq;

        let new_order_id = state_v2::encode_order_id(new_limit_ticks, new_seq, side_is_bid);
        let order = state_v2::RestingOrderV2 {
            order_id: new_order_id,
            seq: new_seq,
            price_ticks: new_limit_ticks,
            size_lots: new_size_lots,
            expires_at_slot: new_expires_at_slot,
            trader: trader_pk,
            last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
            side,
            order_type: 0,
            flags: new_flags,
            // Phase 2f — modify preserves the original order's sub_index.
            sub_index: old_sub_index,
        };
        let new_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };

        emit!(OrderModifiedV2Event {
            market: market_key,
            trader: trader_pk,
            side,
            old_seq,
            new_seq,
            new_price_ticks: new_limit_ticks,
            new_size_lots,
            new_node_index: new_idx,
            new_order_id,
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

    /// Authority-only: directly set the insurance fund's pause threshold.
    /// Governance lever: raising the threshold makes the ADL trigger easier
    /// to hit (protocol enters "auto-deleverage-eligible" state sooner —
    /// useful during stress). Lowering it relaxes the gate.
    ///
    /// No solvency invariants are enforced here — the threshold is purely
    /// a governance-set floor. Operators are free to set it ABOVE the
    /// current balance (which is what the e2e-adl proof needs), or BELOW.
    /// Withdraw_insurance_fund's own guard (`new_balance >= pause_threshold`)
    /// continues to protect against accidental drains via withdrawal.
    pub fn set_insurance_pause_threshold(
        ctx: Context<SetInsurancePauseThreshold>,
        new_threshold_quote_lots: u64,
    ) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.insurance_fund.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        let f = &mut ctx.accounts.insurance_fund;
        let previous = f.pause_threshold_quote_lots;
        f.pause_threshold_quote_lots = new_threshold_quote_lots;
        emit!(InsurancePauseThresholdUpdatedEvent {
            authority: ctx.accounts.authority.key(),
            previous_threshold_quote_lots: previous,
            new_threshold_quote_lots,
            current_balance_quote_lots: f.balance_quote_lots,
        });
        Ok(())
    }

    /// Initialize per-trader state.
    pub fn open_trader_state(ctx: Context<OpenTraderState>) -> Result<()> {
        let trader_key = ctx.accounts.trader.key();
        let bump = ctx.bumps.trader_state;
        let slot = Clock::get()?.slot;
        let mut s = ctx.accounts.trader_state.load_init()?;
        s.trader = trader_key;
        s.bump = bump;
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
        s.volume_window_start_slot = slot;
        s.sub_index = 0; // main account
        Ok(())
    }

    // ─── Sub-accounts (Phase 1 scaffolding) ───────────────────────────
    //
    // DESIGN: a wallet's MAIN trader_state lives at the existing PDA
    //   [TraderStateAccount::SEED, trader.key().as_ref()]
    // and sub-accounts (sub_index in 1..=255) live at the extended PDA
    //   [TraderStateAccount::SEED, trader.key().as_ref(), &[sub_index]].
    // sub_index = 0 is reserved as a synonym for the main account so
    // off-chain code can treat sub_index as a single dimension.
    //
    // Phase 1 (this commit) provides only:
    //   * `open_trader_sub_account(sub_index)`     — create the sub PDA
    //   * `transfer_main_to_sub(sub_index, amount)` — move collateral in
    //   * `transfer_sub_to_main(sub_index, amount)` — move collateral out
    // The sub-account is a collateral parking spot — it cannot YET be
    // used as a trading account (place_*/cancel_*/liquidate_* still
    // require the main PDA via their `seeds = [...]` constraint).
    //
    // Phase 2 (separate session, audited diff) will relax the existing
    // Accounts structs so any of the trader's PDAs (main or sub) can
    // sign for trading, and route PositionAccount PDAs by sub_index.
    // The Phase 2 work is intentionally separated because relaxing
    // seeds-validation in favor of handler-side checks is a security-
    // critical pattern that wants a focused review.
    //
    // The sub-account's `delegate` field works independently from the
    // main account's — different hot keys can drive different sub-
    // accounts. `referrer` and `builder` are also per-sub-account.
    pub fn open_trader_sub_account(
        ctx: Context<OpenTraderSubAccount>,
        sub_index: u8,
    ) -> Result<()> {
        // sub_index 0 is reserved for the main account (which uses the
        // legacy PDA seeds and `open_trader_state`).
        require!(sub_index >= 1, FlashBookError::OutOfRange);
        let trader_key = ctx.accounts.trader.key();
        let bump = ctx.bumps.trader_sub_account;
        let slot = Clock::get()?.slot;
        {
            let mut s = ctx.accounts.trader_sub_account.load_init()?;
            s.trader = trader_key;
            s.bump = bump;
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
            s.volume_window_start_slot = slot;
            s.sub_index = sub_index;
        }
        emit!(SubAccountOpenedEvent {
            trader: ctx.accounts.trader.key(),
            sub_index,
            sub_account: ctx.accounts.trader_sub_account.key(),
        });
        Ok(())
    }

    /// Move `amount` quote-lots from the trader's MAIN trader_state into
    /// their sub-account at `sub_index`. Useful as a "savings" or
    /// strategy-segregation pattern — the sub-account is isolated from
    /// the main account's trading exposure until Phase 2 wires trading
    /// from sub-accounts.
    ///
    /// REJECTS if the main account has open positions whose maintenance
    /// margin would be violated by the withdrawal — caller must close
    /// or reduce exposure first. (Implemented by reading
    /// `main.open_positions` and refusing the transfer when > 0. The
    /// looser, position-aware check is deferred to Phase 2 alongside
    /// the assess_margin refactor.)
    pub fn transfer_main_to_sub(
        ctx: Context<TransferBetweenSubAccounts>,
        sub_index: u8,
        amount: u64,
    ) -> Result<()> {
        require!(sub_index >= 1, FlashBookError::OutOfRange);
        require!(amount > 0, FlashBookError::ZeroSize);
        let trader_key = ctx.accounts.trader.key();
        let mut main = ctx.accounts.main_trader_state.load_mut()?;
        let mut sub = ctx.accounts.sub_trader_state.load_mut()?;
        require!(
            main.trader == sub.trader && main.trader == trader_key,
            FlashBookError::WrongTrader
        );
        require!(
            main.open_positions == 0,
            FlashBookError::OpenInterestCapExceeded
        );
        main.collateral_quote_lots = main
            .collateral_quote_lots
            .checked_sub(amount)
            .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        sub.collateral_quote_lots = sub
            .collateral_quote_lots
            .checked_add(amount)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        emit!(SubAccountTransferEvent {
            trader: trader_key,
            sub_index,
            amount_quote_lots: amount,
            direction: 0, // 0 = main → sub
            main_balance_after: main.collateral_quote_lots,
            sub_balance_after: sub.collateral_quote_lots,
        });
        Ok(())
    }

    /// Mirror of `transfer_main_to_sub`: move `amount` quote-lots from
    /// the sub-account back to main. Same guard — refuses if the sub
    /// account has open positions (which it can't have until Phase 2
    /// wires trading; included now for forward-compatibility).
    pub fn transfer_sub_to_main(
        ctx: Context<TransferBetweenSubAccounts>,
        sub_index: u8,
        amount: u64,
    ) -> Result<()> {
        require!(sub_index >= 1, FlashBookError::OutOfRange);
        require!(amount > 0, FlashBookError::ZeroSize);
        let trader_key = ctx.accounts.trader.key();
        let mut main = ctx.accounts.main_trader_state.load_mut()?;
        let mut sub = ctx.accounts.sub_trader_state.load_mut()?;
        require!(
            main.trader == sub.trader && main.trader == trader_key,
            FlashBookError::WrongTrader
        );
        require!(
            sub.open_positions == 0,
            FlashBookError::OpenInterestCapExceeded
        );
        sub.collateral_quote_lots = sub
            .collateral_quote_lots
            .checked_sub(amount)
            .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        main.collateral_quote_lots = main
            .collateral_quote_lots
            .checked_add(amount)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        emit!(SubAccountTransferEvent {
            trader: trader_key,
            sub_index,
            amount_quote_lots: amount,
            direction: 1, // 1 = sub → main
            main_balance_after: main.collateral_quote_lots,
            sub_balance_after: sub.collateral_quote_lots,
        });
        Ok(())
    }

    /// Phase 2c migration — move a Position from the LEGACY PDA
    /// `[POS_SEED, market, wallet]` to the NEW Phase 2c PDA
    /// `[POS_SEED, market, trader_state.key()]`. The new derivation
    /// makes sub-account positions distinct from main, which is the
    /// prerequisite for sub-account trading.
    ///
    /// Per (wallet, market). Only main-account positions need to be
    /// migrated — sub-account positions don't exist pre-2c. The legacy
    /// PDA is closed and rent refunded to the trader. The new PDA is
    /// init'd with the same on-chain state (size, side, entry, funding
    /// indices, realized PnL, isolated collateral, timing fields).
    ///
    /// Idempotent in the sense that calling it a second time fails
    /// because the legacy PDA no longer exists at the old address;
    /// callers can safely re-run after partial failure.
    pub fn migrate_position_to_trader_state_key(
        ctx: Context<MigratePositionToTraderStateKey>,
    ) -> Result<()> {
        let new_bump = ctx.bumps.new_position;
        let market_key = ctx.accounts.market.key();

        // Snapshot the legacy position's state, then drop the read guard
        // before initializing the new (zero-copy) position account.
        let legacy = *ctx.accounts.legacy_position.load()?;

        require!(
            legacy.trader == ctx.accounts.trader.key(),
            FlashBookError::WrongTrader
        );
        require!(legacy.market == market_key, FlashBookError::WrongMarket);
        // Note: legacy.collateral_quote_lots can be > 0 post Phase 2 if
        // the position was isolated — that value is preserved.

        {
            let mut new_pos = ctx.accounts.new_position.load_init()?;
            new_pos.market = legacy.market;
            new_pos.trader = legacy.trader;
            new_pos.side = legacy.side;
            new_pos.size_lots = legacy.size_lots;
            new_pos.entry_price_ticks = legacy.entry_price_ticks;
            new_pos.cum_funding_index_at_entry = legacy.cum_funding_index_at_entry;
            new_pos.realized_pnl_quote_lots = legacy.realized_pnl_quote_lots;
            new_pos.funding_paid_quote_lots = legacy.funding_paid_quote_lots;
            new_pos.last_settlement_batch = legacy.last_settlement_batch;
            new_pos.unhealthy_since_slot = legacy.unhealthy_since_slot;
            new_pos.last_liquidated_at_slot = legacy.last_liquidated_at_slot;
            new_pos.collateral_quote_lots = legacy.collateral_quote_lots;
            new_pos.bump = new_bump;
        }

        emit!(PositionMigratedEvent {
            trader: legacy.trader,
            market: market_key,
            size_lots: legacy.size_lots,
            collateral_quote_lots: legacy.collateral_quote_lots,
        });
        // Anchor closes `legacy_position` via the `close = trader`
        // constraint on the Accounts struct, refunding rent.
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
        let trader = {
            let mut s = ctx.accounts.trader_state.load_mut()?;
            require!(s.referrer == Pubkey::default(), FlashBookError::OutOfRange);
            s.referrer = referrer;
            s.trader
        };
        emit!(TraderReferrerSetEvent { trader, referrer });
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
        let (trader, prev) = {
            let mut s = ctx.accounts.trader_state.load_mut()?;
            let prev = s.delegate;
            s.delegate = new_delegate;
            (s.trader, prev)
        };
        emit!(TraderDelegateUpdatedEvent {
            trader,
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
        let trader = {
            let mut s = ctx.accounts.trader_state.load_mut()?;
            s.fee_discount_bps = discount_bps;
            s.trader
        };
        emit!(TraderFeeTierUpdatedEvent {
            trader,
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
        let (prev, trader) = {
            let mut position = ctx.accounts.position.load_mut()?;
            let prev = position.leverage_cap;
            position.leverage_cap = cap;
            (prev, position.trader)
        };
        emit!(PositionLeverageUpdatedEvent {
            market: market.key(),
            trader,
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
        let signer = ctx.accounts.authority.key();
        let (from_trader, from_open_positions, from_collateral) = {
            let from = ctx.accounts.from_state.load()?;
            let to = ctx.accounts.to_state.load()?;
            require!(from.trader != to.trader, FlashBookError::OutOfRange);
            require!(from.is_authorized(&signer), FlashBookError::Unauthorized);
            require!(to.is_authorized(&signer), FlashBookError::Unauthorized);
            (from.trader, from.open_positions, from.collateral_quote_lots)
        };

        // Position-aware margin gate. If source has open positions:
        //   1. Walk remaining_accounts as [market, position] pairs.
        //   2. Verify count matches from.open_positions exactly.
        //   3. Build snapshots and assess against the stress lattice
        //      using POST-sweep collateral (`from.collateral - amount`).
        //   4. Reject if not healthy.
        // No open positions → skip the walk (cheap fast path).
        if from_open_positions > 0 {
            let post_sweep_collateral = from_collateral
                .checked_sub(amount)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
            let expected = from_open_positions as usize;
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
                require!(position.trader == from_trader, FlashBookError::WrongTrader);
                require!(position.market == market_ai.key(), FlashBookError::WrongMarket);
                // C-2: bind each position to THIS trader_state (positions are
                // PDA-keyed on trader_state.key(), but `.trader` is only the
                // WALLET — without this a different sub-account's / stale
                // position of the same wallet could be substituted to
                // under-state risk), require liveness, and reject duplicate
                // markets (so the exact-count check can't be padded with a
                // duplicate while a real position is omitted). Mirrors the
                // partial_withdraw_collateral guard set (the 2026-06-21 fix).
                verify_position_pda(
                    &market_ai.key(),
                    &ctx.accounts.from_state.key(),
                    position.bump,
                    &position_ai.key(),
                    ctx.program_id,
                )?;
                require!(position.size_lots > 0, FlashBookError::ZeroSize);
                require!(
                    !market_keys.contains(&market_ai.key()),
                    FlashBookError::OutOfRange
                );

                snaps.push(RiskPosSnap {
                    market: position.market,
                    side: if position.side == 0 { Side::Long } else { Side::Short },
                    size_lots: position.size_lots,
                    entry_price: Ticks(position.entry_price_ticks),
                    cum_funding_index_at_entry: position.cum_funding_index(),
                    collateral_quote_lots: position.collateral_quote_lots,
                });
                market_snaps.push(RiskMarketSnap {
                    market: market_ai.key(),
                    mark_price: Ticks(market_acct.mark_price_ticks),
                    cum_funding_index: market_acct.cum_funding_index,
                    // RISK-2: withdraw gate enforces INITIAL margin (IM > MM) so a
                    // trader keeps a buffer above the liquidation line. Liquidation
                    // paths intentionally keep maintenance_margin_ratio_bps.
                    maintenance_margin_bps: market_acct.params.initial_margin_ratio_bps,
                    tick_size: market_acct.params.tick_size,
                    concentration_threshold_lots: market_acct.params.concentration_threshold_lots,
                    concentration_extra_mmr_bps: market_acct.params.concentration_extra_mmr_bps,
                    side_oi_lots: 0,
                    oi_mmr_slope_bps_per_million_lots: 0,
                    oi_mmr_max_extra_bps: 0,
                });
                market_keys.push(market_ai.key());
            }
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_unified_fn(
                &snaps,
                &market_snaps,
                &scenarios,
                post_sweep_collateral,
            )?;
            require!(assessment.is_healthy, FlashBookError::TraderLiquidatable);
        }

        let to_trader = {
            let mut from = ctx.accounts.from_state.load_mut()?;
            from.collateral_quote_lots = from
                .collateral_quote_lots
                .checked_sub(amount)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
            let mut to = ctx.accounts.to_state.load_mut()?;
            to.collateral_quote_lots = to
                .collateral_quote_lots
                .checked_add(amount)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            to.trader
        };

        emit!(CollateralSweptEvent {
            authority: signer,
            from: from_trader,
            to: to_trader,
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
        let (ts_open_positions, ts_collateral) = {
            let ts = ctx.accounts.vault_trader_state.load()?;
            (ts.open_positions, ts.collateral_quote_lots)
        };
        require!(ts_open_positions == 0, FlashBookError::SweepRequiresFlat);

        let shares_outstanding = vault.shares_outstanding;
        // No depositors yet → nothing to settle. Anchor HWM at unit price.
        if shares_outstanding == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = constants::USD_UNIT;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        let nav = ts_collateral as u128;
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
        let (trader, prev, new_max_share) = {
            let mut s = ctx.accounts.trader_state.load_mut()?;
            let prev = s.builder;
            s.builder = builder;
            s.builder_max_fee_share_bps = if builder == Pubkey::default() {
                0
            } else {
                max_fee_share_bps
            };
            (s.trader, prev, s.builder_max_fee_share_bps)
        };
        emit!(TraderBuilderUpdatedEvent {
            trader,
            previous: prev,
            new: builder,
            max_fee_share_bps: new_max_share,
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
        let (trader, new_balance) = {
            let mut s = ctx.accounts.trader_state.load_mut()?;
            s.collateral_quote_lots = s
                .collateral_quote_lots
                .checked_add(amount_quote_lots)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            (s.trader, s.collateral_quote_lots)
        };
        emit!(CollateralDepositedEvent {
            trader,
            amount: amount_quote_lots,
            new_balance,
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
            let s = ctx.accounts.trader_state.load()?;
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
        let (trader, new_balance) = {
            let mut s = ctx.accounts.trader_state.load_mut()?;
            s.collateral_quote_lots = s
                .collateral_quote_lots
                .checked_sub(amount_quote_lots)
                .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
            (s.trader, s.collateral_quote_lots)
        };
        emit!(CollateralWithdrawnEvent {
            trader,
            amount: amount_quote_lots,
            new_balance,
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

        // Walk remaining_accounts in (market, position) pairs to build
        // the post-withdrawal margin snapshot.
        let trader_state_key = ctx.accounts.trader_state.key();
        let (trader_pk, open, current_collateral) = {
            let s = ctx.accounts.trader_state.load()?;
            // Pre-flight: amount available.
            require!(
                amount_quote_lots <= s.collateral_quote_lots,
                FlashBookError::InsufficientCollateral,
            );
            (s.trader, s.open_positions as usize, s.collateral_quote_lots)
        };
        let program_id = ctx.program_id;
        let remaining = ctx.remaining_accounts;
        // C-2: the caller MUST supply EVERY open position. Exact-count +
        // size>0 + market-dedupe + PDA-binding (below) together make it
        // impossible to omit a risky position to understate the margin
        // requirement and over-withdraw. `% 2 == 0` alone was the bug.
        require!(remaining.len() == open * 2, FlashBookError::OutOfRange);

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
            // C-2: bind this position to THIS trader_state (blocks
            // substituting another sub-account's / a stale position).
            verify_position_pda(
                &m_ai.key(),
                &trader_state_key,
                position.bump,
                &p_ai.key(),
                program_id,
            )?;
            // C-2: every supplied position must be live, and no market may
            // appear twice (else the exact-count check could be padded
            // with a duplicate while a real position is omitted).
            require!(position.size_lots > 0, FlashBookError::ZeroSize);
            require!(
                !market_keys.contains(&m_ai.key()),
                FlashBookError::OutOfRange
            );

            let notional = (position.size_lots as u128)
                .saturating_mul(market.mark_price_ticks as u128)
                .saturating_mul(market.params.tick_size as u128);
            total_notional_quote = total_notional_quote.saturating_add(notional);

            snaps.push(RiskPosSnap {
                market: position.market,
                side: if position.side == 0 { Side::Long } else { Side::Short },
                size_lots: position.size_lots,
                entry_price: Ticks(position.entry_price_ticks),
                cum_funding_index_at_entry: position.cum_funding_index(),
                collateral_quote_lots: position.collateral_quote_lots,
            });
            market_snaps.push(RiskMarketSnap {
                market: m_ai.key(),
                mark_price: Ticks(market.mark_price_ticks),
                cum_funding_index: market.cum_funding_index,
                // RISK-2: withdraw gate enforces INITIAL margin (buffer above liquidation).
                maintenance_margin_bps: market.params.initial_margin_ratio_bps,
                tick_size: market.params.tick_size,
                concentration_threshold_lots: market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
                side_oi_lots: 0,
                oi_mmr_slope_bps_per_million_lots: 0,
                oi_mmr_max_extra_bps: 0,
            });
            market_keys.push(m_ai.key());
            i += 2;
        }

        // Pre-mutate snapshot of the post-withdrawal collateral.
        let post_collateral = current_collateral
            .checked_sub(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;

        // Compute the safety floor.
        // (a) IM required under joint stress lattice.
        let im_required: u64 = if snaps.is_empty() {
            0
        } else {
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_unified_fn(
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
        {
            let mut s = ctx.accounts.trader_state.load_mut()?;
            s.collateral_quote_lots = post_collateral;
        }
        emit!(PartialCollateralWithdrawnEvent {
            trader: trader_pk,
            amount: amount_quote_lots,
            new_balance: post_collateral,
            im_required,
            notional_floor,
            applied_floor: floor,
        });
        Ok(())
    }

    /// Transition a position from cross-margin to isolated-margin by
    /// reserving `amount_quote_lots` of the trader's pooled collateral
    /// against this specific position. Post-transfer health check must
    /// pass on BOTH:
    ///   (a) the cross set (other positions + reduced trader_state pool)
    ///   (b) the isolated position itself (its new collateral vs its
    ///       own maintenance margin under stress)
    ///
    /// Phase 2 constraint: AT MOST ONE position can be isolated per
    /// trader (the one being modified). If the trader has another
    /// position that's already isolated (`collateral_quote_lots > 0`),
    /// this ix rejects — un-isolate it first. Phase 3 lifts this to
    /// multi-isolated-positions.
    ///
    /// remaining_accounts layout: alternating (market, position) pairs
    /// for every OTHER position the trader has a non-zero size on. The
    /// target position is passed inline (see Accounts struct); do not
    /// re-include it in remaining_accounts.
    pub fn set_position_isolated<'info>(
        ctx: Context<'_, '_, '_, 'info, SetPositionMarginMode<'info>>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        let (trader_pk, ts_open, ts_collateral) = {
            let ts = ctx.accounts.trader_state.load()?;
            (ts.trader, ts.open_positions as usize, ts.collateral_quote_lots)
        };
        // Snapshot target position scalars (read guard dropped immediately).
        let (
            tp_trader,
            tp_market,
            tp_side,
            tp_size_lots,
            tp_entry_price_ticks,
            tp_cum_funding_index_at_entry,
            tp_collateral_quote_lots,
        ) = {
            let p = ctx.accounts.target_position.load()?;
            (
                p.trader,
                p.market,
                p.side,
                p.size_lots,
                p.entry_price_ticks,
                p.cum_funding_index(),
                p.collateral_quote_lots,
            )
        };
        require!(
            tp_trader == trader_pk,
            FlashBookError::WrongTrader
        );
        require!(
            tp_market == ctx.accounts.target_market.key(),
            FlashBookError::WrongMarket
        );
        require!(
            tp_size_lots > 0,
            FlashBookError::ZeroSize
        );
        require!(
            tp_collateral_quote_lots == 0,
            FlashBookError::OutOfRange
        );
        require!(
            amount_quote_lots <= ts_collateral,
            FlashBookError::InsufficientCollateral
        );

        // ─── Walk remaining_accounts: build snapshots for OTHER positions.
        let program_id = ctx.program_id;
        let trader_state_key = ctx.accounts.trader_state.key();
        let open = ts_open;
        let target_market_key_pre = ctx.accounts.target_market.key();
        let remaining = ctx.remaining_accounts;
        // H-1: the target position is passed as a named account, so the
        // walk must cover every OTHER open position — exactly
        // `(open_positions - 1)` pairs. Exact-count + size>0 + dedupe
        // (incl. the target market) + PDA-binding stop a caller from
        // hiding a risky position to pass the post-transition health check.
        require!(
            remaining.len() == open.saturating_sub(1) * 2,
            FlashBookError::OutOfRange
        );

        let mut snaps: Vec<RiskPosSnap> = Vec::new();
        let mut market_snaps: Vec<RiskMarketSnap> = Vec::new();
        let mut market_keys: Vec<Pubkey> = Vec::new();

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
            require!(position.trader == trader_pk, FlashBookError::WrongTrader);
            require!(position.market == m_ai.key(), FlashBookError::WrongMarket);
            verify_position_pda(
                &m_ai.key(),
                &trader_state_key,
                position.bump,
                &p_ai.key(),
                program_id,
            )?;
            // Phase 2 single-isolated guard.
            require!(
                position.collateral_quote_lots == 0,
                FlashBookError::OutOfRange
            );
            // H-1: live position, no duplicate market, and never the
            // target market (which is accounted separately below).
            require!(position.size_lots > 0, FlashBookError::ZeroSize);
            require!(
                m_ai.key() != target_market_key_pre,
                FlashBookError::OutOfRange
            );
            require!(
                !market_keys.contains(&m_ai.key()),
                FlashBookError::OutOfRange
            );
            snaps.push(RiskPosSnap {
                market: position.market,
                side: if position.side == 0 { Side::Long } else { Side::Short },
                size_lots: position.size_lots,
                entry_price: Ticks(position.entry_price_ticks),
                cum_funding_index_at_entry: position.cum_funding_index(),
                collateral_quote_lots: position.collateral_quote_lots,
            });
            market_snaps.push(RiskMarketSnap {
                market: m_ai.key(),
                mark_price: Ticks(market.mark_price_ticks),
                cum_funding_index: market.cum_funding_index,
                maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
                tick_size: market.params.tick_size,
                concentration_threshold_lots: market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
                side_oi_lots: 0,
                oi_mmr_slope_bps_per_million_lots: 0,
                oi_mmr_max_extra_bps: 0,
            });
            market_keys.push(m_ai.key());
            i += 2;
        }

        // Add the target position too (will be in the isolated bucket).
        let target_market = &ctx.accounts.target_market;
        let target_market_key = target_market.key();
        snaps.push(RiskPosSnap {
            market: tp_market,
            side: if tp_side == 0 { Side::Long } else { Side::Short },
            size_lots: tp_size_lots,
            entry_price: Ticks(tp_entry_price_ticks),
            cum_funding_index_at_entry: tp_cum_funding_index_at_entry,
            // Post-transition: target gets `amount_quote_lots` reserved as
            // its isolated collateral. The split assessment derives the
            // bucket assignment from `isolated_map` below, but we keep
            // the snapshot field in sync as the source of truth.
            collateral_quote_lots: amount_quote_lots,
        });
        market_snaps.push(RiskMarketSnap {
            market: target_market_key,
            mark_price: Ticks(target_market.mark_price_ticks),
            cum_funding_index: target_market.cum_funding_index,
            maintenance_margin_bps: target_market.params.maintenance_margin_ratio_bps,
            tick_size: target_market.params.tick_size,
            concentration_threshold_lots: target_market.params.concentration_threshold_lots,
            concentration_extra_mmr_bps: target_market.params.concentration_extra_mmr_bps,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        });
        market_keys.push(target_market_key);

        // Post-transfer state for the split assessment.
        let post_cross_collateral = ts_collateral
            .checked_sub(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        let post_isolated_collateral = amount_quote_lots; // currently 0 + amount
        let isolated_map: [(Pubkey, u64); 1] = [(target_market_key, post_isolated_collateral)];

        let scenarios = default_scenarios_fn(&market_keys);
        let assessment = matcher::risk::assess_margin_split(
            &snaps,
            &market_snaps,
            &scenarios,
            post_cross_collateral,
            &isolated_map,
        )?;
        require!(
            assessment.is_healthy,
            FlashBookError::InsufficientCollateral
        );

        // Apply the transfer.
        ctx.accounts.trader_state.load_mut()?.collateral_quote_lots = post_cross_collateral;
        ctx.accounts.target_position.load_mut()?.collateral_quote_lots = post_isolated_collateral;

        emit!(PositionMarginModeChangedEvent {
            trader: trader_pk,
            market: target_market_key,
            new_isolated_collateral_quote_lots: post_isolated_collateral,
            new_cross_collateral_quote_lots: post_cross_collateral,
            mode: 1, // 0 = cross, 1 = isolated
        });
        Ok(())
    }

    /// Transition a position from isolated-margin back to cross-margin.
    /// All of `position.collateral_quote_lots` is returned to the
    /// trader's pooled `trader_state.collateral_quote_lots`. The
    /// post-transfer cross set (now including this position) must pass
    /// the standard assess_margin health check.
    ///
    /// remaining_accounts layout: same as `set_position_isolated` —
    /// alternating (market, position) pairs for every OTHER position
    /// the trader has a non-zero size on.
    pub fn set_position_cross<'info>(
        ctx: Context<'_, '_, '_, 'info, SetPositionMarginMode<'info>>,
    ) -> Result<()> {
        let (trader_pk, ts_open, ts_collateral) = {
            let ts = ctx.accounts.trader_state.load()?;
            (ts.trader, ts.open_positions as usize, ts.collateral_quote_lots)
        };
        // Snapshot target position scalars (read guard dropped immediately).
        let (
            tp_trader,
            tp_market,
            tp_side,
            tp_size_lots,
            tp_entry_price_ticks,
            tp_cum_funding_index_at_entry,
            tp_collateral_quote_lots,
        ) = {
            let p = ctx.accounts.target_position.load()?;
            (
                p.trader,
                p.market,
                p.side,
                p.size_lots,
                p.entry_price_ticks,
                p.cum_funding_index(),
                p.collateral_quote_lots,
            )
        };
        require!(
            tp_trader == trader_pk,
            FlashBookError::WrongTrader
        );
        require!(
            tp_market == ctx.accounts.target_market.key(),
            FlashBookError::WrongMarket
        );
        let returned = tp_collateral_quote_lots;
        require!(returned > 0, FlashBookError::OutOfRange);

        let program_id = ctx.program_id;
        let trader_state_key = ctx.accounts.trader_state.key();
        let open = ts_open;
        let target_market_key_pre = ctx.accounts.target_market.key();
        let remaining = ctx.remaining_accounts;
        // H-1: same coverage invariant as set_position_isolated — the
        // target is named, so every OTHER open position must be supplied.
        require!(
            remaining.len() == open.saturating_sub(1) * 2,
            FlashBookError::OutOfRange
        );

        let mut snaps: Vec<RiskPosSnap> = Vec::new();
        let mut market_snaps: Vec<RiskMarketSnap> = Vec::new();
        let mut market_keys: Vec<Pubkey> = Vec::new();

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
            require!(position.trader == trader_pk, FlashBookError::WrongTrader);
            require!(position.market == m_ai.key(), FlashBookError::WrongMarket);
            verify_position_pda(
                &m_ai.key(),
                &trader_state_key,
                position.bump,
                &p_ai.key(),
                program_id,
            )?;
            // Phase 2 single-isolated guard: only the target can have
            // collateral_quote_lots > 0; siblings must be cross.
            require!(
                position.collateral_quote_lots == 0,
                FlashBookError::OutOfRange
            );
            require!(position.size_lots > 0, FlashBookError::ZeroSize);
            require!(
                m_ai.key() != target_market_key_pre,
                FlashBookError::OutOfRange
            );
            require!(
                !market_keys.contains(&m_ai.key()),
                FlashBookError::OutOfRange
            );
            snaps.push(RiskPosSnap {
                market: position.market,
                side: if position.side == 0 { Side::Long } else { Side::Short },
                size_lots: position.size_lots,
                entry_price: Ticks(position.entry_price_ticks),
                cum_funding_index_at_entry: position.cum_funding_index(),
                collateral_quote_lots: position.collateral_quote_lots,
            });
            market_snaps.push(RiskMarketSnap {
                market: m_ai.key(),
                mark_price: Ticks(market.mark_price_ticks),
                cum_funding_index: market.cum_funding_index,
                maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
                tick_size: market.params.tick_size,
                concentration_threshold_lots: market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
                side_oi_lots: 0,
                oi_mmr_slope_bps_per_million_lots: 0,
                oi_mmr_max_extra_bps: 0,
            });
            market_keys.push(m_ai.key());
            i += 2;
        }

        // Add the target position into the cross set (post-transition).
        let target_market = &ctx.accounts.target_market;
        let target_market_key = target_market.key();
        if tp_size_lots > 0 {
            snaps.push(RiskPosSnap {
                market: tp_market,
                side: if tp_side == 0 { Side::Long } else { Side::Short },
                size_lots: tp_size_lots,
                entry_price: Ticks(tp_entry_price_ticks),
                cum_funding_index_at_entry: tp_cum_funding_index_at_entry,
                // Post-transition: target is cross-margined.
                collateral_quote_lots: 0,
            });
            market_snaps.push(RiskMarketSnap {
                market: target_market_key,
                mark_price: Ticks(target_market.mark_price_ticks),
                cum_funding_index: target_market.cum_funding_index,
                maintenance_margin_bps: target_market.params.maintenance_margin_ratio_bps,
                tick_size: target_market.params.tick_size,
                concentration_threshold_lots: target_market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: target_market.params.concentration_extra_mmr_bps,
                side_oi_lots: 0,
                oi_mmr_slope_bps_per_million_lots: 0,
                oi_mmr_max_extra_bps: 0,
            });
            market_keys.push(target_market_key);
        }

        let post_cross_collateral = ts_collateral
            .checked_add(returned)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        // No isolated positions remain — straight assess_margin (cross-only).
        let scenarios = default_scenarios_fn(&market_keys);
        if !snaps.is_empty() {
            let assessment = assess_margin_fn(
                &snaps,
                &market_snaps,
                &scenarios,
                post_cross_collateral,
            )?;
            require!(
                assessment.is_healthy,
                FlashBookError::InsufficientCollateral
            );
        }

        // Apply the transfer.
        ctx.accounts.trader_state.load_mut()?.collateral_quote_lots = post_cross_collateral;
        ctx.accounts.target_position.load_mut()?.collateral_quote_lots = 0;

        emit!(PositionMarginModeChangedEvent {
            trader: trader_pk,
            market: target_market_key,
            new_isolated_collateral_quote_lots: 0,
            new_cross_collateral_quote_lots: post_cross_collateral,
            mode: 0,
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
        let mut position = ctx.accounts.position.load_mut()?;
        let mut trader_state = ctx.accounts.trader_state.load_mut()?;

        // No-op for empty positions.
        if position.size_lots == 0 {
            position.set_cum_funding_index(market.cum_funding_index);
            return Ok(());
        }

        // RISK-M2: funding notional uses the MARK price (current), not the
        // entry price — matching `assess_margin`'s funding term and the perp
        // standard. Previously settle charged entry-priced funding while the
        // risk/health check assessed mark-priced funding, so the amount a
        // trader was *shown* to owe diverged from what they were *charged*.
        // notional = size × mark_price × tick_size, in quote lots.
        let notional_u128 = (position.size_lots as u128)
            .checked_mul(market.mark_price_ticks as u128)
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
            position.cum_funding_index(),
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

        // ── Phase 2 isolated-margin funding routing ──────────────────────
        // For an isolated position, funding owed/received moves between
        // the per-position bucket and the protocol — NEVER the trader's
        // cross pool. That insulation is the entire point of isolation:
        // a runaway funding bill on an isolated short cannot drain the
        // trader's other (cross) positions. For a cross position,
        // behavior is unchanged: funding settles to/from the pooled
        // `trader_state.collateral_quote_lots`.
        //
        // If the isolated bucket is exhausted by the owed-funding case,
        // we truncate (same conservative `.min()` semantics as the
        // cross path) — the unpaid remainder is effectively absorbed
        // because `cum_funding_index_at_entry` advances unconditionally.
        // The next health check will mark the position liquidatable.
        let is_isolated = position.collateral_quote_lots > 0;
        // Track the ACTUAL collateral moved (clamped to availability) so the
        // solvency residual delta matches the real change in total committed
        // trader collateral (C_tot).
        let mut paid: u64 = 0;
        let mut received: u64 = 0;
        if owed_i64 > 0 {
            let owed_u64 = owed_i64 as u64;
            if is_isolated {
                paid = owed_u64.min(position.collateral_quote_lots);
                position.collateral_quote_lots -= paid;
            } else {
                paid = owed_u64.min(trader_state.collateral_quote_lots);
                trader_state.collateral_quote_lots -= paid;
            }
        } else if owed_i64 < 0 {
            received = owed_i64.unsigned_abs();
            if is_isolated {
                position.collateral_quote_lots = position
                    .collateral_quote_lots
                    .checked_add(received)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            } else {
                trader_state.collateral_quote_lots = trader_state
                    .collateral_quote_lots
                    .checked_add(received)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            }
        }

        // RISK-1: funding is not zero-sum (entry-priced + lazy per-position),
        // so it must move the solvency residual `V − C_tot − I`:
        //   trader PAYS    → C_tot ↓ by `paid`     → residual ↑ (more backing per claim)
        //   trader RECEIVES→ C_tot ↑ by `received` → residual ↓ (underflow ⇒ insolvency, rejected)
        // Net-zero-sum funding nets to zero across positions; any genuine
        // drift now moves the residual so the haircut / kill-switch bounds it,
        // instead of silently minting/burning protocol collateral.
        let haircut = &mut ctx.accounts.haircut_state;
        if paid > 0 {
            haircut.residual_quote_lots = haircut
                .residual_quote_lots
                .checked_add(paid as u128)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        }
        if received > 0 {
            haircut.residual_quote_lots = haircut
                .residual_quote_lots
                .checked_sub(received as u128)
                .ok_or_else(|| error!(FlashBookError::HaircutResidualUnderflow))?;
        }

        position.funding_paid_quote_lots = position
            .funding_paid_quote_lots
            .checked_add(owed_i64)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        position.set_cum_funding_index(market.cum_funding_index);

        emit!(FundingSettledEvent {
            market: market.key(),
            trader: position.trader,
            owed_quote_lots: owed_i64,
            new_collateral: trader_state.collateral_quote_lots,
        });

        Ok(())
    }

    /// Apply a single fill against the taker's and maker's Position PDAs.
    /// Called by the off-chain sequencer once per `FillEntry` row in a
    /// `FillBatchEvent` emitted from `place_taker_order_v2`, or by an
    /// off-chain bookkeeper that aggregates multiple fills per tx.
    ///
    /// Trust model: the `sequencer` signer is the configured authority;
    /// the fill data is taken at face value (a future version verifies
    /// via per-tx fill buffer or Merkle proof against the emitted event).
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
        taker_sub_index: u8,
        maker_sub_index: u8,
    ) -> Result<()> {
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(price_ticks > 0, FlashBookError::ZeroPrice);
        require!(taker_side <= 1, FlashBookError::OutOfRange);

        // ── C-1 settlement authorization ────────────────────────────
        // `apply_fill` takes fill data at face value, so the signer MUST
        // be the market's authorized sequencer. Without this gate ANY
        // signer could fabricate fills against arbitrary positions and
        // drain the quote vault. Decoupled from `market.authority` so it
        // survives the authority-burn ladder (see `state.rs` MarketAccount
        // .sequencer). A market whose sequencer is still the zero pubkey
        // (pre-field markets) fails closed here — settlement halts until
        // `set_market_sequencer` is called.
        require_keys_eq!(
            ctx.accounts.sequencer.key(),
            ctx.accounts.market.sequencer,
            FlashBookError::Unauthorized
        );

        // ── Phase 2i sub-account PDA verification ───────────────────
        // Closes the 1-byte routing-attack surface left open by Phase 2d.
        // The Accounts struct's `seeds = [...]` constraint was dropped to
        // permit sub-account TraderStates, but that left the sequencer
        // free to pass ANY trader_state whose .trader field matches.
        //
        // This block re-derives the expected TraderState PDA from
        // (account.trader, ix-param sub_index) and asserts it matches
        // the account's actual PDA. The sub_index values arrive via
        // `FillBatchEvent.{taker,maker}_sub_index` (Phase 2e), so an
        // honest sequencer already has them; this verification just
        // makes that contract on-chain-enforceable.
        verify_trader_state_pda(
            taker_sub_index,
            ctx.accounts.taker_trader_state.load()?.trader,
            ctx.accounts.taker_trader_state.key(),
            ctx.accounts.taker_trader_state.load()?.bump,
            ctx.program_id,
        )?;
        verify_trader_state_pda(
            maker_sub_index,
            ctx.accounts.maker_trader_state.load()?.trader,
            ctx.accounts.maker_trader_state.key(),
            ctx.accounts.maker_trader_state.load()?.bump,
            ctx.program_id,
        )?;

        // init_if_needed zero-copy: a freshly-created PositionAccount has a
        // zeroed discriminator until `load_init()` writes it — without this,
        // the first load()/load_mut() below fails AccountDiscriminatorMismatch.
        // On an already-existing position load_init() returns Err with NO side
        // effect, so this is a safe one-time discriminator stamp.
        // init_if_needed zero-copy: Anchor's AccountLoader writes the
        // discriminator only in exit() (instruction end), so a freshly
        // created position's disc is still zero during this handler and the
        // load()/load_mut() calls below would fail AccountDiscriminatorMismatch.
        // Stamp it now; no-op for an already-initialized position.
        stamp_zc_discriminator(
            &ctx.accounts.taker_position.to_account_info(),
            <state::PositionAccount as anchor_lang::Discriminator>::DISCRIMINATOR,
        )?;
        stamp_zc_discriminator(
            &ctx.accounts.maker_position.to_account_info(),
            <state::PositionAccount as anchor_lang::Discriminator>::DISCRIMINATOR,
        )?;

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
            let taker_volume = ctx.accounts.taker_trader_state.load()?.volume_30d_quote_lots;
            let maker_volume = ctx.accounts.maker_trader_state.load()?.volume_30d_quote_lots;
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
            ctx.accounts.taker_trader_state.load()?.volume_30d_quote_lots,
        );
        let pre_maker_tier_index = tier_index_for_volume(
            &tier_pairs,
            ctx.accounts.maker_trader_state.load()?.volume_30d_quote_lots,
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
        let discount_bps_full = ctx.accounts.taker_trader_state.load()?.fee_discount_bps as u128;
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
        let taker_trader_pk = ctx.accounts.taker_trader_state.load()?.trader;
        let maker_trader_pk = ctx.accounts.maker_trader_state.load()?.trader;

        // Apply fees BEFORE position state is mutated, so reads are clean.
        // Taker pays fee from collateral (must have it; place_limit_order's
        // margin gate ensured this at intake time, but we double-check).
        // For NEGATIVE-fee tier traders, taker_fee == 0 and we credit the
        // taker the rebate sourced from the protocol contribution.
        //
        // ── Phase 2 isolated-margin fee routing ──────────────────────────
        // Read the per-position `collateral_quote_lots` BEFORE the
        // apply_fill_to_position mutation — its value is the source of
        // truth for whether the fill counts as "fee against an isolated
        // bucket" (>0) or "fee against the cross pool" (0). For a brand-
        // new position freshly init'd by this ix the value is 0 (cross
        // by default), matching the intent.
        let taker_pos_isolated = ctx.accounts.taker_position.load()?.collateral_quote_lots > 0;
        let maker_pos_isolated = ctx.accounts.maker_position.load()?.collateral_quote_lots > 0;
        {
            if taker_fee > 0 {
                if taker_pos_isolated {
                    let mut p = ctx.accounts.taker_position.load_mut()?;
                    p.collateral_quote_lots = p
                        .collateral_quote_lots
                        .checked_sub(taker_fee)
                        .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
                } else {
                    let mut s = ctx.accounts.taker_trader_state.load_mut()?;
                    s.collateral_quote_lots = s
                        .collateral_quote_lots
                        .checked_sub(taker_fee)
                        .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
                }
            }
            if taker_negative_rebate_u128 > 0 {
                let neg_rebate_u64 = if taker_negative_rebate_u128 > u64::MAX as u128 {
                    u64::MAX
                } else {
                    taker_negative_rebate_u128 as u64
                };
                if taker_pos_isolated {
                    let mut p = ctx.accounts.taker_position.load_mut()?;
                    p.collateral_quote_lots = p
                        .collateral_quote_lots
                        .checked_add(neg_rebate_u64)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                } else {
                    let mut s = ctx.accounts.taker_trader_state.load_mut()?;
                    s.collateral_quote_lots = s
                        .collateral_quote_lots
                        .checked_add(neg_rebate_u64)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                }
            }
        }
        // Maker receives rebate (positive `maker_rebate_bps` path) OR
        // pays a fee (negative path — wave 22 retail tier). Mutually
        // exclusive: at most one of `maker_rebate` / `maker_fee` is
        // non-zero per the sign split above.
        {
            if maker_rebate > 0 {
                if maker_pos_isolated {
                    let mut p = ctx.accounts.maker_position.load_mut()?;
                    p.collateral_quote_lots = p
                        .collateral_quote_lots
                        .checked_add(maker_rebate)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                } else {
                    let mut s = ctx.accounts.maker_trader_state.load_mut()?;
                    s.collateral_quote_lots = s
                        .collateral_quote_lots
                        .checked_add(maker_rebate)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                }
            }
            if maker_fee > 0 {
                if maker_pos_isolated {
                    let mut p = ctx.accounts.maker_position.load_mut()?;
                    p.collateral_quote_lots = p
                        .collateral_quote_lots
                        .checked_sub(maker_fee)
                        .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
                } else {
                    let mut s = ctx.accounts.maker_trader_state.load_mut()?;
                    s.collateral_quote_lots = s
                        .collateral_quote_lots
                        .checked_sub(maker_fee)
                        .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
                }
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
        let taker_referrer = ctx.accounts.taker_trader_state.load()?.referrer;
        if taker_referrer != Pubkey::default() && market.params.referrer_share_bps > 0 {
            let share = ((net_fee as u128)
                .saturating_mul(market.params.referrer_share_bps as u128)
                / (constants::BPS_DENOM as u128)) as u64;
            if share > 0 {
                emit!(ReferralOwedEvent {
                    taker: taker_trader_pk,
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
        let (taker_builder, trader_builder_cap) = {
            let s = ctx.accounts.taker_trader_state.load()?;
            (s.builder, s.builder_max_fee_share_bps)
        };
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
                    taker: taker_trader_pk,
                    builder: taker_builder,
                    amount_quote_lots: share,
                });
            }
        }

        // ── Curated-creator share ────────────────────────────────────
        // If the market was migrated to a curated creator by the
        // protocol authority, credit the creator with
        // `creator_share_bps` of net fee. Pull-based event — no on-chain
        // creator account walk on the hot path. For authority-deployed
        // markets `market.creator == Pubkey::default()` and this branch
        // is a no-op (no permissionless market creation in V3).
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
            )?;
            credit_volume_for_tier(
                &mut ctx.accounts.maker_trader_state,
                notional_quote_lots,
                now_slot,
            )?;

            // ── Wave 22 phase 2 — emit TraderTierUpgradedEvent on
            //    tier boundary crossings (silent on no-change).
            if !tier_pairs.is_empty() {
                let taker_post_volume = ctx.accounts.taker_trader_state.load()?.volume_30d_quote_lots;
                let post_taker_tier_index = tier_index_for_volume(
                    &tier_pairs,
                    taker_post_volume,
                );
                if post_taker_tier_index != pre_taker_tier_index {
                    emit!(TraderTierUpgradedEvent {
                        trader: taker_trader_pk,
                        previous_tier_index: pre_taker_tier_index,
                        new_tier_index: post_taker_tier_index,
                        volume_quote_lots: taker_post_volume,
                    });
                }
                let maker_post_volume = ctx.accounts.maker_trader_state.load()?.volume_30d_quote_lots;
                let post_maker_tier_index = tier_index_for_volume(
                    &tier_pairs,
                    maker_post_volume,
                );
                if post_maker_tier_index != pre_maker_tier_index {
                    emit!(TraderTierUpgradedEvent {
                        trader: maker_trader_pk,
                        previous_tier_index: pre_maker_tier_index,
                        new_tier_index: post_maker_tier_index,
                        volume_quote_lots: maker_post_volume,
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
            let tax = tax_uncapped.min(ctx.accounts.taker_trader_state.load()?.collateral_quote_lots);
            if tax > 0 {
                // Deduct from taker.
                ctx.accounts.taker_trader_state.load_mut()?.collateral_quote_lots -= tax;
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
                    let mut maker_state = ctx.accounts.maker_trader_state.load_mut()?;
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

        // Scope the position write-guards: they MUST drop before the
        // realized-PnL helpers below re-`load_mut()` the same accounts.
        let (
            taker_was_open,
            maker_was_open,
            taker_pre_side,
            maker_pre_side,
            taker_pre_size,
            maker_pre_size,
            taker_pre_realized,
            maker_pre_realized,
            taker_pos_isolated_for_pnl,
            maker_pos_isolated_for_pnl,
        ) = {
            let mut taker_pos = ctx.accounts.taker_position.load_mut()?;
            let mut maker_pos = ctx.accounts.maker_position.load_mut()?;

            // Initialize Position state on first ever fill against this PDA.
            if taker_pos.market == Pubkey::default() {
                taker_pos.market = market_key;
                taker_pos.trader = taker_trader_pk;
                taker_pos.bump = ctx.bumps.taker_position;
                taker_pos.set_cum_funding_index(funding_index);
                taker_pos.last_settlement_batch = current_batch;
            }
            if maker_pos.market == Pubkey::default() {
                maker_pos.market = market_key;
                maker_pos.trader = maker_trader_pk;
                maker_pos.bump = ctx.bumps.maker_position;
                maker_pos.set_cum_funding_index(funding_index);
                maker_pos.last_settlement_batch = current_batch;
            }

            // Snapshot pre-state so we can detect open/close transitions
            // AND compute the realized-PnL delta this fill produced. The
            // delta is what `apply_fill_to_position` accumulates onto
            // `pos.realized_pnl_quote_lots` for the closed-portion legs.
            let taker_was_open = taker_pos.size_lots > 0;
            let maker_was_open = maker_pos.size_lots > 0;
            let taker_pre_side = taker_pos.side;
            let maker_pre_side = maker_pos.side;
            let taker_pre_size = taker_pos.size_lots;
            let maker_pre_size = maker_pos.size_lots;
            let taker_pre_realized = taker_pos.realized_pnl_quote_lots;
            let maker_pre_realized = maker_pos.realized_pnl_quote_lots;
            let taker_pos_isolated_for_pnl = taker_pos.collateral_quote_lots > 0;
            let maker_pos_isolated_for_pnl = maker_pos.collateral_quote_lots > 0;

            apply_fill_to_position(&mut taker_pos, taker_side_enum, size_lots, price_ticks, funding_index)?;
            apply_fill_to_position(&mut maker_pos, maker_side_enum, size_lots, price_ticks, funding_index)?;
            (
                taker_was_open,
                maker_was_open,
                taker_pre_side,
                maker_pre_side,
                taker_pre_size,
                maker_pre_size,
                taker_pre_realized,
                maker_pre_realized,
                taker_pos_isolated_for_pnl,
                maker_pos_isolated_for_pnl,
            )
        };

        // ── Phase 2g realized-PnL materialisation ───────────────────
        // `apply_fill_to_position` writes the realized-PnL delta into
        // `pos.realized_pnl_quote_lots` on the closing leg(s) but does
        // NOT touch any collateral bucket. That left realized PnL as
        // an informational tally — a trader closing a winning position
        // could see realized_pnl rise but had no way to access the
        // profit through any subsequent ix.
        //
        // We close that loop here: subtract pre from post on each side
        // to get the delta this fill produced, then debit/credit the
        // appropriate collateral bucket. Bucket selection follows the
        // same rule as fee routing above (isolated_for_pnl flag
        // sampled BEFORE the apply_fill_to_position mutation):
        //   isolated → position.collateral_quote_lots
        //   cross    → trader_state.collateral_quote_lots
        //
        // Losses on an isolated bucket truncate at 0 (same conservative
        // semantics as settle_funding) — the shortfall will trip the
        // next health check and the position becomes liquidatable. The
        // cross path uses checked_sub and surfaces an error rather
        // than silently going negative.
        let taker_post_realized = ctx.accounts.taker_position.load()?.realized_pnl_quote_lots;
        let maker_post_realized = ctx.accounts.maker_position.load()?.realized_pnl_quote_lots;
        let taker_pnl_delta = (taker_post_realized as i128)
            .saturating_sub(taker_pre_realized as i128);
        let maker_pnl_delta = (maker_post_realized as i128)
            .saturating_sub(maker_pre_realized as i128);

        // Wave 24d: route positive deltas through the H-haircut reserve
        // when the per-position haircut state is provided. Losses always
        // debit collateral directly (loss seniority).
        let now_slot = Clock::get()?.slot;
        if taker_pnl_delta != 0 {
            apply_realized_pnl_delta_v2(
                taker_pnl_delta,
                taker_pos_isolated_for_pnl,
                &mut ctx.accounts.taker_position,
                &mut ctx.accounts.taker_trader_state,
                ctx.accounts.taker_position_haircut.as_mut(),
                now_slot,
            )?;
        }
        if maker_pnl_delta != 0 {
            apply_realized_pnl_delta_v2(
                maker_pnl_delta,
                maker_pos_isolated_for_pnl,
                &mut ctx.accounts.maker_position,
                &mut ctx.accounts.maker_trader_state,
                ctx.accounts.maker_position_haircut.as_mut(),
                now_slot,
            )?;
        }

        // Post-fill position scalars (read guards dropped immediately).
        let (taker_post_side, taker_post_size) = {
            let p = ctx.accounts.taker_position.load()?;
            (p.side, p.size_lots)
        };
        let (maker_post_side, maker_post_size) = {
            let p = ctx.accounts.maker_position.load()?;
            (p.side, p.size_lots)
        };

        // Update OI counters: walk pre→post for each side.
        update_oi(market, taker_pre_side, taker_pre_size, taker_post_side, taker_post_size)?;
        update_oi(market, maker_pre_side, maker_pre_size, maker_post_side, maker_post_size)?;

        // Update open_positions transitions on TraderState.
        let taker_is_open = taker_post_size > 0;
        let maker_is_open = maker_post_size > 0;
        {
            let mut taker_state = ctx.accounts.taker_trader_state.load_mut()?;
            if !taker_was_open && taker_is_open {
                taker_state.open_positions = taker_state.open_positions.saturating_add(1);
            } else if taker_was_open && !taker_is_open {
                taker_state.open_positions = taker_state.open_positions.saturating_sub(1);
            }
        }
        {
            let mut maker_state = ctx.accounts.maker_trader_state.load_mut()?;
            if !maker_was_open && maker_is_open {
                maker_state.open_positions = maker_state.open_positions.saturating_add(1);
            } else if maker_was_open && !maker_is_open {
                maker_state.open_positions = maker_state.open_positions.saturating_sub(1);
            }
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

        // ── V3 mark-price engine: last-trade-price tracking ─────────
        // Blend the fill price into `mark_price_ticks` via EMA so the
        // gate that reads `mark_price` (the health check, the funding
        // premium, the FLP fair value seed) tracks the tape between
        // batches. Without this, `mark_price_ticks` would stay frozen at
        // the value `initialize_market` wrote and `update_oracle` would
        // never tip live positions underwater.
        //
        //   new_mark = (alpha * fill_price + (BPS_DENOM - alpha) * old_mark) / BPS_DENOM
        //
        // Then clamp the absolute change to ±`mark_max_change_bps` so a
        // single outlier fill cannot flash-crash the mark. EMA-CLAMP, not
        // reject — the fill itself already settled; we just dampen the
        // effect on mark.
        //
        // alpha == 0 keeps the legacy behaviour (mark never updates from
        // fills). Production should set 2_000 (20% weight per fill).
        let alpha_bps = (market.params.mark_ema_alpha_bps as u128).min(constants::BPS_DENOM as u128);
        let old_mark = market.mark_price_ticks;
        if alpha_bps > 0 && old_mark > 0 {
            let one_minus_alpha = (constants::BPS_DENOM as u128) - alpha_bps;
            let blended_u128 = ((price_ticks as u128).saturating_mul(alpha_bps)
                + (old_mark as u128).saturating_mul(one_minus_alpha))
                / (constants::BPS_DENOM as u128);
            let mut new_mark = if blended_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                blended_u128 as u64
            };
            // Clamp absolute change to ±mark_max_change_bps.
            let max_change_bps = market.params.mark_max_change_bps as u128;
            if max_change_bps > 0 && old_mark > 0 {
                let max_delta = (old_mark as u128).saturating_mul(max_change_bps)
                    / (constants::BPS_DENOM as u128);
                let max_delta_u64 = if max_delta > u64::MAX as u128 {
                    u64::MAX
                } else {
                    max_delta as u64
                };
                let upper = old_mark.saturating_add(max_delta_u64);
                let lower = old_mark.saturating_sub(max_delta_u64);
                if new_mark > upper {
                    new_mark = upper;
                } else if new_mark < lower {
                    new_mark = lower.max(1);
                }
            }
            if new_mark != old_mark && new_mark > 0 {
                market.mark_price_ticks = new_mark;
                emit!(MarkPriceUpdatedEvent {
                    market: market_key,
                    old_mark_ticks: old_mark,
                    new_mark_ticks: new_mark,
                    oracle_ticks: market.oracle_price_ticks,
                    source: 0, // 0 = fill
                });
                // Drift alert: if mark has drifted from oracle by more
                // than `drift_alert_bps`, emit a `MarkPriceDriftEvent`
                // so off-chain observers can nudge `settle_mark`.
                let drift_bps_cfg = market.params.drift_alert_bps;
                if drift_bps_cfg > 0 && market.oracle_price_ticks > 0 {
                    let oracle_u128 = market.oracle_price_ticks as u128;
                    let mark_u128 = new_mark as u128;
                    let diff_u128 = if mark_u128 > oracle_u128 {
                        mark_u128 - oracle_u128
                    } else {
                        oracle_u128 - mark_u128
                    };
                    let drift_bps_u128 = diff_u128
                        .saturating_mul(constants::BPS_DENOM as u128)
                        / oracle_u128;
                    if drift_bps_u128 >= drift_bps_cfg as u128 {
                        let dbg = if drift_bps_u128 > u32::MAX as u128 {
                            u32::MAX
                        } else {
                            drift_bps_u128 as u32
                        };
                        emit!(MarkPriceDriftEvent {
                            market: market_key,
                            mark_ticks: new_mark,
                            oracle_ticks: market.oracle_price_ticks,
                            drift_bps: dbg,
                        });
                    }
                }
            }
        }

        // ── Multi-threshold margin warning ──────────────────────────
        // Single-position equity-vs-MMR view (cheap, no portfolio walk):
        //   equity   = collateral + unrealized_pnl(pos, mark)
        //   required = position_notional × mmr_bps / 10_000
        // Emit on threshold crossings (250%/200%/125%) so off-chain UIs
        // can push pre-liquidation alerts. Hyperliquid pattern.
        let taker_collateral_now = ctx.accounts.taker_trader_state.load()?.collateral_quote_lots;
        let maker_collateral_now = ctx.accounts.maker_trader_state.load()?.collateral_quote_lots;
        let mark_ticks = market.mark_price_ticks;
        let tick_size = market.params.tick_size;
        let mmr_bps = market.params.maintenance_margin_ratio_bps;
        {
            let pos = ctx.accounts.taker_position.load()?;
            if pos.size_lots > 0 {
                emit_margin_threshold_if_crossed(
                    taker_trader_pk,
                    market_key,
                    &*pos,
                    mark_ticks,
                    tick_size,
                    mmr_bps,
                    taker_collateral_now,
                );
            }
        }
        {
            let pos = ctx.accounts.maker_position.load()?;
            if pos.size_lots > 0 {
                emit_margin_threshold_if_crossed(
                    maker_trader_pk,
                    market_key,
                    &*pos,
                    mark_ticks,
                    tick_size,
                    mmr_bps,
                    maker_collateral_now,
                );
            }
        }

        Ok(())
    }

    /// Update oracle price (authority-only). Bootstrap / dev / fallback path
    /// only — `update_oracle_from_pyth` (`init_market_oracle_config` +
    /// `update_oracle_from_pyth`) is the production path and pulls directly
    /// from the on-chain `PriceUpdateV2` account posted by the Pyth Solana
    /// Receiver.
    ///
    /// IMPORTANT: any future ix that updates `market.oracle_*` MUST validate
    /// `market.authority == ctx.accounts.authority.key()` (this ix) OR derive
    /// the price from a verified Pyth account (the Pyth-pull ix). Both gates
    /// are first-class — do not strip one assuming the other suffices.
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

        // Wave 26b — optional envelope gate. Rejects out-of-cap moves
        // before mutating the market.
        let now_slot = Clock::get()?.slot;
        gate_oracle_update(
            ctx.accounts.envelope_config.as_mut(),
            price_ticks,
            now_slot,
        )?;

        let market = &mut ctx.accounts.market;
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

        // Wave 26b — optional envelope gate on the accepted median.
        let now_slot = Clock::get()?.slot;
        gate_oracle_update(
            ctx.accounts.envelope_config.as_mut(),
            median,
            now_slot,
        )?;

        let market = &mut ctx.accounts.market;
        market.oracle_price_ticks = median;
        market.oracle_confidence = combined_conf;
        market.oracle_published_at_unix_seconds = combined_published_at;
        Ok(())
    }

    /// PERMISSIONLESS: hard-reset `mark_price_ticks` to the current
    /// `oracle_price_ticks`. The V3 mark-engine's second mark-update path.
    ///
    /// Anyone (sequencers, keepers, even traders) can call this — the only
    /// gate is the per-market `mark_settle_min_slots` rate limit, which
    /// prevents a single signer from spamming. The oracle's freshness and
    /// confidence are re-validated (same `OracleTooStale` /
    /// `OracleConfidenceTooWide` rules as `update_oracle`) so this can only
    /// snap to a price the protocol already trusts.
    ///
    /// Unlike `apply_fill`'s EMA blend, this is a hard reset — designed for
    /// thin-tape conditions where mark has drifted from the (fresh) oracle
    /// and no fills are flowing to nudge it back. Pairs naturally with the
    /// `MarkPriceDriftEvent` alert: off-chain observers see the drift,
    /// call `settle_mark`, mark snaps back.
    pub fn settle_mark(ctx: Context<SettleMark>) -> Result<()> {
        let market = &mut ctx.accounts.market;
        let market_key = market.key();
        let current_slot = Clock::get()?.slot;

        // Rate-limit: must wait `mark_settle_min_slots` between calls.
        let min_slots = market.params.mark_settle_min_slots as u64;
        if min_slots > 0 && market.last_mark_settle_slot > 0 {
            let elapsed = current_slot.saturating_sub(market.last_mark_settle_slot);
            require!(elapsed >= min_slots, FlashBookError::RateLimited);
        }

        // Oracle freshness gate — same rules as update_oracle.
        let oracle = market.oracle_price_ticks;
        require!(oracle > 0, FlashBookError::ZeroPrice);
        let now = Clock::get()?.unix_timestamp.max(0) as u64;
        let max_age = market.params.oracle_staleness_max_seconds as u64;
        if max_age > 0 {
            let age = now.saturating_sub(market.oracle_published_at_unix_seconds);
            require!(age <= max_age, FlashBookError::OracleTooStale);
        }
        let max_conf = market.params.oracle_confidence_max_bps;
        if max_conf > 0 {
            let conf_bps = ((market.oracle_confidence as u128)
                * (constants::BPS_DENOM as u128))
                .checked_div(oracle as u128)
                .ok_or_else(|| error!(FlashBookError::DivisionByZero))?;
            require!(
                conf_bps <= max_conf as u128,
                FlashBookError::OracleConfidenceTooWide,
            );
        }

        let old_mark = market.mark_price_ticks;
        market.mark_price_ticks = oracle;
        market.last_mark_settle_slot = current_slot;

        emit!(MarkPriceUpdatedEvent {
            market: market_key,
            old_mark_ticks: old_mark,
            new_mark_ticks: oracle,
            oracle_ticks: oracle,
            source: 1, // 1 = oracle_settle
        });
        // No drift event here — by definition mark = oracle after settle.
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

        // AUDIT FIX — Explicit absolute caps on margin ratios.
        // Without these, authority could set MMR/IM to >= 100%, which
        // would instantly mark every existing position liquidatable.
        // 5000 bps (50%) is a generous ceiling — well above any plausible
        // market parameter (BTC = 5%, exotic alt = 30% in typical CEX
        // configs). Tighter than BPS_DENOM to prevent griefing.
        require!(
            new_params.maintenance_margin_ratio_bps < 5_000,
            FlashBookError::OutOfRange
        );
        require!(
            new_params.initial_margin_ratio_bps < 5_000,
            FlashBookError::OutOfRange
        );
        // Concentration extra MMR must also be bounded so MMR + extra
        // doesn't push past 50% combined.
        require!(
            new_params
                .maintenance_margin_ratio_bps
                .saturating_add(new_params.concentration_extra_mmr_bps)
                < 5_000,
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
        let s = ctx.accounts.trader_state.load()?;
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

    /// Wave 30 — Authority burn. Permanently relinquish authority over
    /// this market by setting `market.authority` to `Pubkey::default()`.
    /// This is a one-way operation: once burned, no further
    /// `update_market_params`, `set_market_status`, or
    /// `transfer_market_authority` can succeed on this market.
    ///
    /// The kill-switch (`set_market_status`) is intentionally also
    /// burned along with the rest — true progressive decentralization
    /// means giving up the ability to pause too. Operators who need a
    /// safety net should burn authority only AFTER the market has
    /// proven stable for some operating period.
    ///
    /// Emits `MarketAuthorityBurnedEvent` as the canonical
    /// "this market is now fully decentralised" signal — clients,
    /// indexers, and risk monitors should treat it as a permanent
    /// state change.
    pub fn burn_market_authority(ctx: Context<UpdateMarketAuthority>) -> Result<()> {
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        require!(
            market.authority != Pubkey::default(),
            FlashBookError::Unauthorized
        );
        let prev = market.authority;
        market.authority = Pubkey::default();
        emit!(MarketAuthorityBurnedEvent {
            market: market.key(),
            previous_authority: prev,
            burn_slot: Clock::get()?.slot,
        });
        Ok(())
    }

    /// C-1 — Rotate the fill-settlement signer (`market.sequencer`) that
    /// gates `apply_fill` / `apply_flp_fill`. Authority-gated, so it must
    /// be done BEFORE `burn_market_authority` (once authority is burned,
    /// the sequencer is frozen at its last value — which keeps settlement
    /// running under the decentralised, non-rotatable key).
    ///
    /// `new_sequencer` must be non-default; setting it to the zero pubkey
    /// would brick settlement (the `address = market.sequencer` gate
    /// becomes unsignable). Use a key you control off-chain to run the
    /// sequencer process.
    pub fn set_market_sequencer(
        ctx: Context<UpdateMarketAuthority>,
        new_sequencer: Pubkey,
    ) -> Result<()> {
        let market = &mut ctx.accounts.market;
        require_keys_eq!(
            market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        require!(
            market.authority != Pubkey::default(),
            FlashBookError::Unauthorized
        );
        require!(
            new_sequencer != Pubkey::default(),
            FlashBookError::OutOfRange
        );
        let prev = market.sequencer;
        market.sequencer = new_sequencer;
        emit!(MarketSequencerRotatedEvent {
            market: market.key(),
            previous_sequencer: prev,
            new_sequencer,
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
        let position_a = ctx.accounts.position_a.load()?;
        let position_b = ctx.accounts.position_b.load()?;
        check_caps_for_leg(
            &ctx.accounts.market_a,
            &position_a,
            &ctx.accounts.flp_exposure,
            &leg_a,
        )?;
        check_caps_for_leg(
            &ctx.accounts.market_b,
            &position_b,
            &ctx.accounts.flp_exposure,
            &leg_b,
        )?;

        let market_a = &ctx.accounts.market_a;
        let market_b = &ctx.accounts.market_b;
        let market_a_key = mkt_a;
        let market_b_key = mkt_b;
        let trader_key = ctx.accounts.trader.key();

        let proj_a = project_post_leg(&position_a, &leg_a, market_a, market_a_key, trader_key)?;
        let proj_b = project_post_leg(&position_b, &leg_b, market_b, market_b_key, trader_key)?;
        drop(position_a);
        drop(position_b);

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
                // RISK-2: open gate enforces INITIAL margin (buffer above liquidation).
                maintenance_margin_bps: market.params.initial_margin_ratio_bps,
                tick_size: market.params.tick_size,
                concentration_threshold_lots: market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
                side_oi_lots: 0,
                oi_mmr_slope_bps_per_million_lots: 0,
                oi_mmr_max_extra_bps: 0,
            });
        }
        if !snaps.is_empty() {
            let mkt_keys: Vec<Pubkey> = markets.iter().map(|m| m.market).collect();
            let scenarios = default_scenarios_fn(&mkt_keys);
            let assessment = assess_margin_unified_fn(
                &snaps,
                &markets,
                &scenarios,
                ctx.accounts.trader_state.load()?.collateral_quote_lots,
            )?;
            require!(assessment.is_healthy, FlashBookError::TraderLiquidatable);
        }

        // Rate limit (parity).
        {
            let mut trader_state = ctx.accounts.trader_state.load_mut()?;
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
        }

        // V2 inject — into BOTH market_book PDAs.
        let now_slot = Clock::get()?.slot;
        let trader_sub_index = ctx.accounts.trader_state.load()?.sub_index;
        let (seq_a, idx_a) = inject_leg_into_hypertree(
            &ctx.accounts.market_book_a,
            market_a_key,
            trader_key,
            &leg_a,
            now_slot,
            trader_sub_index,
        )?;
        let (seq_b, idx_b) = inject_leg_into_hypertree(
            &ctx.accounts.market_book_b,
            market_b_key,
            trader_key,
            &leg_b,
            now_slot,
            trader_sub_index,
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
                // RISK-2: open gate enforces INITIAL margin (buffer above liquidation).
                maintenance_margin_bps: markets[i].params.initial_margin_ratio_bps,
                tick_size: markets[i].params.tick_size,
                concentration_threshold_lots: markets[i].params.concentration_threshold_lots,
                concentration_extra_mmr_bps: markets[i].params.concentration_extra_mmr_bps,
                side_oi_lots: 0,
                oi_mmr_slope_bps_per_million_lots: 0,
                oi_mmr_max_extra_bps: 0,
            });
        }
        if !snaps.is_empty() {
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_unified_fn(
                &snaps,
                &market_snaps,
                &scenarios,
                ctx.accounts.trader_state.load()?.collateral_quote_lots,
            )?;
            require!(assessment.is_healthy, FlashBookError::TraderLiquidatable);
        }

        // Rate limit (parity).
        let leg_count_u32 = u32::try_from(legs.len()).unwrap_or(u32::MAX);
        let trader_sub_index = {
            let mut trader_state = ctx.accounts.trader_state.load_mut()?;
            if trader_state.last_batch_seen != markets[0].current_batch {
                trader_state.last_batch_seen = markets[0].current_batch;
                trader_state.orders_this_batch = 0;
            }
            require!(
                trader_state.orders_this_batch.saturating_add(leg_count_u32)
                    <= MAX_ORDERS_PER_TRADER_PER_BATCH,
                FlashBookError::RateLimited
            );
            trader_state.orders_this_batch = trader_state
                .orders_this_batch
                .saturating_add(leg_count_u32);
            trader_state.sub_index
        };

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
                trader_sub_index,
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

        // AUDIT FIX (Wave 27c) — Oracle staleness gate on trigger
        // execution. Without this, a stale oracle could force-fire
        // a trigger at a price that doesn't reflect the live market,
        // bypassing the trader's intent. Mirrors liquidate_position_v2's
        // staleness gate.
        let oracle_max_age = market.params.oracle_staleness_max_seconds as u64;
        if oracle_max_age > 0 && market.oracle_published_at_unix_seconds > 0 {
            let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            let oracle_age = now_unix.saturating_sub(market.oracle_published_at_unix_seconds);
            require!(oracle_age <= oracle_max_age, FlashBookError::OracleTooStale);
        }

        let oracle = market.oracle_price_ticks;
        let fired = if trigger.kind == 0 {
            oracle <= trigger.trigger_price_ticks
        } else {
            oracle >= trigger.trigger_price_ticks
        };
        require!(fired, FlashBookError::OutOfRange);

        if trigger.flags & state::TriggerOrderAccount::FLAG_REDUCE_ONLY != 0 {
            let position = ctx.accounts.position.load()?;
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
            // Phase 2f — V1 trigger carries its trader's sub_index.
            sub_index: trigger.sub_index,
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
        let twap_sub_index = twap.sub_index;
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
                // Phase 2f — V1 TWAP slice inherits the TWAP's sub_index.
                sub_index: twap_sub_index,
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
        let iceberg_sub_index = iceberg.sub_index;

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
                // Phase 2f — V1 iceberg child inherits the iceberg's sub_index.
                sub_index: iceberg_sub_index,
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

    /// View ix: snapshot the FLP quoter's would-be next quote ladder
    /// (no state change). Runs the same `generate_quotes` computation
    /// the matcher consumes but doesn't mutate any state. Emits
    /// `QuoteLadderSnapshotEvent` carrying the top-N levels (each side)
    /// for off-chain UI rendering.
    ///
    /// Levels are interleaved bid/ask in seq order: [bid0, ask0,
    /// bid1, ask1, ...] — same emission order as the matcher would
    /// see. `levels_emitted` is bounded by `params.flp_quote_levels`.
    pub fn view_quote_ladder(ctx: Context<ViewMarket>) -> Result<()> {
        let market = &ctx.accounts.market;
        let flp = &ctx.accounts.flp_exposure;

        // Set up FlpQuoterParams + Inputs (mirrors the matcher's setup).
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
        let trader_state = ctx.accounts.trader_state.load()?;
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
                cum_funding_index_at_entry: position.cum_funding_index(),
                collateral_quote_lots: position.collateral_quote_lots,
            });
            market_snaps.push(RiskMarketSnap {
                market: market_ai.key(),
                mark_price: Ticks(market_acct.mark_price_ticks),
                cum_funding_index: market_acct.cum_funding_index,
                maintenance_margin_bps: market_acct.params.maintenance_margin_ratio_bps,
                tick_size: market_acct.params.tick_size,
                concentration_threshold_lots: market_acct.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market_acct.params.concentration_extra_mmr_bps,
                side_oi_lots: 0,
                oi_mmr_slope_bps_per_million_lots: 0,
                oi_mmr_max_extra_bps: 0,
            });
            market_keys.push(market_ai.key());
        }

        let (required, equity_signed, worst_idx) = if open == 0 {
            (0u64, trader_state.collateral_quote_lots as i128, 0u32)
        } else {
            let scenarios = default_scenarios_fn(&market_keys);
            let assessment = assess_margin_unified_fn(
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
        taker_sub_index: u8,
    ) -> Result<()> {
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(price_ticks > 0, FlashBookError::ZeroPrice);
        require!(taker_side <= 1, FlashBookError::OutOfRange);

        // ── C-1 settlement authorization (see apply_fill) ───────────
        require_keys_eq!(
            ctx.accounts.sequencer.key(),
            ctx.accounts.market.sequencer,
            FlashBookError::Unauthorized
        );

        // Phase 2i — verify the trader_state PDA matches
        // (taker_trader_state.trader, taker_sub_index). See apply_fill
        // for the full rationale.
        verify_trader_state_pda(
            taker_sub_index,
            ctx.accounts.taker_trader_state.load()?.trader,
            ctx.accounts.taker_trader_state.key(),
            ctx.accounts.taker_trader_state.load()?.bump,
            ctx.program_id,
        )?;

        // init_if_needed zero-copy: stamp the discriminator on a freshly
        // created taker position (see apply_fill for the full rationale).
        stamp_zc_discriminator(
            &ctx.accounts.taker_position.to_account_info(),
            <state::PositionAccount as anchor_lang::Discriminator>::DISCRIMINATOR,
        )?;

        let market = &mut ctx.accounts.market;
        let market_key = market.key();
        let funding_index = market.cum_funding_index;
        let current_batch = market.current_batch;
        let taker_side_enum = if taker_side == 0 { Side::Long } else { Side::Short };
        let flp_side_enum = taker_side_enum.opposite();
        let taker_trader_pk = ctx.accounts.taker_trader_state.load()?.trader;

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
                let taker_volume = ctx.accounts.taker_trader_state.load()?.volume_30d_quote_lots;
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
            ctx.accounts.taker_trader_state.load()?.volume_30d_quote_lots,
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
            let mut taker_state = ctx.accounts.taker_trader_state.load_mut()?;
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
            let tax = tax_uncapped.min(ctx.accounts.taker_trader_state.load()?.collateral_quote_lots);
            if tax > 0 {
                ctx.accounts.taker_trader_state.load_mut()?.collateral_quote_lots -= tax;
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

        // Scope the position write-guard so it drops before the realized-
        // PnL helper below re-`load_mut()`s the same account.
        let (
            taker_was_open,
            taker_pre_side,
            taker_pre_size,
            taker_pre_realized,
            taker_pos_isolated_for_pnl,
        ) = {
            let mut taker_pos = ctx.accounts.taker_position.load_mut()?;
            if taker_pos.market == Pubkey::default() {
                taker_pos.market = market_key;
                taker_pos.trader = taker_trader_pk;
                taker_pos.bump = ctx.bumps.taker_position;
                taker_pos.set_cum_funding_index(funding_index);
                taker_pos.last_settlement_batch = current_batch;
            }

            let taker_was_open = taker_pos.size_lots > 0;
            let taker_pre_side = taker_pos.side;
            let taker_pre_size = taker_pos.size_lots;
            let taker_pre_realized = taker_pos.realized_pnl_quote_lots;
            let taker_pos_isolated_for_pnl = taker_pos.collateral_quote_lots > 0;

            apply_fill_to_position(&mut taker_pos, taker_side_enum, size_lots, price_ticks, funding_index)?;
            (
                taker_was_open,
                taker_pre_side,
                taker_pre_size,
                taker_pre_realized,
                taker_pos_isolated_for_pnl,
            )
        };

        // Phase 2g — materialise realized-PnL delta on the FLP fill
        // path. Same routing rule as `apply_fill`: isolated → per-
        // position bucket; cross → trader_state. The FLP side itself
        // tracks PnL on its FlpMarketExposure entry (`apply_fill_to_flp_market`
        // below) and the LP-share NAV walks that to compute LP value;
        // it doesn't accumulate on `pos.realized_pnl_quote_lots` so
        // there's nothing to settle on the FLP maker side.
        let taker_post_realized = ctx.accounts.taker_position.load()?.realized_pnl_quote_lots;
        let taker_pnl_delta = (taker_post_realized as i128)
            .saturating_sub(taker_pre_realized as i128);
        // Wave 24d: route positive deltas through H-haircut reserve when
        // the per-position haircut state is provided. Losses always
        // debit collateral directly.
        let now_slot_flp = Clock::get()?.slot;
        if taker_pnl_delta != 0 {
            apply_realized_pnl_delta_v2(
                taker_pnl_delta,
                taker_pos_isolated_for_pnl,
                &mut ctx.accounts.taker_position,
                &mut ctx.accounts.taker_trader_state,
                ctx.accounts.taker_position_haircut.as_mut(),
                now_slot_flp,
            )?;
        }

        let (taker_post_side, taker_post_size) = {
            let p = ctx.accounts.taker_position.load()?;
            (p.side, p.size_lots)
        };

        update_oi(market, taker_pre_side, taker_pre_size, taker_post_side, taker_post_size)?;

        // Update FLP per-market entry on the OPPOSITE side.
        let flp = &mut ctx.accounts.flp_exposure;
        let flp_pre = flp_market_pre_state(flp, market_key);
        apply_fill_to_flp_market(flp, market_key, flp_side_enum, size_lots, price_ticks)?;
        let flp_post = flp_market_pre_state(flp, market_key);
        update_oi(market, flp_pre.0, flp_pre.1, flp_post.0, flp_post.1)?;

        // Update open_positions on TraderState.
        let taker_is_open = taker_post_size > 0;
        {
            let mut taker_state = ctx.accounts.taker_trader_state.load_mut()?;
            if !taker_was_open && taker_is_open {
                taker_state.open_positions = taker_state.open_positions.saturating_add(1);
            } else if taker_was_open && !taker_is_open {
                taker_state.open_positions = taker_state.open_positions.saturating_sub(1);
            }
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
            )?;
            if !fee_tier_pairs.is_empty() {
                let taker_post_volume = ctx.accounts.taker_trader_state.load()?.volume_30d_quote_lots;
                let post_taker_tier_index = tier_index_for_volume(
                    &fee_tier_pairs,
                    taker_post_volume,
                );
                if post_taker_tier_index != pre_taker_tier_index {
                    emit!(TraderTierUpgradedEvent {
                        trader: taker_trader_pk,
                        previous_tier_index: pre_taker_tier_index,
                        new_tier_index: post_taker_tier_index,
                        volume_quote_lots: taker_post_volume,
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
        // Snapshot the position (Copy) so reads don't hold a borrow that
        // would collide with the later `load_mut()` write-back.
        let position = *ctx.accounts.position.load()?;
        let (trader_state_pre_trader, trader_state_pre_collateral) = {
            let ts = ctx.accounts.trader_state.load()?;
            (ts.trader, ts.collateral_quote_lots)
        };

        require!(position.size_lots > 0, FlashBookError::LiquidationStale);
        require!(
            position.trader == trader_state_pre_trader,
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

        // ─── ORACLE FRESHNESS GATE ─────────────────────────────────────
        // The dual-source health check below picks the WORSE of mark and
        // oracle. That defense relies on the oracle actually being fresh.
        // If both mark and oracle are stale simultaneously (e.g. ER
        // undelegate before settle_mark resyncs the cached mark AND Pyth
        // publishing gap), the worse-of can pick a stale price and
        // wrongfully liquidate a trader who's healthy at the live price.
        // Refuse to liquidate when oracle is stale; let the trader keep
        // their position until the operator either resyncs mark via
        // settle_mark or the Pyth keeper posts a fresh price.
        let oracle_max_age = market.params.oracle_staleness_max_seconds as u64;
        if oracle_max_age > 0 && market.oracle_published_at_unix_seconds > 0 {
            let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            let oracle_age = now_unix.saturating_sub(market.oracle_published_at_unix_seconds);
            require!(oracle_age <= oracle_max_age, FlashBookError::OracleTooStale);
        }

        // V3 health gate — DUAL-SOURCE PRICE.
        // The current `mark_price_ticks` is the EMA-blended mark that
        // lags the live tape by 1-2 fills. The current `oracle_price_ticks`
        // is the freshly attested oracle (validated above for staleness).
        // Picking the worse of the two for the position's direction means
        // a fresh oracle move can immediately tip a position underwater
        // without waiting for an explicit `settle_mark` call.
        //
        //   LONG:  health_price = min(mark, oracle)   (lower = worse)
        //   SHORT: health_price = max(mark, oracle)   (higher = worse)
        //
        // No other on-chain DEX runs dual-source health checks — most
        // either trust oracle alone (Drift) or mark alone (Phoenix-style).
        // The smarter combo is BOTH and pick the adverse one.
        let pos_side = if position.side == 0 { Side::Long } else { Side::Short };
        let mark_t = market.mark_price_ticks;
        let oracle_t = market.oracle_price_ticks;
        let (health_price_ticks, hp_source) = match pos_side {
            Side::Long => {
                if oracle_t > 0 && oracle_t < mark_t {
                    (oracle_t, 1u8)
                } else if oracle_t > 0 && oracle_t == mark_t {
                    (mark_t, 2u8)
                } else {
                    (mark_t, 0u8)
                }
            }
            Side::Short => {
                if oracle_t > mark_t {
                    (oracle_t, 1u8)
                } else if oracle_t == mark_t {
                    (mark_t, 2u8)
                } else {
                    (mark_t, 0u8)
                }
            }
        };

        let pos_snap = RiskPosSnap {
            market: position.market,
            side: pos_side,
            size_lots: position.size_lots,
            entry_price: Ticks(position.entry_price_ticks),
            cum_funding_index_at_entry: position.cum_funding_index(),
            collateral_quote_lots: position.collateral_quote_lots,
        };
        let market_snap = RiskMarketSnap {
            market: market.key(),
            mark_price: Ticks(health_price_ticks),
            cum_funding_index: market.cum_funding_index,
            maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
            tick_size: market.params.tick_size,
            concentration_threshold_lots: market.params.concentration_threshold_lots,
            concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        };
        let scenarios = default_scenarios_fn(&[market.key()]);
        // Unified dispatch: if position is isolated (pos_snap.collateral_quote_lots > 0),
        // health is checked against the per-position bucket only; the cross
        // pool is irrelevant. Cross positions check against trader_state pool.
        let assessment = assess_margin_unified_fn(
            &[pos_snap],
            &[market_snap],
            &scenarios,
            trader_state_pre_collateral,
        )?;
        require!(!assessment.is_healthy, FlashBookError::NotLiquidatable);

        // Emit the source for transparency — keepers and UIs can show
        // "liquidated by oracle move" vs "liquidated by mark drift".
        emit!(HealthGateSourceEvent {
            market: market.key(),
            trader: trader_state_pre_trader,
            mark_ticks: mark_t,
            oracle_ticks: oracle_t,
            health_price_ticks,
            source: hp_source,
        });
        let close_side = pos_side.opposite();
        let penalty = market.params.liq_penalty_bps as u128;
        let oracle = market.oracle_price_ticks as u128;
        let penalty_delta = (oracle * penalty) / constants::BPS_DENOM as u128;
        let synthetic_limit_u128: u128 = match close_side {
            Side::Short => oracle.saturating_sub(penalty_delta),
            Side::Long => oracle.saturating_add(penalty_delta),
        };
        let synthetic_limit = synthetic_limit_u128 as u64;

        // ─── JIT LIQUIDATION AUCTION — walk pre-committed maker offers ───
        //
        // Each `JitLiquidationOfferAccount` in remaining_accounts that
        // matches (market, side, target_trader-or-default, remaining>0,
        // not-expired) is a candidate. We pick the BEST candidate — best
        // for the protocol/trader:
        //   close_side == Short (selling out a long): HIGHER price is BETTER
        //                       (trader sells higher → less loss, smaller
        //                        insurance draw, JIT maker pays a tighter
        //                        spread).
        //   close_side == Long  (buying back a short): LOWER price is BETTER
        //                       (trader buys back cheaper).
        //
        // The selected offer's price MUST beat the synthetic limit for the
        // trader's direction; otherwise fall back to the synthetic. This
        // guarantees JIT NEVER makes the trader's outcome worse.
        //
        // No other on-chain DEX has a permissionless JIT-liquidation
        // primitive — HL keeps liquidations private, Drift / dYdX route
        // through external keepers + insurance. JIT auctions let any
        // maker pre-commit a tighter price and earn the guaranteed fill.
        let market_key_for_jit = market.key();
        let mut jit_best_price: Option<u64> = None;
        let mut jit_best_idx: Option<usize> = None;
        let mut jit_best_remaining: u64 = 0;
        let mut jit_best_maker: Pubkey = Pubkey::default();
        for (idx, acct) in ctx.remaining_accounts.iter().enumerate() {
            // Owner gate — must be our program.
            if acct.owner != ctx.program_id {
                continue;
            }
            // Discriminator check.
            let data = match acct.try_borrow_data() {
                Ok(d) => d,
                Err(_) => continue,
            };
            if data.len() < 8 {
                continue;
            }
            let mut disc = [0u8; 8];
            disc.copy_from_slice(&data[..8]);
            let expected_disc =
                <state_v3::JitLiquidationOfferAccount as anchor_lang::Discriminator>::DISCRIMINATOR;
            if disc != expected_disc {
                continue;
            }
            drop(data);
            // Anchor-deserialize.
            let parsed: state_v3::JitLiquidationOfferAccount = {
                let data = match acct.try_borrow_data() {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let mut slice: &[u8] = &data[8..];
                match anchor_lang::AccountDeserialize::try_deserialize_unchecked(
                    &mut slice,
                ) {
                    Ok(v) => {
                        drop(data);
                        v
                    }
                    Err(_) => continue,
                }
            };
            if parsed.market != market_key_for_jit {
                continue;
            }
            // target_trader filter — match this trader or wildcard.
            if parsed.target_trader != Pubkey::default()
                && parsed.target_trader != trader_state_pre_trader
            {
                continue;
            }
            // side must equal close_side — offer.side==0 closes LONGs, side==1 closes SHORTs.
            // pos_side==Long → close_side==Short → expects offer.side==0 (close long).
            // pos_side==Short → close_side==Long → expects offer.side==1 (close short).
            let expected_offer_side = if pos_side == Side::Long { 0u8 } else { 1u8 };
            if parsed.side != expected_offer_side {
                continue;
            }
            if parsed.remaining_size_lots == 0 {
                continue;
            }
            if parsed.expires_at_slot != 0 && current_slot >= parsed.expires_at_slot {
                continue;
            }
            // Must beat the synthetic limit. Better for trader =
            //   close_side==Short: higher price
            //   close_side==Long : lower price
            let beats_synthetic = match close_side {
                Side::Short => parsed.offer_price_ticks > synthetic_limit,
                Side::Long => parsed.offer_price_ticks < synthetic_limit,
            };
            if !beats_synthetic {
                continue;
            }
            // Pick best so far.
            let better_than_best = match jit_best_price {
                None => true,
                Some(curr) => match close_side {
                    Side::Short => parsed.offer_price_ticks > curr,
                    Side::Long => parsed.offer_price_ticks < curr,
                },
            };
            if better_than_best {
                jit_best_price = Some(parsed.offer_price_ticks);
                jit_best_idx = Some(idx);
                jit_best_remaining = parsed.remaining_size_lots;
                jit_best_maker = parsed.maker;
            }
        }

        // Final close-limit price + JIT bookkeeping. If a JIT offer was
        // selected, decrement its remaining_size and use its price.
        let mut limit = synthetic_limit;
        let mut jit_filled: bool = false;
        let mut jit_fill_size: u64 = 0;
        let mut jit_fill_price: u64 = 0;
        let mut jit_maker_for_event: Pubkey = Pubkey::default();
        if let (Some(best_price), Some(best_idx)) = (jit_best_price, jit_best_idx) {
            // JIT fill size = min(close_size, JIT offer's remaining).
            let fill_size = close_size.min(jit_best_remaining);
            if fill_size > 0 {
                let acct = &ctx.remaining_accounts[best_idx];
                // Re-borrow, update remaining_size, serialize back.
                let mut data = acct.try_borrow_mut_data()?;
                let mut slice: &[u8] = &data[8..];
                let mut offer: state_v3::JitLiquidationOfferAccount =
                    anchor_lang::AccountDeserialize::try_deserialize_unchecked(
                        &mut slice,
                    )?;
                offer.remaining_size_lots = offer
                    .remaining_size_lots
                    .checked_sub(fill_size)
                    .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
                // Serialize back. Anchor account: 8-byte disc + body.
                let mut writer: &mut [u8] = &mut data[8..];
                anchor_lang::AccountSerialize::try_serialize(&offer, &mut writer)?;
                drop(data);

                limit = best_price;
                jit_filled = true;
                jit_fill_size = fill_size;
                jit_fill_price = best_price;
                jit_maker_for_event = jit_best_maker;
            }
        }

        // Lazy-init caller_trader_state (parity-port).
        {
            let caller_key = ctx.accounts.caller.key();
            let caller_bump = ctx.bumps.caller_trader_state;
            let mut cts = ctx.accounts.caller_trader_state.load_mut()?;
            if cts.trader == Pubkey::default() {
                cts.trader = caller_key;
                cts.bump = caller_bump;
            }
        }

        let market_key = market.key();
        // Hoist values out of the immutable `position` borrow before
        // the reward block, which needs a mutable borrow of the same
        // account on the isolated-margin path.
        let trader = position.trader;

        // Dutch-auction reward (parity-port from v1).
        let mut reward_paid: u64 = 0;
        if market.params.liquidator_reward_bps > 0 {
            let notional_u128 = (close_size as u128)
                .saturating_mul(oracle)
                .saturating_mul(market.params.tick_size as u128);
            let mut reward_bps_eff = market.params.liquidator_reward_bps as u128;
            let auction_duration =
                market.params.liquidation_auction_duration_slots as u64;
            if auction_duration > 0 && position.unhealthy_since_slot > 0 {
                let elapsed = current_slot
                    .saturating_sub(position.unhealthy_since_slot);
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

            // ── Phase 2 isolated-margin reward routing ───────────────────
            // For an isolated position, the liquidator reward comes from
            // the per-position bucket and NEVER the trader's cross pool.
            // Capped at the isolated bucket's balance; shortfall is
            // absorbed by the broader liquidation flow (the synthetic
            // close-order + insurance fund settle the remaining loss).
            // For a cross position, behavior is unchanged: reward debits
            // the pooled trader_state.collateral_quote_lots.
            let is_isolated = position.collateral_quote_lots > 0;
            if is_isolated {
                let mut pos = ctx.accounts.position.load_mut()?;
                reward_paid = reward_u64.min(pos.collateral_quote_lots);
                if reward_paid > 0 {
                    pos.collateral_quote_lots -= reward_paid;
                    let mut caller_ts = ctx.accounts.caller_trader_state.load_mut()?;
                    caller_ts.collateral_quote_lots = caller_ts
                        .collateral_quote_lots
                        .checked_add(reward_paid)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                }
            } else {
                reward_paid =
                    reward_u64.min(ctx.accounts.trader_state.load()?.collateral_quote_lots);
                if reward_paid > 0 {
                    ctx.accounts.trader_state.load_mut()?.collateral_quote_lots -= reward_paid;
                    let mut caller_ts = ctx.accounts.caller_trader_state.load_mut()?;
                    caller_ts.collateral_quote_lots = caller_ts
                        .collateral_quote_lots
                        .checked_add(reward_paid)
                        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
                }
            }
        }

        // V2 inject — into the hypertree, with order_type = Liquidation (3).
        // `trader` was hoisted above the reward block.
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
                last_valid_slot: u32::try_from(current_slot).unwrap_or(u32::MAX),
                side: close_side_u8,
                order_type: 3, // 3 = Liquidation (matcher promotes priority)
                flags: 0,
                // Phase 2f — carry the liquidatee's sub_index so ApplyFill
                // routes the close fill back to the same TraderState
                // that's being liquidated (main vs sub).
                sub_index: ctx.accounts.trader_state.load()?.sub_index,
            };
            inserted_idx = if side_is_bid {
                handle.insert_bid(order)?
            } else {
                handle.insert_ask(order)?
            };
        }

        {
            let mut position = ctx.accounts.position.load_mut()?;
            if position.unhealthy_since_slot == 0 {
                position.unhealthy_since_slot = current_slot;
            }
            position.last_liquidated_at_slot = current_slot;
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
        if reward_paid > 0 {
            emit!(LiquidatorRewardedEvent {
                market: market_key,
                liquidator: ctx.accounts.caller.key(),
                liquidatee: trader,
                reward_quote_lots: reward_paid,
            });
        }
        if jit_filled {
            // Savings expressed in bps of the oracle: how much tighter
            // the JIT close-price is vs the synthetic close-price for
            // the trader's direction. Positive = JIT was better.
            let oracle_u128 = market.oracle_price_ticks as u128;
            let savings_bps: u32 = if oracle_u128 == 0 {
                0
            } else {
                let diff = match close_side {
                    // Short close: better = higher price → diff = jit - synthetic.
                    Side::Short => (jit_fill_price as u128)
                        .saturating_sub(synthetic_limit as u128),
                    // Long close: better = lower price → diff = synthetic - jit.
                    Side::Long => (synthetic_limit as u128)
                        .saturating_sub(jit_fill_price as u128),
                };
                let bps = diff
                    .saturating_mul(constants::BPS_DENOM as u128)
                    / oracle_u128;
                if bps > u32::MAX as u128 {
                    u32::MAX
                } else {
                    bps as u32
                }
            };
            emit!(JitLiquidationFilledEvent {
                market: market_key,
                trader,
                jit_maker: jit_maker_for_event,
                fill_size_lots: jit_fill_size,
                fill_price_ticks: jit_fill_price,
                synthetic_price_ticks: synthetic_limit,
                savings_vs_synthetic_bps: savings_bps,
            });
        }
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
    ///   • underwater realizes loss (Phase 2h: routed to
    ///     position.collateral_quote_lots if isolated, else
    ///     trader_state.collateral_quote_lots; saturating at 0 in both
    ///     cases — the insurance fund waterfall absorbs the remainder)
    ///   • counter realizes profit at bp (Phase 2h: credited to
    ///     position.collateral_quote_lots if isolated, else
    ///     trader_state.collateral_quote_lots; checked_add for
    ///     overflow safety)
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
        // Snapshot both positions (Copy) so reads don't hold borrows that
        // collide with the later `load_mut()` write-backs. These are
        // DIFFERENT accounts, but the snapshot keeps each read clean.
        let underwater = *ctx.accounts.underwater_position.load()?;
        let counter = *ctx.accounts.counter_position.load()?;

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
            ctx.accounts.underwater_trader_state.load()?.trader == underwater.trader,
            FlashBookError::WrongTrader
        );
        require!(
            ctx.accounts.counter_trader_state.load()?.trader == counter.trader,
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
            cum_funding_index_at_entry: underwater.cum_funding_index(),
            collateral_quote_lots: underwater.collateral_quote_lots,
        };
        let market_snap = RiskMarketSnap {
            market: market.key(),
            mark_price: Ticks(market.mark_price_ticks),
            cum_funding_index: market.cum_funding_index,
            maintenance_margin_bps: market.params.maintenance_margin_ratio_bps,
            tick_size: market.params.tick_size,
                concentration_threshold_lots: market.params.concentration_threshold_lots,
                concentration_extra_mmr_bps: market.params.concentration_extra_mmr_bps,
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        };
        let scenarios = default_scenarios_fn(&[market.key()]);
        let assessment = assess_margin_unified_fn(
            &[pos_snap],
            &[market_snap],
            &scenarios,
            ctx.accounts.underwater_trader_state.load()?.collateral_quote_lots,
        )?;
        require!(!assessment.is_healthy, FlashBookError::NotLiquidatable);

        // Compute bankruptcy price: the mark at which underwater equity
        // is exactly zero. For long: bp = entry - C / (S × tick).
        //                  short: bp = entry + C / (S × tick).
        // We compute in u128 ticks to avoid overflow at large notional.
        //
        // ── Phase 2h ADL routing for isolated positions ───────────────
        // `C` (the collateral backing this position) is the per-position
        // bucket if the position is isolated, else the trader's cross
        // pool. Using the cross pool for an isolated position would
        // over-estimate the available backstop and produce a too-favorable
        // bp — the underwater trader would absorb more loss than their
        // isolated bucket has, and the counter-trader would be force-
        // closed at a price worse than they should be. The whole point
        // of isolation is that THIS bucket is what backs this position.
        let tick_size = market.params.tick_size as u128;
        require!(tick_size > 0, FlashBookError::ZeroPrice);
        let underwater_isolated = underwater.collateral_quote_lots > 0;
        let collateral: u128 = if underwater_isolated {
            underwater.collateral_quote_lots as u128
        } else {
            ctx.accounts.underwater_trader_state.load()?.collateral_quote_lots as u128
        };
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
        // Sample the counter's isolation flag BEFORE mutating, mirroring
        // the apply_fill / settle_funding routing pattern.
        let counter_isolated = counter.collateral_quote_lots > 0;
        // Hoist the size/side snapshots from the immutable `underwater`
        // and `counter` borrows so the mutable per-position routing
        // below (which needs `&mut ctx.accounts.{underwater,counter}_position`)
        // doesn't conflict via NLL.
        let uw_was_open = underwater.size_lots > 0;
        let ct_was_open = counter.size_lots > 0;
        let uw_pre_side = underwater.side;
        let ct_pre_side = counter.side;
        let uw_pre_size = underwater.size_lots;
        let ct_pre_size = counter.size_lots;

        // ── Phase 2h: route the underwater loss ──────────────────────
        // Isolated underwater → debit the per-position bucket (saturate
        // to 0; the remainder is absorbed by the insurance fund via the
        // existing waterfall — the cross pool is never touched, which
        // preserves invariant I-3).
        // Cross underwater → debit the cross pool as before.
        // Either way, `uts.realized_pnl_quote_lots` is tracked for
        // indexers regardless of which bucket actually moved.
        {
            let (new_pos_collat, new_ts_collat) = route_adl_loss(
                underwater_isolated,
                loss_quote_lots,
                underwater.collateral_quote_lots,
                ctx.accounts.underwater_trader_state.load()?.collateral_quote_lots,
            );
            ctx.accounts.underwater_position.load_mut()?.collateral_quote_lots = new_pos_collat;
            ctx.accounts.underwater_trader_state.load_mut()?.collateral_quote_lots = new_ts_collat;
        }
        {
            let mut uts = ctx.accounts.underwater_trader_state.load_mut()?;
            uts.realized_pnl_quote_lots = uts
                .realized_pnl_quote_lots
                .saturating_sub(loss_quote_lots as i64);
        }

        // ── Phase 2h: route the counter gain ─────────────────────────
        // Isolated counter → credit the per-position bucket.
        // Cross counter    → credit the cross pool (legacy path).
        {
            let (new_pos_collat, new_ts_collat) = route_adl_gain(
                counter_isolated,
                counter_gain,
                counter.collateral_quote_lots,
                ctx.accounts.counter_trader_state.load()?.collateral_quote_lots,
            )?;
            ctx.accounts.counter_position.load_mut()?.collateral_quote_lots = new_pos_collat;
            ctx.accounts.counter_trader_state.load_mut()?.collateral_quote_lots = new_ts_collat;
        }
        {
            let mut cts = ctx.accounts.counter_trader_state.load_mut()?;
            cts.realized_pnl_quote_lots = cts
                .realized_pnl_quote_lots
                .saturating_add(counter_gain as i64);
        }

        // Reduce both positions by close_size. If a side closes to zero,
        // decrement open_positions on that trader_state. (Snapshot
        // values were hoisted above the settlement block.)
        let (uw_post_side, uw_post_size) = {
            let mut uw = ctx.accounts.underwater_position.load_mut()?;
            uw.size_lots = uw.size_lots.saturating_sub(close_size_lots);
            if uw.size_lots == 0 {
                // Reset settlement anchors on close.
                uw.entry_price_ticks = 0;
                uw.unhealthy_since_slot = 0;
                uw.last_liquidated_at_slot = 0;
            }
            (uw.side, uw.size_lots)
        };
        let (ct_post_side, ct_post_size) = {
            let mut ct = ctx.accounts.counter_position.load_mut()?;
            ct.size_lots = ct.size_lots.saturating_sub(close_size_lots);
            if ct.size_lots == 0 {
                ct.entry_price_ticks = 0;
            }
            (ct.side, ct.size_lots)
        };

        // OI updates: walk pre→post for each side.
        let market = &mut ctx.accounts.market;
        update_oi(market, uw_pre_side, uw_pre_size, uw_post_side, uw_post_size)?;
        update_oi(market, ct_pre_side, ct_pre_size, ct_post_side, ct_post_size)?;

        // open_positions transitions on TraderStates.
        if uw_was_open && uw_post_size == 0 {
            let mut uts = ctx.accounts.underwater_trader_state.load_mut()?;
            uts.open_positions = uts.open_positions.saturating_sub(1);
        }
        if ct_was_open && ct_post_size == 0 {
            let mut cts = ctx.accounts.counter_trader_state.load_mut()?;
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
        let exec_position = *ctx.accounts.execution_position.load()?;
        let trader_state = ctx.accounts.trader_state.load()?;

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
            side_oi_lots: 0,
            oi_mmr_slope_bps_per_million_lots: 0,
            oi_mmr_max_extra_bps: 0,
        });
        position_snaps.push(RiskPosSnap {
            market: exec_position.market,
            side: if exec_position.side == 0 { Side::Long } else { Side::Short },
            size_lots: exec_position.size_lots,
            entry_price: Ticks(exec_position.entry_price_ticks),
            cum_funding_index_at_entry: exec_position.cum_funding_index(),
            collateral_quote_lots: exec_position.collateral_quote_lots,
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
                    side_oi_lots: 0,
                    oi_mmr_slope_bps_per_million_lots: 0,
                    oi_mmr_max_extra_bps: 0,
                });
                position_snaps.push(RiskPosSnap {
                    market: position.market,
                    side: if position.side == 0 { Side::Long } else { Side::Short },
                    size_lots: position.size_lots,
                    entry_price: Ticks(position.entry_price_ticks),
                    cum_funding_index_at_entry: position.cum_funding_index(),
                    collateral_quote_lots: position.collateral_quote_lots,
                });
            }
            i += 2;
        }

        let market_keys: Vec<Pubkey> = market_snaps.iter().map(|m| m.market).collect();
        let scenarios = default_scenarios_fn(&market_keys);
        let assessment = assess_margin_unified_fn(
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
                last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
                side: close_side_u8,
                order_type: 3, // Liquidation
                flags: 0,
                // Phase 2f — propagate the liquidatee's sub_index.
                sub_index: trader_state.sub_index,
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


    // ER delegation lifecycle ixs (delegate_market_book, undelegate_market_book,
    // delegate_market, undelegate_market) ship in waves 19b + 19g via
    // in-house CPI wrappers in `src/er.rs`. flp_exposure is intentionally
    // NOT ER-delegatable: it's a singleton, so delegating it would
    // bottleneck ALL markets to a single ER instance. Per-market FLP
    // exposure is queued for wave 21 (modular wrapper programs).
    //
    // Historical note: in-house CPI sidesteps the upstream
    // `ephemeral-rollups-sdk` Solana 2.x compat issue. When upstream lands
    // 2.1-compatible release we can swap to it drop-in via the Delegate /
    // Undelegate ix builders in `src/er.rs`.

    // ─── V3 monolithic ixs (merged from former wrapper programs) ─────
    //
    // The 3 wrapper programs (`flash-book-orders`, `flash-book-flp`,
    // `flash-book-vaults`) have been collapsed into core. All V3 PDAs
    // (trigger_v3, twap_v3, iceberg_v3, vault_v3, vault_position_v3,
    // flp_per_market, flp_position_v3) now live under THIS program ID.
    // The wrapper CPI authority + 3-program whitelist are gone — these
    // ixs do their work directly without invoke_signed across program
    // boundaries.

    // ─── Trigger orders v3 ──────────────────────────────────────────

    /// Create a v3 trigger order PDA. Same trigger semantics as
    /// `execute_trigger_order_v2` (validates side / kind / size / price /
    /// tick alignment / expiry).
    /// PDA seeds: `[b"trigger_v3", market, trader, trigger_id]`.
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
        sub_index: u8,
        // Wave 27a — slippage cap. 0 = no cap (legacy behavior).
        // For side==0 (long buying): cap = MAX admissible oracle price
        //   at execution time. Trigger refuses to fire if oracle > cap.
        // For side==1 (short selling): cap = MIN admissible oracle.
        acceptable_price_ticks: u64,
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(kind <= 1, FlashBookError::OutOfRange);
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(trigger_price_ticks > 0, FlashBookError::ZeroPrice);
        require!(limit_price_ticks > 0, FlashBookError::ZeroPrice);

        let market = &ctx.accounts.market;
        require!(
            size_lots >= market.params.min_base_lots,
            FlashBookError::SizeBelowMinLot
        );
        require!(
            limit_price_ticks % market.params.tick_size == 0,
            FlashBookError::PriceNotOnTick
        );
        require!(
            trigger_price_ticks % market.params.tick_size == 0,
            FlashBookError::PriceNotOnTick
        );

        // Wave 27a: validate acceptable_price tick alignment (when set).
        if acceptable_price_ticks > 0 {
            require!(
                acceptable_price_ticks % market.params.tick_size == 0,
                FlashBookError::PriceNotOnTick
            );
            // Sanity: cap must sit on the "worse" side of the trigger
            // price. A long-buy cap below the trigger would always
            // breach (trigger fired ≥ trigger_price ≥ cap), making the
            // trigger un-fireable. Catch the misuse at place time.
            match side {
                0 => require!(
                    acceptable_price_ticks >= trigger_price_ticks,
                    FlashBookError::OutOfRange
                ),
                1 => require!(
                    acceptable_price_ticks <= trigger_price_ticks,
                    FlashBookError::OutOfRange
                ),
                _ => unreachable!(),
            }
        }

        let now = Clock::get()?.slot;
        if expires_at_slot > 0 {
            require!(expires_at_slot > now, FlashBookError::OutOfRange);
        }

        let trigger = &mut ctx.accounts.trigger_order;
        trigger.trader = ctx.accounts.trader.key();
        trigger.market = market.key();
        trigger.bump = ctx.bumps.trigger_order;
        trigger.trigger_id = trigger_id;
        trigger.side = side;
        trigger.kind = kind;
        trigger.flags = state_v3::TriggerOrderAccountV3::FLAG_ACTIVE
            | if reduce_only { state_v3::TriggerOrderAccountV3::FLAG_REDUCE_ONLY } else { 0 };
        trigger.size_lots = size_lots;
        trigger.trigger_price_ticks = trigger_price_ticks;
        trigger.limit_price_ticks = limit_price_ticks;
        trigger.created_at_slot = now;
        trigger.expires_at_slot = expires_at_slot;
        trigger.sub_index = sub_index;
        trigger.acceptable_price_ticks = acceptable_price_ticks;
        trigger._reserved = [0; 8];

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

    /// Execute a v3 trigger — validates fire condition then injects the
    /// resulting limit order into the hypertree directly (no CPI). The
    /// caller is permissionless; trader was pre-authorized at placement.
    pub fn execute_trigger_order_v3(ctx: Context<ExecuteTriggerOrderV3>) -> Result<()> {
        let trigger = &ctx.accounts.trigger_order;
        let market = &ctx.accounts.market;
        require!(
            trigger.flags & state_v3::TriggerOrderAccountV3::FLAG_ACTIVE != 0,
            FlashBookError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if trigger.expires_at_slot > 0 {
            require!(trigger.expires_at_slot >= now, FlashBookError::OutOfRange);
        }

        // AUDIT FIX (Wave 27c) — Oracle staleness gate on v3 trigger
        // execution. Same rationale as v2 path.
        let oracle_max_age = market.params.oracle_staleness_max_seconds as u64;
        if oracle_max_age > 0 && market.oracle_published_at_unix_seconds > 0 {
            let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            let oracle_age = now_unix.saturating_sub(market.oracle_published_at_unix_seconds);
            require!(oracle_age <= oracle_max_age, FlashBookError::OracleTooStale);
        }

        let oracle = market.oracle_price_ticks;
        let fired = if trigger.kind == 0 {
            oracle <= trigger.trigger_price_ticks
        } else {
            oracle >= trigger.trigger_price_ticks
        };
        require!(fired, FlashBookError::OutOfRange);

        // Wave 27a — slippage cap check. If oracle has gapped past the
        // acceptable price, the trigger deactivates (so it doesn't
        // re-fire at this gapped value) and emits the cancellation
        // event. Trader can re-place if they still want to act.
        if state_v3::TriggerOrderAccountV3::slippage_cap_breached(
            trigger.acceptable_price_ticks,
            trigger.side,
            oracle,
        ) {
            let market_key = market.key();
            let trader_pk = trigger.trader;
            let trigger_id = trigger.trigger_id;
            let acceptable_price = trigger.acceptable_price_ticks;
            let trigger_mut = &mut ctx.accounts.trigger_order;
            trigger_mut.flags &= !state_v3::TriggerOrderAccountV3::FLAG_ACTIVE;
            emit!(TriggerOrderV3SlippageCancelledEvent {
                market: market_key,
                trader: trader_pk,
                trigger_id,
                oracle_price_ticks: oracle,
                acceptable_price_ticks: acceptable_price,
            });
            return Err(error!(FlashBookError::TriggerSlippageExceeded));
        }

        if trigger.flags & state_v3::TriggerOrderAccountV3::FLAG_REDUCE_ONLY != 0 {
            let position = ctx.accounts.position.load()?;
            require!(position.size_lots > 0, FlashBookError::OutOfRange);
            require!(position.side != trigger.side, FlashBookError::OutOfRange);
            require!(
                trigger.size_lots <= position.size_lots,
                FlashBookError::OutOfRange
            );
        }

        let side = trigger.side;
        let size_lots = trigger.size_lots;
        let limit_ticks = trigger.limit_price_ticks;
        let trigger_id = trigger.trigger_id;
        let trader_pk = trigger.trader;
        let trigger_sub_index = trigger.sub_index;
        let market_key = market.key();

        // Inline order injection into the hypertree.
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
        require!(seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
        handle.header.order_seq_counter = seq;

        let side_is_bid = side == 0;
        let order = state_v2::RestingOrderV2 {
            order_id: state_v2::encode_order_id(limit_ticks, seq, side_is_bid),
            seq,
            price_ticks: limit_ticks,
            size_lots,
            expires_at_slot: 0,
            trader: trader_pk,
            last_valid_slot: now as u32,
            side,
            order_type: 0,
            flags: 0,
            // Phase 2f — propagate the trigger's sub_index into the
            // synthetic order so fills route to the right TraderState.
            sub_index: trigger_sub_index,
        };
        let inserted_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };
        drop(book_data);

        let trigger = &mut ctx.accounts.trigger_order;
        trigger.flags &= !state_v3::TriggerOrderAccountV3::FLAG_ACTIVE;

        emit!(TriggerOrderV3ExecutedEvent {
            market: market_key,
            trader: trader_pk,
            trigger_id,
            executor: ctx.accounts.caller.key(),
            oracle_price_ticks: oracle,
            order_seq: seq,
            node_index: inserted_idx,
        });
        Ok(())
    }

    /// Cancel a v3 trigger and close the account, refunding rent.
    pub fn cancel_trigger_order_v3(ctx: Context<CancelTriggerOrderV3>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.trigger_order.trader == trader,
            FlashBookError::WrongTrader
        );
        emit!(TriggerOrderV3CancelledEvent {
            market: ctx.accounts.trigger_order.market,
            trader,
            trigger_id: ctx.accounts.trigger_order.trigger_id,
        });
        Ok(())
    }

    // ─── TWAP orders v3 ─────────────────────────────────────────────

    /// Create a v3 TWAP order PDA.
    pub fn place_twap_order_v3(
        ctx: Context<PlaceTwapOrderV3>,
        twap_id: u8,
        side: u8,
        slice_size_lots: u64,
        total_size_lots: u64,
        limit_price_ticks: u64,
        slot_interval: u64,
        end_slot: u64,
        sub_index: u8,
        // Wave 27b — slippage cap, same semantics as
        // TriggerOrderAccountV3.acceptable_price_ticks. 0 = no cap.
        acceptable_price_ticks: u64,
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(slice_size_lots > 0, FlashBookError::ZeroSize);
        require!(total_size_lots >= slice_size_lots, FlashBookError::OutOfRange);
        require!(limit_price_ticks > 0, FlashBookError::ZeroPrice);
        require!(slot_interval > 0, FlashBookError::OutOfRange);

        let market = &ctx.accounts.market;
        require!(
            limit_price_ticks % market.params.tick_size == 0,
            FlashBookError::PriceNotOnTick
        );
        require!(
            slice_size_lots >= market.params.min_base_lots,
            FlashBookError::SizeBelowMinLot
        );

        // Wave 27b — cap tick alignment + direction sanity.
        if acceptable_price_ticks > 0 {
            require!(
                acceptable_price_ticks % market.params.tick_size == 0,
                FlashBookError::PriceNotOnTick
            );
            // For a long buy, cap must be ≥ limit_price. For a short
            // sell, cap must be ≤ limit_price. Otherwise the cap is
            // wrong-sided and would always reject — catch at place.
            match side {
                0 => require!(
                    acceptable_price_ticks >= limit_price_ticks,
                    FlashBookError::OutOfRange
                ),
                1 => require!(
                    acceptable_price_ticks <= limit_price_ticks,
                    FlashBookError::OutOfRange
                ),
                _ => unreachable!(),
            }
        }

        let now = Clock::get()?.slot;
        if end_slot > 0 {
            require!(end_slot > now, FlashBookError::OutOfRange);
        }

        let twap = &mut ctx.accounts.twap_order;
        twap.trader = ctx.accounts.trader.key();
        twap.market = market.key();
        twap.bump = ctx.bumps.twap_order;
        twap.twap_id = twap_id;
        twap.side = side;
        twap.flags = state_v3::TwapOrderAccountV3::FLAG_ACTIVE;
        twap.slice_size_lots = slice_size_lots;
        twap.total_size_lots = total_size_lots;
        twap.size_executed_lots = 0;
        twap.limit_price_ticks = limit_price_ticks;
        twap.start_slot = now;
        twap.slot_interval = slot_interval;
        twap.end_slot = end_slot;
        twap.last_slice_at_slot = 0;
        twap.sub_index = sub_index;
        twap.acceptable_price_ticks = acceptable_price_ticks;
        twap._reserved = [0; 7];

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

    /// Permissionless TWAP slice executor.
    pub fn execute_twap_slice_v3(ctx: Context<ExecuteTwapSliceV3>) -> Result<()> {
        let twap = &ctx.accounts.twap_order;
        let market = &ctx.accounts.market;
        require!(
            twap.flags & state_v3::TwapOrderAccountV3::FLAG_ACTIVE != 0,
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
            .ok_or_else(|| error!(FlashBookError::OutOfRange))?;
        require!(remaining > 0, FlashBookError::OutOfRange);
        let slice_size = core::cmp::min(twap.slice_size_lots, remaining);
        require!(
            slice_size >= market.params.min_base_lots || slice_size == remaining,
            FlashBookError::SizeBelowMinLot
        );

        // Wave 27b — slippage cap on the slice. If oracle has moved
        // past the cap, abort this slice (but keep TWAP active for
        // next interval). Different from triggers, which deactivate
        // on breach — TWAPs are durable schedulers.
        //
        // AUDIT FIX (Wave 27c) — Only consult oracle when it's fresh.
        // Stale oracle could bypass or trigger the slippage cap based
        // on stale data; either is wrong. When stale, skip the slice
        // entirely (TWAP stays active for next interval).
        let oracle_max_age_twap = market.params.oracle_staleness_max_seconds as u64;
        if oracle_max_age_twap > 0 && market.oracle_published_at_unix_seconds > 0 {
            let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            let oracle_age = now_unix.saturating_sub(market.oracle_published_at_unix_seconds);
            require!(oracle_age <= oracle_max_age_twap, FlashBookError::OracleTooStale);
        }
        let oracle = market.oracle_price_ticks;
        if state_v3::TriggerOrderAccountV3::slippage_cap_breached(
            twap.acceptable_price_ticks,
            twap.side,
            oracle,
        ) {
            emit!(TwapSliceV3SlippageSkippedEvent {
                market: market.key(),
                trader: twap.trader,
                twap_id: twap.twap_id,
                oracle_price_ticks: oracle,
                acceptable_price_ticks: twap.acceptable_price_ticks,
            });
            return Err(error!(FlashBookError::TriggerSlippageExceeded));
        }

        let side = twap.side;
        let limit = twap.limit_price_ticks;
        let twap_id = twap.twap_id;
        let trader_pk = twap.trader;
        let twap_sub_index = twap.sub_index;
        let market_key = market.key();

        // Inline order injection.
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
        require!(seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
        handle.header.order_seq_counter = seq;

        let side_is_bid = side == 0;
        let order = state_v2::RestingOrderV2 {
            order_id: state_v2::encode_order_id(limit, seq, side_is_bid),
            seq,
            price_ticks: limit,
            size_lots: slice_size,
            expires_at_slot: 0,
            trader: trader_pk,
            last_valid_slot: now as u32,
            side,
            order_type: 0,
            flags: 0,
            // Phase 2f — each slice carries the parent TWAP's sub_index.
            sub_index: twap_sub_index,
        };
        let inserted_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };
        drop(book_data);

        let twap = &mut ctx.accounts.twap_order;
        twap.size_executed_lots = twap
            .size_executed_lots
            .checked_add(slice_size)
            .ok_or_else(|| error!(FlashBookError::OutOfRange))?;
        twap.last_slice_at_slot = now;
        if twap.size_executed_lots >= twap.total_size_lots {
            twap.flags &= !state_v3::TwapOrderAccountV3::FLAG_ACTIVE;
        }

        emit!(TwapSliceV3ExecutedEvent {
            market: market_key,
            trader: trader_pk,
            twap_id,
            executor: ctx.accounts.caller.key(),
            slice_size_lots: slice_size,
            cumulative_executed_lots: twap.size_executed_lots,
            order_seq: seq,
            node_index: inserted_idx,
        });
        Ok(())
    }

    /// Cancel a v3 TWAP order and close the account.
    pub fn cancel_twap_order_v3(ctx: Context<CancelTwapOrderV3>) -> Result<()> {
        let trader = ctx.accounts.trader.key();
        require!(
            ctx.accounts.twap_order.trader == trader,
            FlashBookError::WrongTrader
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

    // ─── Iceberg orders v3 ──────────────────────────────────────────

    /// Create a v3 iceberg + seed first visible chunk directly into the
    /// hypertree.
    pub fn place_iceberg_order_v3(
        ctx: Context<PlaceIcebergOrderV3>,
        iceberg_id: u8,
        side: u8,
        total_size_lots: u64,
        displayed_size_lots: u64,
        limit_ticks: u64,
        expires_at_slot: u64,
        sub_index: u8,
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(total_size_lots > 0, FlashBookError::ZeroSize);
        require!(displayed_size_lots > 0, FlashBookError::ZeroSize);
        require!(
            displayed_size_lots <= total_size_lots,
            FlashBookError::OutOfRange
        );
        require!(limit_ticks > 0, FlashBookError::ZeroPrice);

        let market = &ctx.accounts.market;
        require!(
            displayed_size_lots >= market.params.min_base_lots,
            FlashBookError::SizeBelowMinLot
        );
        require!(
            limit_ticks % market.params.tick_size == 0,
            FlashBookError::PriceNotOnTick
        );

        let now = Clock::get()?.slot;
        if expires_at_slot > 0 {
            require!(expires_at_slot > now, FlashBookError::OutOfRange);
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();
        let first_chunk = displayed_size_lots.min(total_size_lots);

        // Inline first-chunk insertion.
        let inserted_seq;
        {
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
            require!(seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
            handle.header.order_seq_counter = seq;

            let side_is_bid = side == 0;
            let order = state_v2::RestingOrderV2 {
                order_id: state_v2::encode_order_id(limit_ticks, seq, side_is_bid),
                seq,
                price_ticks: limit_ticks,
                size_lots: first_chunk,
                expires_at_slot,
                trader: trader_pk,
                last_valid_slot: now as u32,
                side,
                order_type: 0,
                flags: 0,
                sub_index,
            };
            if side_is_bid {
                handle.insert_bid(order)?;
            } else {
                handle.insert_ask(order)?;
            }
            inserted_seq = seq;
        }

        let iceberg = &mut ctx.accounts.iceberg_order;
        iceberg.trader = trader_pk;
        iceberg.market = market_key;
        iceberg.bump = ctx.bumps.iceberg_order;
        iceberg.iceberg_id = iceberg_id;
        iceberg.side = side;
        iceberg.flags = state_v3::IcebergOrderAccountV3::FLAG_ACTIVE;
        iceberg.sub_index = sub_index;
        iceberg._pad0 = [0u8; 3];
        iceberg.limit_ticks = limit_ticks;
        iceberg.total_size_lots = total_size_lots;
        iceberg.remaining_lots = total_size_lots.saturating_sub(first_chunk);
        iceberg.displayed_size_lots = displayed_size_lots;
        iceberg.child_order_seq = inserted_seq;
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
    pub fn replenish_iceberg_v3(ctx: Context<ReplenishIcebergV3>) -> Result<()> {
        let iceberg = &ctx.accounts.iceberg_order;
        let market = &ctx.accounts.market;
        require!(
            iceberg.flags & state_v3::IcebergOrderAccountV3::FLAG_ACTIVE != 0,
            FlashBookError::OutOfRange
        );

        let now = Clock::get()?.slot;
        if iceberg.expires_at_slot > 0 {
            require!(iceberg.expires_at_slot >= now, FlashBookError::OutOfRange);
        }
        require!(iceberg.remaining_lots > 0, FlashBookError::OutOfRange);

        let chunk = iceberg.displayed_size_lots.min(iceberg.remaining_lots);
        let side = iceberg.side;
        let limit = iceberg.limit_ticks;
        let expires = iceberg.expires_at_slot;
        let iceberg_id = iceberg.iceberg_id;
        let trader_pk = iceberg.trader;
        let iceberg_sub_index = iceberg.sub_index;
        let market_key = market.key();

        let inserted_seq;
        {
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
            require!(seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
            handle.header.order_seq_counter = seq;

            let side_is_bid = side == 0;
            let order = state_v2::RestingOrderV2 {
                order_id: state_v2::encode_order_id(limit, seq, side_is_bid),
                seq,
                price_ticks: limit,
                size_lots: chunk,
                expires_at_slot: expires,
                trader: trader_pk,
                last_valid_slot: now as u32,
                side,
                order_type: 0,
                flags: 0,
                // Phase 2f — replenished chunk inherits the iceberg's
                // sub_index so children route to the same TraderState.
                sub_index: iceberg_sub_index,
            };
            if side_is_bid {
                handle.insert_bid(order)?;
            } else {
                handle.insert_ask(order)?;
            }
            inserted_seq = seq;
        }

        let iceberg = &mut ctx.accounts.iceberg_order;
        iceberg.remaining_lots = iceberg.remaining_lots.saturating_sub(chunk);
        iceberg.child_order_seq = inserted_seq;
        if iceberg.remaining_lots == 0 {
            iceberg.flags &= !state_v3::IcebergOrderAccountV3::FLAG_ACTIVE;
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
            FlashBookError::WrongTrader
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

    // ─── Bracket orders v3 ──────────────────────────────────────────

    /// Atomic bracket: parent limit order + 2 OCO-linked TP/SL triggers.
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
        sub_index: u8,
    ) -> Result<()> {
        require!(parent_side <= 1, FlashBookError::OutOfRange);
        require!(tp_trigger_id != sl_trigger_id, FlashBookError::OutOfRange);
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(parent_limit_ticks > 0, FlashBookError::ZeroPrice);
        require!(tp_trigger_price_ticks > 0, FlashBookError::ZeroPrice);
        require!(sl_trigger_price_ticks > 0, FlashBookError::ZeroPrice);
        require!(tp_limit_ticks > 0, FlashBookError::ZeroPrice);
        require!(sl_limit_ticks > 0, FlashBookError::ZeroPrice);

        let market = &ctx.accounts.market;
        require!(
            size_lots >= market.params.min_base_lots,
            FlashBookError::SizeBelowMinLot
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
                FlashBookError::PriceNotOnTick
            );
        }

        let now = Clock::get()?.slot;
        if expires_at_slot > 0 {
            require!(expires_at_slot > now, FlashBookError::OutOfRange);
        }

        if parent_side == 0 {
            require!(
                tp_trigger_price_ticks > parent_limit_ticks,
                FlashBookError::OutOfRange
            );
            require!(
                sl_trigger_price_ticks < parent_limit_ticks,
                FlashBookError::OutOfRange
            );
        } else {
            require!(
                tp_trigger_price_ticks < parent_limit_ticks,
                FlashBookError::OutOfRange
            );
            require!(
                sl_trigger_price_ticks > parent_limit_ticks,
                FlashBookError::OutOfRange
            );
        }

        let trader_pk = ctx.accounts.trader.key();
        let market_key = market.key();

        // 1. Parent limit order injected directly.
        {
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
            require!(seq < FLP_SEQ_RESERVED_OFFSET, FlashBookError::OutOfRange);
            handle.header.order_seq_counter = seq;

            let side_is_bid = parent_side == 0;
            let order = state_v2::RestingOrderV2 {
                order_id: state_v2::encode_order_id(parent_limit_ticks, seq, side_is_bid),
                seq,
                price_ticks: parent_limit_ticks,
                size_lots,
                expires_at_slot: 0,
                trader: trader_pk,
                last_valid_slot: now as u32,
                side: parent_side,
                order_type: 0,
                flags: 0,
                // Phase 2f — parent + both children carry the same sub_index.
                sub_index,
            };
            if side_is_bid {
                handle.insert_bid(order)?;
            } else {
                handle.insert_ask(order)?;
            }
        }

        // 2. Wire the two reduce-only triggers.
        let close_side = 1 - parent_side;
        let (tp_kind, sl_kind) = if parent_side == 0 {
            (1u8, 0u8)
        } else {
            (0u8, 1u8)
        };
        let common_flags = state_v3::TriggerOrderAccountV3::FLAG_ACTIVE
            | state_v3::TriggerOrderAccountV3::FLAG_REDUCE_ONLY;

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
        tp.sub_index = sub_index;

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
        sl.sub_index = sub_index;

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

    // ─── Vaults v3 ──────────────────────────────────────────────────

    /// Strategist creates a new vault. Vault id is per-strategist (0..255).
    pub fn create_vault_v3(
        ctx: Context<CreateVaultV3>,
        vault_id: u8,
        name: [u8; 32],
        perf_fee_bps: u32,
    ) -> Result<()> {
        require!(perf_fee_bps <= 5_000, FlashBookError::OutOfRange);

        let v = &mut ctx.accounts.vault;
        v.strategist = ctx.accounts.strategist.key();
        v.bump = ctx.bumps.vault;
        v.vault_id = vault_id;
        v.accept_deposits = 1;
        v._pad0 = 0;
        v.name = name;
        v.perf_fee_bps = perf_fee_bps;
        v.shares_outstanding = 0;
        v.total_capital_quote_lots = 0;
        v.hwm_nav_per_share_u64x6 = 0;
        v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
        v.total_perf_shares_minted = 0;

        emit!(VaultV3CreatedEvent {
            vault: v.key(),
            strategist: v.strategist,
            vault_id,
            perf_fee_bps,
        });
        Ok(())
    }

    /// Bootstrap a `TraderStateAccount` for the vault PDA. Strategist
    /// signs + pays rent. One-time setup before the first deposit.
    pub fn vault_open_trader_state_v3(
        ctx: Context<VaultOpenTraderStateV3>,
    ) -> Result<()> {
        require!(
            ctx.accounts.strategist.key() == ctx.accounts.vault.strategist,
            FlashBookError::Unauthorized
        );

        let vault_key = ctx.accounts.vault.key();
        let bump = ctx.bumps.vault_trader_state;
        let slot = Clock::get()?.slot;
        {
            let mut s = ctx.accounts.vault_trader_state.load_init()?;
            s.trader = vault_key;
            s.bump = bump;
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
            s.volume_window_start_slot = slot;
        }

        emit!(VaultTraderStateOpenedV3Event {
            vault: vault_key,
            vault_trader_state: ctx.accounts.vault_trader_state.key(),
        });
        Ok(())
    }

    /// Depositor adds capital, mints shares pro-rata. SPL transfer
    /// depositor ATA → protocol quote_vault; vault TraderState credited.
    pub fn vault_deposit_v3(
        ctx: Context<VaultDepositV3>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);
        require!(
            ctx.accounts.vault.accept_deposits == 1,
            FlashBookError::OutOfRange
        );

        // Pull tokens from depositor → quote_vault. Depositor signs as ATA owner.
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

        // Credit vault's TraderState collateral.
        let post_deposit_collateral = {
            let mut ts = ctx.accounts.vault_trader_state.load_mut()?;
            ts.collateral_quote_lots = ts
                .collateral_quote_lots
                .checked_add(amount_quote_lots)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            ts.collateral_quote_lots
        };

        // Share-mint math: NAV-from-collateral (pre-deposit) for accuracy.
        let pre_deposit_nav = post_deposit_collateral.saturating_sub(amount_quote_lots);

        let vault = &mut ctx.accounts.vault;
        let shares_to_mint: u64 = if vault.shares_outstanding == 0 || pre_deposit_nav == 0 {
            amount_quote_lots
        } else {
            let prod = (amount_quote_lots as u128)
                .checked_mul(vault.shares_outstanding as u128)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            let s = prod / (pre_deposit_nav as u128);
            require!(s <= u64::MAX as u128, FlashBookError::ArithmeticOverflow);
            s as u64
        };
        require!(shares_to_mint > 0, FlashBookError::ZeroSize);

        vault.total_capital_quote_lots = vault
            .total_capital_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        vault.shares_outstanding = vault
            .shares_outstanding
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        let pos = &mut ctx.accounts.position;
        if pos.depositor == Pubkey::default() {
            pos.vault = vault.key();
            pos.depositor = ctx.accounts.depositor.key();
            pos.bump = ctx.bumps.position;
        }
        pos.shares = pos
            .shares
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        pos.total_deposited_quote_lots = pos
            .total_deposited_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        emit!(VaultDepositV3Event {
            vault: vault.key(),
            depositor: pos.depositor,
            amount_quote_lots,
            shares_minted: shares_to_mint,
            shares_outstanding_after: vault.shares_outstanding,
        });
        Ok(())
    }

    /// Burn shares for pro-rata payout. Debits vault TraderState
    /// collateral + SPL release from quote_vault → depositor ATA
    /// (signed by InsuranceFund PDA).
    pub fn vault_withdraw_v3(
        ctx: Context<VaultWithdrawV3>,
        shares_to_burn: u64,
    ) -> Result<()> {
        require!(shares_to_burn > 0, FlashBookError::ZeroSize);

        let vault_key = ctx.accounts.vault.key();
        let depositor_key = ctx.accounts.depositor.key();
        let total_shares = ctx.accounts.vault.shares_outstanding;

        require!(
            ctx.accounts.position.shares >= shares_to_burn,
            FlashBookError::OutOfRange
        );
        require!(total_shares >= shares_to_burn, FlashBookError::OutOfRange);
        require!(total_shares > 0, FlashBookError::OutOfRange);

        let live_nav = ctx.accounts.vault_trader_state.load()?.collateral_quote_lots as u128;
        require!(live_nav > 0, FlashBookError::InsufficientCollateral);

        let amount_u128 = (shares_to_burn as u128)
            .checked_mul(live_nav)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?
            / (total_shares as u128);
        let amount: u64 = if amount_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            amount_u128 as u64
        };
        require!(amount > 0, FlashBookError::ZeroSize);

        // Burn shares + decrement vault state first.
        {
            let vault = &mut ctx.accounts.vault;
            let pos = &mut ctx.accounts.position;
            vault.shares_outstanding = vault
                .shares_outstanding
                .saturating_sub(shares_to_burn);
            pos.shares = pos.shares.saturating_sub(shares_to_burn);
            pos.total_withdrawn_quote_lots = pos
                .total_withdrawn_quote_lots
                .checked_add(amount)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        }

        // Debit core TraderState collateral.
        {
            let mut ts = ctx.accounts.vault_trader_state.load_mut()?;
            ts.collateral_quote_lots = ts
                .collateral_quote_lots
                .checked_sub(amount)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        }

        // SPL release from quote_vault → depositor's ATA, signed by InsuranceFund PDA.
        let bump = ctx.accounts.insurance_fund.bump;
        let signer_seeds: &[&[u8]] = &[InsuranceFundAccount::SEED, &[bump]];
        let signers = &[signer_seeds];
        let cpi_accounts = Transfer {
            from: ctx.accounts.quote_vault.to_account_info(),
            to: ctx.accounts.depositor_quote_ata.to_account_info(),
            authority: ctx.accounts.insurance_fund.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signers,
        );
        token::transfer(cpi_ctx, amount)?;

        emit!(VaultWithdrawV3Event {
            vault: vault_key,
            depositor: depositor_key,
            shares_burned: shares_to_burn,
            amount_quote_lots: amount,
            shares_outstanding_after: ctx.accounts.vault.shares_outstanding,
        });
        Ok(())
    }

    /// Strategist places a limit order with the vault PDA as the trader.
    pub fn vault_place_order_v3(
        ctx: Context<VaultPlaceOrderV3>,
        side: u8,
        size_lots: u64,
        limit_ticks: u64,
        flags: u8,
        expires_at_slot: u64,
    ) -> Result<()> {
        require!(
            ctx.accounts.strategist.key() == ctx.accounts.vault.strategist,
            FlashBookError::Unauthorized
        );
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(limit_ticks > 0, FlashBookError::ZeroPrice);
        require!(flags & !0b0111_1111 == 0, FlashBookError::OutOfRange);
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

        let vault_pk = ctx.accounts.vault.key();
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
            trader: vault_pk,
            last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
            side,
            order_type: 0,
            flags,
            sub_index: 0,
        };
        let inserted_idx = if side_is_bid {
            handle.insert_bid(order)?
        } else {
            handle.insert_ask(order)?
        };

        emit!(VaultOrderPlacedV3Event {
            vault: vault_pk,
            market: market_key,
            side,
            size_lots,
            limit_ticks,
            order_seq: seq,
            node_index: inserted_idx,
        });
        Ok(())
    }

    /// Strategist cancels a vault-PDA order.
    pub fn vault_cancel_order_v3(
        ctx: Context<VaultCancelOrderV3>,
        side: u8,
        order_id: u64,
    ) -> Result<()> {
        require!(
            ctx.accounts.strategist.key() == ctx.accounts.vault.strategist,
            FlashBookError::Unauthorized
        );
        require!(side <= 1, FlashBookError::OutOfRange);
        let vault_pk = ctx.accounts.vault.key();
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
        require!(idx != crate::hypertree::NIL, FlashBookError::LiquidationStale);

        let order_seq = {
            let order = handle.order_at(idx);
            require!(order.trader == vault_pk, FlashBookError::WrongTrader);
            order.seq
        };

        if side_is_bid {
            handle.remove_bid_node(idx);
        } else {
            handle.remove_ask_node(idx);
        }

        emit!(VaultOrderCancelledV3Event {
            vault: vault_pk,
            market: market_key,
            side,
            order_id,
            order_seq,
            node_index: idx,
        });
        Ok(())
    }

    /// Strategist crystallizes the performance fee.
    pub fn settle_vault_perf_fee_v3(
        ctx: Context<SettleVaultPerfFeeV3>,
    ) -> Result<()> {
        let vault = &ctx.accounts.vault;
        require!(
            ctx.accounts.strategist.key() == vault.strategist,
            FlashBookError::Unauthorized
        );

        let shares_outstanding = vault.shares_outstanding;
        if shares_outstanding == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = constants::USD_UNIT;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        let nav = ctx.accounts.vault_trader_state.load()?.collateral_quote_lots as u128;
        require!(nav > 0, FlashBookError::InsufficientCollateral);

        let nav_per_share_x6 =
            nav.saturating_mul(constants::USD_UNIT as u128) / (shares_outstanding as u128);
        let nav_per_share_u64 = if nav_per_share_x6 > u64::MAX as u128 {
            u64::MAX
        } else {
            nav_per_share_x6 as u64
        };

        let prev_hwm = vault.hwm_nav_per_share_u64x6;
        if prev_hwm == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = nav_per_share_u64;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        require!(
            nav_per_share_u64 > prev_hwm,
            FlashBookError::OutOfRange
        );

        let gain_per_share_x6 = (nav_per_share_u64 - prev_hwm) as u128;
        let total_gain =
            gain_per_share_x6.saturating_mul(shares_outstanding as u128) / (constants::USD_UNIT as u128);
        let fee_qlots =
            total_gain.saturating_mul(vault.perf_fee_bps as u128) / (constants::BPS_DENOM as u128);
        let shares_to_mint_u128 = fee_qlots.saturating_mul(shares_outstanding as u128) / nav;
        let shares_to_mint = if shares_to_mint_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            shares_to_mint_u128 as u64
        };

        if shares_to_mint == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = nav_per_share_u64;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        {
            let sp = &mut ctx.accounts.strategist_position;
            if sp.depositor == Pubkey::default() {
                sp.vault = ctx.accounts.vault.key();
                sp.depositor = ctx.accounts.strategist.key();
                sp.bump = ctx.bumps.strategist_position;
            }
            sp.shares = sp
                .shares
                .checked_add(shares_to_mint)
                .ok_or(FlashBookError::ArithmeticOverflow)?;
        }

        let vault_key = ctx.accounts.vault.key();
        let v = &mut ctx.accounts.vault;
        v.shares_outstanding = v
            .shares_outstanding
            .checked_add(shares_to_mint)
            .ok_or(FlashBookError::ArithmeticOverflow)?;
        v.total_perf_shares_minted = v.total_perf_shares_minted.saturating_add(shares_to_mint);

        let new_nav_per_share_x6 =
            nav.saturating_mul(constants::USD_UNIT as u128) / (v.shares_outstanding as u128);
        v.hwm_nav_per_share_u64x6 = if new_nav_per_share_x6 > u64::MAX as u128 {
            u64::MAX
        } else {
            new_nav_per_share_x6 as u64
        };
        v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;

        emit!(VaultPerfFeeSettledV3Event {
            vault: vault_key,
            strategist: v.strategist,
            shares_minted: shares_to_mint,
            new_hwm_per_share_u64x6: v.hwm_nav_per_share_u64x6,
        });
        Ok(())
    }

    // ─── Per-market FLP v3 ──────────────────────────────────────────

    /// Initialize a per-market FLP exposure account. Authority-only.
    pub fn init_flp_per_market_v3(ctx: Context<InitFlpPerMarketV3>) -> Result<()> {
        let acct = &mut ctx.accounts.exposure;
        acct.market = ctx.accounts.market.key();
        acct.authority = ctx.accounts.authority.key();
        acct.bump = ctx.bumps.exposure;
        acct.side = 255;
        acct.size_lots = 0;
        acct.entry_price_ticks = 0;
        acct.total_capital_quote_lots = 0;
        acct.realized_pnl = 0;
        acct.lp_shares_outstanding = 0;

        emit!(FlpPerMarketInitV3Event {
            market: acct.market,
            authority: acct.authority,
        });
        Ok(())
    }

    /// Record a fill on this per-market FLP exposure.
    pub fn record_flp_fill_v3(
        ctx: Context<RecordFlpFillV3>,
        size_lots: u64,
        price_ticks: u64,
        side: u8,
        realized_pnl_delta: i64,
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(size_lots > 0, FlashBookError::ZeroSize);
        require!(price_ticks > 0, FlashBookError::ZeroPrice);

        let acct = &mut ctx.accounts.exposure;
        require!(
            acct.authority == ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );

        if acct.size_lots == 0 {
            acct.side = side;
            acct.size_lots = size_lots;
            acct.entry_price_ticks = price_ticks;
        } else if acct.side == side {
            let new_size = acct.size_lots.saturating_add(size_lots);
            let new_entry_u128 = (acct.entry_price_ticks as u128)
                .saturating_mul(acct.size_lots as u128)
                .saturating_add((price_ticks as u128).saturating_mul(size_lots as u128))
                / new_size as u128;
            acct.size_lots = new_size;
            acct.entry_price_ticks = if new_entry_u128 > u64::MAX as u128 {
                u64::MAX
            } else {
                new_entry_u128 as u64
            };
        } else if size_lots <= acct.size_lots {
            acct.size_lots -= size_lots;
            if acct.size_lots == 0 {
                acct.side = 255;
                acct.entry_price_ticks = 0;
            }
        } else {
            let remaining = size_lots - acct.size_lots;
            acct.side = side;
            acct.size_lots = remaining;
            acct.entry_price_ticks = price_ticks;
        }

        acct.realized_pnl = acct.realized_pnl.saturating_add(realized_pnl_delta);

        emit!(FlpFillRecordedV3Event {
            market: acct.market,
            side,
            size_lots,
            price_ticks,
            realized_pnl_delta,
            new_side: acct.side,
            new_size_lots: acct.size_lots,
            new_realized_pnl: acct.realized_pnl,
        });
        Ok(())
    }

    /// Deposit FLP capital with REAL SPL transfer + mint pro-rata shares.
    pub fn flp_deposit_v3(ctx: Context<FlpDepositV3>, amount_quote_lots: u64) -> Result<()> {
        require!(amount_quote_lots > 0, FlashBookError::ZeroSize);

        let cpi_accounts = Transfer {
            from: ctx.accounts.lp_quote_ata.to_account_info(),
            to: ctx.accounts.quote_vault.to_account_info(),
            authority: ctx.accounts.lp.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
        );
        token::transfer(cpi_ctx, amount_quote_lots)?;

        let acct = &mut ctx.accounts.exposure;
        let shares = if acct.lp_shares_outstanding == 0 || acct.total_capital_quote_lots == 0 {
            amount_quote_lots
        } else {
            let s = (amount_quote_lots as u128).saturating_mul(acct.lp_shares_outstanding as u128)
                / (acct.total_capital_quote_lots as u128).max(1);
            if s > u64::MAX as u128 { u64::MAX } else { s as u64 }
        };
        require!(shares > 0, FlashBookError::ZeroSize);

        acct.total_capital_quote_lots = acct
            .total_capital_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or(FlashBookError::ArithmeticOverflow)?;
        acct.lp_shares_outstanding = acct
            .lp_shares_outstanding
            .checked_add(shares)
            .ok_or(FlashBookError::ArithmeticOverflow)?;

        let pos = &mut ctx.accounts.position;
        if pos.shares == 0 {
            pos.market = acct.market;
            pos.lp = ctx.accounts.lp.key();
            pos.bump = ctx.bumps.position;
        }
        pos.shares = pos
            .shares
            .checked_add(shares)
            .ok_or(FlashBookError::ArithmeticOverflow)?;

        emit!(FlpDepositedV3Event {
            market: acct.market,
            lp: ctx.accounts.lp.key(),
            amount_quote_lots,
            shares_minted: shares,
            new_total_capital: acct.total_capital_quote_lots,
            new_shares_outstanding: acct.lp_shares_outstanding,
        });
        Ok(())
    }

    /// Withdraw FLP capital by burning shares + SPL release.
    pub fn flp_withdraw_v3(ctx: Context<FlpWithdrawV3>, shares_to_burn: u64) -> Result<()> {
        require!(shares_to_burn > 0, FlashBookError::ZeroSize);

        let market_key = ctx.accounts.exposure.market;
        let pos_shares = ctx.accounts.position.shares;
        require!(
            shares_to_burn <= pos_shares,
            FlashBookError::InsufficientCollateral
        );

        let total_capital = ctx.accounts.exposure.total_capital_quote_lots;
        let total_shares = ctx.accounts.exposure.lp_shares_outstanding;
        require!(total_shares > 0, FlashBookError::OutOfRange);

        let amount_u128 = (shares_to_burn as u128).saturating_mul(total_capital as u128)
            / (total_shares as u128);
        let amount = if amount_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            amount_u128 as u64
        };
        require!(amount > 0, FlashBookError::ZeroSize);

        {
            let acct = &mut ctx.accounts.exposure;
            acct.lp_shares_outstanding = acct
                .lp_shares_outstanding
                .checked_sub(shares_to_burn)
                .ok_or(FlashBookError::ArithmeticUnderflow)?;
            acct.total_capital_quote_lots = acct
                .total_capital_quote_lots
                .checked_sub(amount)
                .ok_or(FlashBookError::ArithmeticUnderflow)?;
            let pos = &mut ctx.accounts.position;
            pos.shares = pos
                .shares
                .checked_sub(shares_to_burn)
                .ok_or(FlashBookError::ArithmeticUnderflow)?;
        }

        // SPL release from quote_vault → LP's ATA, signed by InsuranceFund PDA.
        let bump = ctx.accounts.insurance_fund.bump;
        let signer_seeds: &[&[u8]] = &[InsuranceFundAccount::SEED, &[bump]];
        let signers = &[signer_seeds];
        let cpi_accounts = Transfer {
            from: ctx.accounts.quote_vault.to_account_info(),
            to: ctx.accounts.lp_quote_ata.to_account_info(),
            authority: ctx.accounts.insurance_fund.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signers,
        );
        token::transfer(cpi_ctx, amount)?;

        emit!(FlpWithdrawnV3Event {
            market: market_key,
            lp: ctx.accounts.lp.key(),
            shares_burned: shares_to_burn,
            amount_quote_lots: amount,
            new_total_capital: ctx.accounts.exposure.total_capital_quote_lots,
            new_shares_outstanding: ctx.accounts.exposure.lp_shares_outstanding,
        });
        Ok(())
    }

    /// JIT LIQUIDATION OFFER — a maker pre-commits a "tighter than
    /// synthetic" close price to be used when an underwater trader is
    /// liquidated on this market. When `liquidate_position_v2` fires
    /// against a matching trader, the matcher selects the BEST JIT
    /// offer beating `oracle ± liq_penalty_bps` and uses that price as
    /// the close-order limit. The trader loses less collateral, the
    /// insurance fund draws less, the JIT maker gets a guaranteed fill
    /// at a tighter spread than open-market.
    ///
    /// `side`:
    ///   0 = will close LONG positions  (offer to BUY back the long)
    ///   1 = will close SHORT positions (offer to SELL into the short)
    ///
    /// `target_trader = Pubkey::default()` is a wildcard offer (any
    /// trader's liquidation on this market). Otherwise scoped to one
    /// trader (useful for high-touch keeper bots).
    ///
    /// Collateral reservation is intentionally NOT enforced here — the
    /// maker handles their own risk. If at fill time the maker can't
    /// absorb the position, the normal collateral path will reject
    /// when the maker walks the close-ask. Simpler design, no new
    /// fields on TraderState.
    ///
    /// PDA seeds: `[b"jit_liq_offer", market, maker, &nonce.to_le_bytes()]`.
    pub fn place_jit_liquidation_offer(
        ctx: Context<PlaceJitLiquidationOffer>,
        nonce: u32,
        target_trader: Pubkey,
        side: u8,
        offer_price_ticks: u64,
        max_size_lots: u64,
        expires_at_slot: u64,
        maker_sub_index: u8,
    ) -> Result<()> {
        require!(side <= 1, FlashBookError::OutOfRange);
        require!(offer_price_ticks > 0, FlashBookError::ZeroPrice);
        require!(max_size_lots > 0, FlashBookError::ZeroSize);

        let market = &ctx.accounts.market;
        require!(
            offer_price_ticks % market.params.tick_size == 0,
            FlashBookError::PriceNotOnTick
        );
        require!(
            max_size_lots >= market.params.min_base_lots,
            FlashBookError::SizeBelowMinLot
        );

        let now = Clock::get()?.slot;
        if expires_at_slot != 0 {
            require!(expires_at_slot > now, FlashBookError::OutOfRange);
        }

        let offer = &mut ctx.accounts.jit_offer;
        offer.bump = ctx.bumps.jit_offer;
        offer.side = side;
        offer.maker_sub_index = maker_sub_index;
        offer._pad0 = [0u8; 1];
        offer.nonce = nonce;
        offer.market = market.key();
        offer.maker = ctx.accounts.maker.key();
        offer.target_trader = target_trader;
        offer.offer_price_ticks = offer_price_ticks;
        offer.max_size_lots = max_size_lots;
        offer.remaining_size_lots = max_size_lots;
        offer.created_at_slot = now;
        offer.expires_at_slot = expires_at_slot;

        emit!(JitLiquidationOfferPlacedEvent {
            market: market.key(),
            maker: offer.maker,
            target_trader,
            nonce,
            side,
            offer_price_ticks,
            max_size_lots,
            expires_at_slot,
        });
        Ok(())
    }

    /// Cancel a JIT liquidation offer and close the account, refunding
    /// rent to the maker. Only the maker can cancel.
    pub fn cancel_jit_liquidation_offer(
        ctx: Context<CancelJitLiquidationOffer>,
    ) -> Result<()> {
        let maker = ctx.accounts.maker.key();
        require!(
            ctx.accounts.jit_offer.maker == maker,
            FlashBookError::Unauthorized
        );
        emit!(JitLiquidationOfferCancelledEvent {
            market: ctx.accounts.jit_offer.market,
            maker,
            nonce: ctx.accounts.jit_offer.nonce,
            unfilled_size_lots: ctx.accounts.jit_offer.remaining_size_lots,
        });
        Ok(())
    }

    /// Migrate a market account from the pre-V3 mark-engine layout (1024 B body)
    /// to the V3 layout (1152 B body). V3 added `last_mark_settle_slot` plus
    /// four mark-engine params (mark_ema_alpha_bps, mark_max_change_bps,
    /// mark_settle_min_slots, drift_alert_bps).
    ///
    /// Idempotent: if the account is already at the V3 size and the mark-engine
    /// params look set, returns Ok with a log message and no state change.
    ///
    /// Authority-gated: only `market.authority` may call.
    pub fn migrate_market_to_v3(ctx: Context<MigrateMarketToV3>) -> Result<()> {
        let market_ai = &ctx.accounts.market;
        let target_size = MarketAccount::space();
        let current_size = market_ai.data_len();
        let authority_key = ctx.accounts.authority.key();

        // Defence in depth: target_size is computed from MarketAccount::space()
        // which is compile-time bounded by the type layout, BUT a future
        // schema change that inadvertently bloats MarketAccount past Solana's
        // 10 MB account ceiling would otherwise blow up inside the runtime.
        // Refuse at the boundary so the error is legible and the tx's compute
        // budget isn't burned reaching a deeper panic.
        require!(
            target_size <= constants::SOLANA_MAX_ACCOUNT_SIZE,
            FlashBookError::OutOfRange
        );

        // Already migrated?
        if current_size == target_size {
            // Check V3 fields are populated (mark_ema_alpha_bps != 0 indicates a
            // post-migration account). If zeroed, fall through and write defaults.
            let data = market_ai.try_borrow_data()?;
            let mut market: MarketAccount = MarketAccount::try_deserialize(&mut &data[..])?;
            drop(data);
            require!(market.authority == authority_key, FlashBookError::Unauthorized);
            if market.params.mark_ema_alpha_bps != 0 {
                msg!("migrate_market_to_v3: account already V3 (size={}, alpha={})",
                    current_size, market.params.mark_ema_alpha_bps);
                return Ok(());
            }
            // Size OK but params zeroed — write defaults
            market.params.mark_ema_alpha_bps = 2_000;
            market.params.mark_max_change_bps = 500;
            market.params.mark_settle_min_slots = 10;
            market.params.drift_alert_bps = 100;
            if market.last_mark_settle_slot == 0 {
                market.last_mark_settle_slot = Clock::get()?.slot;
            }
            let mut data = market_ai.try_borrow_mut_data()?;
            let mut writer = &mut data[..];
            market.try_serialize(&mut writer)?;
            emit!(MarketMigratedToV3Event {
                market: market_ai.key(),
                old_size: current_size as u32,
                new_size: target_size as u32,
                slot: Clock::get()?.slot,
            });
            return Ok(());
        }

        require!(
            current_size < target_size,
            FlashBookError::AlreadyInitialized
        );

        // 1. Read OLD bytes — deserialize using try_deserialize_unchecked which
        //    skips the size check (the discriminator must still match).
        let old_bytes = market_ai.try_borrow_data()?.to_vec();
        let mut market: MarketAccount = MarketAccount::try_deserialize_unchecked(
            &mut &old_bytes[..]
        )?;
        require!(market.authority == authority_key, FlashBookError::Unauthorized);

        // 2. Realloc to V3 size. Fund the rent diff from the authority.
        let new_minimum_balance = Rent::get()?.minimum_balance(target_size);
        let lamports_diff = new_minimum_balance.saturating_sub(market_ai.lamports());
        if lamports_diff > 0 {
            let cpi_ctx = CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.authority.to_account_info(),
                    to: market_ai.to_account_info(),
                },
            );
            anchor_lang::system_program::transfer(cpi_ctx, lamports_diff)?;
        }
        market_ai.realloc(target_size, true)?;

        // 3. Write V3 defaults
        market.params.mark_ema_alpha_bps = 2_000;
        market.params.mark_max_change_bps = 500;
        market.params.mark_settle_min_slots = 10;
        market.params.drift_alert_bps = 100;
        market.last_mark_settle_slot = Clock::get()?.slot;

        // 4. Serialize back
        let mut data = market_ai.try_borrow_mut_data()?;
        let mut writer = &mut data[..];
        market.try_serialize(&mut writer)?;
        drop(data);

        emit!(MarketMigratedToV3Event {
            market: market_ai.key(),
            old_size: current_size as u32,
            new_size: target_size as u32,
            slot: Clock::get()?.slot,
        });
        Ok(())
    }

    /// Initialize the per-market Pyth oracle config PDA. After this is
    /// initialized, `update_oracle_from_pyth` can be called by anyone to
    /// pull a fresh price from the on-chain `PriceUpdateV2` account
    /// posted by the Pyth Solana Receiver.
    ///
    /// Authority-gated: only `market.authority` may install/rotate the
    /// feed binding (a malicious wrong feed would break the market).
    pub fn init_market_oracle_config(
        ctx: Context<InitMarketOracleConfig>,
        pyth_price_feed_id: [u8; 32],
        max_staleness_seconds: u32,
        max_confidence_bps: u32,
        tick_decimals: i8,
    ) -> Result<()> {
        require!(
            ctx.accounts.market.authority == ctx.accounts.authority.key(),
            FlashBookError::Unauthorized,
        );
        require!(max_staleness_seconds > 0, FlashBookError::OutOfRange);
        require!(max_confidence_bps > 0 && max_confidence_bps <= 1000, FlashBookError::OutOfRange);

        let cfg = &mut ctx.accounts.oracle_config;
        cfg.bump = ctx.bumps.oracle_config;
        cfg.source = state_v3::MarketOracleConfigAccount::SOURCE_PYTH;
        cfg.market = ctx.accounts.market.key();
        cfg.pyth_price_feed_id = pyth_price_feed_id;
        cfg.max_staleness_seconds = max_staleness_seconds;
        cfg.max_confidence_bps = max_confidence_bps;
        cfg.tick_decimals = tick_decimals;

        emit!(MarketOracleConfigInitializedEvent {
            market: ctx.accounts.market.key(),
            pyth_price_feed_id,
            max_staleness_seconds,
            max_confidence_bps,
            tick_decimals,
        });
        Ok(())
    }

    /// Permissionless: pull a fresh price from a Pyth `PriceUpdateV2`
    /// account into the market's `oracle_*` fields. Validates:
    ///   • feed_id matches the configured binding
    ///   • publish_time is within `max_staleness_seconds` of current slot
    ///   • confidence (conf/price) is within `max_confidence_bps`
    ///
    /// On success, writes:
    ///   • market.oracle_price_ticks (scaled by `tick_decimals`)
    ///   • market.oracle_confidence  (raw conf in same scale)
    ///   • market.oracle_published_at_unix_seconds
    ///
    /// Anyone can call. Pyth account is the trust anchor — the caller
    /// just funds the tx.
    pub fn update_oracle_from_pyth(
        ctx: Context<UpdateOracleFromPyth>,
    ) -> Result<()> {
        use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

        let cfg = &ctx.accounts.oracle_config;
        require!(
            cfg.source == state_v3::MarketOracleConfigAccount::SOURCE_PYTH,
            FlashBookError::OutOfRange,
        );

        let price_update: &Account<PriceUpdateV2> = &ctx.accounts.price_update;
        let max_age = cfg.max_staleness_seconds as u64;
        let price_data = price_update.get_price_no_older_than(
            &Clock::get()?,
            max_age,
            &cfg.pyth_price_feed_id,
        ).map_err(|_| error!(FlashBookError::OracleTooStale))?;

        require!(price_data.price > 0, FlashBookError::ZeroPrice);

        // Convert Pyth price (scaled by 10^exponent) to our tick units.
        //   real_usd = pyth_price * 10^pyth.exponent
        //   ticks    = real_usd / tick_value
        //   tick_value = 10^(-tick_decimals)
        //   ticks = pyth_price * 10^(pyth.exponent + tick_decimals)
        let scale_exp: i32 = price_data.exponent + cfg.tick_decimals as i32;
        let new_ticks: i64 = if scale_exp >= 0 {
            let mul = 10i64.checked_pow(scale_exp as u32)
                .ok_or(error!(FlashBookError::ArithmeticOverflow))?;
            price_data.price.checked_mul(mul)
                .ok_or(error!(FlashBookError::ArithmeticOverflow))?
        } else {
            let div = 10i64.checked_pow((-scale_exp) as u32)
                .ok_or(error!(FlashBookError::ArithmeticOverflow))?;
            price_data.price.checked_div(div)
                .ok_or(error!(FlashBookError::DivisionByZero))?
        };
        require!(new_ticks > 0, FlashBookError::ZeroPrice);

        // Confidence in bps of price. Pyth `conf` is in the same scale as `price`.
        let conf_bps = (price_data.conf as u128)
            .checked_mul(10_000)
            .ok_or(error!(FlashBookError::ArithmeticOverflow))?
            .checked_div(price_data.price.unsigned_abs() as u128)
            .unwrap_or(u128::MAX);
        require!(
            conf_bps <= cfg.max_confidence_bps as u128,
            FlashBookError::OracleConfidenceTooWide,
        );

        // Wave 26b — optional envelope gate on the new Pyth price.
        let now_slot = Clock::get()?.slot;
        gate_oracle_update(
            ctx.accounts.envelope_config.as_mut(),
            new_ticks as u64,
            now_slot,
        )?;

        let market = &mut ctx.accounts.market;
        market.oracle_price_ticks = new_ticks as u64;
        market.oracle_confidence = price_data.conf;
        market.oracle_published_at_unix_seconds = price_data.publish_time as u64;

        emit!(OracleUpdatedFromPythEvent {
            market: ctx.accounts.market.key(),
            pyth_price: price_data.price,
            pyth_conf: price_data.conf,
            pyth_exponent: price_data.exponent,
            new_oracle_ticks: new_ticks as u64,
            confidence_bps: conf_bps.min(u32::MAX as u128) as u32,
            publish_time: price_data.publish_time,
        });
        Ok(())
    }

    // ─── Wave 24b — H-haircut instructions ──────────────────────────
    //
    // Four permissionless ix that operate on the sibling PDAs added in
    // Wave 24a (`MarketHaircutStateAccount` + `PositionHaircutStateAccount`).
    // Pure math lives in `matcher::haircut`; these are the on-chain
    // surface that exposes it.

    /// One-time per market: seed `MarketHaircutStateAccount` with the
    /// configured warmup window and an initial residual. Authority-gated
    /// because the initial residual is a trust input until automated
    /// reconciliation lands.
    pub fn initialize_haircut_state(
        ctx: Context<InitializeHaircutState>,
        h_min_slots: u64,
        h_max_slots: u64,
        initial_residual_quote_lots: u128,
    ) -> Result<()> {
        matcher::haircut::validate_market_params(h_min_slots, h_max_slots)
            .map_err(map_haircut_error)?;

        let st = &mut ctx.accounts.haircut_state;
        st.market = ctx.accounts.market.key();
        st.bump = ctx.bumps.haircut_state;
        st._pad0 = [0; 7];
        st.residual_quote_lots = initial_residual_quote_lots;
        st.matured_pos_total_quote_lots = 0;
        st.realized_loss_total_quote_lots = 0;
        st.dust_accrued_quote_lots = 0;
        st.h_min_slots = h_min_slots;
        st.h_max_slots = h_max_slots;
        st.h_scaled_cached = matcher::haircut::H_DENOM as u64;
        st.h_cached_at_slot = Clock::get()?.slot;
        st._reserved = [0; 64];

        emit!(HaircutInitializedEvent {
            market: st.market,
            h_min_slots,
            h_max_slots,
            initial_residual_quote_lots,
        });
        Ok(())
    }

    /// Permissionless: advance a position's reserve → matured pipeline.
    /// Anyone can call. Idempotent — calling at the same slot twice
    /// produces a no-op on the second call.
    ///
    /// Refuses (errors `HaircutNothingToMature`) when nothing has
    /// matured since the last call. This is intentional: keepers
    /// should batch positions and only crank ones that have actually
    /// progressed, to keep tx churn low.
    pub fn mature_position(ctx: Context<MaturePosition>) -> Result<()> {
        let now_slot = Clock::get()?.slot;
        let pos_haircut = &mut ctx.accounts.position_haircut;
        let market_haircut = &mut ctx.accounts.haircut_state;

        let pre = matcher::haircut::PositionHaircutSnapshot {
            released_reserve_quote_lots: pos_haircut.released_reserve_quote_lots,
            released_attached_at_slot: pos_haircut.released_attached_at_slot,
            matured_pos_quote_lots: pos_haircut.matured_pos_quote_lots,
            original_reserve_at_attach: pos_haircut.original_reserve_at_attach,
        };

        let (post, delta) = matcher::haircut::apply_mature(
            pre,
            now_slot,
            market_haircut.h_min_slots,
            market_haircut.h_max_slots,
        )
        .map_err(map_haircut_error)?;

        require!(delta > 0, FlashBookError::HaircutNothingToMature);

        pos_haircut.released_reserve_quote_lots = post.released_reserve_quote_lots;
        pos_haircut.released_attached_at_slot = post.released_attached_at_slot;
        pos_haircut.matured_pos_quote_lots = post.matured_pos_quote_lots;
        pos_haircut.original_reserve_at_attach = post.original_reserve_at_attach;

        market_haircut.matured_pos_total_quote_lots = market_haircut
            .matured_pos_total_quote_lots
            .checked_add(delta as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        emit!(PositionMaturedEvent {
            market: market_haircut.market,
            position: pos_haircut.position,
            matured_delta: delta,
            matured_pos_after: post.matured_pos_quote_lots,
        });
        Ok(())
    }

    /// Permissionless: convert a position's `matured_pos` into a
    /// collateral credit at the current `h`, with the dust accrued to
    /// the market's haircut state (drains to insurance via
    /// `flush_haircut_dust`).
    ///
    /// The credit is **NOT** moved into the trader's collateral by this
    /// ix yet — that's Wave 24c (because the wire-in needs to touch
    /// `apply_realized_pnl_delta` and pick the isolated/cross routing).
    /// Wave 24b lands the math + state-only mutation surface so the
    /// math can be exercised on-chain independently of the credit path.
    /// `PositionConvertedEvent` carries the credit amount so keepers
    /// and indexers can see it.
    pub fn convert_position(ctx: Context<ConvertPosition>) -> Result<()> {
        let pos_haircut = &mut ctx.accounts.position_haircut;
        let market_haircut = &mut ctx.accounts.haircut_state;

        require!(
            pos_haircut.matured_pos_quote_lots > 0,
            FlashBookError::HaircutNothingToConvert
        );

        let pre = matcher::haircut::PositionHaircutSnapshot {
            released_reserve_quote_lots: pos_haircut.released_reserve_quote_lots,
            released_attached_at_slot: pos_haircut.released_attached_at_slot,
            matured_pos_quote_lots: pos_haircut.matured_pos_quote_lots,
            original_reserve_at_attach: pos_haircut.original_reserve_at_attach,
        };
        let matured_at_call = pre.matured_pos_quote_lots;

        let h_scaled = matcher::haircut::compute_h(
            market_haircut.residual_quote_lots,
            market_haircut.matured_pos_total_quote_lots,
        );
        let (post, credit, dust) = matcher::haircut::apply_convert(pre, h_scaled);

        pos_haircut.matured_pos_quote_lots = post.matured_pos_quote_lots;

        // Market state: subtract matured from the global denominator,
        // accrue dust, debit residual by the credit (the trader extracted
        // real value).
        market_haircut.matured_pos_total_quote_lots = market_haircut
            .matured_pos_total_quote_lots
            .checked_sub(matured_at_call as u128)
            .ok_or_else(|| error!(FlashBookError::HaircutResidualUnderflow))?;
        market_haircut.dust_accrued_quote_lots = market_haircut
            .dust_accrued_quote_lots
            .checked_add(dust as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        market_haircut.residual_quote_lots = market_haircut
            .residual_quote_lots
            .checked_sub(credit as u128)
            .ok_or_else(|| error!(FlashBookError::HaircutResidualUnderflow))?;

        // Cache the freshly-computed h for off-chain readers.
        market_haircut.h_scaled_cached = h_scaled.min(u64::MAX as u128) as u64;
        market_haircut.h_cached_at_slot = Clock::get()?.slot;

        emit!(PositionConvertedEvent {
            market: market_haircut.market,
            position: pos_haircut.position,
            credit_quote_lots: credit,
            dust_quote_lots: dust,
            h_scaled,
        });
        Ok(())
    }

    /// Permissionless: drain accumulated haircut dust to the insurance
    /// fund. Keeper hook; runs at most once per slot per market in
    /// practice (the dust accrues slowly from many converts).
    ///
    /// Wave 24b lands the math + event emission. Actual SPL transfer
    /// from haircut state's claim into the insurance fund's balance
    /// counter happens here; the underlying vault token doesn't move
    /// (insurance and haircut both live in the same vault) — only the
    /// accounting counters do.
    pub fn flush_haircut_dust(ctx: Context<FlushHaircutDust>) -> Result<()> {
        let market_haircut = &mut ctx.accounts.haircut_state;
        let insurance = &mut ctx.accounts.insurance_fund;

        let dust = market_haircut.dust_accrued_quote_lots;
        require!(dust > 0, FlashBookError::HaircutNothingToConvert);

        // Move accounting: dust → insurance balance counter.
        let dust_u64: u64 = if dust > u64::MAX as u128 {
            u64::MAX
        } else {
            dust as u64
        };
        insurance.balance_quote_lots = insurance
            .balance_quote_lots
            .checked_add(dust_u64)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        insurance.total_contributions = insurance
            .total_contributions
            .checked_add(dust_u64)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        market_haircut.dust_accrued_quote_lots = market_haircut
            .dust_accrued_quote_lots
            .checked_sub(dust_u64 as u128)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;

        emit!(HaircutDustFlushedEvent {
            market: market_haircut.market,
            dust_quote_lots: dust_u64,
            insurance_balance_after: insurance.balance_quote_lots,
        });
        Ok(())
    }

    // ─── Wave 24c — additional H-haircut surface ────────────────────

    /// Permissionless: lazy-init a per-position `PositionHaircutStateAccount`.
    /// Called once per (market, position) before the first
    /// `release_gain_to_haircut` against it. Wave 24d/e will replace
    /// this with `init_if_needed` inside `apply_fill`'s gain path.
    pub fn init_position_haircut_state(ctx: Context<InitPositionHaircutState>) -> Result<()> {
        let position_market = ctx.accounts.position.load()?.market;
        let position_key = ctx.accounts.position.key();
        let st = &mut ctx.accounts.position_haircut;
        st.market = position_market;
        st.position = position_key;
        st.bump = ctx.bumps.position_haircut;
        st._pad0 = [0; 7];
        st.released_reserve_quote_lots = 0;
        st.released_attached_at_slot = 0;
        st.matured_pos_quote_lots = 0;
        st.original_reserve_at_attach = 0;
        st._reserved = [0; 24];

        emit!(PositionHaircutInitializedEvent {
            market: st.market,
            position: st.position,
        });
        Ok(())
    }

    /// Wave 24c — interim: route an explicit amount of a position's
    /// already-credited collateral INTO the haircut reserve.
    ///
    /// This is what Wave 24d will do automatically inside `apply_fill`:
    /// when a fill realizes positive PnL on an opted-in market, the
    /// credit flows to the reserve instead of straight to collateral.
    /// Until that wire-in lands, this ix lets the sequencer / authority
    /// retroactively opt a position into the haircut by moving N lots
    /// of its collateral into the reserve.
    ///
    /// Authority-gated (market.authority) for safety — anyone calling
    /// this could move trader funds into a reserve the trader doesn't
    /// know about. In practice the sequencer holds market authority.
    ///
    /// Bucket: routes from `position.collateral_quote_lots` (isolated
    /// bucket) if positive, else from `trader_state.collateral_quote_lots`
    /// (cross bucket). Pre-existing behaviour mirrored from
    /// `compute_realized_pnl_routing`.
    pub fn release_gain_to_haircut(
        ctx: Context<ReleaseGainToHaircut>,
        gain_quote_lots: u64,
    ) -> Result<()> {
        require!(gain_quote_lots > 0, FlashBookError::HaircutZeroGain);
        require_keys_eq!(
            ctx.accounts.market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );

        // Same bucket-selection rule as `compute_realized_pnl_routing`.
        let isolated = ctx.accounts.position.load()?.collateral_quote_lots > 0;
        if isolated {
            let mut position = ctx.accounts.position.load_mut()?;
            position.collateral_quote_lots = position
                .collateral_quote_lots
                .checked_sub(gain_quote_lots)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        } else {
            let mut trader_state = ctx.accounts.trader_state.load_mut()?;
            trader_state.collateral_quote_lots = trader_state
                .collateral_quote_lots
                .checked_sub(gain_quote_lots)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
        }

        let pos_haircut = &mut ctx.accounts.position_haircut;
        let market_haircut = &mut ctx.accounts.haircut_state;

        // Push the moved amount into the position's reserve via the
        // pure module — same path apply_fill will use in Wave 24d.
        let now_slot = Clock::get()?.slot;
        let pre = matcher::haircut::PositionHaircutSnapshot {
            released_reserve_quote_lots: pos_haircut.released_reserve_quote_lots,
            released_attached_at_slot: pos_haircut.released_attached_at_slot,
            matured_pos_quote_lots: pos_haircut.matured_pos_quote_lots,
            original_reserve_at_attach: pos_haircut.original_reserve_at_attach,
        };
        let post = matcher::haircut::apply_release(pre, gain_quote_lots, now_slot)
            .map_err(map_haircut_error)?;

        pos_haircut.released_reserve_quote_lots = post.released_reserve_quote_lots;
        pos_haircut.released_attached_at_slot = post.released_attached_at_slot;
        pos_haircut.matured_pos_quote_lots = post.matured_pos_quote_lots;
        pos_haircut.original_reserve_at_attach = post.original_reserve_at_attach;

        // The collateral that just moved into the reserve was previously
        // backed by Residual (the protocol's surplus). Now that it lives
        // in the reserve and will eventually convert at h ≤ 1, the
        // protocol's effective liability to this trader is bounded
        // above by `gain_quote_lots`. Residual itself doesn't change at
        // the release step — only at the convert step (which decrements
        // Residual by the credit actually paid out).

        emit!(GainReleasedToHaircutEvent {
            market: market_haircut.market,
            position: pos_haircut.position,
            gain_quote_lots,
            reserve_after: pos_haircut.released_reserve_quote_lots,
            isolated,
        });
        Ok(())
    }

    // ─── Wave 26a — Envelope config ─────────────────────────────────

    /// Authority-only: set or update the per-market envelope config.
    /// Validates via `matcher::envelope::prove_envelope` — bad params
    /// cannot be written. Bumps `version` on every successful call so
    /// downstream readers can detect updates.
    ///
    /// Idempotent re-runs (passing identical params) succeed and bump
    /// the version; this is intentional so authorities can prove the
    /// envelope on every rotation without needing to detect changes
    /// off-chain.
    pub fn set_envelope_config(
        ctx: Context<SetEnvelopeConfig>,
        max_price_move_bps_per_slot: u32,
        max_accrual_dt_slots: u64,
        max_abs_funding_e9_per_slot: i64,
        maintenance_bps: u32,
        liquidation_fee_bps: u32,
        min_liquidation_abs_lots: u64,
        min_nonzero_mm_req_lots: u64,
    ) -> Result<()> {
        let params = matcher::envelope::EnvelopeParams {
            max_price_move_bps_per_slot,
            max_accrual_dt_slots,
            max_abs_funding_e9_per_slot,
            maintenance_bps,
            liquidation_fee_bps,
            min_liquidation_abs_lots,
            min_nonzero_mm_req_lots,
        };
        matcher::envelope::prove_envelope(&params).map_err(map_envelope_error)?;

        let cfg = &mut ctx.accounts.envelope_config;
        if cfg.market == Pubkey::default() {
            // First init.
            cfg.market = ctx.accounts.market.key();
            cfg.bump = ctx.bumps.envelope_config;
            cfg._pad0 = [0; 7];
            cfg._pad1 = [0; 4];
            cfg._reserved = [0; 32];
            cfg.version = 0;
            // Wave 26b gate-state defaults (zero = no prior observation).
            cfg.last_observed_slot = 0;
            cfg.last_observed_price_ticks = 0;
            cfg.gate_passes = 0;
            cfg.gate_rejects = 0;
        }
        cfg.write_params(&params);
        cfg.last_proven_at_slot = Clock::get()?.slot;
        cfg.version = cfg.version.saturating_add(1);

        emit!(EnvelopeConfigSetEvent {
            market: cfg.market,
            version: cfg.version,
            max_price_move_bps_per_slot,
            max_accrual_dt_slots,
            maintenance_bps,
            liquidation_fee_bps,
        });
        Ok(())
    }

    /// Permissionless: re-run `prove_envelope` against the stored
    /// params. Emits a structured event with `passed: bool`. Lets
    /// keepers / monitors verify periodically that the params still
    /// satisfy the envelope inequality (in case future changes to
    /// the envelope's absolute bounds make a previously-valid config
    /// no longer prove).
    pub fn verify_envelope_config(ctx: Context<VerifyEnvelopeConfig>) -> Result<()> {
        let cfg = &ctx.accounts.envelope_config;
        let params = cfg.params();
        let passed = matcher::envelope::prove_envelope(&params).is_ok();
        emit!(EnvelopeVerifiedEvent {
            market: cfg.market,
            version: cfg.version,
            passed,
        });
        Ok(())
    }

    /// Permissionless runtime gate: given a proposed (old_price,
    /// new_price, dt) triple, verify it respects the per-slot price
    /// move cap stored in the envelope config. Returns successfully
    /// iff the move is admissible; reverts otherwise.
    ///
    /// This is the surface Wave 26b will hook into the mark-EMA
    /// update path inside `apply_fill`. Today it's a callable ix so
    /// off-chain bots / sequencers can pre-flight a price update
    /// without simulating the full apply_fill.
    pub fn gate_envelope_price_move(
        ctx: Context<GateEnvelopePriceMove>,
        old_price_ticks: u64,
        new_price_ticks: u64,
        dt_slots: u64,
    ) -> Result<()> {
        let cfg = &ctx.accounts.envelope_config;
        matcher::envelope::gate_price_move(
            old_price_ticks,
            new_price_ticks,
            dt_slots,
            cfg.max_price_move_bps_per_slot,
        )
        .map_err(map_envelope_error)?;
        Ok(())
    }

    // ─── Wave 25a — A/K/F/B side accrual init ───────────────────────

    /// Authority-only: initialize the per-market side accrual state.
    /// Both sides start in Normal mode with A == ADL_ONE and K/F/B
    /// at zero. Wave 25b will rewire `settle_funding` and
    /// `auto_deleverage` to operate on this account.
    ///
    /// Idempotent in the sense that re-running the ix on an existing
    /// account is impossible (Anchor's `init` constraint).
    pub fn initialize_side_accrual(
        ctx: Context<InitializeSideAccrual>,
        initial_price_ticks: u64,
        initial_slot: u64,
    ) -> Result<()> {
        let acc = &mut ctx.accounts.side_accrual;
        acc.market = ctx.accounts.market.key();
        acc.bump = ctx.bumps.side_accrual;
        acc._pad0 = [0; 7];

        // Long side defaults.
        acc.long_a = matcher::side_accrual::ADL_ONE;
        acc.long_k = 0;
        acc.long_f = 0;
        acc.long_b = 0;
        acc.long_mode = 0; // Normal
        acc.long_epoch = 0;
        acc._long_pad = [0; 3];
        acc.long_slot_last = initial_slot;
        acc.long_price_last = initial_price_ticks;

        // Short side defaults.
        acc.short_a = matcher::side_accrual::ADL_ONE;
        acc.short_k = 0;
        acc.short_f = 0;
        acc.short_b = 0;
        acc.short_mode = 0;
        acc.short_epoch = 0;
        acc._short_pad = [0; 3];
        acc.short_slot_last = initial_slot;
        acc.short_price_last = initial_price_ticks;

        acc._reserved = [0; 64];

        emit!(SideAccrualInitializedEvent {
            market: acc.market,
            initial_price_ticks,
            initial_slot,
        });
        Ok(())
    }

    /// Wave 24e — Permissionless: walk all internal-consistency
    /// invariants over the market's haircut state and emit a structured
    /// report event.
    ///
    /// Checks:
    ///   • residual ≥ 0 (trivial for u128, but for future-proofing)
    ///   • h_min ≤ h_max ≤ ABS_MAX_H_MAX_SLOTS
    ///   • h_scaled_cached ∈ [0, H_DENOM]
    ///   • h_scaled_cached == compute_h(residual, matured_total)  (if cache fresh)
    ///   • dust_accrued ≤ realized_loss_total + matured_pos_total
    ///
    /// Does NOT cross-check against the live SPL vault balance — that
    /// needs per-market committed-collateral accounting (Wave 28).
    /// Keepers should run this every N slots; downstream monitoring
    /// catches `HaircutInvariantsCheckedEvent` and pages if `passed`
    /// drops to false.
    pub fn verify_haircut_invariants(ctx: Context<VerifyHaircutInvariants>) -> Result<()> {
        let st = &ctx.accounts.haircut_state;
        let report = matcher::haircut::verify_invariants(
            st.residual_quote_lots,
            st.matured_pos_total_quote_lots,
            st.realized_loss_total_quote_lots,
            st.dust_accrued_quote_lots,
            st.h_min_slots,
            st.h_max_slots,
            st.h_scaled_cached,
            st.h_cached_at_slot,
        );
        emit!(HaircutInvariantsCheckedEvent {
            market: st.market,
            passed: report.all_ok(),
            bitmask: report.bitmask(),
            residual_quote_lots: st.residual_quote_lots,
            matured_pos_total_quote_lots: st.matured_pos_total_quote_lots,
            dust_accrued_quote_lots: st.dust_accrued_quote_lots,
            h_scaled_cached: st.h_scaled_cached,
        });
        Ok(())
    }

    /// Wave 24f — Permissionless: probe the protocol-level solvency
    /// invariant by reading on-chain balances directly. Asserts
    /// `vault.amount ≥ insurance.balance + flp.total_capital`. Any
    /// deviation indicates accounting drift between the SPL vault and
    /// the protocol's bookkeeping — call the kill switch.
    ///
    /// Foundational sanity check. Doesn't reconcile committed trader
    /// collateral (that requires iterating positions; out of scope
    /// for a single-tx check). Catches the gross failure modes:
    ///   • Vault drained below the LP claims (programmatic bug)
    ///   • Insurance balance > vault (double-credit accounting bug)
    ///   • FLP capital not reflected in vault (lost-funds bug)
    pub fn verify_protocol_solvency(ctx: Context<VerifyProtocolSolvency>) -> Result<()> {
        let vault_amount = ctx.accounts.quote_vault.amount;
        let insurance_bal = ctx.accounts.insurance_fund.balance_quote_lots;
        let flp_capital = ctx.accounts.flp_exposure.total_capital_quote_lots;

        let minimum_required = insurance_bal
            .checked_add(flp_capital)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;

        let solvent = vault_amount >= minimum_required;
        let surplus = if solvent {
            vault_amount.saturating_sub(minimum_required)
        } else {
            0
        };

        emit!(ProtocolSolvencyCheckedEvent {
            vault_quote_lots: vault_amount,
            insurance_quote_lots: insurance_bal,
            flp_capital_quote_lots: flp_capital,
            minimum_required_quote_lots: minimum_required,
            surplus_quote_lots: surplus,
            solvent,
        });

        // Hard fail if insolvent. The transaction reverts; the event
        // is rolled back. Off-chain monitors should poll this ix
        // regularly and page if it errors.
        require!(solvent, FlashBookError::HaircutResidualUnderflow);
        Ok(())
    }

    /// Authority-only: set/grow the per-market Residual explicitly.
    /// Interim mechanism until Wave 28's per-market FLP attribution
    /// wires Residual into automatic delta-tracking on every money
    /// move. Production deployments should reconcile against on-chain
    /// balances before each call. Signed deltas — pass negative to
    /// shrink (e.g. on payout audit).
    pub fn seed_residual(
        ctx: Context<SeedResidual>,
        delta: i128,
    ) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.market.authority,
            ctx.accounts.authority.key(),
            FlashBookError::Unauthorized
        );
        let st = &mut ctx.accounts.haircut_state;
        let new_residual = matcher::haircut::apply_residual_delta(st.residual_quote_lots, delta)
            .map_err(map_haircut_error)?;
        st.residual_quote_lots = new_residual;
        emit!(ResidualSeededEvent {
            market: st.market,
            delta,
            new_residual,
        });
        Ok(())
    }
}

/// Shared init body for `initialize_market`. Permissionless market
/// creation has been removed — markets are authority-gated and the
/// `creator` field on the market account is zeroed (no HIP-3-style
/// creator share is paid out unless the authority later migrates the
/// market to a curated creator).
fn initialize_market_inner(
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
    // Authority-gated market: creator is always zeroed (no creator
    // share). The HIP-3-style creator-share branch in `apply_fill`
    // remains for future protocol-managed curated creators but is dead
    // code for markets initialized today.
    market.creator = Pubkey::default();
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
    market.last_mark_settle_slot = 0;
    market.params = params;
    // C-1: settlement signer defaults to the deployer/authority. Rotate
    // via `set_market_sequencer` before burning authority so fills keep
    // settling after decentralization.
    market.sequencer = ctx.accounts.authority.key();

    emit!(MarketInitializedEvent {
        market: market.key(),
        authority: market.authority,
        initial_oracle_ticks,
    });
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

// ─── Wave 24b — H-haircut account contexts ──────────────────────────

#[derive(Accounts)]
pub struct InitializeHaircutState<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// Authority must match the market's authority — same trust model
    /// as `update_market_params`.
    #[account(
        seeds = [state::MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
    )]
    pub market: Box<Account<'info, state::MarketAccount>>,
    #[account(
        init,
        payer = authority,
        space = state_v3::MarketHaircutStateAccount::space(),
        seeds = [state_v3::MarketHaircutStateAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MaturePosition<'info> {
    /// Permissionless — anyone can crank.
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, haircut_state.market.as_ref()],
        bump = haircut_state.bump,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,

    /// The Position the per-position haircut state attaches to.
    /// Re-derive its PDA via the account's stored fields so we don't
    /// have to thread market/trader through ix args.
    #[account(
        seeds = [state::PositionAccount::SEED, position.load()?.market.as_ref(), position.load()?.trader.as_ref()],
        bump = position.load()?.bump,
        constraint = position.load()?.market == haircut_state.market @ FlashBookError::HaircutStateMismatch,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,

    #[account(
        mut,
        seeds = [
            state_v3::PositionHaircutStateAccount::SEED,
            position.load()?.market.as_ref(),
            position.key().as_ref(),
        ],
        bump = position_haircut.bump,
        constraint = position_haircut.market == haircut_state.market @ FlashBookError::HaircutStateMismatch,
        constraint = position_haircut.position == position.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub position_haircut: Box<Account<'info, state_v3::PositionHaircutStateAccount>>,
}

#[derive(Accounts)]
pub struct ConvertPosition<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, haircut_state.market.as_ref()],
        bump = haircut_state.bump,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,

    #[account(
        seeds = [state::PositionAccount::SEED, position.load()?.market.as_ref(), position.load()?.trader.as_ref()],
        bump = position.load()?.bump,
        constraint = position.load()?.market == haircut_state.market @ FlashBookError::HaircutStateMismatch,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,

    #[account(
        mut,
        seeds = [
            state_v3::PositionHaircutStateAccount::SEED,
            position.load()?.market.as_ref(),
            position.key().as_ref(),
        ],
        bump = position_haircut.bump,
        constraint = position_haircut.market == haircut_state.market @ FlashBookError::HaircutStateMismatch,
        constraint = position_haircut.position == position.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub position_haircut: Box<Account<'info, state_v3::PositionHaircutStateAccount>>,
}

#[derive(Accounts)]
pub struct FlushHaircutDust<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, haircut_state.market.as_ref()],
        bump = haircut_state.bump,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,

    #[account(
        mut,
        seeds = [state::InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, state::InsuranceFundAccount>>,
}

#[derive(Accounts)]
pub struct InitPositionHaircutState<'info> {
    /// Permissionless — anyone can pay for lazy init.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        seeds = [state::PositionAccount::SEED, position.load()?.market.as_ref(), position.load()?.trader.as_ref()],
        bump = position.load()?.bump,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,

    /// Market haircut state — verifies the market is opted in.
    #[account(
        seeds = [state_v3::MarketHaircutStateAccount::SEED, position.load()?.market.as_ref()],
        bump = haircut_state.bump,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,

    #[account(
        init,
        payer = payer,
        space = state_v3::PositionHaircutStateAccount::space(),
        seeds = [
            state_v3::PositionHaircutStateAccount::SEED,
            position.load()?.market.as_ref(),
            position.key().as_ref(),
        ],
        bump,
    )]
    pub position_haircut: Box<Account<'info, state_v3::PositionHaircutStateAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReleaseGainToHaircut<'info> {
    pub authority: Signer<'info>,

    #[account(
        seeds = [state::MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, state::MarketAccount>>,

    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, position.load()?.market.as_ref(), position.load()?.trader.as_ref()],
        bump = position.load()?.bump,
        constraint = position.load()?.market == market.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,

    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, position.load()?.trader.as_ref()],
        bump = trader_state.load()?.bump,
        constraint = trader_state.load()?.trader == position.load()?.trader @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, market.key().as_ref()],
        bump = haircut_state.bump,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,

    #[account(
        mut,
        seeds = [
            state_v3::PositionHaircutStateAccount::SEED,
            position.load()?.market.as_ref(),
            position.key().as_ref(),
        ],
        bump = position_haircut.bump,
        constraint = position_haircut.market == market.key() @ FlashBookError::HaircutStateMismatch,
        constraint = position_haircut.position == position.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub position_haircut: Box<Account<'info, state_v3::PositionHaircutStateAccount>>,
}

#[derive(Accounts)]
pub struct VerifyHaircutInvariants<'info> {
    /// Permissionless — anyone can crank.
    pub keeper: Signer<'info>,

    #[account(
        seeds = [state_v3::MarketHaircutStateAccount::SEED, haircut_state.market.as_ref()],
        bump = haircut_state.bump,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,
}

#[derive(Accounts)]
pub struct SetEnvelopeConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [state::MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
    )]
    pub market: Box<Account<'info, state::MarketAccount>>,

    /// init_if_needed so the same ix handles both first-set and update.
    #[account(
        init_if_needed,
        payer = authority,
        space = state_v3::MarketEnvelopeConfigAccount::space(),
        seeds = [state_v3::MarketEnvelopeConfigAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub envelope_config: Box<Account<'info, state_v3::MarketEnvelopeConfigAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VerifyProtocolSolvency<'info> {
    /// Permissionless caller.
    pub keeper: Signer<'info>,

    #[account(
        seeds = [state::InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, state::InsuranceFundAccount>>,

    #[account(
        seeds = [FlpExposureAccount::SEED],
        bump = flp_exposure.bump,
    )]
    pub flp_exposure: Box<Account<'info, FlpExposureAccount>>,

    /// The protocol's quote-token vault.
    #[account(address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,
}

#[derive(Accounts)]
pub struct VerifyEnvelopeConfig<'info> {
    pub keeper: Signer<'info>,

    #[account(
        seeds = [state_v3::MarketEnvelopeConfigAccount::SEED, envelope_config.market.as_ref()],
        bump = envelope_config.bump,
    )]
    pub envelope_config: Box<Account<'info, state_v3::MarketEnvelopeConfigAccount>>,
}

#[derive(Accounts)]
pub struct GateEnvelopePriceMove<'info> {
    pub keeper: Signer<'info>,

    #[account(
        seeds = [state_v3::MarketEnvelopeConfigAccount::SEED, envelope_config.market.as_ref()],
        bump = envelope_config.bump,
    )]
    pub envelope_config: Box<Account<'info, state_v3::MarketEnvelopeConfigAccount>>,
}

#[derive(Accounts)]
pub struct InitializeSideAccrual<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [state::MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
    )]
    pub market: Box<Account<'info, state::MarketAccount>>,

    #[account(
        init,
        payer = authority,
        space = state_v3::MarketSideAccrualAccount::space(),
        seeds = [state_v3::MarketSideAccrualAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub side_accrual: Box<Account<'info, state_v3::MarketSideAccrualAccount>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SeedResidual<'info> {
    pub authority: Signer<'info>,

    #[account(
        seeds = [state::MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, state::MarketAccount>>,

    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, market.key().as_ref()],
        bump = haircut_state.bump,
        constraint = haircut_state.market == market.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,
}

/// Map pure-math haircut errors to FlashBookError codes. Used by every
/// haircut ix that calls into `matcher::haircut`.
fn map_haircut_error(e: matcher::haircut::HaircutError) -> anchor_lang::error::Error {
    use matcher::haircut::HaircutError::*;
    match e {
        Overflow => error!(FlashBookError::ArithmeticOverflow),
        Underflow => error!(FlashBookError::ArithmeticUnderflow),
        InvertedWindow => error!(FlashBookError::HaircutInvertedWindow),
        WindowTooLarge => error!(FlashBookError::HaircutWindowTooLarge),
        ZeroGain => error!(FlashBookError::HaircutZeroGain),
    }
}

/// Wave 26b — shared runtime gate used by all three oracle-update ix.
///
/// When `envelope_config` is `Some(...)`:
///   1. If `last_observed_slot == 0` (first observation), the gate is
///      skipped (no prior price to compare). `now_slot`/`new_price`
///      are recorded as the new baseline.
///   2. Otherwise: compute `dt = now_slot - last_observed_slot`, call
///      `gate_price_move(last_price, new_price, dt,
///      max_price_move_bps_per_slot)`. Reject → bubble up.
///   3. On success: record `(now_slot, new_price)` and bump
///      `gate_passes`.
///
/// When `envelope_config` is `None`: no-op (legacy behavior).
fn gate_oracle_update<'info>(
    envelope_config: Option<&mut Box<Account<'info, state_v3::MarketEnvelopeConfigAccount>>>,
    new_price_ticks: u64,
    now_slot: u64,
) -> Result<()> {
    let Some(cfg) = envelope_config else {
        return Ok(());
    };
    if cfg.last_observed_slot == 0 {
        // First observation — seed and skip gate.
        cfg.last_observed_slot = now_slot;
        cfg.last_observed_price_ticks = new_price_ticks;
        cfg.gate_passes = cfg.gate_passes.saturating_add(1);
        return Ok(());
    }
    let dt = now_slot.saturating_sub(cfg.last_observed_slot);
    // Same-slot updates are allowed iff the new price equals the old
    // one (no movement). gate_price_move rejects strict zero dt.
    if dt == 0 {
        if new_price_ticks == cfg.last_observed_price_ticks {
            return Ok(());
        }
        cfg.gate_rejects = cfg.gate_rejects.saturating_add(1);
        return Err(error!(FlashBookError::EnvelopeSameSlotMove));
    }
    let last_price = cfg.last_observed_price_ticks;
    matcher::envelope::gate_price_move(
        last_price,
        new_price_ticks,
        dt,
        cfg.max_price_move_bps_per_slot,
    )
    .map_err(|e| {
        cfg.gate_rejects = cfg.gate_rejects.saturating_add(1);
        map_envelope_error(e)
    })?;
    cfg.last_observed_slot = now_slot;
    cfg.last_observed_price_ticks = new_price_ticks;
    cfg.gate_passes = cfg.gate_passes.saturating_add(1);
    Ok(())
}

/// Map pure-math envelope errors to FlashBookError codes. Wave 26.
fn map_envelope_error(e: matcher::envelope::EnvelopeError) -> anchor_lang::error::Error {
    use matcher::envelope::EnvelopeError::*;
    match e {
        PriceCapZero | PriceCapTooLarge => error!(FlashBookError::EnvelopePriceCapInvalid),
        AccrualDtZero | AccrualDtTooLarge => error!(FlashBookError::EnvelopeAccrualWindowInvalid),
        FundingCapTooLarge => error!(FlashBookError::EnvelopeFundingCapInvalid),
        MaintenanceZero | MaintenanceTooLarge => error!(FlashBookError::EnvelopeMaintenanceInvalid),
        LiqFeeTooLarge => error!(FlashBookError::EnvelopeLiqFeeInvalid),
        Overflow => error!(FlashBookError::ArithmeticOverflow),
        SameSlotMove => error!(FlashBookError::EnvelopeSameSlotMove),
        PriceMoveExceedsCap => error!(FlashBookError::EnvelopePriceMoveExceedsCap),
        EnvelopeViolated { .. } => error!(FlashBookError::EnvelopeViolated),
    }
}

#[derive(Accounts)]
pub struct DelegateMarketBook<'info> {
    /// Pays for the delegation buffer + delegation record allocation.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
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
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
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
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
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
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
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
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
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

/// Grow a market_book PDA in place (`expand_market_book`). Same authority
/// gate and seed binding as `InitMarketBook`; the book is reallocated in the
/// handler so it's `UncheckedAccount` (Anchor can't size a variable account).
#[derive(Accounts)]
pub struct ExpandMarketBook<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
        constraint = market.authority == authority.key() @ FlashBookError::Unauthorized,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: PDA we own; reallocated via `AccountInfo::realloc` in the
    /// handler. The seeds derivation binds it to this market; the handler
    /// re-asserts program ownership before growing it.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

/// ER-side commit (and commit-and-undelegate). Permissionless like Phoenix-ER:
/// the Magic program + validator enforce commit semantics, and the CPI fails
/// loudly on a non-delegated account. Authority control lives on the base-layer
/// `delegate_*` instructions (audit ER-1).
#[derive(Accounts)]
pub struct CommitMarketBook<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: the delegated market_book PDA being committed. On the ER its
    /// owner is the delegation program; `cpi_commit` (→ Magic program) fails
    /// loudly if this is not actually a delegated account.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    /// CHECK: MagicBlock magic context (writable). Address-pinned to MAGIC_CONTEXT_ID.
    #[account(mut, address = er::MAGIC_CONTEXT_ID)]
    pub magic_context: UncheckedAccount<'info>,

    /// CHECK: MagicBlock magic program. Address-pinned to MAGIC_PROGRAM_ID.
    #[account(address = er::MAGIC_PROGRAM_ID)]
    pub magic_program: UncheckedAccount<'info>,
}

/// Base-layer undelegation callback (audit ER-2). Mirrors the
/// `ephemeral-rollups-sdk` `InitializeAfterUndelegation` account contract
/// exactly: `[delegated_account, buffer, payer, system_program]`, all
/// unchecked (the delegation program is the caller). The handler asserts
/// `buffer.is_signer && buffer.owner == DELEGATION_PROGRAM_ID`.
#[derive(Accounts)]
pub struct ProcessUndelegation<'info> {
    /// CHECK: the delegated PDA being returned to this program; re-created and
    /// filled from `buffer` by the handler.
    #[account(mut)]
    pub delegated_account: UncheckedAccount<'info>,

    /// CHECK: the delegation program's committed-state buffer (signer; owner
    /// == DELEGATION_PROGRAM_ID — both verified in the handler).
    pub buffer: UncheckedAccount<'info>,

    /// CHECK: rent payer for the re-created PDA.
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    /// CHECK: system program (create_account / allocate / assign).
    pub system_program: UncheckedAccount<'info>,
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
pub struct SetInsurancePauseThreshold<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Box<Account<'info, InsuranceFundAccount>>,
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
    pub trader_state: AccountLoader<'info, TraderStateAccount>,
    pub system_program: Program<'info, System>,
}

/// Sub-account scaffolding (Phase 1). The sub PDA extends the main
/// seed with a `sub_index` byte; sub_index = 0 is reserved for the
/// main account (which uses the legacy `OpenTraderState` ix), so this
/// ix accepts sub_index in 1..=255.
#[derive(Accounts)]
#[instruction(sub_index: u8)]
pub struct OpenTraderSubAccount<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,
    #[account(
        init,
        payer = trader,
        space = TraderStateAccount::space(),
        seeds = [TraderStateAccount::SEED, trader.key().as_ref(), &[sub_index]],
        bump,
    )]
    pub trader_sub_account: AccountLoader<'info, TraderStateAccount>,
    pub system_program: Program<'info, System>,
}

/// Move collateral between a trader's main account and one of their
/// sub-accounts. Same trader signs both sides; both PDAs are derived
/// from the same `trader` pubkey so the seeds alone enforce that the
/// caller can only touch their own collateral.
#[derive(Accounts)]
#[instruction(sub_index: u8)]
pub struct TransferBetweenSubAccounts<'info> {
    pub trader: Signer<'info>,
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = main_trader_state.load()?.bump,
        constraint = main_trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub main_trader_state: AccountLoader<'info, TraderStateAccount>,
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, trader.key().as_ref(), &[sub_index]],
        bump = sub_trader_state.load()?.bump,
        constraint = sub_trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub sub_trader_state: AccountLoader<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct SetTraderReferrer<'info> {
    /// Trader signs to set their own referrer. One-time-write enforced
    /// inside the handler.
    pub trader: Signer<'info>,

    /// Phase 2d: relaxed seed — accepts main or sub-account TraderState.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,
}

#[derive(Accounts)]
pub struct SetTraderDelegate<'info> {
    /// The trader signs to set/clear their own delegate. Only the trader
    /// can change this field — the delegate cannot rotate themselves out.
    pub trader: Signer<'info>,

    /// Phase 2d: relaxed seed — accepts main or sub-account TraderState.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,
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

    /// Phase 2d: dropped strict seed so the authority can target a
    /// sub-account TraderState too. No additional constraint needed —
    /// the `authority` Signer is gated by `insurance_fund.authority`
    /// above, so only the protocol authority can call this ix.
    #[account(mut)]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,
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
        constraint = trader_state.load()?.trader == position.load()?.trader @ FlashBookError::WrongTrader,
        constraint = trader_state.load()?.is_authorized(&authority.key()) @ FlashBookError::Unauthorized,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader_state.key().as_ref()],
        bump = position.load()?.bump,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,
}

#[derive(Accounts)]
pub struct SweepCollateral<'info> {
    /// Master authority (trader OR delegate of BOTH source and destination
    /// trader_states). Authorization is checked against each via
    /// `is_authorized` in the handler.
    pub authority: Signer<'info>,

    /// Phase 2d: dropped strict seeds so sweep can move collateral
    /// between main and sub-account TraderStates (and between two
    /// sub-accounts). The handler's `is_authorized(&authority.key())`
    /// gate on each side ensures the signer owns/delegates BOTH
    /// endpoints.
    #[account(mut)]
    pub from_state: AccountLoader<'info, TraderStateAccount>,

    #[account(mut)]
    pub to_state: AccountLoader<'info, TraderStateAccount>,
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
        bump = vault_trader_state.load()?.bump,
        constraint = vault_trader_state.key() == vault.trader_state @ FlashBookError::OutOfRange,
    )]
    pub vault_trader_state: AccountLoader<'info, TraderStateAccount>,

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

    /// Phase 2d: relaxed seed — accepts main or sub-account TraderState.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,
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

    /// Wave 26b — optional envelope gate. When supplied, the proposed
    /// price move is checked against `gate_price_move(p_last, p_new,
    /// dt_slots, max_price_move_bps_per_slot)`. Reject → entire ix
    /// reverts. When omitted: pre-Wave-26b behavior (no gate).
    #[account(
        mut,
        seeds = [state_v3::MarketEnvelopeConfigAccount::SEED, market.key().as_ref()],
        bump = envelope_config.bump,
        constraint = envelope_config.market == market.key() @ FlashBookError::EnvelopePriceCapInvalid,
    )]
    pub envelope_config: Option<Box<Account<'info, state_v3::MarketEnvelopeConfigAccount>>>,
}

/// V3 mark-engine: permissionless `settle_mark` accounts. The caller pays
/// the tx fee; the per-market `mark_settle_min_slots` rate-limit
/// prevents spam. No PDA mutations beyond the market itself.
#[derive(Accounts)]
pub struct SettleMark<'info> {
    /// Permissionless caller (anyone can settle).
    pub caller: Signer<'info>,

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
        bump = trader_state.load()?.bump,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        seeds = [state::FeeTiersAccount::SEED],
        bump = fee_tiers.bump,
    )]
    pub fee_tiers: Account<'info, state::FeeTiersAccount>,
}

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    pub trader: Signer<'info>,

    /// Phase 2d: dropped strict `seeds = [SEED, trader.key().as_ref()]`
    /// so this ix accepts either the main TraderState PDA or any sub-
    /// account TraderState PDA. The `.trader == trader.key()`
    /// constraint proves the signer owns the trader_state passed in
    /// (sub PDAs are created with `.trader = parent_wallet.key()`).
    /// Anchor's `AccountLoader<'info, TraderStateAccount>` still enforces
    /// program ownership + discriminator match.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

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

    /// Phase 2d: relaxed seed — see DepositCollateral above.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

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

    /// Phase 2d: relaxed seed — see DepositCollateral above.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

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

/// Shared Accounts context for `set_position_isolated` and
/// `set_position_cross`. Both ixs transfer collateral between the
/// trader's pooled `TraderStateAccount.collateral_quote_lots` and the
/// Phase 2c migration — move a Position from the LEGACY PDA
/// `[POS_SEED, market, wallet]` to the NEW PDA
/// `[POS_SEED, market, trader_state.key()]`. The trader signs to
/// receive the rent refund on the closed legacy account. The new
/// account is `init`'d (it must not pre-exist) so this ix is a
/// one-shot per (wallet, market). Failure to land leaves both PDAs in
/// a clean state for retry.
#[derive(Accounts)]
pub struct MigratePositionToTraderStateKey<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    #[account(
        seeds = [TraderStateAccount::SEED, trader.key().as_ref()],
        bump = trader_state.load()?.bump,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    /// LEGACY position at `[POS_SEED, market, wallet]`. Closed by this
    /// ix; rent refunded to `trader`.
    #[account(
        mut,
        close = trader,
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader.key().as_ref()],
        bump = legacy_position.load()?.bump,
    )]
    pub legacy_position: AccountLoader<'info, state::PositionAccount>,

    /// NEW position at the Phase 2c PDA. `init` (not init_if_needed) so
    /// migration can only run when the new slot is empty — protects
    /// against accidental double-migration overwriting state.
    #[account(
        init,
        payer = trader,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader_state.key().as_ref()],
        bump,
    )]
    pub new_position: AccountLoader<'info, state::PositionAccount>,

    pub system_program: Program<'info, System>,
}

/// Shared Accounts context for `set_position_isolated` and
/// `set_position_cross`. Both ixs transfer collateral between the
/// trader's pooled `TraderStateAccount.collateral_quote_lots` and the
/// target position's `PositionAccount.collateral_quote_lots`, gated by
/// a post-transfer health check on every other position the trader
/// owns (passed via remaining_accounts).
#[derive(Accounts)]
pub struct SetPositionMarginMode<'info> {
    pub trader: Signer<'info>,

    /// Phase 2d: relaxed seed — accepts main or sub-account
    /// TraderState. The `target_position` PDA seeds use
    /// `trader_state.key()` (Phase 2c), so positions are correctly
    /// scoped to whichever TraderState transitioned.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        seeds = [MarketAccount::SEED, target_market.base_mint.as_ref(), target_market.quote_mint.as_ref()],
        bump = target_market.bump,
    )]
    pub target_market: Account<'info, MarketAccount>,

    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, target_market.key().as_ref(), trader_state.key().as_ref()],
        bump = target_position.load()?.bump,
    )]
    pub target_position: AccountLoader<'info, state::PositionAccount>,
    // remaining_accounts: alternating (market, position) pairs for OTHER
    // open positions the trader holds (NOT the target). Each pair is
    // validated by the handler — owner==program_id, trader matches, etc.
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

    /// The trader being settled — informational only post-Phase-2d.
    /// Settle is permissionless and the identity binding is now the
    /// (trader_state, position) PDA pair, not the `trader` field on
    /// this struct. Kept on the Accounts struct for backwards-compat
    /// with off-chain callers that already pass it.
    /// CHECK: validated against `trader_state.trader` in the handler.
    pub trader: UncheckedAccount<'info>,

    /// Phase 2d: dropped strict seed so settle_funding can operate on
    /// sub-account TraderStates too. The position PDA below binds the
    /// pair via Phase 2c trader_state.key()-based seeds.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader_state.key().as_ref()],
        bump,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,

    /// RISK-1: funding is a money-moving ix and MUST delta-track the
    /// solvency residual (`V − C_tot − I`), exactly like deposit/withdraw/
    /// fee/liquidation. Without it, lazy/entry-priced funding silently
    /// mints or burns protocol collateral and the haircut/kill-switch
    /// baseline drifts. Bound to this market's haircut PDA.
    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, market.key().as_ref()],
        bump = haircut_state.bump,
        constraint = haircut_state.market == market.key() @ FlashBookError::WrongMarket,
    )]
    pub haircut_state: Box<Account<'info, state_v3::MarketHaircutStateAccount>>,
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

    /// Phase 2d: dropped strict seed so ApplyFill can route fills to
    /// sub-account TraderStates. The position PDAs below bind each
    /// trader_state to its position via Phase 2c trader_state.key()-
    /// based derivation. Phase 2e (RestingOrderV2.sub_index) will
    /// extend this so the sequencer can identify which sub-account a
    /// resting order belongs to at fill time; until then this relaxation
    /// is structurally safe — the position seed pair is the binding —
    /// but only the main TraderState is reachable via the current
    /// matcher fill-routing logic.
    #[account(mut)]
    pub taker_trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(mut)]
    pub maker_trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), taker_trader_state.key().as_ref()],
        bump,
    )]
    pub taker_position: AccountLoader<'info, state::PositionAccount>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), maker_trader_state.key().as_ref()],
        bump,
    )]
    pub maker_position: AccountLoader<'info, state::PositionAccount>,

    /// WAVE 22 phase 2: optional global fee-tier table. When supplied,
    /// per-trader maker rebate / taker fee bps are resolved from this
    /// account against the trader's `volume_30d_quote_lots` (HL/Binance
    /// pattern). When omitted (None), apply_fill falls back to the flat
    /// `market.params.{maker_rebate_bps, taker_fee_bps}` per the
    /// pre-tier behavior. Singleton PDA at `[b"fee_tiers"]`.
    pub fee_tiers: Option<Box<Account<'info, state::FeeTiersAccount>>>,

    /// WAVE 24d — optional per-market H-haircut state. When provided
    /// (along with the per-position haircut accounts below), positive
    /// realized PnL on this fill routes into the position's reserve
    /// instead of crediting collateral directly. Losses always debit
    /// collateral (loss seniority).
    ///
    /// When omitted: bit-for-bit identical to pre-Wave-24d behavior.
    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, market.key().as_ref()],
        bump = market_haircut.bump,
        constraint = market_haircut.market == market.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub market_haircut: Option<Box<Account<'info, state_v3::MarketHaircutStateAccount>>>,

    /// Per-taker-position haircut state. Must be present iff
    /// `market_haircut` is present. The taker_position must already
    /// have been initialized via `init_position_haircut_state`.
    #[account(
        mut,
        seeds = [
            state_v3::PositionHaircutStateAccount::SEED,
            market.key().as_ref(),
            taker_position.key().as_ref(),
        ],
        bump = taker_position_haircut.bump,
        constraint = taker_position_haircut.market == market.key() @ FlashBookError::HaircutStateMismatch,
        constraint = taker_position_haircut.position == taker_position.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub taker_position_haircut: Option<Box<Account<'info, state_v3::PositionHaircutStateAccount>>>,

    /// Per-maker-position haircut state. Same shape as taker.
    #[account(
        mut,
        seeds = [
            state_v3::PositionHaircutStateAccount::SEED,
            market.key().as_ref(),
            maker_position.key().as_ref(),
        ],
        bump = maker_position_haircut.bump,
        constraint = maker_position_haircut.market == market.key() @ FlashBookError::HaircutStateMismatch,
        constraint = maker_position_haircut.position == maker_position.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub maker_position_haircut: Option<Box<Account<'info, state_v3::PositionHaircutStateAccount>>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceBasketOrderV2<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    /// Phase 2d: relaxed seed — accepts main or sub-account TraderState.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

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
        seeds = [state::PositionAccount::SEED, market_a.key().as_ref(), trader_state.key().as_ref()],
        bump,
    )]
    pub position_a: AccountLoader<'info, state::PositionAccount>,

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
        seeds = [state::PositionAccount::SEED, market_b.key().as_ref(), trader_state.key().as_ref()],
        bump,
    )]
    pub position_b: AccountLoader<'info, state::PositionAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceBasketOrderNV2<'info> {
    pub trader: Signer<'info>,

    /// Phase 2d: relaxed seed — accepts main or sub-account TraderState.
    #[account(
        mut,
        constraint = trader_state.load()?.trader == trader.key() @ FlashBookError::WrongTrader,
    )]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

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
    ///
    /// Phase 2c migration: trigger orders are wallet-scoped (the
    /// TriggerOrderAccount carries the wallet pubkey, not a
    /// trader_state PDA). Anchor cannot derive the trader_state PDA
    /// inside a seeds expression, so we drop the strict seed
    /// constraint here and validate the position's identity via
    /// data-field checks in the handler:
    ///   require!(position.market == market.key())
    ///   require!(position.trader == trigger_order.trader)
    /// Sub-account triggers will require a TriggerOrderAccount
    /// schema update (add `sub_index`) before they're enableable.
    #[account(
        constraint = position.load()?.market == market.key() @ FlashBookError::WrongMarket,
        constraint = position.load()?.trader == trigger_order.trader @ FlashBookError::WrongTrader,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,
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
    /// Phase 2d: view-only ix; relaxed seed accepts any TraderState.
    pub trader_state: AccountLoader<'info, TraderStateAccount>,
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

    /// Phase 2d: dropped strict seed — see ApplyFill above.
    #[account(mut)]
    pub taker_trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        init_if_needed,
        payer = sequencer,
        space = state::PositionAccount::space(),
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), taker_trader_state.key().as_ref()],
        bump,
    )]
    pub taker_position: AccountLoader<'info, state::PositionAccount>,

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

    /// WAVE 24d — optional H-haircut routing on the FLP fill path.
    /// Same opt-in shape as ApplyFill: provide market_haircut +
    /// taker_position_haircut to route the trader's positive PnL into
    /// the reserve. FLP itself doesn't accumulate realized PnL on a
    /// Position PDA — there's nothing maker-side to route, so no
    /// maker_position_haircut here.
    #[account(
        mut,
        seeds = [state_v3::MarketHaircutStateAccount::SEED, market.key().as_ref()],
        bump = market_haircut.bump,
        constraint = market_haircut.market == market.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub market_haircut: Option<Box<Account<'info, state_v3::MarketHaircutStateAccount>>>,

    #[account(
        mut,
        seeds = [
            state_v3::PositionHaircutStateAccount::SEED,
            market.key().as_ref(),
            taker_position.key().as_ref(),
        ],
        bump = taker_position_haircut.bump,
        constraint = taker_position_haircut.market == market.key() @ FlashBookError::HaircutStateMismatch,
        constraint = taker_position_haircut.position == taker_position.key() @ FlashBookError::HaircutStateMismatch,
    )]
    pub taker_position_haircut: Option<Box<Account<'info, state_v3::PositionHaircutStateAccount>>>,

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

    /// Phase 2d: dropped strict seed — sub-account TraderStates have
    /// PDAs at `[SEED, wallet, &[sub_index]]` which the self-referential
    /// seed `[SEED, trader_state.trader.as_ref()]` does not match.
    /// Identity is enforced by the `execution_position` PDA below,
    /// which is keyed on `trader_state.key()` (Phase 2c). The matched
    /// pair is what binds the liquidatee.
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        seeds = [state::PositionAccount::SEED, execution_market.key().as_ref(), trader_state.key().as_ref()],
        bump = execution_position.load()?.bump,
    )]
    pub execution_position: AccountLoader<'info, state::PositionAccount>,
    // remaining_accounts: alternating [Market, Position] pairs for the
    // trader's other markets (cross-margin assessment).
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

    /// Phase 2d: dropped strict seed — see LiquidatePortfolioV2 above.
    /// The `position` PDA below binds liquidatee identity via Phase 2c
    /// trader_state.key()-based derivation.
    #[account(mut)]
    pub trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        init_if_needed,
        payer = caller,
        space = TraderStateAccount::space(),
        seeds = [TraderStateAccount::SEED, caller.key().as_ref()],
        bump,
    )]
    pub caller_trader_state: AccountLoader<'info, TraderStateAccount>,

    /// MUT — wave 19e fixes a v1 latent bug: v1's LiquidatePosition has
    /// this WITHOUT `mut`, so writes to `unhealthy_since_slot` /
    /// `last_liquidated_at_slot` silently don't persist. Anchor doesn't
    /// serialize back accounts not declared mut. v2 correctly marks it.
    #[account(
        mut,
        seeds = [state::PositionAccount::SEED, market.key().as_ref(), trader_state.key().as_ref()],
        bump = position.load()?.bump,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,

    pub system_program: Program<'info, System>,
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

    /// Phase 2d: dropped strict seed — see LiquidatePortfolioV2 above.
    #[account(mut)]
    pub underwater_trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [
            state::PositionAccount::SEED,
            market.key().as_ref(),
            underwater_trader_state.key().as_ref(),
        ],
        bump = underwater_position.load()?.bump,
    )]
    pub underwater_position: AccountLoader<'info, state::PositionAccount>,

    /// Phase 2d: dropped strict seed — see LiquidatePortfolioV2 above.
    #[account(mut)]
    pub counter_trader_state: AccountLoader<'info, TraderStateAccount>,

    #[account(
        mut,
        seeds = [
            state::PositionAccount::SEED,
            market.key().as_ref(),
            counter_trader_state.key().as_ref(),
        ],
        bump = counter_position.load()?.bump,
    )]
    pub counter_position: AccountLoader<'info, state::PositionAccount>,
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
pub struct MarketBookExpandedEvent {
    pub market: Pubkey,
    pub market_book: Pubkey,
    pub old_bytes: u32,
    pub new_bytes: u32,
    /// Node capacity after this expansion = (new_bytes − 264) / 96.
    pub max_nodes: u32,
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

/// CLOB summary — emitted at the END of `place_taker_order_v2`. One
/// per CLOB-style placement, regardless of how many fills landed.
/// Mirrors Phoenix's "OrderPlaced + Fills" composite event but as a
/// single summary so off-chain consumers can detect "this is a CLOB
/// path order" vs a pure resting limit.
#[event]
pub struct TakerOrderClearedEvent {
    pub market: Pubkey,
    pub taker: Pubkey,
    pub side: u8,
    /// Original size requested.
    pub taker_size_lots: u64,
    /// Sum of all `FillEntry.size_lots` emitted in the trailing
    /// `FillBatchEvent` from this ix.
    pub filled_lots: u64,
    /// Size remaining after the walk that was inserted as a resting
    /// limit (0 if fully filled or IOC).
    pub residual_resting_lots: u64,
    /// Number of distinct maker orders matched.
    pub match_count: u32,
    pub taker_order_id: u64,
    /// Hypertree node index of the residual resting order (NIL if none).
    pub residual_node_index: u32,
}

/// Per-fill payload inside a `FillBatchEvent`. The redundant `market`
/// / `taker` / `taker_side` / `taker_id` fields live once on the parent.
/// `maker == Pubkey::default()` signals an FLP virtual-quote fill — the
/// off-chain sequencer dispatches `apply_flp_fill` for those entries
/// and `apply_fill` for the rest.
#[derive(Clone, Copy, AnchorSerialize, AnchorDeserialize)]
pub struct FillEntry {
    pub maker: Pubkey,
    pub size_lots: u64,
    pub price_ticks: u64,
    pub maker_id: u64,
    /// Phase 2e — sub-account index of the matched maker. The
    /// sequencer reads this to derive
    /// `[STATE_SEED, maker.as_ref(), &[maker_sub_index]]` and pass that
    /// account as `maker_trader_state` to ApplyFill. `0` = main.
    pub maker_sub_index: u8,
}

/// CLOB hot-path fill feed — one event per `place_taker_order_v2` ix
/// carrying every match in a single `Vec<FillEntry>`. The off-chain
/// sequencer iterates `fills` and dispatches `apply_fill` / `apply_flp_fill`
/// per entry on mainnet; `taker_id` is the dedup key for the outbox.
#[event]
pub struct FillBatchEvent {
    pub market: Pubkey,
    pub taker: Pubkey,
    pub taker_side: u8,
    pub taker_id: u64,
    pub fills: Vec<FillEntry>,
    /// Phase 2e — sub-account index of the taker. Same routing rule
    /// as `FillEntry.maker_sub_index`, but for the taker side.
    pub taker_sub_index: u8,
}

/// How many price levels per side `view_book_depth_v2` returns.
/// Capped to keep the event log payload bounded; off-chain depth
/// reconstruction watches `OrderPlacedV2Event` + `OrderCancelledV2Event`
/// for the long tail.
pub const BOOK_DEPTH_LEVELS: usize = 4;

/// Per-side ceiling on resting orders the taker can walk in a single
/// `place_taker_order_v2` ix. Bounded by the BPF compute budget — at
/// 256 the worst-case walk consumes ~50K CU, well within the per-tx
/// 200K default and the 1.4M maximum.
pub const MAX_BATCH_ORDERS_PER_SIDE_V2: usize = 256;

/// HL withdrawal floor — wave 20b. When a trader pulls collateral from
/// `partial_withdraw_collateral` with positions still open, the remaining
/// collateral must satisfy `>= max(IM_required, WITHDRAWAL_FLOOR_BPS ×
/// total_notional)`. 1000 bps = 10% — Hyperliquid's exact value. Defends
/// against deposit-then-withdraw attacks where a trader briefly tops up
/// to satisfy IM, places a trade, then yanks the temporary collateral
/// leaving only enough for maintenance margin.
pub const WITHDRAWAL_FLOOR_BPS: u32 = 1000;

#[event]
pub struct OrderCancelledV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub order_seq: u64,
    pub side: u8,
    pub node_index: u32,
    pub total_orders_after: u32,
}

/// Emitted by `open_trader_sub_account`. The sub_account PDA is at
/// `[TraderStateAccount::SEED, trader.key(), &[sub_index]]`; off-chain
/// reconstruction uses this event to map (trader, sub_index) → PDA.
#[event]
pub struct SubAccountOpenedEvent {
    pub trader: Pubkey,
    pub sub_index: u8,
    pub sub_account: Pubkey,
}

/// Emitted by `transfer_main_to_sub` and `transfer_sub_to_main`.
/// `direction` is 0 for main→sub, 1 for sub→main. Off-chain wallets use
/// this to update per-sub-account balance views.
#[event]
pub struct SubAccountTransferEvent {
    pub trader: Pubkey,
    pub sub_index: u8,
    pub amount_quote_lots: u64,
    pub direction: u8,
    pub main_balance_after: u64,
    pub sub_balance_after: u64,
}

/// Emitted by `migrate_position_to_trader_state_key`. Carries the
/// position's pre-migration core state for off-chain audit; indexers
/// re-key from the legacy `[POS_SEED, market, wallet]` PDA to the
/// new `[POS_SEED, market, trader_state]` PDA after this event lands.
#[event]
pub struct PositionMigratedEvent {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub size_lots: u64,
    pub collateral_quote_lots: u64,
}

/// Emitted by `set_position_isolated` and `set_position_cross`.
/// `mode`: 0 = cross, 1 = isolated. The two collateral fields show the
/// post-transition state for the trader's pooled vs per-position pools.
#[event]
pub struct PositionMarginModeChangedEvent {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub new_isolated_collateral_quote_lots: u64,
    pub new_cross_collateral_quote_lots: u64,
    pub mode: u8,
}

/// Emitted by `cancel_all_v2`. `cancelled_count` is 0 when the trader had
/// no resting orders (the ix is silently a no-op in that case — useful as
/// defensive cleanup from a bot). Bounded by `MAX_CANCELS_PER_IX_V2`.
#[event]
pub struct BulkOrderCancelledV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub cancelled_count: u32,
    pub total_orders_after: u32,
}

/// Emitted by `modify_order_v2` after the atomic cancel+place lands.
/// `new_order_id` is the encoded id of the replacement order — off-chain
/// consumers track open orders by this id (the old id is dead the moment
/// `cancel` lands inside the ix; no one should reference it again).
#[event]
pub struct OrderModifiedV2Event {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub side: u8,
    pub old_seq: u64,
    pub new_seq: u64,
    pub new_price_ticks: u64,
    pub new_size_lots: u64,
    pub new_node_index: u32,
    pub new_order_id: u64,
}

/// Hard cap on the number of resting orders `cancel_all_v2` can remove in
/// a single ix. Bounded by the BPF compute budget — at 24 cancels the
/// worst-case (24 removes × ~500 CU each + 100-node walk) lands at
/// ~14K CU, well inside the 200K default.
pub const MAX_CANCELS_PER_IX_V2: usize = 24;

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

// ─── Wave 24b — H-haircut events ────────────────────────────────────

#[event]
pub struct HaircutInitializedEvent {
    pub market: Pubkey,
    pub h_min_slots: u64,
    pub h_max_slots: u64,
    pub initial_residual_quote_lots: u128,
}

#[event]
pub struct PositionMaturedEvent {
    pub market: Pubkey,
    pub position: Pubkey,
    pub matured_delta: u64,
    pub matured_pos_after: u64,
}

#[event]
pub struct PositionConvertedEvent {
    pub market: Pubkey,
    pub position: Pubkey,
    pub credit_quote_lots: u64,
    pub dust_quote_lots: u64,
    pub h_scaled: u128,
}

#[event]
pub struct HaircutDustFlushedEvent {
    pub market: Pubkey,
    pub dust_quote_lots: u64,
    pub insurance_balance_after: u64,
}

#[event]
pub struct PositionHaircutInitializedEvent {
    pub market: Pubkey,
    pub position: Pubkey,
}

#[event]
pub struct GainReleasedToHaircutEvent {
    pub market: Pubkey,
    pub position: Pubkey,
    pub gain_quote_lots: u64,
    pub reserve_after: u64,
    pub isolated: bool,
}

#[event]
pub struct ResidualSeededEvent {
    pub market: Pubkey,
    pub delta: i128,
    pub new_residual: u128,
}

#[event]
pub struct HaircutInvariantsCheckedEvent {
    pub market: Pubkey,
    pub passed: bool,
    pub bitmask: u8,
    pub residual_quote_lots: u128,
    pub matured_pos_total_quote_lots: u128,
    pub dust_accrued_quote_lots: u128,
    pub h_scaled_cached: u64,
}

#[event]
pub struct SideAccrualInitializedEvent {
    pub market: Pubkey,
    pub initial_price_ticks: u64,
    pub initial_slot: u64,
}

#[event]
pub struct EnvelopeConfigSetEvent {
    pub market: Pubkey,
    pub version: u32,
    pub max_price_move_bps_per_slot: u32,
    pub max_accrual_dt_slots: u64,
    pub maintenance_bps: u32,
    pub liquidation_fee_bps: u32,
}

#[event]
pub struct EnvelopeVerifiedEvent {
    pub market: Pubkey,
    pub version: u32,
    pub passed: bool,
}

/// Wave 24f — emitted on every `verify_protocol_solvency` invocation.
/// Carries the full balance snapshot so monitors can detect drift even
/// in the absence of `passed: false` (e.g., surplus declining over time
/// might signal a slow leak).
/// Wave 30 — emitted when an authority permanently burns their
/// control over a market. One-way state change; the market is now
/// fully decentralised.
#[event]
pub struct MarketAuthorityBurnedEvent {
    pub market: Pubkey,
    pub previous_authority: Pubkey,
    pub burn_slot: u64,
}

#[event]
pub struct ProtocolSolvencyCheckedEvent {
    pub vault_quote_lots: u64,
    pub insurance_quote_lots: u64,
    pub flp_capital_quote_lots: u64,
    pub minimum_required_quote_lots: u64,
    pub surplus_quote_lots: u64,
    pub solvent: bool,
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
pub struct MarketSequencerRotatedEvent {
    pub market: Pubkey,
    pub previous_sequencer: Pubkey,
    pub new_sequencer: Pubkey,
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

/// V3 mark engine — emitted whenever `mark_price_ticks` is updated.
/// `source` byte:
///   0 = `apply_fill` EMA blend (last-trade-price tracking)
///   1 = `settle_mark` hard reset to oracle
///   2 = future paths (kept for forward-compat)
#[event]
pub struct MarkPriceUpdatedEvent {
    pub market: Pubkey,
    pub old_mark_ticks: u64,
    pub new_mark_ticks: u64,
    pub oracle_ticks: u64,
    pub source: u8,
}

/// V3 mark engine — emitted when |mark - oracle| / oracle exceeds
/// `params.drift_alert_bps`. Off-chain observers use this as a nudge to
/// call `settle_mark` and snap the mark back to the (fresh) oracle.
#[event]
pub struct MarkPriceDriftEvent {
    pub market: Pubkey,
    pub mark_ticks: u64,
    pub oracle_ticks: u64,
    /// Absolute drift in bps of oracle: |mark - oracle| × 10_000 / oracle.
    pub drift_bps: u32,
}

/// V3 liquidation engine — emitted by `liquidate_position_v2` so the
/// off-chain UI / keeper logs can show *which* price source flagged the
/// trader as unhealthy. `source` byte:
///   0 = mark_price was the more adverse price
///   1 = oracle_price was the more adverse price (dual-source gate fired)
///   2 = both were equal (rare)
#[event]
pub struct HealthGateSourceEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub mark_ticks: u64,
    pub oracle_ticks: u64,
    pub health_price_ticks: u64,
    pub source: u8,
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
pub struct AutoDeleveragedEvent {
    pub market: Pubkey,
    pub underwater_trader: Pubkey,
    pub counter_trader: Pubkey,
    pub close_size_lots: u64,
    pub bankruptcy_price_ticks: u64,
    pub counter_gain_quote_lots: u64,
    pub executor: Pubkey,
}

/// Emitted when authority updates the insurance fund's pause threshold via
/// `set_insurance_pause_threshold`. Lets indexers/keepers see the gate move
/// without re-fetching the InsuranceFundAccount.
#[event]
pub struct InsurancePauseThresholdUpdatedEvent {
    pub authority: Pubkey,
    pub previous_threshold_quote_lots: u64,
    pub new_threshold_quote_lots: u64,
    pub current_balance_quote_lots: u64,
}

#[event]
pub struct LiquidatorRewardedEvent {
    pub market: Pubkey,
    pub liquidator: Pubkey,
    pub liquidatee: Pubkey,
    pub reward_quote_lots: u64,
}

// ─── JIT liquidation auction events ─────────────────────────────────

/// A maker pre-committed a JIT liquidation close offer. Off-chain
/// keepers watch this event to learn about active offers without
/// needing to scan all program accounts.
#[event]
pub struct JitLiquidationOfferPlacedEvent {
    pub market: Pubkey,
    pub maker: Pubkey,
    /// `Pubkey::default()` = wildcard (any trader on this market).
    pub target_trader: Pubkey,
    pub nonce: u32,
    pub side: u8,
    pub offer_price_ticks: u64,
    pub max_size_lots: u64,
    /// 0 = never expires.
    pub expires_at_slot: u64,
}

#[event]
pub struct JitLiquidationOfferCancelledEvent {
    pub market: Pubkey,
    pub maker: Pubkey,
    pub nonce: u32,
    pub unfilled_size_lots: u64,
}

/// Emitted by `liquidate_position_v2` when a JIT offer beat the
/// synthetic `oracle ± liq_penalty_bps` close price and was used as
/// the close-order's limit. `savings_vs_synthetic_bps` shows the
/// magnitude of improvement for the trader (bps of oracle price).
#[event]
pub struct JitLiquidationFilledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub jit_maker: Pubkey,
    pub fill_size_lots: u64,
    pub fill_price_ticks: u64,
    pub synthetic_price_ticks: u64,
    pub savings_vs_synthetic_bps: u32,
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
            collateral_quote_lots: position.collateral_quote_lots,
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
        cum_funding_index_at_entry: position.cum_funding_index(),
        collateral_quote_lots: position.collateral_quote_lots,
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
    sub_index: u8,
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
        last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
        side: leg.side,
        order_type: 0, // limit
        flags: if leg.post_only { 0b0000_0001 } else { 0 },
        sub_index,
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
    sub_index: u8,
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
        last_valid_slot: u32::try_from(now_slot).unwrap_or(u32::MAX),
        side: leg.side,
        order_type: 0,
        flags: if leg.post_only { 0b0000_0001 } else { 0 },
        sub_index,
    };
    if side_is_bid {
        handle.insert_bid(order)?;
    } else {
        handle.insert_ask(order)?;
    }
    Ok(())
}

/// Map a `RestingOrderV2.order_type` byte to the matcher's OrderType.
/// Preserves the matcher's FIFO priority weighting (limits behind
/// takers, takers behind liquidations, ADL highest).
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
fn credit_volume_for_tier<'info>(
    state: &mut AccountLoader<'info, TraderStateAccount>,
    notional_quote_lots: u64,
    now_slot: u64,
) -> Result<()> {
    let mut state = state.load_mut()?;
    let elapsed = now_slot.saturating_sub(state.volume_window_start_slot);
    if state.volume_window_start_slot == 0 || elapsed > constants::DEFAULT_VOLUME_WINDOW_SLOTS {
        state.volume_30d_quote_lots = 0;
        state.volume_window_start_slot = now_slot;
    }
    state.volume_30d_quote_lots = state
        .volume_30d_quote_lots
        .saturating_add(notional_quote_lots);
    Ok(())
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
        pos.set_cum_funding_index(funding_index_now);
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
            pos.set_cum_funding_index(funding_index_now);
        }
    } else {
        // Flip side. Remaining = fill - existing.
        let remaining = fill_size_lots
            .checked_sub(pos.size_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
        pos.side = fill_side as u8;
        pos.size_lots = remaining;
        pos.entry_price_ticks = fill_price_ticks;
        pos.set_cum_funding_index(funding_index_now);
    }
    Ok(())
}

fn clamp_i128_to_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 { i64::MAX }
    else if v < i64::MIN as i128 { i64::MIN }
    else { v as i64 }
}

/// Phase 2g — materialise a realized-PnL delta into the trader's
/// collateral. Closes the gap documented in `docs/MARGIN_MATH.md §8.1`:
/// `apply_fill_to_position` accumulates realized PnL on
/// `pos.realized_pnl_quote_lots` (informational), and this helper
/// converts it into actual spendable collateral so a trader closing a
/// winning position can withdraw the profit and a trader closing a
/// losing position has their collateral debited.
///
/// Bucket routing matches the fee-routing rule in `apply_fill`:
///
///   isolated (position.collateral_quote_lots > 0)
///     → mutate position.collateral_quote_lots
///   cross
///     → mutate trader_state.collateral_quote_lots
///
/// Loss truncation:
///
///   * Isolated path: saturating_sub on the per-position bucket. If the
///     loss exceeds the isolated collateral the bucket goes to 0 and
///     the position becomes liquidatable on the next health check —
///     same conservative semantics as `settle_funding`.
///   * Cross path: checked_sub. A loss that exceeds
///     `trader_state.collateral_quote_lots` would put the trader's
///     pooled collateral negative, which is impossible; we return
///     `InsufficientCollateral` so the fill aborts. In practice the
///     pre-fill margin check should already have caught this — but
///     defense in depth: never let collateral go negative on the cross
///     path.
///
/// `delta` is signed: positive = trader gained (credit), negative =
/// trader lost (debit).
fn apply_realized_pnl_delta<'info>(
    delta: i128,
    isolated: bool,
    position: &mut AccountLoader<'info, state::PositionAccount>,
    trader_state: &mut AccountLoader<'info, TraderStateAccount>,
) -> Result<()> {
    let cross_collateral = trader_state.load()?.collateral_quote_lots;
    let iso_collateral = position.load()?.collateral_quote_lots;
    let (new_iso, new_cross) = compute_realized_pnl_routing(
        delta,
        isolated,
        iso_collateral,
        cross_collateral,
    )?;
    position.load_mut()?.collateral_quote_lots = new_iso;
    trader_state.load_mut()?.collateral_quote_lots = new_cross;
    Ok(())
}

/// Wave 24d — apply_realized_pnl_delta with optional H-haircut routing.
///
/// When `position_haircut` is `Some(...)`:
///   - positive delta (gain) is pushed into the reserve via
///     `matcher::haircut::apply_release`. **No collateral mutation.**
///     The credit lands in collateral only after the warmup completes
///     and `convert_position` is called (Wave 24c ix).
///   - negative delta (loss) bypasses the haircut entirely and debits
///     collateral directly — losses are senior to capital.
///
/// When `position_haircut` is `None`: identical to
/// `apply_realized_pnl_delta` v1 (the legacy direct credit/debit path).
///
/// This is the **opt-in** wire-in: markets without a HaircutState
/// keep their existing behaviour bit-for-bit. Markets with HaircutState
/// (+ a per-position HaircutState provided in remaining accounts) gain
/// the junior-claim protection automatically on every fill.
fn apply_realized_pnl_delta_v2<'info>(
    delta: i128,
    isolated: bool,
    position: &mut AccountLoader<'info, state::PositionAccount>,
    trader_state: &mut AccountLoader<'info, TraderStateAccount>,
    position_haircut: Option<&mut Box<Account<'info, state_v3::PositionHaircutStateAccount>>>,
    now_slot: u64,
) -> Result<()> {
    if delta > 0 {
        if let Some(ph) = position_haircut {
            // Haircut path: route to reserve, no collateral mutation.
            let gain_u64: u64 = if delta > u64::MAX as i128 {
                u64::MAX
            } else {
                delta as u64
            };
            let pre = matcher::haircut::PositionHaircutSnapshot {
                released_reserve_quote_lots: ph.released_reserve_quote_lots,
                released_attached_at_slot: ph.released_attached_at_slot,
                matured_pos_quote_lots: ph.matured_pos_quote_lots,
                original_reserve_at_attach: ph.original_reserve_at_attach,
            };
            let post = matcher::haircut::apply_release(pre, gain_u64, now_slot)
                .map_err(map_haircut_error)?;
            ph.released_reserve_quote_lots = post.released_reserve_quote_lots;
            ph.released_attached_at_slot = post.released_attached_at_slot;
            ph.matured_pos_quote_lots = post.matured_pos_quote_lots;
            ph.original_reserve_at_attach = post.original_reserve_at_attach;
            return Ok(());
        }
    }
    // Loss path AND legacy non-haircut gain path both fall through to v1.
    apply_realized_pnl_delta(delta, isolated, position, trader_state)
}

/// Pure math for the realized-PnL routing. Returns
/// `(new_position_collateral, new_trader_state_collateral)`. Separated
/// from `apply_realized_pnl_delta` so the routing rules are unit
/// testable without Anchor-account scaffolding.
///
/// Rules (matching `apply_realized_pnl_delta`'s contract):
///
/// * `delta > 0` (gain) — credit the isolated bucket if isolated,
///   else the cross pool. `checked_add` overflows → ArithmeticOverflow.
/// * `delta < 0` (loss) — debit the isolated bucket if isolated, else
///   the cross pool. Isolated bucket saturates to 0 (loss-truncation,
///   same semantics as `settle_funding`). Cross pool uses
///   `checked_sub` and surfaces `InsufficientCollateral` rather than
///   going negative.
/// * `delta == 0` — no-op.
fn compute_realized_pnl_routing(
    delta: i128,
    isolated: bool,
    iso_collateral: u64,
    cross_collateral: u64,
) -> Result<(u64, u64)> {
    if delta > 0 {
        let credit_u64 = if delta > u64::MAX as i128 {
            u64::MAX
        } else {
            delta as u64
        };
        if isolated {
            let new_iso = iso_collateral
                .checked_add(credit_u64)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            Ok((new_iso, cross_collateral))
        } else {
            let new_cross = cross_collateral
                .checked_add(credit_u64)
                .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
            Ok((iso_collateral, new_cross))
        }
    } else if delta < 0 {
        let debit_u64 = if delta < -(u64::MAX as i128) {
            u64::MAX
        } else {
            (-delta) as u64
        };
        if isolated {
            // Loss-truncation: saturate to 0; remainder surfaces via
            // the next health check.
            Ok((iso_collateral.saturating_sub(debit_u64), cross_collateral))
        } else {
            let new_cross = cross_collateral
                .checked_sub(debit_u64)
                .ok_or_else(|| error!(FlashBookError::InsufficientCollateral))?;
            Ok((iso_collateral, new_cross))
        }
    } else {
        Ok((iso_collateral, cross_collateral))
    }
}

/// Phase 2i — verify that an account presented as a TraderState was
/// derived from the expected `(wallet, sub_index)` seed pair. Returns
/// `WrongTrader` if the derivation doesn't match the account's actual
/// pubkey.
///
/// This closes the 1-byte routing-attack surface left by Phase 2d's
/// seeds relaxation on `ApplyFill`. The handler relaxation lets the
/// sequencer pass any TraderState whose `.trader` field matches the
/// order's `.trader`; without this re-derivation check, a hostile
/// sequencer could pass `[STATE_SEED, trader, &[2]]` for an order
/// placed by sub_index=1.
///
/// Seed convention (matches `open_trader_state` and
/// `open_trader_sub_account`):
///
///   sub_index == 0 → `[TraderStateAccount::SEED, trader.as_ref()]`        (main)
///   sub_index >  0 → `[TraderStateAccount::SEED, trader.as_ref(), &[sub_index]]` (sub)
///
/// CU: uses the account's stored canonical `bump` + `create_program_address`
/// (one hash) instead of `find_program_address` (a descending bump search,
/// ~1.5k CU). Same security: `open_trader_state` / `open_trader_sub_account`
/// store the canonical bump at init via Anchor's `init`+`bump`, and a
/// non-canonical-bump TraderState cannot be program-initialized — so the
/// stored bump IS the canonical one. This mirrors `verify_position_pda`.
/// init_if_needed zero-copy helper: Anchor's `AccountLoader` writes the 8-byte
/// discriminator only in `exit()` (end of instruction), so a freshly-created
/// account has a zeroed discriminator *during* the handler and any
/// `load()`/`load_mut()` fails `AccountDiscriminatorMismatch`. This stamps the
/// discriminator immediately if absent; it is a no-op for an account that is
/// already initialized.
fn stamp_zc_discriminator(ai: &AccountInfo<'_>, disc: &[u8]) -> Result<()> {
    let is_fresh = ai.try_borrow_data()?[..8].iter().all(|&b| b == 0);
    if is_fresh {
        ai.try_borrow_mut_data()?[..disc.len()].copy_from_slice(disc);
    }
    Ok(())
}

fn verify_trader_state_pda(
    sub_index: u8,
    trader: Pubkey,
    actual: Pubkey,
    bump: u8,
    program_id: &Pubkey,
) -> Result<()> {
    let expected = if sub_index == 0 {
        Pubkey::create_program_address(
            &[TraderStateAccount::SEED, trader.as_ref(), &[bump]],
            program_id,
        )
    } else {
        Pubkey::create_program_address(
            &[TraderStateAccount::SEED, trader.as_ref(), &[sub_index], &[bump]],
            program_id,
        )
    }
    .map_err(|_| error!(FlashBookError::WrongTrader))?;
    require_keys_eq!(expected, actual, FlashBookError::WrongTrader);
    Ok(())
}

/// C-2 — re-derive a Position PDA from `(market, trader_state)` and assert
/// it matches `actual`. This binds a `remaining_accounts` position to THIS
/// trader_state, so a margin walk cannot be fed a different sub-account's
/// position, a stale/closed position, or any other program-owned account
/// with a chosen `mark_price`/size to understate risk. Uses the position's
/// stored bump + `create_program_address` (cheap — no bump search).
fn verify_position_pda(
    market_key: &Pubkey,
    trader_state_key: &Pubkey,
    bump: u8,
    actual: &Pubkey,
    program_id: &Pubkey,
) -> Result<()> {
    let expected = Pubkey::create_program_address(
        &[
            state::PositionAccount::SEED,
            market_key.as_ref(),
            trader_state_key.as_ref(),
            &[bump],
        ],
        program_id,
    )
    .map_err(|_| error!(FlashBookError::OutOfRange))?;
    require_keys_eq!(expected, *actual, FlashBookError::WrongTrader);
    Ok(())
}

/// Phase 2h — pure-math helper for the underwater loss side of ADL
/// settlement. Returns `(new_position_collateral,
/// new_trader_state_collateral)`.
///
/// Rule:
///   isolated  → debit the per-position bucket, saturating to 0.
///               Cross pool is NEVER touched (preserves I-3 cross-pool
///               insulation). Any unpaid remainder is absorbed by the
///               insurance fund + ADL waterfall — same conservative
///               semantics ADL has always used for the cross path.
///   cross     → debit the cross pool, saturating to 0.
fn route_adl_loss(
    isolated: bool,
    loss_quote_lots: u64,
    pos_collateral: u64,
    trader_state_collateral: u64,
) -> (u64, u64) {
    if isolated {
        (
            pos_collateral.saturating_sub(loss_quote_lots),
            trader_state_collateral,
        )
    } else {
        (
            pos_collateral,
            trader_state_collateral.saturating_sub(loss_quote_lots),
        )
    }
}

/// Phase 2h — pure-math helper for the counter gain side of ADL
/// settlement. Returns `(new_position_collateral,
/// new_trader_state_collateral)`.
///
/// Rule:
///   isolated  → credit the per-position bucket (checked_add).
///   cross     → credit the cross pool (checked_add).
/// Overflow → ArithmeticOverflow.
fn route_adl_gain(
    isolated: bool,
    gain_quote_lots: u64,
    pos_collateral: u64,
    trader_state_collateral: u64,
) -> Result<(u64, u64)> {
    if isolated {
        let new_pos = pos_collateral
            .checked_add(gain_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        Ok((new_pos, trader_state_collateral))
    } else {
        let new_ts = trader_state_collateral
            .checked_add(gain_quote_lots)
            .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
        Ok((pos_collateral, new_ts))
    }
}

#[cfg(test)]
mod adl_routing_tests {
    use super::*;

    // ─── Loss side ──────────────────────────────────────────────────

    #[test]
    fn loss_isolated_debits_position_bucket() {
        let (pos, ts) = route_adl_loss(true, 100, 500, 10_000);
        assert_eq!(pos, 400);
        assert_eq!(ts, 10_000, "cross pool must NOT be touched on isolated loss");
    }

    #[test]
    fn loss_isolated_saturates_at_zero_never_touches_cross() {
        // Loss larger than the isolated bucket — saturates to 0.
        // The remainder will be absorbed by the insurance fund via the
        // ADL waterfall. The cross pool is NEVER debited (I-3).
        let (pos, ts) = route_adl_loss(true, 1_000, 500, 10_000);
        assert_eq!(pos, 0);
        assert_eq!(ts, 10_000, "MUST NOT bleed into cross pool on isolated loss");
    }

    #[test]
    fn loss_cross_debits_cross_pool() {
        let (pos, ts) = route_adl_loss(false, 250, 0, 1_000);
        assert_eq!(pos, 0, "position bucket untouched on cross path");
        assert_eq!(ts, 750);
    }

    #[test]
    fn loss_cross_saturates_at_zero() {
        // Cross-path losses saturate too — ADL is the last resort and
        // must always make progress. The remainder is absorbed by the
        // existing insurance + ADL waterfall.
        let (pos, ts) = route_adl_loss(false, 5_000, 0, 1_000);
        assert_eq!(pos, 0);
        assert_eq!(ts, 0);
    }

    #[test]
    fn loss_zero_amount_is_no_op() {
        let (pos, ts) = route_adl_loss(true, 0, 500, 10_000);
        assert_eq!(pos, 500);
        assert_eq!(ts, 10_000);
        let (pos, ts) = route_adl_loss(false, 0, 500, 10_000);
        assert_eq!(pos, 500);
        assert_eq!(ts, 10_000);
    }

    // ─── Gain side ──────────────────────────────────────────────────

    #[test]
    fn gain_isolated_credits_position_bucket() {
        let (pos, ts) = route_adl_gain(true, 250, 500, 10_000).unwrap();
        assert_eq!(pos, 750);
        assert_eq!(ts, 10_000, "cross pool untouched on isolated gain");
    }

    #[test]
    fn gain_cross_credits_cross_pool() {
        let (pos, ts) = route_adl_gain(false, 250, 500, 10_000).unwrap();
        assert_eq!(pos, 500, "position bucket untouched on cross path");
        assert_eq!(ts, 10_250);
    }

    #[test]
    fn gain_isolated_overflow_errors() {
        let result = route_adl_gain(true, 10, u64::MAX, 0);
        assert!(result.is_err(), "isolated bucket overflow must error");
    }

    #[test]
    fn gain_cross_overflow_errors() {
        let result = route_adl_gain(false, 10, 0, u64::MAX);
        assert!(result.is_err(), "cross pool overflow must error");
    }

    #[test]
    fn gain_zero_amount_is_no_op() {
        let (pos, ts) = route_adl_gain(true, 0, 500, 10_000).unwrap();
        assert_eq!(pos, 500);
        assert_eq!(ts, 10_000);
        let (pos, ts) = route_adl_gain(false, 0, 500, 10_000).unwrap();
        assert_eq!(pos, 500);
        assert_eq!(ts, 10_000);
    }

    // ─── End-to-end consistency: isolated underwater + isolated counter
    //     leaves both cross pools untouched. ────────────────────────

    #[test]
    fn isolated_adl_leaves_both_cross_pools_untouched() {
        let uw_cross_before = 100_000u64;
        let ct_cross_before = 50_000u64;
        let (uw_pos_after, uw_cross_after) = route_adl_loss(
            /* isolated */ true,
            /* loss      */ 800,
            /* pos       */ 1_000,
            /* cross     */ uw_cross_before,
        );
        let (ct_pos_after, ct_cross_after) = route_adl_gain(
            /* isolated */ true,
            /* gain      */ 600,
            /* pos       */ 200,
            /* cross     */ ct_cross_before,
        ).unwrap();
        assert_eq!(uw_pos_after, 200);
        assert_eq!(ct_pos_after, 800);
        // Critical I-3 invariant: neither cross pool moved.
        assert_eq!(uw_cross_after, uw_cross_before);
        assert_eq!(ct_cross_after, ct_cross_before);
    }
}

#[cfg(test)]
mod realized_pnl_routing_tests {
    use super::*;

    #[test]
    fn zero_delta_is_no_op() {
        // Cross
        let (iso, cross) = compute_realized_pnl_routing(0, false, 100, 5_000).unwrap();
        assert_eq!(iso, 100);
        assert_eq!(cross, 5_000);
        // Isolated
        let (iso, cross) = compute_realized_pnl_routing(0, true, 100, 5_000).unwrap();
        assert_eq!(iso, 100);
        assert_eq!(cross, 5_000);
    }

    #[test]
    fn gain_credits_isolated_bucket_when_isolated() {
        let (iso, cross) = compute_realized_pnl_routing(250, true, 1_000, 5_000).unwrap();
        assert_eq!(iso, 1_250, "isolated bucket should receive the credit");
        assert_eq!(cross, 5_000, "cross pool untouched on isolated path");
    }

    #[test]
    fn gain_credits_cross_pool_when_not_isolated() {
        let (iso, cross) = compute_realized_pnl_routing(250, false, 1_000, 5_000).unwrap();
        assert_eq!(iso, 1_000, "isolated bucket untouched on cross path");
        assert_eq!(cross, 5_250, "cross pool should receive the credit");
    }

    #[test]
    fn loss_debits_isolated_bucket_when_isolated() {
        let (iso, cross) = compute_realized_pnl_routing(-250, true, 1_000, 5_000).unwrap();
        assert_eq!(iso, 750);
        assert_eq!(cross, 5_000);
    }

    #[test]
    fn loss_truncates_at_zero_on_isolated_path() {
        // Loss larger than isolated balance — saturates to 0, NEVER goes
        // negative and NEVER bleeds into the cross pool. The unpaid
        // remainder is absorbed; the next health check will trip.
        let (iso, cross) = compute_realized_pnl_routing(-2_000, true, 1_000, 5_000).unwrap();
        assert_eq!(iso, 0);
        assert_eq!(cross, 5_000, "cross pool MUST NOT be touched on isolated loss");
    }

    #[test]
    fn loss_debits_cross_pool_when_not_isolated() {
        let (iso, cross) = compute_realized_pnl_routing(-250, false, 1_000, 5_000).unwrap();
        assert_eq!(iso, 1_000);
        assert_eq!(cross, 4_750);
    }

    #[test]
    fn loss_exceeding_cross_pool_returns_insufficient_collateral() {
        let result = compute_realized_pnl_routing(-6_000, false, 1_000, 5_000);
        assert!(result.is_err(), "loss > cross pool must error, not go negative");
    }

    #[test]
    fn gain_overflowing_isolated_bucket_returns_overflow() {
        let result = compute_realized_pnl_routing(10, true, u64::MAX, 5_000);
        assert!(result.is_err(), "overflow on the isolated bucket must error");
    }

    #[test]
    fn gain_overflowing_cross_pool_returns_overflow() {
        let result = compute_realized_pnl_routing(10, false, 1_000, u64::MAX);
        assert!(result.is_err(), "overflow on the cross pool must error");
    }

    #[test]
    fn extreme_positive_delta_clamps_to_u64_max_credit() {
        let huge = (u64::MAX as i128) + 1;
        // Credit clamped to u64::MAX. From cross_collateral=0 this means
        // post-credit equals u64::MAX exactly (no overflow).
        let (_iso, cross) = compute_realized_pnl_routing(huge, false, 0, 0).unwrap();
        assert_eq!(cross, u64::MAX);
    }

    #[test]
    fn extreme_negative_delta_clamps_to_u64_max_debit() {
        let huge_neg = -((u64::MAX as i128) + 1);
        // Debit clamped to u64::MAX on the cross path; with cross_collateral
        // = u64::MAX we land at 0 exactly.
        let (_iso, cross) = compute_realized_pnl_routing(huge_neg, false, 0, u64::MAX).unwrap();
        assert_eq!(cross, 0);
    }
}


// ─── V3 monolithic account contexts (merged from former wrappers) ─────────

#[derive(Accounts)]
#[instruction(trigger_id: u8)]
pub struct PlaceTriggerOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    #[account(
        init,
        payer = trader,
        space = state_v3::TriggerOrderAccountV3::space(),
        seeds = [
            state_v3::TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[trigger_id],
        ],
        bump,
    )]
    pub trigger_order: Account<'info, state_v3::TriggerOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ExecuteTriggerOrderV3<'info> {
    /// Permissionless caller pays tx fee. Trader pre-authorized at placement.
    pub caller: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA — disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            state_v3::TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trigger_order.trader.as_ref(),
            &[trigger_order.trigger_id],
        ],
        bump = trigger_order.bump,
    )]
    pub trigger_order: Account<'info, state_v3::TriggerOrderAccountV3>,

    /// Trader's position — required for reduce-only triggers.
    /// Phase 2c: dropped strict seed (TriggerOrderAccountV3 is
    /// wallet-scoped; Anchor cannot derive trader_state PDA inside
    /// the seeds expression). Identity is enforced by data fields.
    /// Sub-account triggers require a TriggerOrderAccountV3 schema
    /// addition for sub_index before they become useful.
    #[account(
        constraint = position.load()?.market == market.key() @ FlashBookError::WrongMarket,
        constraint = position.load()?.trader == trigger_order.trader @ FlashBookError::WrongTrader,
    )]
    pub position: AccountLoader<'info, state::PositionAccount>,
}

#[derive(Accounts)]
pub struct CancelTriggerOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            state_v3::TriggerOrderAccountV3::SEED,
            trigger_order.market.as_ref(),
            trigger_order.trader.as_ref(),
            &[trigger_order.trigger_id],
        ],
        bump = trigger_order.bump,
    )]
    pub trigger_order: Account<'info, state_v3::TriggerOrderAccountV3>,
}

// ─── JIT Liquidation Offer contexts ─────────────────────────────────

#[derive(Accounts)]
#[instruction(nonce: u32)]
pub struct PlaceJitLiquidationOffer<'info> {
    #[account(mut)]
    pub maker: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    #[account(
        init,
        payer = maker,
        space = state_v3::JitLiquidationOfferAccount::space(),
        seeds = [
            state_v3::JitLiquidationOfferAccount::SEED,
            market.key().as_ref(),
            maker.key().as_ref(),
            &nonce.to_le_bytes(),
        ],
        bump,
    )]
    pub jit_offer: Account<'info, state_v3::JitLiquidationOfferAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CancelJitLiquidationOffer<'info> {
    #[account(mut)]
    pub maker: Signer<'info>,

    #[account(
        mut,
        close = maker,
        seeds = [
            state_v3::JitLiquidationOfferAccount::SEED,
            jit_offer.market.as_ref(),
            jit_offer.maker.as_ref(),
            &jit_offer.nonce.to_le_bytes(),
        ],
        bump = jit_offer.bump,
    )]
    pub jit_offer: Account<'info, state_v3::JitLiquidationOfferAccount>,
}

#[derive(Accounts)]
pub struct MigrateMarketToV3<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: layout is OLD pre-V3; we deserialize via try_deserialize_unchecked
    /// and gate by `authority == market.authority` inside the handler.
    /// Owner-checked: must be owned by this program (the discriminator check
    /// inside the handler validates it's actually a Market account).
    #[account(mut, owner = crate::ID)]
    pub market: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}

#[event]
pub struct MarketMigratedToV3Event {
    pub market: Pubkey,
    pub old_size: u32,
    pub new_size: u32,
    pub slot: u64,
}

#[derive(Accounts)]
pub struct InitMarketOracleConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    #[account(
        init,
        payer = authority,
        space = state_v3::MarketOracleConfigAccount::space(),
        seeds = [state_v3::MarketOracleConfigAccount::SEED, market.key().as_ref()],
        bump,
    )]
    pub oracle_config: Account<'info, state_v3::MarketOracleConfigAccount>,

    pub system_program: Program<'info, System>,
}

#[event]
pub struct MarketOracleConfigInitializedEvent {
    pub market: Pubkey,
    pub pyth_price_feed_id: [u8; 32],
    pub max_staleness_seconds: u32,
    pub max_confidence_bps: u32,
    pub tick_decimals: i8,
}

#[derive(Accounts)]
pub struct UpdateOracleFromPyth<'info> {
    /// Permissionless caller; pays the tx fee. Pyth's PriceUpdateV2 account
    /// is the trust anchor — the caller signing is just to pay for compute.
    pub caller: Signer<'info>,

    #[account(mut)]
    pub market: Account<'info, MarketAccount>,

    #[account(
        seeds = [state_v3::MarketOracleConfigAccount::SEED, market.key().as_ref()],
        bump = oracle_config.bump,
        constraint = oracle_config.market == market.key() @ FlashBookError::WrongMarket,
    )]
    pub oracle_config: Account<'info, state_v3::MarketOracleConfigAccount>,

    /// CHECK: deserialized via pyth-solana-receiver-sdk's account type.
    /// The SDK's get_price_no_older_than validates feed_id matches.
    pub price_update: Account<'info, pyth_solana_receiver_sdk::price_update::PriceUpdateV2>,

    /// Wave 26b — optional envelope gate. Same semantics as on
    /// `UpdateOracle`. When omitted: legacy Pyth-pull behavior.
    #[account(
        mut,
        seeds = [state_v3::MarketEnvelopeConfigAccount::SEED, market.key().as_ref()],
        bump = envelope_config.bump,
        constraint = envelope_config.market == market.key() @ FlashBookError::EnvelopePriceCapInvalid,
    )]
    pub envelope_config: Option<Box<Account<'info, state_v3::MarketEnvelopeConfigAccount>>>,
}

#[event]
pub struct OracleUpdatedFromPythEvent {
    pub market: Pubkey,
    pub pyth_price: i64,
    pub pyth_conf: u64,
    pub pyth_exponent: i32,
    pub new_oracle_ticks: u64,
    pub confidence_bps: u32,
    pub publish_time: i64,
}

#[derive(Accounts)]
#[instruction(twap_id: u8)]
pub struct PlaceTwapOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    #[account(
        init,
        payer = trader,
        space = state_v3::TwapOrderAccountV3::space(),
        seeds = [
            state_v3::TwapOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[twap_id],
        ],
        bump,
    )]
    pub twap_order: Account<'info, state_v3::TwapOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ExecuteTwapSliceV3<'info> {
    pub caller: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA — disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            state_v3::TwapOrderAccountV3::SEED,
            market.key().as_ref(),
            twap_order.trader.as_ref(),
            &[twap_order.twap_id],
        ],
        bump = twap_order.bump,
    )]
    pub twap_order: Account<'info, state_v3::TwapOrderAccountV3>,
}

#[derive(Accounts)]
pub struct CancelTwapOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            state_v3::TwapOrderAccountV3::SEED,
            twap_order.market.as_ref(),
            twap_order.trader.as_ref(),
            &[twap_order.twap_id],
        ],
        bump = twap_order.bump,
    )]
    pub twap_order: Account<'info, state_v3::TwapOrderAccountV3>,
}

#[derive(Accounts)]
#[instruction(iceberg_id: u8)]
pub struct PlaceIcebergOrderV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA — first chunk inserted inline.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        init,
        payer = trader,
        space = state_v3::IcebergOrderAccountV3::space(),
        seeds = [
            state_v3::IcebergOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[iceberg_id],
        ],
        bump,
    )]
    pub iceberg_order: Account<'info, state_v3::IcebergOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReplenishIcebergV3<'info> {
    pub caller: Signer<'info>,

    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA — disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [
            state_v3::IcebergOrderAccountV3::SEED,
            market.key().as_ref(),
            iceberg_order.trader.as_ref(),
            &[iceberg_order.iceberg_id],
        ],
        bump = iceberg_order.bump,
    )]
    pub iceberg_order: Account<'info, state_v3::IcebergOrderAccountV3>,
}

#[derive(Accounts)]
pub struct CancelIcebergV3<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        close = trader,
        seeds = [
            state_v3::IcebergOrderAccountV3::SEED,
            iceberg_order.market.as_ref(),
            iceberg_order.trader.as_ref(),
            &[iceberg_order.iceberg_id],
        ],
        bump = iceberg_order.bump,
    )]
    pub iceberg_order: Account<'info, state_v3::IcebergOrderAccountV3>,
}

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

    /// CHECK: market_book PDA — parent injected inline.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,

    #[account(
        init,
        payer = trader,
        space = state_v3::TriggerOrderAccountV3::space(),
        seeds = [
            state_v3::TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[tp_trigger_id],
        ],
        bump,
    )]
    pub tp_trigger: Account<'info, state_v3::TriggerOrderAccountV3>,

    #[account(
        init,
        payer = trader,
        space = state_v3::TriggerOrderAccountV3::space(),
        seeds = [
            state_v3::TriggerOrderAccountV3::SEED,
            market.key().as_ref(),
            trader.key().as_ref(),
            &[sl_trigger_id],
        ],
        bump,
    )]
    pub sl_trigger: Account<'info, state_v3::TriggerOrderAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(vault_id: u8)]
pub struct CreateVaultV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        init,
        payer = strategist,
        space = state_v3::VaultAccountV3::space(),
        seeds = [state_v3::VaultAccountV3::SEED, strategist.key().as_ref(), &[vault_id]],
        bump,
    )]
    pub vault: Account<'info, state_v3::VaultAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultOpenTraderStateV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        seeds = [state_v3::VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, state_v3::VaultAccountV3>,

    /// Vault PDA's TraderState — initialized here, seeded by vault PDA.
    #[account(
        init,
        payer = strategist,
        space = TraderStateAccount::space(),
        seeds = [TraderStateAccount::SEED, vault.key().as_ref()],
        bump,
    )]
    pub vault_trader_state: AccountLoader<'info, TraderStateAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultDepositV3<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, state_v3::VaultAccountV3>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = state_v3::VaultPositionAccountV3::space(),
        seeds = [
            state_v3::VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump,
    )]
    pub position: Account<'info, state_v3::VaultPositionAccountV3>,

    /// Insurance fund PDA — owns the protocol vault (binds quote_vault + mint).
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    /// Depositor's USDC ATA — debited. C1: bound to the protocol quote mint +
    /// the depositor so a foreign/worthless-mint source cannot be substituted.
    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = depositor,
    )]
    pub depositor_quote_ata: Account<'info, TokenAccount>,

    /// Protocol vault — credited. C1: MUST be the canonical insurance-fund vault.
    /// Previously unbound `#[account(mut)]`, which let an attacker pass a
    /// self-owned destination and still be credited collateral + minted shares
    /// redeemable against the real vault (Cashio-class drain).
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// Vault's TraderState — credited (collateral_quote_lots).
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, vault.key().as_ref()],
        bump = vault_trader_state.load()?.bump,
    )]
    pub vault_trader_state: AccountLoader<'info, TraderStateAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultWithdrawV3<'info> {
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, state_v3::VaultAccountV3>,

    #[account(
        mut,
        seeds = [
            state_v3::VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ FlashBookError::Unauthorized,
    )]
    pub position: Account<'info, state_v3::VaultPositionAccountV3>,

    /// InsuranceFund PDA — signs SPL release.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    /// Protocol vault — debited.
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// Depositor's USDC ATA — credited.
    #[account(mut)]
    pub depositor_quote_ata: Account<'info, TokenAccount>,

    /// Vault's TraderState — collateral debited.
    #[account(
        mut,
        seeds = [TraderStateAccount::SEED, vault.key().as_ref()],
        bump = vault_trader_state.load()?.bump,
    )]
    pub vault_trader_state: AccountLoader<'info, TraderStateAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct VaultPlaceOrderV3<'info> {
    pub strategist: Signer<'info>,

    #[account(
        seeds = [state_v3::VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, state_v3::VaultAccountV3>,

    #[account(
        mut,
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Account<'info, MarketAccount>,

    /// CHECK: market_book PDA — disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct VaultCancelOrderV3<'info> {
    pub strategist: Signer<'info>,

    #[account(
        seeds = [state_v3::VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, state_v3::VaultAccountV3>,

    #[account(
        seeds = [MarketAccount::SEED, market.base_mint.as_ref(), market.quote_mint.as_ref()],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, MarketAccount>>,

    /// CHECK: market_book PDA — disc validated inside handler.
    #[account(
        mut,
        seeds = [state_v2::MARKET_BOOK_SEED, market.key().as_ref()],
        bump,
    )]
    pub market_book: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SettleVaultPerfFeeV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
        constraint = vault.strategist == strategist.key() @ FlashBookError::Unauthorized,
    )]
    pub vault: Account<'info, state_v3::VaultAccountV3>,

    #[account(
        init_if_needed,
        payer = strategist,
        space = state_v3::VaultPositionAccountV3::space(),
        seeds = [
            state_v3::VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            strategist.key().as_ref(),
        ],
        bump,
    )]
    pub strategist_position: Account<'info, state_v3::VaultPositionAccountV3>,

    /// Vault's TraderState — read-only; NAV source.
    #[account(
        seeds = [TraderStateAccount::SEED, vault.key().as_ref()],
        bump = vault_trader_state.load()?.bump,
    )]
    pub vault_trader_state: AccountLoader<'info, TraderStateAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitFlpPerMarketV3<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: market pubkey — used as PDA seed only.
    pub market: UncheckedAccount<'info>,

    #[account(
        init,
        payer = authority,
        space = state_v3::FlpExposurePerMarketAccountV3::space(),
        seeds = [state_v3::FlpExposurePerMarketAccountV3::SEED, market.key().as_ref()],
        bump,
    )]
    pub exposure: Account<'info, state_v3::FlpExposurePerMarketAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RecordFlpFillV3<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::FlpExposurePerMarketAccountV3::SEED, exposure.market.as_ref()],
        bump = exposure.bump,
    )]
    pub exposure: Account<'info, state_v3::FlpExposurePerMarketAccountV3>,
}

#[derive(Accounts)]
pub struct FlpDepositV3<'info> {
    #[account(mut)]
    pub lp: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::FlpExposurePerMarketAccountV3::SEED, exposure.market.as_ref()],
        bump = exposure.bump,
    )]
    pub exposure: Account<'info, state_v3::FlpExposurePerMarketAccountV3>,

    #[account(
        init_if_needed,
        payer = lp,
        space = state_v3::FlpPositionAccountV3::space(),
        seeds = [state_v3::FlpPositionAccountV3::SEED, exposure.key().as_ref(), lp.key().as_ref()],
        bump,
    )]
    pub position: Account<'info, state_v3::FlpPositionAccountV3>,

    /// Insurance fund PDA — owns the protocol vault (binds quote_vault + mint).
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    #[account(address = insurance_fund.quote_mint)]
    pub quote_mint: Account<'info, Mint>,

    /// LP's USDC ATA — debited. C1: bound to the protocol quote mint + the LP
    /// so a foreign/worthless-mint source cannot be substituted.
    #[account(
        mut,
        associated_token::mint = quote_mint,
        associated_token::authority = lp,
    )]
    pub lp_quote_ata: Account<'info, TokenAccount>,

    /// Protocol vault — credited. C1: MUST be the canonical insurance-fund vault.
    /// Previously unbound `#[account(mut)]`, which let an attacker pass a
    /// self-owned destination and still be minted FLP shares redeemable against
    /// the real vault (Cashio-class drain).
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FlpWithdrawV3<'info> {
    #[account(mut)]
    pub lp: Signer<'info>,

    #[account(
        mut,
        seeds = [state_v3::FlpExposurePerMarketAccountV3::SEED, exposure.market.as_ref()],
        bump = exposure.bump,
    )]
    pub exposure: Account<'info, state_v3::FlpExposurePerMarketAccountV3>,

    #[account(
        mut,
        seeds = [state_v3::FlpPositionAccountV3::SEED, exposure.key().as_ref(), lp.key().as_ref()],
        bump = position.bump,
        constraint = position.lp == lp.key() @ FlashBookError::Unauthorized,
    )]
    pub position: Account<'info, state_v3::FlpPositionAccountV3>,

    /// InsuranceFund PDA — signs SPL release.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    /// Protocol vault — debited.
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// LP's USDC ATA — credited.
    #[account(mut)]
    pub lp_quote_ata: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

// ─── V3 events ────────────────────────────────────────────────────────────

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
    pub order_seq: u64,
    pub node_index: u32,
}

/// Wave 27a — emitted when a trigger fires but the oracle has gapped
/// past the trader's acceptable_price slippage cap. The trigger
/// deactivates rather than filling at a much worse price than intended.
#[event]
pub struct TriggerOrderV3SlippageCancelledEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub trigger_id: u8,
    pub oracle_price_ticks: u64,
    pub acceptable_price_ticks: u64,
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
    pub order_seq: u64,
    pub node_index: u32,
}

/// Wave 27b — emitted when a TWAP slice would fire but the slippage
/// cap is breached. The slice is skipped; the TWAP stays active for
/// future intervals.
#[event]
pub struct TwapSliceV3SlippageSkippedEvent {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub twap_id: u8,
    pub oracle_price_ticks: u64,
    pub acceptable_price_ticks: u64,
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

#[event]
pub struct VaultV3CreatedEvent {
    pub vault: Pubkey,
    pub strategist: Pubkey,
    pub vault_id: u8,
    pub perf_fee_bps: u32,
}

#[event]
pub struct VaultTraderStateOpenedV3Event {
    pub vault: Pubkey,
    pub vault_trader_state: Pubkey,
}

#[event]
pub struct VaultDepositV3Event {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub amount_quote_lots: u64,
    pub shares_minted: u64,
    pub shares_outstanding_after: u64,
}

#[event]
pub struct VaultWithdrawV3Event {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub shares_burned: u64,
    pub amount_quote_lots: u64,
    pub shares_outstanding_after: u64,
}

#[event]
pub struct VaultOrderPlacedV3Event {
    pub vault: Pubkey,
    pub market: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub limit_ticks: u64,
    pub order_seq: u64,
    pub node_index: u32,
}

#[event]
pub struct VaultOrderCancelledV3Event {
    pub vault: Pubkey,
    pub market: Pubkey,
    pub side: u8,
    pub order_id: u64,
    pub order_seq: u64,
    pub node_index: u32,
}

#[event]
pub struct VaultPerfFeeSettledV3Event {
    pub vault: Pubkey,
    pub strategist: Pubkey,
    pub shares_minted: u64,
    pub new_hwm_per_share_u64x6: u64,
}

#[event]
pub struct FlpPerMarketInitV3Event {
    pub market: Pubkey,
    pub authority: Pubkey,
}

#[event]
pub struct FlpFillRecordedV3Event {
    pub market: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub price_ticks: u64,
    pub realized_pnl_delta: i64,
    pub new_side: u8,
    pub new_size_lots: u64,
    pub new_realized_pnl: i64,
}

#[event]
pub struct FlpDepositedV3Event {
    pub market: Pubkey,
    pub lp: Pubkey,
    pub amount_quote_lots: u64,
    pub shares_minted: u64,
    pub new_total_capital: u64,
    pub new_shares_outstanding: u64,
}

#[event]
pub struct FlpWithdrawnV3Event {
    pub market: Pubkey,
    pub lp: Pubkey,
    pub shares_burned: u64,
    pub amount_quote_lots: u64,
    pub new_total_capital: u64,
    pub new_shares_outstanding: u64,
}
