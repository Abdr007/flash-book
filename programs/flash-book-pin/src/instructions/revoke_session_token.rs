//! revoke_session_token — the owner revokes a session by closing its token
//! account and reclaiming the rent. Closing invalidates the session immediately
//! (the account no longer exists, so any session-auth check fails closed).
//! Owner-gated. Mirrors the `cancel_trigger_order` close/refund pattern.
//!
//! accounts: [owner (signer, w), session_token (program-owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{SessionToken, SESSION_TOKEN_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [owner, session_token, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(owner)?;
    assert_owned_by(session_token, program_id)?;
    assert_disc(session_token, &SESSION_TOKEN_DISC)?;
    {
        let d = session_token.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const SessionToken) };
        if &t.owner != owner.key() {
            return Err(ProgramError::InvalidArgument);
        }
    } // drop the data borrow before close()

    let lamports = session_token.lamports();
    unsafe {
        let to = owner.borrow_mut_lamports_unchecked();
        *to = to
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        *session_token.borrow_mut_lamports_unchecked() = 0;
    }
    session_token.close()?;
    Ok(())
}
