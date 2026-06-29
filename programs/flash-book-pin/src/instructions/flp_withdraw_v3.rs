//! flp_withdraw_v3 — an LP burns per-market FLP-v3 shares and receives quote
//! tokens pro-rata to the pool's capital, paid out of the shared vault
//! (PDA-signed by the Insurance PDA, the vault authority). Capital-based pricing
//! (v3 has no unrealized-PnL NAV), so no flat-gate is needed.
//!
//! accounts: [lp (signer), exposure (PDA, owned, w), position (PDA, owned, w),
//!            insurance (PDA, owned, r), quote_vault (w), lp_quote_ata (w),
//!            token_program]
//! data: shares_to_burn (u64 LE)

use crate::cpi::{token_transfer_signed, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::{FLP_PER_MARKET_SEED, FLP_POSITION_V3_SEED, INSURANCE_SEED};
use crate::state::{
    FlpExposurePerMarketV3, FlpPositionV3, Insurance, FLP_PER_MARKET_V3_DISC, FLP_POSITION_V3_DISC,
    INSURANCE_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [lp, exposure, position, insurance, quote_vault, lp_quote_ata, token_program, ..] = accounts
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
    assert_owned_by(exposure, program_id)?;
    assert_disc(exposure, &FLP_PER_MARKET_V3_DISC)?;
    let (exp_market, capital, nav, total_shares, pool_size) = {
        let d = exposure.try_borrow_data()?;
        let e = unsafe { &*(d.as_ptr() as *const FlpExposurePerMarketV3) };
        (e.market, e.total_capital_quote_lots, e.nav(), e.lp_shares_outstanding, e.size_lots)
    };
    // Flat-gate (parity with vault_withdraw_v3 / the singleton FLP, audit H-6):
    // NAV = capital + REALIZED pnl excludes the pool's OPEN-position unrealized
    // mark PnL. Redeeming while the pool holds an open inventory position lets the
    // first LP exit at the stale NAV before a pending loss is realized, dumping it
    // on the remaining LPs / shared vault. Require the pool flat to redeem.
    if pool_size != 0 {
        return Err(ProgramError::InvalidArgument); // pool has an open position
    }
    assert_pda(exposure, &[FLP_PER_MARKET_SEED, &exp_market[..]], program_id)?;

    assert_owned_by(position, program_id)?;
    assert_disc(position, &FLP_POSITION_V3_DISC)?;
    assert_pda(
        position,
        &[FLP_POSITION_V3_SEED, &exposure.key()[..], &lp.key()[..]],
        program_id,
    )?;
    {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const FlpPositionV3) };
        if &p.lp != lp.key() || p.market != exp_market {
            return Err(ProgramError::InvalidArgument);
        }
        if shares_to_burn > p.shares {
            return Err(ProgramError::InsufficientFunds);
        }
    }

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

    // ── price the redemption on NAV (capital + realized_pnl) ────────────
    let amount = FlpExposurePerMarketV3::amount_for_shares_v3(shares_to_burn, nav, total_shares)
        .ok_or(ProgramError::InvalidArgument)?;
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    // A realized GAIN makes NAV exceed the pool's actual token capital; cap the
    // payout at capital so the shared vault is never over-paid (the gain stays as
    // a buffer until it's realized into capital). A loss already discounts `amount`
    // below the capital share, so this only bites on gains. Mirrors the singleton.
    if amount > capital {
        return Err(ProgramError::InsufficientFunds);
    }

    // ── effects (debit) before interaction (payout) ─────────────────────
    unsafe {
        let e = &mut *(exposure.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut FlpExposurePerMarketV3);
        e.lp_shares_outstanding = e
            .lp_shares_outstanding
            .checked_sub(shares_to_burn)
            .ok_or(ProgramError::InsufficientFunds)?;
        e.total_capital_quote_lots = e
            .total_capital_quote_lots
            .checked_sub(amount)
            .ok_or(ProgramError::InsufficientFunds)?;
    }
    unsafe {
        let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut FlpPositionV3);
        p.shares = p
            .shares
            .checked_sub(shares_to_burn)
            .ok_or(ProgramError::InsufficientFunds)?;
    }

    // ── PDA-signed payout: vault → LP ATA (authority = Insurance PDA) ────
    let bump_arr = [ins_bump];
    let seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    token_transfer_signed(token_program, quote_vault, lp_quote_ata, insurance, amount, &signer)?;
    Ok(())
}
