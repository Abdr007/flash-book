//! withdraw_flp_capital — an LP burns FLP shares and receives quote tokens at
//! the current NAV, paid out of the shared vault (PDA-signed by the Insurance
//! PDA, the vault authority).
//!
//! POOL-FLAT GATE: redemption is only allowed when the pool holds NO open
//! exposure (`flp_exposure.markets_count == 0`). Then NAV = capital +
//! realized_pnl is EXACT, so shares price correctly. With open positions the NAV
//! would omit unrealized PnL and LPs could redeem at a stale price — disallowed
//! until the unrealized-PnL/NAV path is ported (mirrors the vault flat-to-redeem
//! rule).
//!
//! accounts: [lp (signer, w), flp_exposure (PDA, w), lp_position (PDA, w),
//!            insurance (PDA, r), quote_vault (w), lp_quote_ata (w), token_program]
//! data: shares_to_burn (u64 LE)

use crate::cpi::{token_transfer_signed, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::{FLP_EXPOSURE_SEED, INSURANCE_SEED, LP_POSITION_SEED};
use crate::state::{
    FlpExposure, Insurance, LpPosition, FLP_EXPOSURE_DISC, INSURANCE_DISC, LP_POSITION_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
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
    let shares_to_burn = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if shares_to_burn == 0 {
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
    let ins_bump = assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // ── price the redemption (pool must be flat for an exact NAV) ────────
    let amount = {
        let d = flp_exposure.try_borrow_data()?;
        let f = unsafe { &*(d.as_ptr() as *const FlpExposure) };
        if f.markets_count != 0 {
            return Err(ProgramError::InvalidArgument);
        }
        if shares_to_burn > f.lp_shares_outstanding {
            return Err(ProgramError::InvalidArgument);
        }
        let a = FlpExposure::amount_for_shares(shares_to_burn, f.lp_shares_outstanding, f.nav())
            .ok_or(ProgramError::InvalidArgument)?;
        if a > f.total_capital_quote_lots {
            return Err(ProgramError::InsufficientFunds);
        }
        a
    };

    // ── the LP must actually hold the shares ────────────────────────────
    {
        let d = lp_position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const LpPosition) };
        if &p.lp != lp.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if p.shares < shares_to_burn {
            return Err(ProgramError::InsufficientFunds);
        }
    }

    // ── effects (debit) before interaction (payout) ─────────────────────
    unsafe {
        let f = &mut *(flp_exposure.borrow_mut_data_unchecked().as_mut_ptr() as *mut FlpExposure);
        f.total_capital_quote_lots = f
            .total_capital_quote_lots
            .checked_sub(amount)
            .ok_or(ProgramError::InsufficientFunds)?;
        f.lp_shares_outstanding = f
            .lp_shares_outstanding
            .checked_sub(shares_to_burn)
            .ok_or(ProgramError::InsufficientFunds)?;
    }
    unsafe {
        let p = &mut *(lp_position.borrow_mut_data_unchecked().as_mut_ptr() as *mut LpPosition);
        p.shares = p
            .shares
            .checked_sub(shares_to_burn)
            .ok_or(ProgramError::InsufficientFunds)?;
        p.total_withdrawn_quote_lots = p
            .total_withdrawn_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    // ── PDA-signed payout: vault → LP ATA (authority = Insurance PDA) ────
    let bump_arr = [ins_bump];
    let seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    token_transfer_signed(
        token_program,
        quote_vault,
        lp_quote_ata,
        insurance,
        amount,
        &signer,
    )?;
    Ok(())
}
