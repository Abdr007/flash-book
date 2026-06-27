//! open_trader_sub_account — create an isolated sub-account for a wallet.
//!
//! PDA `[b"trader_state", wallet, [sub_index]]` (sub_index 1..=255; index 0 is
//! the main account from `open_trader_state`). Secure-by-default: the wallet
//! signs, the target is fresh, the PDA is re-derived. The sub-account carries
//! `.trader = wallet` (so deposit/withdraw bind to it the same way) and records
//! its `sub_index`.
//!
//! accounts: [wallet (signer, payer, w), sub_state (PDA, w), system_program]
//! data: sub_index (1 byte, 1..=255)

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::TRADER_STATE_SEED;
use crate::state::{TraderState, TRADER_STATE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const TRADER_STATE_LEN: usize = core::mem::size_of::<TraderState>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [wallet, sub_state, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let sub_index = data[0];
    if sub_index == 0 {
        // index 0 is the main account (open_trader_state); subs are 1..=255.
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(wallet)?;
    assert_uninitialized(sub_state)?;
    let sub_arr = [sub_index];
    let bump = assert_pda(
        sub_state,
        &[TRADER_STATE_SEED, &wallet.key()[..], &sub_arr[..]],
        program_id,
    )?;

    let lamports = Rent::get()?.minimum_balance(TRADER_STATE_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(TRADER_STATE_SEED),
        Seed::from(&wallet.key()[..]),
        Seed::from(&sub_arr[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        wallet,
        sub_state,
        system_program,
        lamports,
        TRADER_STATE_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let ts = &mut *(sub_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.disc = TRADER_STATE_DISC;
        ts.trader = *wallet.key();
        ts.collateral_quote_lots = 0;
        ts.open_positions = 0;
        ts.sub_index = sub_index;
    }
    Ok(())
}
