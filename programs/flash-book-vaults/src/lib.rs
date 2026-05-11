#![allow(unexpected_cfgs)]
//! Flash Book Vaults — wave 21 program split.
//!
//! Strategist vaults (`VaultAccountV3`), depositor share accounting
//! (`VaultPositionAccountV3`).
//!
//! ── Status: PHASE 9 + 9b — full SPL deposit/withdraw shipped
//!
//! Functional ixs:
//!   • `create_vault_v3`       — strategist creates a new vault
//!   • `vault_deposit_v3`      — depositor signs SPL transfer (their
//!                                ATA → core's quote_vault) + wrapper
//!                                mints shares pro-rata to NAV
//!   • `vault_withdraw_v3`     — wrapper burns shares + CPIs into
//!                                core's `cpi_release_collateral_to_user`
//!                                so the depositor receives the
//!                                pro-rata payout from the protocol
//!                                quote_vault (signed by core's
//!                                InsuranceFund PDA)
//!
//! Trading on behalf of depositors (vault places orders via
//! `place_limit_order_v2_cpi`) is in scope but lands as a separate
//! follow-up because risk semantics for vault-owned positions live
//! in core's `assess_margin` path and require additional plumbing.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use flash_book::cpi::accounts::{
    CancelOrderV2Cpi as CoreCancelOrderV2Cpi,
    CpiCreditOrDebitCollateral as CoreCpiCreditOrDebitCollateral,
    CpiOpenTraderStateForTrader as CoreCpiOpenTraderStateForTrader,
    CpiReleaseCollateralToUser as CoreCpiReleaseCollateralToUser,
    PlaceLimitOrderV2Cpi as CorePlaceLimitOrderV2Cpi,
};
use flash_book::program::FlashBook;
use flash_book::state::{
    InsuranceFundAccount, TraderStateAccount as CoreTraderStateAccount,
    VaultAccount as CoreVaultAccount, VaultPositionAccount as CoreVaultPositionAccount,
};

declare_id!("GH7jCw81XvM5DsS647HNctqjy3SHvEGzG7bBVMDwYXCt");

/// Seed for this program's CPI authority PDA — must match the value
/// hardcoded in core's `WAVE21_VAULTS_PROGRAM_ID` whitelist.
pub const CPI_AUTHORITY_SEED: &[u8] = b"cpi_authority";

/// Mirror of core's `constants::USD_UNIT` (10^6) — fixed-point scaler
/// for NAV-per-share precision in the perf-fee crystallization math.
/// Re-declared locally to avoid pulling core's full constants module
/// just for one number.
pub const USD_UNIT: u64 = 1_000_000;

/// Mirror of core's `constants::BPS_DENOM` (10_000).
pub const BPS_DENOM: u32 = 10_000;

#[program]
pub mod flash_book_vaults {
    use super::*;

    /// Liveness check.
    pub fn ping(ctx: Context<Ping>) -> Result<()> {
        let slot = Clock::get()?.slot;
        emit!(Pong {
            program: *ctx.program_id,
            caller: ctx.accounts.caller.key(),
            slot,
        });
        Ok(())
    }

