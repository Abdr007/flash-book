//! withdraw_insurance_fund — the protocol admin withdraws surplus from the
//! insurance fund's quote vault to their own ATA. Authority-gated and FLOORED:
//! the post-withdrawal balance may never fall below `pause_threshold_quote_lots`
//! (the protocol's solvency floor), and the vault must actually hold the tokens.
//! PDA-signed by the Insurance account (the vault authority).
//!
//! accounts: [authority (signer), insurance (PDA, owned, w),
//!            quote_vault (token acct, w), authority_quote_ata (w), token_program]
//! data: amount_quote_lots (u64 LE)

use crate::cpi::{spl_token_amount, token_transfer_signed, TOKEN_ACCOUNT_LEN, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, INSURANCE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, insurance, quote_vault, authority_quote_ata, token_program, ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    // ── auth + guards ───────────────────────────────────────────────────
    assert_signer(authority)?;
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    assert_owned_by(insurance, program_id)?;
    let ins_bump = assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    if quote_vault.data_len() != TOKEN_ACCOUNT_LEN as usize
        || !quote_vault.is_owned_by(&TOKEN_PROGRAM_ID)
    {
        return Err(ProgramError::InvalidAccountData);
    }

    // ── compute + check the floored withdrawal ──────────────────────────
    let new_balance = {
        let d = insurance.try_borrow_data()?;
        let f = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &f.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
        if &f.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
        let nb = f
            .balance_quote_lots
            .checked_sub(amount)
            .ok_or(ProgramError::InsufficientFunds)?;
        // Solvency floor: never withdraw below the pause threshold.
        if nb < f.pause_threshold_quote_lots {
            return Err(ProgramError::InsufficientFunds);
        }
        nb
    };
    // The vault must hold the tokens.
    {
        let d = quote_vault.try_borrow_data()?;
        let vault_amount = spl_token_amount(&d).map_err(|_| ProgramError::InvalidAccountData)?;
        if vault_amount < amount {
            return Err(ProgramError::InsufficientFunds);
        }
    }

    // ── PDA-signed payout: vault → authority ATA ────────────────────────
    let bump_arr = [ins_bump];
    let seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    token_transfer_signed(
        token_program,
        quote_vault,
        authority_quote_ata,
        insurance,
        amount,
        &signer,
    )?;

    // ── book the debit (effects after a successful interaction) ─────────
    unsafe {
        let f = &mut *(insurance.borrow_mut_data_unchecked().as_mut_ptr() as *mut Insurance);
        f.balance_quote_lots = new_balance;
        f.total_payouts = f
            .total_payouts
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
