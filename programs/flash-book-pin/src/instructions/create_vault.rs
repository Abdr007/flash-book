//! create_vault — a strategist creates a v3 vault account, PDA
//! `[b"vault_v3", strategist, vault_id]`. Records the vault config (name, perf
//! fee) and opens it for deposits; all balances start at 0. NO funds move at
//! creation — depositor flows (deposit/withdraw/settle perf fee) are later
//! batches.
//!
//! accounts: [strategist (signer, payer, w), vault (PDA, w, uninit), system_program]
//! data: [vault_id u8][name [u8;32]][perf_fee_bps u32]   — 37 bytes

use crate::cpi::create_pda_account;
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::VAULT_SEED;
use crate::state::{VaultV3, VAULT_V3_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    ProgramResult,
};

const VAULT_LEN: usize = core::mem::size_of::<VaultV3>();
/// Max performance fee (bps) — mirrors the anchor bound (50%).
const MAX_PERF_FEE_BPS: u32 = 5_000;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [strategist, vault, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 37 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let vault_id = data[0];
    let mut name = [0u8; 32];
    name.copy_from_slice(&data[1..33]);
    let perf_fee_bps = u32::from_le_bytes(data[33..37].try_into().unwrap());
    if perf_fee_bps > MAX_PERF_FEE_BPS {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(strategist)?;
    assert_uninitialized(vault)?;
    let id_arr = [vault_id];
    let bump = assert_pda(
        vault,
        &[VAULT_SEED, &strategist.key()[..], &id_arr[..]],
        program_id,
    )?;

    let lamports = Rent::get()?.minimum_balance(VAULT_LEN);
    let bump_arr = [bump];
    let seeds = [
        Seed::from(VAULT_SEED),
        Seed::from(&strategist.key()[..]),
        Seed::from(&id_arr[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        strategist,
        vault,
        system_program,
        lamports,
        VAULT_LEN as u64,
        program_id,
        &signer,
    )?;

    let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;
    unsafe {
        let v = &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultV3);
        v.disc = VAULT_V3_DISC;
        v.strategist = *strategist.key();
        v.name = name;
        v.shares_outstanding = 0;
        v.total_capital_quote_lots = 0;
        v.hwm_nav_per_share_u64x6 = 0;
        v.last_perf_settlement_unix = now_unix;
        v.total_perf_shares_minted = 0;
        v.perf_fee_bps = perf_fee_bps;
        v.bump = bump;
        v.vault_id = vault_id;
        v.accept_deposits = 1;
        v._pad0 = 0;
        v._reserved = [0u8; 32];
    }
    Ok(())
}
