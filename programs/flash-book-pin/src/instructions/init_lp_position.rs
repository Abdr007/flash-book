//! init_lp_position — create a liquidity provider's FLP share-position account,
//! PDA `[b"lp_position", lp]`. Records the LP's shares + lifetime deposit/
//! withdrawal totals; created empty (0 shares). The LP funds it later via
//! `deposit_flp_capital` (follow-up).
//!
//! accounts: [lp (signer, payer, w), lp_position (PDA, w), system_program]

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::LP_POSITION_SEED;
use crate::state::{LpPosition, LP_POSITION_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const LP_POSITION_LEN: usize = core::mem::size_of::<LpPosition>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [lp, lp_position, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(lp)?;
    assert_uninitialized(lp_position)?;
    let bump = assert_pda(lp_position, &[LP_POSITION_SEED, &lp.key()[..]], program_id)?;

    let lamports = Rent::get()?.minimum_balance(LP_POSITION_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(LP_POSITION_SEED),
        Seed::from(&lp.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        lp,
        lp_position,
        system_program,
        lamports,
        LP_POSITION_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let p = &mut *(lp_position.borrow_mut_data_unchecked().as_mut_ptr() as *mut LpPosition);
        p.disc = LP_POSITION_DISC;
        p.lp = *lp.key();
        p.shares = 0;
        p.total_deposited_quote_lots = 0;
        p.total_withdrawn_quote_lots = 0;
        p.bump = bump;
    }
    Ok(())
}
