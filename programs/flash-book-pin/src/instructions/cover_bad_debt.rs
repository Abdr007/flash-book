//! cover_bad_debt — draw the insurance fund to cover a liquidation SHORTFALL
//! (bad debt) on this market. Sequencer-gated (C-1 trust model, exactly like
//! `apply_fill`): the sequencer attests the shortfall the off-chain matcher
//! computed — pin's host-tested `liquidation::compute_shortfall` is that
//! verification tool. The fund pays up to its balance; any UNCOVERED remainder
//! must be socialized via `auto_deleverage` (ADL) — the caller's next step.
//!
//! INSURANCE-ONLY: no position / OI mutation. The liquidation fill itself
//! (the `liquidate_position_v2` injected order, settled by the matcher /
//! `apply_fill`) already closed BOTH legs and kept `long_oi == short_oi`; this
//! step only refills the solvency gap the bankrupt close left (the maker was
//! paid more than the bankrupt trader's collateral could cover). Drawing the
//! fund (`balance −= covered`) raises the solvency residual `V − C_tot − I`
//! back by exactly the covered amount.
//!
//! accounts: [sequencer (signer), market (program-owned, r), insurance (PDA, w)]
//! data: [shortfall_quote_lots u64]

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda, assert_signer};
use crate::liquidation::cover_shortfall;
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, Market, INSURANCE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [sequencer, market, insurance, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let shortfall = u64::from_le_bytes(data[0..8].try_into().unwrap());

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(sequencer)?;
    assert_market(market, pid)?;
    assert_owned_by(insurance, pid)?;
    assert_pda(insurance, &[INSURANCE_SEED], pid)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    // C-1: only the market's settlement signer may attest a shortfall draw.
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.sequencer != sequencer.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    if shortfall == 0 {
        return Ok(()); // nothing owed — no-op
    }

    // ── draw the fund (host-tested + Kani-proven `cover_shortfall`) ─────
    unsafe {
        let f = &mut *(insurance.borrow_mut_data_unchecked().as_mut_ptr() as *mut Insurance);
        let (covered, _remaining) = cover_shortfall(f.balance_quote_lots, shortfall);
        // covered ≤ balance (proven) ⇒ the subtraction never underflows.
        f.balance_quote_lots -= covered;
        f.total_payouts = f.total_payouts.saturating_add(covered);
        // `_remaining > 0` ⇒ the fund is exhausted; the caller must socialize the
        // rest via auto_deleverage. (No on-chain effect here.)
    }
    Ok(())
}
