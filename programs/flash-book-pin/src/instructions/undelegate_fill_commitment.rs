//! undelegate_fill_commitment — return the fill-commitment ring PDA from the ER
//! back to mainnet. Faithful port of the Anchor `undelegate_fill_commitment`;
//! mirrors undelegate_market_book with the ring's `[b"fill_commit", market]` seeds.
//!
//! accounts: [authority (signer, payer), market (program-owned, r),
//!            fill_commitment (delegated PDA, w, signs), owner_program (THIS
//!            program), delegate_buffer, system_program, delegation_program]
//! data: (none)

use crate::er::{cpi_undelegate, UndelegateAccounts, DELEGATE_BUFFER_TAG};
use crate::fill_commitment::FILL_COMMIT_SEED;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, fill_commitment, owner_program, delegate_buffer, system_program, delegation_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;
    assert_owned_by(market, pid)?;
    assert_disc(market, &MARKET_DISC)?;
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    // The ring PDA is delegated (owned by the delegation program) → bind by seed.
    let bump = assert_pda(fill_commitment, &[FILL_COMMIT_SEED, &market.key()[..]], pid)?;
    if owner_program.key() != pid {
        return Err(ProgramError::InvalidArgument);
    }
    if delegate_buffer.key()
        != &find_program_address(&[DELEGATE_BUFFER_TAG, &fill_commitment.key()[..]], pid).0
    {
        return Err(ProgramError::InvalidArgument);
    }

    let bump_arr = [bump];
    let seeds = [
        Seed::from(FILL_COMMIT_SEED),
        Seed::from(&market.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    cpi_undelegate(
        &UndelegateAccounts {
            payer: authority,
            delegated_account: fill_commitment,
            owner_program,
            buffer: delegate_buffer,
            system_program,
            delegation_program,
        },
        &signer,
    )
}
