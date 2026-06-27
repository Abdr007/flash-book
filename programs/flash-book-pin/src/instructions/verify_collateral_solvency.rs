//! verify_collateral_solvency — permissionless, READ-ONLY one-sided insolvency
//! proof. Sums trader collateral from a caller-supplied set of program-owned
//! collateral accounts (cross = TraderState, isolated = Position) and reverts if
//! that *partial* sum already exceeds the vault's headroom over the
//! protocol-owned buckets — which proves the protocol cannot cover its
//! liabilities. `false` is inconclusive (a larger set may still prove it), so a
//! monitor pages on revert, never on success.
//!
//! Mirrors the anchor `verify_collateral_solvency` (Kani-proven detector). The
//! anchor `fully_covered` arg only fed an off-chain event, so it is dropped here
//! (the port emits no events).
//!
//! accounts: [quote_vault (SPL token acct, r), insurance (PDA, r),
//!            flp_exposure (PDA, r), <collateral accounts...> ]

use crate::cpi::{spl_token_amount, TOKEN_ACCOUNT_LEN, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda};
use crate::seeds::{FLP_EXPOSURE_SEED, INSURANCE_SEED};
use crate::solvency::partial_collateral_proves_insolvent;
use crate::state::{
    FlpExposure, Insurance, Position, TraderState, FLP_EXPOSURE_DISC, INSURANCE_DISC,
    POSITION_DISC, TRADER_STATE_DISC,
};
use crate::guard::check_disc;
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Custom error code: trader collateral proves the protocol insolvent.
pub const PROTOCOL_INSOLVENT: u32 = 102;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [quote_vault, insurance, flp_exposure, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── guard the protocol-owned buckets (owner + PDA + disc) ───────────
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    assert_owned_by(flp_exposure, program_id)?;
    assert_pda(flp_exposure, &[FLP_EXPOSURE_SEED], program_id)?;
    assert_disc(flp_exposure, &FLP_EXPOSURE_DISC)?;

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

    // ── sum collateral across the supplied program-owned accounts ───────
    // Dedup by key (O(n²) scan, no alloc) so a repeated account cannot inflate
    // the partial sum into a FALSE insolvency.
    let mut partial_collateral: u64 = 0;
    for (i, ai) in rest.iter().enumerate() {
        // skip a key already counted earlier in the set.
        if rest[..i].iter().any(|p| p.key() == ai.key()) {
            continue;
        }
        // every collateral account must belong to this program.
        assert_owned_by(ai, program_id)?;
        let d = ai.try_borrow_data()?;
        let add = if check_disc(&d, &TRADER_STATE_DISC) {
            let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
            ts.collateral_quote_lots
        } else if check_disc(&d, &POSITION_DISC) {
            let p = unsafe { &*(d.as_ptr() as *const Position) };
            p.collateral_quote_lots
        } else {
            // program-owned but neither collateral type — reject (don't silently
            // skip, so a caller can't smuggle the wrong account type).
            return Err(ProgramError::InvalidAccountData);
        };
        partial_collateral = partial_collateral
            .checked_add(add)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    // ── one-sided proof (host-tested; anchor parity) ────────────────────
    let insolvent = partial_collateral_proves_insolvent(
        partial_collateral,
        flp_capital,
        insurance_bal,
        vault_amount,
    )
    .map_err(|_| ProgramError::ArithmeticOverflow)?;
    if insolvent {
        return Err(ProgramError::Custom(PROTOCOL_INSOLVENT));
    }
    Ok(())
}
