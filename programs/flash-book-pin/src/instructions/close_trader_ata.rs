//! close_trader_ata — the trader closes their (empty) quote-token ATA and
//! reclaims its rent. A thin wrapper over the SPL-Token `CloseAccount`, which
//! ENFORCES that the account balance is 0 and that the signer is the account's
//! owner — so NO token value can move and a caller can only ever close their own
//! account.
//!
//! accounts: [trader (signer), trader_quote_ata (token acct, w),
//!            rent_destination (w), token_program]

use crate::cpi::close_token_account;
use crate::guard::assert_signer;
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(_program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [trader, trader_quote_ata, rent_destination, token_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(trader)?;
    // The token program enforces: trader == ata.owner AND ata.amount == 0.
    close_token_account(token_program, trader_quote_ata, rent_destination, trader)?;
    Ok(())
}
