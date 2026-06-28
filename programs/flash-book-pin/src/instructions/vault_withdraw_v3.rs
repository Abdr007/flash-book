//! vault_withdraw_v3 — a depositor burns shares for a pro-rata quote payout.
//! Faithful port of the Anchor `vault_withdraw_v3`. Mirror of the deposit: burn
//! shares via the host-tested/Kani-bounded `vault_math::payout_for_shares`,
//! debit the vault's TraderState, then PDA-signed SPL release quote_vault →
//! depositor ATA (authority = the Insurance PDA, like `withdraw_collateral`).
//!
//! H-6 (audit 2026-06): a v3 vault can hold open positions, and NAV =
//! collateral ignores unrealized loss. Redeeming while NOT flat would let an
//! early depositor exit at an inflated NAV and socialize the loss onto the
//! remaining pool — so redemption REQUIRES the vault flat (open_positions == 0).
//!
//! accounts: [depositor (signer, w), vault (program-owned, w),
//!            vault_trader_state (program-owned [b"trader_state", vault], w),
//!            position (program-owned [b"vault_position_v3", vault, depositor], w),
//!            insurance (PDA, r), quote_vault (w), depositor_quote_ata (w),
//!            token_program]
//! data: shares_to_burn (u64 LE)

use crate::cpi::{token_transfer_signed, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
use crate::state::{
    Insurance, TraderState, VaultPositionV3, VaultV3, INSURANCE_DISC, TRADER_STATE_DISC,
    VAULT_POSITION_V3_DISC, VAULT_V3_DISC,
};
use crate::vault_math::payout_for_shares;
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [depositor, vault, vault_trader_state, position, insurance, quote_vault, depositor_quote_ata, token_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let shares_to_burn = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if shares_to_burn == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(depositor)?;
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    assert_owned_by(vault, program_id)?;
    assert_disc(vault, &VAULT_V3_DISC)?;
    assert_owned_by(vault_trader_state, program_id)?;
    assert_disc(vault_trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(position, program_id)?;
    assert_disc(position, &VAULT_POSITION_V3_DISC)?;
    assert_owned_by(insurance, program_id)?;
    let ins_bump = assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    let total_shares = {
        let d = vault.try_borrow_data()?;
        unsafe { (*(d.as_ptr() as *const VaultV3)).shares_outstanding }
    };
    // depositor must own at least the shares they burn + bind (vault, depositor).
    {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const VaultPositionV3) };
        if &p.vault != vault.key() || &p.depositor != depositor.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if p.shares < shares_to_burn {
            return Err(ProgramError::InsufficientFunds);
        }
    }
    // vault's TraderState: keyed to the vault, FLAT (H-6), live NAV.
    let live_nav = {
        let d = vault_trader_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if ts.open_positions != 0 {
            return Err(ProgramError::InvalidArgument); // redemption requires flat
        }
        ts.collateral_quote_lots
    };
    // tokens must come from the canonical protocol vault.
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // floor(shares_to_burn * nav / total_shares); rejects over-burn / empty vault.
    let amount =
        payout_for_shares(shares_to_burn, total_shares, live_nav).map_err(|_| ProgramError::InvalidArgument)?;
    if amount == 0 {
        return Err(ProgramError::InvalidArgument); // dust redemption
    }

    // ── effects before interaction ──────────────────────────────────────
    unsafe {
        let v = &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultV3);
        v.shares_outstanding = v.shares_outstanding.saturating_sub(shares_to_burn);
    }
    unsafe {
        let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultPositionV3);
        p.shares = p.shares.saturating_sub(shares_to_burn);
        p.total_withdrawn_quote_lots = p
            .total_withdrawn_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    unsafe {
        let ts =
            &mut *(vault_trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.collateral_quote_lots = ts
            .collateral_quote_lots
            .checked_sub(amount)
            .ok_or(ProgramError::InsufficientFunds)?;
    }

    // ── PDA-signed payout: vault → depositor ATA (authority = Insurance PDA) ─
    let bump_arr = [ins_bump];
    let seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    token_transfer_signed(
        token_program,
        quote_vault,
        depositor_quote_ata,
        insurance,
        amount,
        &signer,
    )?;
    Ok(())
}
