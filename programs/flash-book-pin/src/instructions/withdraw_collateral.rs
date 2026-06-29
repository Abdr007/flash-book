//! withdraw_collateral — pay quote tokens from the vault back to the trader and
//! debit their recorded collateral. The vault authority is the Insurance PDA,
//! so the payout is PDA-signed.
//!
//! POSITION-SAFE: rejects any withdrawal while the trader has open positions
//! (`open_positions != 0`, maintained by `apply_fill`). This is the conservative
//! "flat to withdraw" rule (same as the Anchor strict path); a future
//! partial/margin-aware withdraw can relax it once the full risk engine is
//! ported. Combined with the recorded-balance check, collateral backing live
//! positions can never be pulled.
//!
//! accounts: [trader (signer, w), trader_state (PDA, w), insurance (PDA, r),
//!            quote_vault (w), trader_quote_ata (w), token_program]
//! data: amount (u64 LE)

use crate::cpi::{token_transfer_signed, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
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
    // Sub-account aware: bind by program-ownership + disc + `.trader` (below),
    // not a strict PDA seed, so withdrawals work on the main OR a sub account.
    // Only a wallet's own trader_states carry `.trader == signer` (Phase-2d).
    assert_owned_by(trader_state, program_id)?;
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
        // ER-active traders (attested ER-reserved margin backing resting orders
        // that live OUTSIDE `open_positions`) MUST withdraw via the xdomain
        // variant that honors `reserved_margin`. Fail closed here — parity with
        // the Anchor strict path (`require!(s.er_active == 0, UseXDomainWithdraw)`).
        if ts.er_active != 0 {
            return Err(ProgramError::Custom(241)); // use xdomain withdraw
        }
        // Position-safe: must be flat to withdraw (collateral may back positions).
        if ts.open_positions != 0 {
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
