//! update_trailing_stop — permissionless crank that ratchets a trailing-stop
//! trigger's price as the mark moves favorably. The stop's `trigger_price_ticks`
//! tightens (never loosens) toward the mark by `trailing_offset_bps`; the
//! running anchor (max mark for a long stop / min for a short) is stored on the
//! trigger. Faithful port of the Anchor `update_trailing_stop` (MARK-only — pin
//! has no separate oracle).
//!
//! Idempotent: a no-progress mark (anchor not beaten) or a sub-tick move is a
//! clean no-op. Rejects a non-trailing (offset 0) or inactive trigger.
//!
//! accounts: [caller (signer), market (program-owned, r), trigger_order
//!            (program-owned, w)]
//! data: (none)

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_signer};
use crate::state::{Market, TriggerOrderV3, TRIGGER_FLAG_ACTIVE, TRIGGER_ORDER_V3_DISC};
use crate::trailing_stop::ratchet;
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [caller, market, trigger_order, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(caller)?;
    assert_market(market, pid)?;
    assert_owned_by(trigger_order, pid)?;
    assert_disc(trigger_order, &TRIGGER_ORDER_V3_DISC)?;

    let (t_market, kind, offset_bps, prev_anchor, cur_trigger, flags) = {
        let d = trigger_order.try_borrow_data()?;
        let t = unsafe { &*(d.as_ptr() as *const TriggerOrderV3) };
        (
            t.market, t.kind, t.trailing_offset_bps, t.trailing_anchor_ticks,
            t.trigger_price_ticks, t.flags,
        )
    };

    if t_market != *market.key() {
        return Err(ProgramError::InvalidArgument);
    }
    if offset_bps == 0 {
        return Err(ProgramError::Custom(180)); // not a trailing trigger
    }
    if flags & TRIGGER_FLAG_ACTIVE == 0 {
        return Err(ProgramError::Custom(181)); // inactive / already fired
    }

    let (mark, tick_size) = {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        (m.mark_price_ticks, m.tick_size)
    };

    // Pure ratchet; None = no progress / sub-tick no-op → clean no-op.
    if let Some((new_anchor, new_trigger)) =
        ratchet(kind, mark, offset_bps, prev_anchor, tick_size, cur_trigger)
    {
        unsafe {
            let t = &mut *(trigger_order.borrow_mut_data_unchecked().as_mut_ptr() as *mut TriggerOrderV3);
            t.trailing_anchor_ticks = new_anchor;
            t.trigger_price_ticks = new_trigger;
        }
    }
    Ok(())
}