    /// Strategist creates a new vault. Vault id is per-strategist
    /// (0..255), so each strategist can run up to 256 vaults.
    pub fn create_vault_v3(
        ctx: Context<CreateVaultV3>,
        vault_id: u8,
        name: [u8; 32],
        perf_fee_bps: u32,
    ) -> Result<()> {
        // Cap perf fee at half (50%) — same as core's vault validation.
        require!(perf_fee_bps <= 5_000, VaultsError::OutOfRange);

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

    /// Depositor adds capital, mints shares pro-rata to current NAV.
    /// Bootstrap (no shares yet) mints 1:1.
    ///
    /// Phase 9b: depositor signs an SPL transfer from their ATA → core's
    /// `quote_vault` (depositor owns their ATA — no PDA signing for the
    /// IN direction). Wrapper records the deposit + mints shares.
    ///
    /// Wave 22 phase 5: ALSO credits the vault PDA's core TraderState
    /// collateral via inverse CPI so the matcher recognizes the
    /// capital when the strategist places orders. The strategist must
    /// have called `vault_open_trader_state_v3` once before the first
    /// deposit (bootstraps the vault's TraderState).
    pub fn vault_deposit_v3(
        ctx: Context<VaultDepositV3>,
        amount_quote_lots: u64,
    ) -> Result<()> {
        require!(amount_quote_lots > 0, VaultsError::ZeroSize);

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

        // Credit the vault's CORE TraderState collateral via inverse CPI.
        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let cpi_accounts = CoreCpiCreditOrDebitCollateral {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader_state: ctx.accounts.vault_trader_state.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            cpi_accounts,
            &signers,
        );
        flash_book::cpi::cpi_credit_collateral(cpi_ctx, amount_quote_lots)?;

        let vault = &mut ctx.accounts.vault;
        require!(vault.accept_deposits == 1, VaultsError::DepositsClosed);

        // ── Wave 22 NAV-source upgrade ───────────────────────────────
        // Use core's TraderState.collateral_quote_lots as the
        // AUTHORITATIVE NAV source (subtract the just-credited deposit
        // to recover pre-deposit NAV). This handles the case where
        // trading PnL has drifted the wrapper's `total_capital_quote_lots`
        // away from actual capital.
        //
        // Bootstrap: shares_outstanding == 0 → 1:1 mint regardless of
        // NAV (matches industry convention for the first depositor).
        let post_deposit_collateral =
            ctx.accounts.vault_trader_state.collateral_quote_lots;
        let pre_deposit_nav = post_deposit_collateral.saturating_sub(amount_quote_lots);
        let shares_to_mint: u64 = if vault.shares_outstanding == 0 || pre_deposit_nav == 0 {
            amount_quote_lots
        } else {
            let prod = (amount_quote_lots as u128)
                .checked_mul(vault.shares_outstanding as u128)
                .ok_or_else(|| error!(VaultsError::OutOfRange))?;
            let s = prod / (pre_deposit_nav as u128);
            require!(s <= u64::MAX as u128, VaultsError::OutOfRange);
            s as u64
        };
        require!(shares_to_mint > 0, VaultsError::ZeroSize);

        // total_capital_quote_lots is now an INFORMATIONAL cumulative
        // counter (lifetime gross deposits — NOT the live NAV).
        vault.total_capital_quote_lots = vault
            .total_capital_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;
        vault.shares_outstanding = vault
            .shares_outstanding
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;

        let pos = &mut ctx.accounts.position;
        if pos.depositor == Pubkey::default() {
            pos.vault = vault.key();
            pos.depositor = ctx.accounts.depositor.key();
            pos.bump = ctx.bumps.position;
        }
        pos.shares = pos
            .shares
            .checked_add(shares_to_mint)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;
        pos.total_deposited_quote_lots = pos
            .total_deposited_quote_lots
            .checked_add(amount_quote_lots)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?;

        emit!(VaultDepositV3Event {
            vault: vault.key(),
            depositor: pos.depositor,
            amount_quote_lots,
            shares_minted: shares_to_mint,
            shares_outstanding_after: vault.shares_outstanding,
        });
        Ok(())
    }

    /// Depositor burns shares to withdraw their pro-rata claim.
    /// Phase 9b: wrapper computes the payout, burns the shares, then
    /// CPIs into core's `cpi_release_collateral_to_user` which signs
    /// the SPL transfer from `quote_vault` → depositor's ATA as the
    /// InsuranceFund PDA.
    pub fn vault_withdraw_v3(
        ctx: Context<VaultWithdrawV3>,
        shares_to_burn: u64,
    ) -> Result<()> {
        require!(shares_to_burn > 0, VaultsError::ZeroSize);

        let vault_key = ctx.accounts.vault.key();
        let depositor_key = ctx.accounts.depositor.key();
        let total_shares = ctx.accounts.vault.shares_outstanding;

        require!(ctx.accounts.position.shares >= shares_to_burn, VaultsError::OutOfRange);
        require!(total_shares >= shares_to_burn, VaultsError::OutOfRange);
        require!(total_shares > 0, VaultsError::OutOfRange);

        // ── Wave 22 NAV-source upgrade ───────────────────────────────
        // Use core's TraderState.collateral_quote_lots as authoritative
        // NAV. Withdraw can only release CASH collateral — open trading
        // positions stay open. If collateral can't cover the pro-rata
        // payout, withdraw rejects (strategist must close positions
        // first to free collateral; matches HL pattern).
        let live_nav = ctx.accounts.vault_trader_state.collateral_quote_lots as u128;
        require!(live_nav > 0, VaultsError::VaultNavNonPositive);

        // Pro-rata withdrawal: amount = shares × NAV / shares_outstanding.
        let amount_u128 = (shares_to_burn as u128)
            .checked_mul(live_nav)
            .ok_or_else(|| error!(VaultsError::OutOfRange))?
            / (total_shares as u128);
        let amount: u64 = if amount_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            amount_u128 as u64
        };
        require!(amount > 0, VaultsError::ZeroSize);

        // Burn shares BEFORE the CPI (defensive — if the SPL transfer
        // fails the whole tx aborts and state rolls back).
        // Note: `total_capital_quote_lots` is the cumulative DEPOSITS
        // counter (informational); not decremented on withdraw. Live
        // NAV comes from core's TraderState.collateral.
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
                .ok_or_else(|| error!(VaultsError::OutOfRange))?;
        }

