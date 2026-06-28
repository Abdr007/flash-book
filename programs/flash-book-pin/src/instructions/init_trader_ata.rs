//! init_trader_ata — create a trader's associated token account for the protocol
//! quote mint, via the ATA program's CreateIdempotent (a no-op if it already
//! exists). NO tokens move — this creates an EMPTY account the trader later
//! deposits from / withdraws to. `payer` funds the rent (may be anyone). The
//! mint is bound to the insurance fund's recorded `quote_mint`.
//!
//! accounts: [payer (signer, w), trader (wallet — key only), insurance (PDA, r),
//!            quote_mint, trader_ata (PDA-of-ATA-program, w), system_program,
//!            token_program, ata_program]

use crate::cpi::create_idempotent_ata;
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, INSURANCE_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [payer, trader, insurance, quote_mint, trader_ata, system_program, token_program, ata_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(payer)?;

    // The mint must be the protocol quote mint the insurance fund records.
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_mint != quote_mint.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // Create the trader's ATA (empty; idempotent). The ATA program verifies
    // `trader_ata` is the canonical address for (trader, token_program, mint).
    create_idempotent_ata(
        ata_program,
        payer,
        trader_ata,
        trader,
        quote_mint,
        system_program,
        token_program,
    )?;
    Ok(())
}
