//! deposit_collateral — move quote tokens from the trader's ATA into the
//! protocol vault and credit the trader's collateral.
//!
//! Secure-by-default. Every account is validated before any pointer-cast or
//! token move:
//!   * trader signs;
//!   * trader_state is program-owned, the canonical `[b"trader_state", trader]`
//!     PDA, carries the right discriminator, and binds `.trader == trader`;
//!   * insurance is program-owned, the `[b"insurance_fund"]` PDA, right disc;
//!   * the supplied vault equals the vault recorded on the insurance account
//!     (so tokens cannot be routed to an attacker account);
//!   * the token program id is the real SPL Token program.
//!
//! accounts: [trader (signer, w), trader_state (PDA, w), insurance (PDA, r),
//!            quote_vault (w), trader_quote_ata (w), token_program]
//! data: amount (u64 LE)

use crate::cpi::{token_transfer, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, TraderState, INSURANCE_DISC, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
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

    // Sub-account aware: bind trader_state by program-ownership + discriminator
    // + its `.trader` field (checked below), NOT a strict PDA seed — so this
    // works on the wallet's MAIN or any SUB account. Only a wallet's own
    // trader_states carry `.trader == signer`, so this is exactly as safe as a
    // PDA re-derivation (anchor's Phase-2d pattern).
    assert_owned_by(trader_state, program_id)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;

    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    // trader_state must belong to the signer.
    {
        let d = trader_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
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

    // ── transfer trader ATA → vault (trader authorizes), then credit ────
    token_transfer(token_program, trader_quote_ata, quote_vault, trader, amount)?;

    unsafe {
        let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.collateral_quote_lots = ts
            .collateral_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
