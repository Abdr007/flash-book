//! migrate_position_to_trader_state_key — authority-gated. In Anchor this copies
//! a legacy position (keyed under an older PDA scheme) into a NEW position PDA
//! keyed by the trader_state and closes the legacy one. On flash-book-pin every
//! Position is created under the final PDA scheme from the start (and pin already
//! restructured per-position liquidation state into a separate
//! `PositionLiquidationState` PDA), so there are no legacy positions to migrate.
//!
//! The faithful pin equivalent verifies the supplied position is already
//! canonical (program-owned, right discriminator, and bound to the signing
//! trader) and is a no-op. Ported for parity.
//!
//! accounts: [trader (signer), position (program-owned)]
//! data: (none)

use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{Position, POSITION_DISC};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [trader, position, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_signer(trader)?;
    assert_owned_by(position, pid)?;
    assert_disc(position, &POSITION_DISC)?;
    {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const Position) };
        if &p.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }
    Ok(()) // already canonical (pin Positions use the final PDA scheme)
}
