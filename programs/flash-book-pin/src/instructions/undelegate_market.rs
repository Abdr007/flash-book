//! undelegate_market — return the Market PDA from the ER back to mainnet (paired
//! with undelegate_market_book). Faithful port of the Anchor `undelegate_market`.
//! Authority-gated; the market PDA signs the Undelegate CPI as itself.
//!
//! The market is currently DELEGATED (owned by the delegation program), so it is
//! bound by PDA re-derivation rather than program-ownership; its preserved data
//! still carries the authority for the gate.
//!
//! accounts: [authority (signer, payer), market (delegated PDA, w, signs),
//!            base_mint (r), quote_mint (r), owner_program (THIS program),
//!            delegate_buffer, system_program, delegation_program]
//! data: (none)

use crate::er::{cpi_undelegate, UndelegateAccounts, DELEGATE_BUFFER_TAG};
use crate::guard::{assert_pda, assert_signer};
use crate::seeds::MARKET_SEED;
use crate::state::Market;
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, base_mint, quote_mint, owner_program, delegate_buffer, system_program, delegation_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(authority)?;

    // Bind the market by canonical PDA (it is delegated → NOT owned by us, so no
    // ownership check); the preserved account data still carries the authority.
    let bump = assert_pda(
        market,
        &[MARKET_SEED, &base_mint.key()[..], &quote_mint.key()[..]],
        pid,
    )?;
    {
        let d = market.try_borrow_data()?;
        if d.len() < core::mem::size_of::<Market>() {
            return Err(ProgramError::InvalidAccountData);
        }
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }

    if owner_program.key() != pid {
        return Err(ProgramError::InvalidArgument);
    }
    if delegate_buffer.key() != &find_program_address(&[DELEGATE_BUFFER_TAG, &market.key()[..]], pid).0
    {
        return Err(ProgramError::InvalidArgument);
    }

    let bump_arr = [bump];
    let seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(&base_mint.key()[..]),
        Seed::from(&quote_mint.key()[..]),
        Seed::from(&bump_arr[..]),
    ];
    let signer = [Signer::from(&seeds[..])];
    cpi_undelegate(
        &UndelegateAccounts {
            payer: authority,
            delegated_account: market,
            owner_program,
            buffer: delegate_buffer,
            system_program,
            delegation_program,
        },
        &signer,
    )
}
