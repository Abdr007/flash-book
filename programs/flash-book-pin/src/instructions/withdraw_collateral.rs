//! withdraw_collateral — pay quote tokens from the vault back to the trader and
//! debit their recorded collateral. The vault authority is the Insurance PDA,
//! so the payout is PDA-signed.
//!
//! ⚠ SAFETY SCOPE: this checks only that the trader has sufficient RECORDED
//! collateral. It does NOT yet gate on open-position margin — the Pinocchio
//! `TraderState` does not track `open_positions`, and the risk / liquidation
//! flow is not yet ported. Until that lands, a withdrawal on a venue that holds
//! open positions is unsafe. This instruction is bootstrap-complete (the
//! init→fund→deposit→withdraw loop), but **margin-gating is a REQUIRED follow-up
//! before any standalone venue carries real positions.**
//!
//! accounts: [trader (signer, w), trader_state (PDA, w), insurance (PDA, r),
//!            quote_vault (w), trader_quote_ata (w), token_program]
//! data: amount (u64 LE)

use crate::cpi::{token_transfer_signed, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::{INSURANCE_SEED, TRADER_STATE_SEED};
use crate::state::{Insurance, TraderState, INSURANCE_DISC, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, trader_state, insurance, quote_vault, trader_quote_ata, token_program, ..] =
        accounts
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

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(trader)?;
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    assert_owned_by(trader_state, program_id)?;
    assert_pda(
        trader_state,
        &[TRADER_STATE_SEED, &trader.key()[..]],
        program_id,
    )?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(insurance, program_id)?;
    let ins_bump = assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    // trader binding + sufficient recorded balance.
    {
        let d = trader_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if ts.collateral_quote_lots < amount {
            return Err(ProgramError::InsufficientFunds);
        }
    }
    // the supplied vault must be the one recorded on the insurance account.
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // ── effects before interaction (checks-effects-interactions) ────────
    // Debit first; if the transfer fails the whole tx reverts atomically.
    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.collateral_quote_lots = ts
            .collateral_quote_lots
            .checked_sub(amount)
            .ok_or(ProgramError::InsufficientFunds)?;
    }

    // ── PDA-signed payout: vault → trader ATA (authority = Insurance PDA) ─
    let bump_arr = [ins_bump];
    let seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    token_transfer_signed(
        token_program,
        quote_vault,
        trader_quote_ata,
        insurance,
        amount,
        &signer,
    )?;
    Ok(())
}