        // ── Wave 22 phase 5 — debit core TraderState collateral so it
        //    stays in sync with wrapper bookkeeping before SPL release.
        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let debit_accounts = CoreCpiCreditOrDebitCollateral {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader_state: ctx.accounts.vault_trader_state.to_account_info(),
        };
        let debit_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            debit_accounts,
            &signers,
        );
        flash_book::cpi::cpi_debit_collateral(debit_ctx, amount)?;

        // CPI into core for SPL release.
        let cpi_accounts = CoreCpiReleaseCollateralToUser {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            insurance_fund: ctx.accounts.insurance_fund.to_account_info(),
            quote_vault: ctx.accounts.quote_vault.to_account_info(),
            user_quote_ata: ctx.accounts.depositor_quote_ata.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            cpi_accounts,
            &signers,
        );
        flash_book::cpi::cpi_release_collateral_to_user(cpi_ctx, amount)?;

        emit!(VaultWithdrawV3Event {
            vault: vault_key,
            depositor: depositor_key,
            shares_burned: shares_to_burn,
            amount_quote_lots: amount,
            shares_outstanding_after: ctx.accounts.vault.shares_outstanding,
        });
        Ok(())
    }

    /// Wave 21 phase 10 — migrate a legacy core vault to VaultAccountV3.
    /// Strategist signs (only the vault owner can migrate). Drops
    /// `trader_state` (the v3 NAV bookkeeping is local; trading via
    /// `place_limit_order_v2_cpi` follows in a later wave).
    pub fn migrate_vault_to_v3(ctx: Context<MigrateVaultToV3>) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            src.strategist == ctx.accounts.strategist.key(),
            VaultsError::Unauthorized
        );

        let dst = &mut ctx.accounts.v3;
        dst.strategist = src.strategist;
        dst.bump = ctx.bumps.v3;
        dst.vault_id = src.vault_id;
        dst.accept_deposits = src.accept_deposits;
        dst._pad0 = 0;
        dst.name = src.name;
        dst.perf_fee_bps = src.perf_fee_bps;
        dst.shares_outstanding = src.shares_outstanding;
        // total_capital_quote_lots is not stored on the legacy
        // VaultAccount (it lived in the legacy TraderState). Initialize
        // to 0; on first deposit_v3 the SPL transfer + share-mint
        // recomputes accurately. Strategist may also call a follow-up
        // recapitalization ix.
        dst.total_capital_quote_lots = 0;

        emit!(LegacyVaultMigratedV3Event {
            strategist: dst.strategist,
            vault_id: dst.vault_id,
            legacy: src.key(),
            v3: dst.key(),
            shares_outstanding: dst.shares_outstanding,
        });
        Ok(())
    }

    /// Wave 21 phase 10 — migrate a depositor's legacy VaultPositionAccount
    /// to VaultPositionAccountV3. Depositor signs.
    pub fn migrate_vault_position_to_v3(
        ctx: Context<MigrateVaultPositionToV3>,
    ) -> Result<()> {
        let src = &ctx.accounts.legacy;
        require!(
            src.depositor == ctx.accounts.depositor.key(),
            VaultsError::Unauthorized
        );

        let dst = &mut ctx.accounts.v3;
        dst.vault = ctx.accounts.v3_vault.key();
        dst.depositor = src.depositor;
        dst.bump = ctx.bumps.v3;
        dst.shares = src.shares;
        dst.total_deposited_quote_lots = src.total_deposited_quote_lots;
        dst.total_withdrawn_quote_lots = src.total_withdrawn_quote_lots;

        emit!(LegacyVaultPositionMigratedV3Event {
            vault: dst.vault,
            depositor: dst.depositor,
            legacy: src.key(),
            v3: dst.key(),
            shares: dst.shares,
        });
        Ok(())
    }

    /// WAVE 22 / Phase 4 — crystallize the vault's performance fee.
    /// Strategist signs. If current NAV/share exceeds the high-water
    /// mark, mints new shares to the strategist's vault_position equal
    /// to the perf-fee share equivalent. Bumps HWM to post-mint
    /// NAV/share.
    ///
    /// Math (mirrors core's legacy `settle_vault_perf_fee`):
    ///   nav_per_share_x6 = total_capital × USD_UNIT / shares_outstanding
    ///   gain_per_share_x6 = nav_per_share - hwm
    ///   total_gain = gain_per_share_x6 × shares_outstanding / USD_UNIT
    ///   fee_qlots = total_gain × perf_fee_bps / BPS_DENOM
    ///   shares_to_mint = fee_qlots × shares_outstanding / total_capital
    ///   new_hwm = total_capital × USD_UNIT / (shares_outstanding + minted)
    ///
    /// Bootstrap: first call when HWM == 0 just anchors the HWM at
    /// then-current NAV/share without minting (no historical
    /// performance to crystallize).
    ///
    /// NAV source: core's TraderState.collateral_quote_lots (live,
    /// authoritative). Vault should be FLAT (no open positions) so
    /// the collateral represents the full vault value; with open
    /// positions, the mark-to-market component is ignored and only
    /// the cash collateral counts toward the HWM. Strategists should
    /// close out before settling for accurate crystallization.
    pub fn settle_vault_perf_fee_v3(
        ctx: Context<SettleVaultPerfFeeV3>,
    ) -> Result<()> {
        let vault = &ctx.accounts.vault;
        require!(
            ctx.accounts.strategist.key() == vault.strategist,
            VaultsError::Unauthorized
        );

        let shares_outstanding = vault.shares_outstanding;
        // No depositors yet → nothing to settle. Anchor HWM at unit.
        if shares_outstanding == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = USD_UNIT;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        // Live NAV = core TraderState.collateral (cash portion).
        let nav = ctx.accounts.vault_trader_state.collateral_quote_lots as u128;
        require!(nav > 0, VaultsError::VaultNavNonPositive);

        // Current NAV per share, scaled by USD_UNIT.
        let nav_per_share_x6 = nav.saturating_mul(USD_UNIT as u128) / (shares_outstanding as u128);
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

        // No gain → reject (caller can detect this client-side and skip
        // the call). Matches HL semantics: never mint without a real
        // crystallization opportunity.
        require!(
            nav_per_share_u64 > prev_hwm,
            VaultsError::VaultBelowHighWaterMark
        );

        let gain_per_share_x6 = (nav_per_share_u64 - prev_hwm) as u128;
        let total_gain = gain_per_share_x6
            .saturating_mul(shares_outstanding as u128)
            / (USD_UNIT as u128);
        let fee_qlots = total_gain
            .saturating_mul(vault.perf_fee_bps as u128)
            / (BPS_DENOM as u128);
        let shares_to_mint_u128 = fee_qlots
            .saturating_mul(shares_outstanding as u128)
            / nav;
        let shares_to_mint = if shares_to_mint_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            shares_to_mint_u128 as u64
        };

        // Rounding pushed to zero — anchor HWM and exit silently.
        if shares_to_mint == 0 {
            let v = &mut ctx.accounts.vault;
            v.hwm_nav_per_share_u64x6 = nav_per_share_u64;
            v.last_perf_settlement_unix = Clock::get()?.unix_timestamp.max(0) as u64;
            return Ok(());
        }

        // Mint to strategist's position.
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
                .ok_or(VaultsError::OutOfRange)?;
        }

        let vault_key = ctx.accounts.vault.key();
        let v = &mut ctx.accounts.vault;
        v.shares_outstanding = v
            .shares_outstanding
            .checked_add(shares_to_mint)
            .ok_or(VaultsError::OutOfRange)?;
        v.total_perf_shares_minted = v.total_perf_shares_minted.saturating_add(shares_to_mint);

        // Anchor HWM at post-mint NAV/share (diluted by mint).
        let new_nav_per_share_x6 =
            nav.saturating_mul(USD_UNIT as u128) / (v.shares_outstanding as u128);
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

    // ─── Wave 22 Phase 5 — Vault trading ────────────────────────────

    /// Bootstrap a core `TraderStateAccount` for this vault (PDA-trader
    /// pattern). One-time setup before the vault can place orders. The
    /// vault PDA can't sign for itself (it's a PDA, not a keypair), so
    /// the wrapper signs as its CPI authority and CPIs into core's
    /// `cpi_open_trader_state_for_trader` which inits the TraderState
    /// seeded by the vault PDA. Strategist signs (and pays rent).
    pub fn vault_open_trader_state_v3(
        ctx: Context<VaultOpenTraderStateV3>,
    ) -> Result<()> {
        require!(
            ctx.accounts.strategist.key() == ctx.accounts.vault.strategist,
            VaultsError::Unauthorized
        );

        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let cpi_accounts = CoreCpiOpenTraderStateForTrader {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader_owner: ctx.accounts.vault.to_account_info(),
            payer: ctx.accounts.strategist.to_account_info(),
            trader_state: ctx.accounts.vault_trader_state.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            cpi_accounts,
            &signers,
        );
        flash_book::cpi::cpi_open_trader_state_for_trader(cpi_ctx)?;

        emit!(VaultTraderStateOpenedV3Event {
            vault: ctx.accounts.vault.key(),
            vault_trader_state: ctx.accounts.vault_trader_state.key(),
        });
        Ok(())
    }

    /// Strategist places a limit order on behalf of vault depositors.
    /// The vault PDA is the trader; wrapper signs the CPI to core's
    /// `place_limit_order_v2_cpi`. Core validates the wrapper authority
    /// + injects the order into the hypertree with the vault PDA
    /// stamped as `RestingOrderV2.trader`. When that order fills,
    /// `apply_fill` settles into the vault's TraderState (already
    /// bootstrapped by `vault_open_trader_state_v3`) and the vault's
    /// PositionAccount (init_if_needed in apply_fill).
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
            VaultsError::Unauthorized
        );

        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let cpi_accounts = CorePlaceLimitOrderV2Cpi {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader: ctx.accounts.vault.to_account_info(),
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

        emit!(VaultOrderPlacedV3Event {
            vault: ctx.accounts.vault.key(),
            market: ctx.accounts.market.key(),
            side,
            size_lots,
            limit_ticks,
        });
        Ok(())
    }

    /// Strategist cancels a vault-PDA order via core's
    /// `cancel_order_v2_cpi`. Order ownership (trader == vault PDA)
    /// is validated inside core.
    pub fn vault_cancel_order_v3(
        ctx: Context<VaultCancelOrderV3>,
        side: u8,
        order_id: u64,
    ) -> Result<()> {
        require!(
            ctx.accounts.strategist.key() == ctx.accounts.vault.strategist,
            VaultsError::Unauthorized
        );

        let auth_bump = ctx.bumps.cpi_authority;
        let signer_seeds: &[&[u8]] = &[CPI_AUTHORITY_SEED, &[auth_bump]];
        let signers: [&[&[u8]]; 1] = [signer_seeds];
        let cpi_accounts = CoreCancelOrderV2Cpi {
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            trader: ctx.accounts.vault.to_account_info(),
            market: ctx.accounts.market.to_account_info(),
            market_book: ctx.accounts.market_book.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.flash_book_program.to_account_info(),
            cpi_accounts,
            &signers,
        );
        flash_book::cpi::cancel_order_v2_cpi(cpi_ctx, side, order_id)?;

        emit!(VaultOrderCancelledV3Event {
            vault: ctx.accounts.vault.key(),
            market: ctx.accounts.market.key(),
            side,
            order_id,
        });
        Ok(())
    }
}

