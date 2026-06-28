//! mature_position — permissionless crank that advances a position's matured
//! positive PnL per the warmup schedule, via the host-tested
//! `haircut::apply_mature`. Drains the position's released reserve into its
//! matured bucket and adds the same delta to the market's matured total. NO
//! funds move and NO book is touched — this is warmup-accounting bookkeeping
//! (the value-releasing `convert_position` is a later, separate batch). Reverts
//! `Custom(122)` when there is nothing to mature this slot.
//!
//! accounts: [position_haircut (PDA, program-owned, w),
//!            haircut_state (PDA, program-owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_pda};
use crate::haircut::{apply_mature, PositionHaircutSnapshot};
use crate::seeds::{HAIRCUT_SEED, POSITION_HAIRCUT_SEED};
use crate::state::{
    MarketHaircutState, PositionHaircutState, HAIRCUT_STATE_DISC, POSITION_HAIRCUT_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [position_haircut, haircut_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── guard both accounts at their canonical PDAs (using their own stored
    //    seed components), bound to the same market ──────────────────────
    assert_owned_by(position_haircut, pid)?;
    assert_disc(position_haircut, &POSITION_HAIRCUT_DISC)?;
    assert_owned_by(haircut_state, pid)?;
    assert_disc(haircut_state, &HAIRCUT_STATE_DISC)?;

    let (pre, market, h_min, h_max) = {
        let pd = position_haircut.try_borrow_data()?;
        let p = unsafe { &*(pd.as_ptr() as *const PositionHaircutState) };
        let hd = haircut_state.try_borrow_data()?;
        let h = unsafe { &*(hd.as_ptr() as *const MarketHaircutState) };
        if p.market != h.market {
            return Err(ProgramError::InvalidArgument);
        }
        (
            PositionHaircutSnapshot {
                released_reserve_quote_lots: p.released_reserve_quote_lots,
                released_attached_at_slot: p.released_attached_at_slot,
                matured_pos_quote_lots: p.matured_pos_quote_lots,
                original_reserve_at_attach: p.original_reserve_at_attach,
            },
            p.market,
            h.h_min_slots,
            h.h_max_slots,
        )
    };
    // Confirm both accounts sit at their canonical PDAs for `market`.
    assert_pda(
        position_haircut,
        &[POSITION_HAIRCUT_SEED, &market[..], &position_haircut_position(position_haircut)?[..]],
        pid,
    )?;
    assert_pda(haircut_state, &[HAIRCUT_SEED, &market[..]], pid)?;

    let now = Clock::get()?.slot;
    let (post, delta) = apply_mature(pre, now, h_min, h_max).map_err(|_| ProgramError::InvalidArgument)?;
    if delta == 0 {
        return Err(ProgramError::Custom(122)); // nothing to mature this slot
    }

    unsafe {
        let p = &mut *(position_haircut.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut PositionHaircutState);
        p.released_reserve_quote_lots = post.released_reserve_quote_lots;
        p.released_attached_at_slot = post.released_attached_at_slot;
        p.matured_pos_quote_lots = post.matured_pos_quote_lots;
        p.original_reserve_at_attach = post.original_reserve_at_attach;

        let h = &mut *(haircut_state.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketHaircutState);
        let total = u128::from_le_bytes(h.matured_pos_total_quote_lots)
            .checked_add(delta as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        h.matured_pos_total_quote_lots = total.to_le_bytes();
    }
    Ok(())
}

/// Read the `position` pubkey a `PositionHaircutState` records (a seed of its own
/// PDA). Borrow-scoped so it doesn't overlap the mutable borrows above.
#[inline]
fn position_haircut_position(ai: &AccountInfo) -> Result<[u8; 32], ProgramError> {
    let d = ai.try_borrow_data()?;
    let p = unsafe { &*(d.as_ptr() as *const PositionHaircutState) };
    Ok(p.position)
}
