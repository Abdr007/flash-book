//! flp_deposit_v3 — an LP funds a market's per-market FLP-v3 pool and receives
//! shares priced on the pool's pre-deposit capital. Tokens go into the shared
//! protocol vault (the insurance fund's `quote_vault`). The LP's per-market share
//! position (`FlpPositionV3`) is created on first deposit (init-if-needed).
//!
//! accounts: [lp (signer, payer, w), exposure (PDA, owned, w),
//!            position (FlpPositionV3 PDA, w), insurance (PDA, owned, r),
//!            quote_vault (w), lp_quote_ata (w), token_program, system_program]
//! data: amount_quote_lots (u64 LE)

use crate::cpi::{create_pda_account, token_transfer, TOKEN_PROGRAM_ID};
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
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const POSITION_LEN: usize = core::mem::size_of::<FlpPositionV3>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [lp, exposure, position, insurance, quote_vault, lp_quote_ata, token_program, system_program, ..] =
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
    assert_owned_by(exposure, program_id)?;
    assert_disc(exposure, &FLP_PER_MARKET_V3_DISC)?;
    let (exp_market, nav, outstanding, pool_size) = {
        let d = exposure.try_borrow_data()?;
        let e = unsafe { &*(d.as_ptr() as *const FlpExposurePerMarketV3) };
        // Price on NAV (capital + realized_pnl), not capital alone.
        (e.market, e.nav(), e.lp_shares_outstanding, e.size_lots)
    };
    // Flat-gate (see flp_withdraw_v3): NAV omits the pool's open-position
    // unrealized PnL, so a deposit while the pool is non-flat misprices vs the
    // pending mark move. Require the pool flat to accept capital.
    if pool_size != 0 {
        return Err(ProgramError::InvalidArgument); // pool has an open position
    }
    assert_pda(exposure, &[FLP_PER_MARKET_SEED, &exp_market[..]], program_id)?;

    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // ── price the deposit on PRE-deposit NAV, reject dust / insolvent pool ──
    let shares = FlpExposurePerMarketV3::shares_for_deposit_v3(amount, outstanding, nav)
        .ok_or(ProgramError::InvalidArgument)?; // None ⇒ shares>0 but NAV ≤ 0 (insolvent)
    if shares == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    // ── pull the tokens (interaction), then credit (effects) ────────────
    token_transfer(token_program, lp_quote_ata, quote_vault, lp, amount)?;

    unsafe {
        let e = &mut *(exposure.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut FlpExposurePerMarketV3);
        e.total_capital_quote_lots = e
            .total_capital_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        e.lp_shares_outstanding = e
            .lp_shares_outstanding
            .checked_add(shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    // ── LP position: create on first deposit, else verify + add ─────────
    let bump = assert_pda(
        position,
        &[FLP_POSITION_V3_SEED, &exposure.key()[..], &lp.key()[..]],
        program_id,
    )?;
    if position.data_len() == 0 {
        let lamports = Rent::get()?.minimum_balance(POSITION_LEN);
        let bump_arr = [bump];
        let seeds = [
            Seed::from(FLP_POSITION_V3_SEED),
            Seed::from(&exposure.key()[..]),
            Seed::from(&lp.key()[..]),
            Seed::from(&bump_arr[..]),
        ];
        let signer = [Signer::from(&seeds[..])];
        create_pda_account(
            lp,
            position,
            system_program,
            lamports,
            POSITION_LEN as u64,
            program_id,
            &signer,
        )?;
        unsafe {
            let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut FlpPositionV3);
            p.disc = FLP_POSITION_V3_DISC;
            p.market = exp_market;
            p.lp = *lp.key();
            p.shares = 0;
            p.bump = bump;
            p._reserved = [0u8; 23];
        }
    } else {
        assert_disc(position, &FLP_POSITION_V3_DISC)?;
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const FlpPositionV3) };
        if &p.lp != lp.key() || p.market != exp_market {
            return Err(ProgramError::InvalidArgument);
        }
    }
    unsafe {
        let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut FlpPositionV3);
        p.shares = p
            .shares
            .checked_add(shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
