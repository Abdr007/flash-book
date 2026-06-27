//! verify_protocol_solvency — permissionless, READ-ONLY protocol health check.
//! Asserts the shared quote vault covers the protocol-owned buckets it backs:
//! the insurance balance plus the FLP capital pool. Reverts (no state change) if
//! insolvent, so off-chain monitors can poll it and page when it errors.
//!
//! Mirrors the anchor `verify_protocol_solvency` (Kani-proven `assess_solvency`).
//! No signer: anyone may call it; it only reads and can only fail.
//!
//! accounts: [quote_vault (SPL token acct, r), insurance (PDA, r),
//!            flp_exposure (PDA, r)]

use crate::cpi::{spl_token_amount, TOKEN_ACCOUNT_LEN, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda};
use crate::seeds::{FLP_EXPOSURE_SEED, INSURANCE_SEED};
use crate::solvency::assess_solvency;
use crate::state::{FlpExposure, Insurance, FLP_EXPOSURE_DISC, INSURANCE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Custom error code returned when the protocol is insolvent (distinct from the
/// per-trader solvency failure, `Custom(100)`, in `verify_solvency`).
pub const PROTOCOL_INSOLVENT: u32 = 101;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [quote_vault, insurance, flp_exposure, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── guard the program-owned accounts (owner + canonical PDA + disc) ──
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    assert_owned_by(flp_exposure, program_id)?;
    assert_pda(flp_exposure, &[FLP_EXPOSURE_SEED], program_id)?;
    assert_disc(flp_exposure, &FLP_EXPOSURE_DISC)?;

    // ── the vault must be the SPL token account the insurance fund records ──
    if !quote_vault.is_owned_by(&TOKEN_PROGRAM_ID) {
        return Err(ProgramError::IllegalOwner);
    }
    if quote_vault.data_len() != TOKEN_ACCOUNT_LEN as usize {
        return Err(ProgramError::InvalidAccountData);
    }
    let insurance_bal;
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
        insurance_bal = ins.balance_quote_lots;
    }

    let flp_capital = {
        let d = flp_exposure.try_borrow_data()?;
        let f = unsafe { &*(d.as_ptr() as *const FlpExposure) };
        f.total_capital_quote_lots
    };

    let vault_amount = {
        let d = quote_vault.try_borrow_data()?;
        spl_token_amount(&d).map_err(|_| ProgramError::InvalidAccountData)?
    };

    // ── solvency arithmetic (host-tested; anchor parity) ────────────────
    let (solvent, _surplus) = assess_solvency(vault_amount, insurance_bal, flp_capital)
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if !solvent {
        return Err(ProgramError::Custom(PROTOCOL_INSOLVENT));
    }
    Ok(())
}