/// V3 vault account. Seeds: `[b"vault_v3", strategist, vault_id]`.
#[account]
#[derive(Debug)]
pub struct VaultAccountV3 {
    pub strategist: Pubkey,
    pub bump: u8,
    pub vault_id: u8,
    pub accept_deposits: u8,
    pub _pad0: u8,
    pub name: [u8; 32],
    pub perf_fee_bps: u32,
    pub shares_outstanding: u64,
    pub total_capital_quote_lots: u64,
    /// HWM of NAV-per-share, scaled by USD_UNIT (1_000_000) for
    /// fixed-point precision. 0 = bootstrap (first settle anchors at
    /// then-current NAV/share with no fee charged).
    pub hwm_nav_per_share_u64x6: u64,
    /// Unix timestamp of last `settle_vault_perf_fee_v3` call.
    pub last_perf_settlement_unix: u64,
    /// Cumulative perf-fee shares minted to strategist (informational).
    pub total_perf_shares_minted: u64,
}
impl VaultAccountV3 {
    pub const SEED: &'static [u8] = b"vault_v3";
    pub fn space() -> usize {
        // 8 disc + 32 + 1 + 1 + 1 + 1 + 32 + 4 + 8 + 8
        // + 8 (hwm) + 8 (last_settle) + 8 (total_perf_minted) = 120.
        // Round 144.
        8 + 144
    }
}

