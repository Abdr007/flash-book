//! init_fee_tiers — create the protocol-singleton volume-based fee-tier table,
//! PDA `[b"fee_tiers"]`. The creator becomes its authority (the PDA is unique, so
//! this can only succeed once). Validated before the account is created.
//!
//! accounts: [authority (signer, payer, w), fee_tiers (PDA, w, uninit), system_program]
//! data: [volume_window_slots u64][tier_count u8]
//!     [ (min_volume u64)(maker_rebate i32)(taker_fee u32) ; tier_count ]

use crate::cpi::create_pda_account;
use crate::fee_tiers::{parse_fee_tiers, validate_fee_tiers};
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::FEE_TIERS_SEED;
use crate::state::{FeeTier, FeeTiers, FEE_TIERS_DISC, MAX_FEE_TIERS};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const FEE_TIERS_LEN: usize = core::mem::size_of::<FeeTiers>();

fn parse_header(data: &[u8]) -> Result<(u64, usize, &[u8]), ProgramError> {
    if data.len() < 9 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let window = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let count = data[8] as usize;
    Ok((window, count, &data[9..]))
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, fee_tiers, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let (window, count, rung_bytes) = parse_header(data)?;

    // parse + validate the table before creating anything.
    let mut buf = [(0u64, 0i32, 0u32); MAX_FEE_TIERS];
    let n = parse_fee_tiers(count, rung_bytes, &mut buf)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    validate_fee_tiers(window, &buf[..n]).map_err(|_| ProgramError::InvalidArgument)?;

    assert_signer(authority)?;
    assert_uninitialized(fee_tiers)?;
    let bump = assert_pda(fee_tiers, &[FEE_TIERS_SEED], program_id)?;

    let lamports = Rent::get()?.minimum_balance(FEE_TIERS_LEN);
    let bump_arr = [bump];
    let seeds = [Seed::from(FEE_TIERS_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    create_pda_account(
        authority,
        fee_tiers,
        system_program,
        lamports,
        FEE_TIERS_LEN as u64,
        program_id,
        &signer,
    )?;

    unsafe {
        let t = &mut *(fee_tiers.borrow_mut_data_unchecked().as_mut_ptr() as *mut FeeTiers);
        t.disc = FEE_TIERS_DISC;
        t.authority = *authority.key();
        t.bump = bump;
        t.tier_count = n as u8;
        t._pad0 = [0u8; 6];
        t.volume_window_slots = window;
        t.tiers = [FeeTier { min_volume_quote_lots: 0, maker_rebate_bps: 0, taker_fee_bps: 0 };
            MAX_FEE_TIERS];
        for (i, &(min_vol, maker, taker)) in buf[..n].iter().enumerate() {
            t.tiers[i] = FeeTier {
                min_volume_quote_lots: min_vol,
                maker_rebate_bps: maker,
                taker_fee_bps: taker,
            };
        }
    }
    Ok(())
}
