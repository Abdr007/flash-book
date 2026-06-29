//! vault_open_trader_state_v3 — bootstrap a TraderState for a vault's PDA so the
//! vault can hold collateral + positions. The strategist signs + pays rent;
//! one-time setup before the first deposit. Faithful port of the Anchor
//! `vault_open_trader_state_v3` (reduced to pin's leaner TraderState).
//!
//! The TraderState is keyed on the VAULT pubkey (`[b"trader_state", vault]`),
//! NOT the strategist — the vault is the "trader" whose collateral/positions
//! this account tracks.
//!
//! accounts: [strategist (signer, payer, w), vault (program-owned, r),
//!            vault_trader_state (PDA [b"trader_state", vault], w, uninit),
//!            system_program]
//! data: (none)

use crate::cpi::create_pda_account;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::TRADER_STATE_SEED;
use crate::state::{TraderState, VaultV3, TRADER_STATE_DISC, VAULT_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const TRADER_STATE_LEN: usize = core::mem::size_of::<TraderState>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [strategist, vault, vault_trader_state, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(strategist)?;
    assert_owned_by(vault, program_id)?;
    assert_disc(vault, &VAULT_V3_DISC)?;

    // Only the vault's strategist may open its TraderState.
    {
        let d = vault.try_borrow_data()?;
        let v = unsafe { &*(d.as_ptr() as *const VaultV3) };
        if &v.strategist != strategist.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // PDA is keyed on the VAULT pubkey — the vault is the "trader".
    assert_uninitialized(vault_trader_state)?;
    let bump = assert_pda(
        vault_trader_state,
        &[TRADER_STATE_SEED, &vault.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(TRADER_STATE_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(TRADER_STATE_SEED),
        Seed::from(&vault.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        strategist,
        vault_trader_state,
        system_program,
        lamports,
        TRADER_STATE_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let ts =
            &mut *(vault_trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.disc = TRADER_STATE_DISC;
        ts.trader = *vault.key();
        ts.collateral_quote_lots = 0;
        ts.open_positions = 0;
        ts.sub_index = 0; // main account
    }
    Ok(())
}
