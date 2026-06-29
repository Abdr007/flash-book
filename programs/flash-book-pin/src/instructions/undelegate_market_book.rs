//! undelegate_market_book — return the order-book PDA from the ER back to this
//! program. Faithful port of the Anchor `undelegate_market_book`. Authority-gated;
//! the book PDA signs the Undelegate CPI as itself. (The permissionless
//! censorship-escape variant `force_undelegate_market_book` is a separate ix.)
//!
//! accounts: [authority (signer, payer), market (program-owned, r),
//!            market_book (delegated PDA, w, signs), owner_program (THIS program),
//!            delegate_buffer, system_program, delegation_program]
//! data: (none)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Market, MARKET_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [authority, market, ..] = accounts else {
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

    // Re-audit 2026-06-30: an L1-initiated Undelegate CPI is no longer a valid DLP
    // entrypoint (the upgraded DLP makes undelegation VALIDATOR-driven); the Anchor
    // reference removed this ix. Undelegate via the ER: `commit_and_undelegate_market_book`
    // (`cpi_commit(allow_undelegation=true)`) → the DLP's `process_undelegation`
    // callback on the base layer. Fail closed rather than issue a phantom CPI.
    Err(ProgramError::Custom(221)) // OwnerForceUndelegateUnavailable — use the ER path
}