/// V3 vault depositor position. Seeds: `[b"vault_position_v3", vault, depositor]`.
#[account]
#[derive(Debug, Default)]
pub struct VaultPositionAccountV3 {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub bump: u8,
    pub shares: u64,
    pub total_deposited_quote_lots: u64,
    pub total_withdrawn_quote_lots: u64,
}
impl VaultPositionAccountV3 {
    pub const SEED: &'static [u8] = b"vault_position_v3";
    pub fn space() -> usize {
        // 8 disc + 32 + 32 + 1 + 8 + 8 + 8 = 97. Round 112.
        8 + 112
    }
}

#[derive(Accounts)]
pub struct Ping<'info> {
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(vault_id: u8)]
pub struct CreateVaultV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        init,
        payer = strategist,
        space = VaultAccountV3::space(),
        seeds = [VaultAccountV3::SEED, strategist.key().as_ref(), &[vault_id]],
        bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultDepositV3<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = VaultPositionAccountV3::space(),
        seeds = [
            VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump,
    )]
    pub position: Account<'info, VaultPositionAccountV3>,

    /// Depositor's USDC ATA — debited.
    #[account(mut)]
    pub depositor_quote_ata: Account<'info, TokenAccount>,

    /// Core's protocol vault — credited.
    #[account(mut)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// CHECK: this program's CPI authority.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// Vault's TraderState in core. Boxed cross-program account read so
    /// the wrapper can use core's authoritative `collateral_quote_lots`
    /// as the NAV source for share-mint math (handles trading PnL drift
    /// from wrapper-side `total_capital_quote_lots`).
    #[account(mut)]
    pub vault_trader_state: Box<Account<'info, CoreTraderStateAccount>>,

    pub flash_book_program: Program<'info, FlashBook>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultWithdrawV3<'info> {
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    #[account(
        mut,
        seeds = [
            VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ VaultsError::Unauthorized,
    )]
    pub position: Account<'info, VaultPositionAccountV3>,

    /// CHECK: this program's CPI authority — derives from
    /// `[CPI_AUTHORITY_SEED]` under this program ID.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// Core's InsuranceFund PDA — signs the SPL transfer out.
    #[account(
        seeds = [InsuranceFundAccount::SEED],
        bump = insurance_fund.bump,
        seeds::program = flash_book_program.key(),
    )]
    pub insurance_fund: Account<'info, InsuranceFundAccount>,

    /// Core's protocol vault — debited via core CPI.
    #[account(mut, address = insurance_fund.quote_vault)]
    pub quote_vault: Account<'info, TokenAccount>,

    /// Depositor's USDC ATA — credited via core CPI.
    #[account(mut)]
    pub depositor_quote_ata: Account<'info, TokenAccount>,

    /// Vault's TraderState in core — read for authoritative NAV +
    /// debited by inner CPI before SPL release.
    #[account(mut)]
    pub vault_trader_state: Box<Account<'info, CoreTraderStateAccount>>,

    pub flash_book_program: Program<'info, FlashBook>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct MigrateVaultToV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    /// Legacy core vault — read-only.
    #[account(constraint = legacy.strategist == strategist.key() @ VaultsError::Unauthorized)]
    pub legacy: Account<'info, CoreVaultAccount>,

    /// Destination v3 vault.
    #[account(
        init,
        payer = strategist,
        space = VaultAccountV3::space(),
        seeds = [VaultAccountV3::SEED, legacy.strategist.as_ref(), &[legacy.vault_id]],
        bump,
    )]
    pub v3: Account<'info, VaultAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateVaultPositionToV3<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    /// Legacy core position — read-only.
    #[account(constraint = legacy.depositor == depositor.key() @ VaultsError::Unauthorized)]
    pub legacy: Account<'info, CoreVaultPositionAccount>,

    /// V3 vault that this position belongs to. Must already have been
    /// migrated via `migrate_vault_to_v3`.
    pub v3_vault: Account<'info, VaultAccountV3>,

    /// Destination v3 position.
    #[account(
        init,
        payer = depositor,
        space = VaultPositionAccountV3::space(),
        seeds = [
            VaultPositionAccountV3::SEED,
            v3_vault.key().as_ref(),
            depositor.key().as_ref(),
        ],
        bump,
    )]
    pub v3: Account<'info, VaultPositionAccountV3>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultOpenTraderStateV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    /// CHECK: this program's CPI authority — derives from
    /// `[CPI_AUTHORITY_SEED]` under this program ID.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// CHECK: vault's TraderState in core's address space — created by
    /// the inner CPI, validated against vault PDA via core's
    /// `cpi_open_trader_state_for_trader` seed constraint.
    #[account(mut)]
    pub vault_trader_state: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, FlashBook>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultPlaceOrderV3<'info> {
    pub strategist: Signer<'info>,

    #[account(
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    /// CHECK: this program's CPI authority.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// CHECK: market in core's address space — passed to the inner CPI
    /// which re-validates the PDA derivation against the
    /// `flash_book_program` ID. We don't deserialize it here (cross-
    /// program Account<T> can't synthesize Bumps).
    pub market: UncheckedAccount<'info>,

    /// CHECK: hypertree PDA in core; disc validated by the inner CPI.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, FlashBook>,
}

