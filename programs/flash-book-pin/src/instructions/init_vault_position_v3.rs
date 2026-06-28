//! init_vault_position_v3 — create a depositor's share-record account for a
//! vault, PDA `[b"vault_position_v3", vault, depositor]`. Pure state init (no
//! funds move); one-time setup before the depositor's first `vault_deposit_v3`.
//!
//! Pin uses an explicit init ix rather than anchor's `init_if_needed` on the
//! deposit (mirrors how `init_position_liquidation_state` is split out), keeping
//! the hot deposit path free of conditional account creation.
//!
//! accounts: [depositor (signer, payer, w), vault (program-owned, r),
//!            position (PDA, w, uninit), system_program]
//! data: (none)

use crate::cpi::create_pda_account;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::VAULT_POSITION_SEED;
use crate::state::{VaultPositionV3, VAULT_POSITION_V3_DISC, VAULT_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const VAULT_POSITION_LEN: usize = core::mem::size_of::<VaultPositionV3>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [depositor, vault, position, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(depositor)?;
    assert_owned_by(vault, program_id)?;
    assert_disc(vault, &VAULT_V3_DISC)?;

    // PDA keyed on (vault, depositor).
    assert_uninitialized(position)?;
    let bump = assert_pda(
        position,
        &[VAULT_POSITION_SEED, &vault.key()[..], &depositor.key()[..]],
        program_id,
    )?;
    let lamports = Rent::get()?.minimum_balance(VAULT_POSITION_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(VAULT_POSITION_SEED),
        Seed::from(&vault.key()[..]),
        Seed::from(&depositor.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        depositor,
        position,
        system_program,
        lamports,
        VAULT_POSITION_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultPositionV3);
        p.disc = VAULT_POSITION_V3_DISC;
        p.vault = *vault.key();
        p.depositor = *depositor.key();
        p.shares = 0;
        p.total_deposited_quote_lots = 0;
        p.total_withdrawn_quote_lots = 0;
        p.bump = bump;
        p._reserved = [0u8; 23];
    }
    Ok(())
}
