//! cancel_jit_liquidation_offer — the maker cancels their pre-committed offer and
//! reclaims the rent (the PDA is closed, lamports refunded to the maker).
//! Maker-gated + PDA-bound. Faithful port of the Anchor `cancel_jit_liquidation_offer`.
//!
//! accounts: [maker (signer, w), jit_offer (PDA, program-owned, w)]
//! data: (none — the offer's own fields identify it)

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::JIT_LIQ_OFFER_SEED;
use crate::state::{JitLiquidationOffer, JIT_LIQ_OFFER_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [maker, jit_offer, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(maker)?;
    assert_owned_by(jit_offer, program_id)?;
    assert_disc(jit_offer, &JIT_LIQ_OFFER_DISC)?;

    let (o_market, o_maker, o_nonce) = {
        let d = jit_offer.try_borrow_data()?;
        let o = unsafe { &*(d.as_ptr() as *const JitLiquidationOffer) };
        (o.market, o.maker, o.nonce)
    };
    if &o_maker != maker.key() {
        return Err(ProgramError::IllegalOwner);
    }
    // Bind to the canonical PDA so a forged account can't be passed.
    let nonce_bytes = o_nonce.to_le_bytes();
    assert_pda(
        jit_offer,
        &[JIT_LIQ_OFFER_SEED, &o_market[..], &o_maker[..], &nonce_bytes],
        program_id,
    )?;

    // Close: refund the offer's lamports to the maker, then close the account.
    unsafe {
        let lamports = jit_offer.lamports();
        let m = maker.borrow_mut_lamports_unchecked();
        *m = m.checked_add(lamports).ok_or(ProgramError::ArithmeticOverflow)?;
        *jit_offer.borrow_mut_lamports_unchecked() = 0;
    }
    jit_offer.close()?;
    Ok(())
}