#[derive(Accounts)]
pub struct VaultCancelOrderV3<'info> {
    pub strategist: Signer<'info>,

    #[account(
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    /// CHECK: this program's CPI authority.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// CHECK: market in core; inner CPI validates.
    pub market: UncheckedAccount<'info>,

    /// CHECK: hypertree PDA in core; disc validated by the inner CPI.
    #[account(mut)]
    pub market_book: UncheckedAccount<'info>,

    pub flash_book_program: Program<'info, FlashBook>,
}

#[derive(Accounts)]
pub struct SettleVaultPerfFeeV3<'info> {
    #[account(mut)]
    pub strategist: Signer<'info>,

    #[account(
        mut,
        seeds = [VaultAccountV3::SEED, vault.strategist.as_ref(), &[vault.vault_id]],
        bump = vault.bump,
        constraint = vault.strategist == strategist.key() @ VaultsError::Unauthorized,
    )]
    pub vault: Account<'info, VaultAccountV3>,

    /// Strategist's own position — created lazily on first perf-fee
    /// crystallization so the minted shares have somewhere to land.
    #[account(
        init_if_needed,
        payer = strategist,
        space = VaultPositionAccountV3::space(),
        seeds = [
            VaultPositionAccountV3::SEED,
            vault.key().as_ref(),
            strategist.key().as_ref(),
        ],
        bump,
    )]
    pub strategist_position: Account<'info, VaultPositionAccountV3>,

    /// Vault's TraderState in core — read-only; provides authoritative
    /// `collateral_quote_lots` for live NAV.
    pub vault_trader_state: Box<Account<'info, CoreTraderStateAccount>>,

    pub system_program: Program<'info, System>,
}

