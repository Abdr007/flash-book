//! deposit_flp_capital — an LP funds the FLP (pool-as-maker) capital pool and
//! receives shares priced at the current NAV. Tokens go into the shared protocol
//! vault (the same vault collateral uses — the anchor model).
//!
//! Secure-by-default: LP signs; flp_exposure / lp_position / insurance are all
//! program-owned + canonical-PDA + correct-disc; the vault equals the one
//! recorded on the insurance account; the lp_position belongs to the signer.
//!
//! accounts: [lp (signer, w), flp_exposure (PDA, w), lp_position (PDA, w),
//!            insurance (PDA, r), quote_vault (w), lp_quote_ata (w), token_program]
//! data: amount (u64 LE)

use crate::cpi::{token_transfer, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::{FLP_EXPOSURE_SEED, INSURANCE_SEED, LP_POSITION_SEED};
use crate::state::{
    FlpExposure, Insurance, LpPosition, FLP_EXPOSURE_DISC, INSURANCE_DISC, LP_POSITION_DISC,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [lp, flp_exposure, lp_position, insurance, quote_vault, lp_quote_ata, token_program, ..] =
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
    assert_signer(lp)?;
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    assert_owned_by(flp_exposure, program_id)?;
    assert_pda(flp_exposure, &[FLP_EXPOSURE_SEED], program_id)?;
    assert_disc(flp_exposure, &FLP_EXPOSURE_DISC)?;
    assert_owned_by(lp_position, program_id)?;
    assert_pda(lp_position, &[LP_POSITION_SEED, &lp.key()[..]], program_id)?;
    assert_disc(lp_position, &LP_POSITION_DISC)?;
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    // vault identity + lp_position ownership.
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }
    {
        let d = lp_position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const LpPosition) };
        if &p.lp != lp.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // ── price the deposit at current NAV ────────────────────────────────
    let shares = {
        let d = flp_exposure.try_borrow_data()?;
        let f = unsafe { &*(d.as_ptr() as *const FlpExposure) };
        FlpExposure::shares_for_deposit(amount, f.lp_shares_outstanding, f.nav())
    }
    .ok_or(ProgramError::InvalidArgument)?;
    if shares == 0 {
        // Dust deposit that rounds to zero shares — reject rather than gift it.
        return Err(ProgramError::InvalidArgument);
    }

    // ── transfer LP ATA → vault, then credit shares + capital ───────────
    token_transfer(token_program, lp_quote_ata, quote_vault, lp, amount)?;

    unsafe {
        let f = &mut *(flp_exposure.borrow_mut_data_unchecked().as_mut_ptr() as *mut FlpExposure);
        f.total_capital_quote_lots = f
            .total_capital_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        f.lp_shares_outstanding = f
            .lp_shares_outstanding
            .checked_add(shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    unsafe {
        let p = &mut *(lp_position.borrow_mut_data_unchecked().as_mut_ptr() as *mut LpPosition);
        p.shares = p
            .shares
            .checked_add(shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        p.total_deposited_quote_lots = p
            .total_deposited_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