#[error_code]
pub enum VaultsError {
    #[msg("argument out of allowed range")]
    OutOfRange,
    #[msg("size cannot be zero")]
    ZeroSize,
    #[msg("vault is closed for deposits")]
    DepositsClosed,
    #[msg("unauthorized caller")]
    Unauthorized,
    #[msg("vault NAV is non-positive (cannot mint shares)")]
    VaultNavNonPositive,
    #[msg("vault NAV/share is below the high-water mark — no perf fee owed")]
    VaultBelowHighWaterMark,
}

#[event]
pub struct Pong {
    pub program: Pubkey,
    pub caller: Pubkey,
    pub slot: u64,
}

#[event]
pub struct VaultV3CreatedEvent {
    pub vault: Pubkey,
    pub strategist: Pubkey,
    pub vault_id: u8,
    pub perf_fee_bps: u32,
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
pub struct LegacyVaultMigratedV3Event {
    pub strategist: Pubkey,
    pub vault_id: u8,
    pub legacy: Pubkey,
    pub v3: Pubkey,
    pub shares_outstanding: u64,
}

#[event]
pub struct LegacyVaultPositionMigratedV3Event {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub legacy: Pubkey,
    pub v3: Pubkey,
    pub shares: u64,
}

#[event]
pub struct VaultPerfFeeSettledV3Event {
    pub vault: Pubkey,
    pub strategist: Pubkey,
    pub shares_minted: u64,
    pub new_hwm_per_share_u64x6: u64,
}

#[event]
pub struct VaultTraderStateOpenedV3Event {
    pub vault: Pubkey,
    pub vault_trader_state: Pubkey,
}

#[event]
pub struct VaultOrderPlacedV3Event {
    pub vault: Pubkey,
    pub market: Pubkey,
    pub side: u8,
    pub size_lots: u64,
    pub limit_ticks: u64,
}

#[event]
pub struct VaultOrderCancelledV3Event {
    pub vault: Pubkey,
    pub market: Pubkey,
    pub side: u8,
    pub order_id: u64,
}
