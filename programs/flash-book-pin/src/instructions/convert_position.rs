//! convert_position — release a position's MATURED positive PnL to the trader at
//! the current haircut ratio `h`. The matured bucket leaves the global
//! denominator; the trader is credited `credit = matured · h` and the rounding
//! remainder `dust = matured − credit` accrues for a later `flush_haircut_dust`.
//!
//! VALUE IS CONSERVED, not invented: `residual −= credit` (the buffer that backed
//! the gain) and the trader's collateral `+= credit` (same amount). The credit
//! routes to the position's ISOLATED bucket if isolated (`collateral > 0`), else
//! the trader's CROSS pool.
//!
//! AUTH (anchor H9): convert moves value, so only the trader OR their delegate
//! may call — never an arbitrary keeper (who could front-run an unfavorable `h`).
//!
//! accounts: [keeper (signer), trader_state (owned, w), position (owned, w),
//!            position_haircut (PDA, owned, w), haircut_state (PDA, owned, w)]

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::haircut::{apply_convert, compute_h, PositionHaircutSnapshot};
use crate::seeds::{HAIRCUT_SEED, POSITION_HAIRCUT_SEED};
use crate::state::{
    MarketHaircutState, Position, PositionHaircutState, TraderState, HAIRCUT_STATE_DISC,
    POSITION_DISC, POSITION_HAIRCUT_DISC, TRADER_STATE_DISC,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [keeper, trader_state, position, position_haircut, haircut_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ── guards: every account program-owned + correct disc ──────────────
    assert_signer(keeper)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(position, pid)?;
    assert_disc(position, &POSITION_DISC)?;
    assert_owned_by(position_haircut, pid)?;
    assert_disc(position_haircut, &POSITION_HAIRCUT_DISC)?;
    assert_owned_by(haircut_state, pid)?;
    assert_disc(haircut_state, &HAIRCUT_STATE_DISC)?;

    // ── read + bind everything (immutable), then compute, then write ────
    let ts_trader;
    let ts_delegate;
    let ts_sub;
    {
        let d = trader_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        ts_trader = ts.trader;
        ts_delegate = ts.delegate;
        ts_sub = ts.sub_index;
    }
    // H9 auth: keeper is the trader or the (non-zero) delegate. The zero key can
    // never sign, so an unset delegate grants nothing.
    if keeper.key() != &ts_trader && keeper.key() != &ts_delegate {
        return Err(ProgramError::IllegalOwner);
    }

    let (pos_market, pos_isolated) = {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const Position) };
        // Bind by sub_index too (not just wallet): else a trader routes a
        // sub-account's matured gain into a DIFFERENT same-wallet pool, evading
        // the per-sub solvency-gated withdraw. Parity with build_snapshot/anchor.
        if p.trader != ts_trader || p.sub_index != ts_sub {
            return Err(ProgramError::InvalidArgument);
        }
        (p.market, p.collateral_quote_lots > 0)
    };

    let pre;
    {
        let d = position_haircut.try_borrow_data()?;
        let ph = unsafe { &*(d.as_ptr() as *const PositionHaircutState) };
        if &ph.position != position.key() || ph.market != pos_market {
            return Err(ProgramError::InvalidArgument);
        }
        pre = PositionHaircutSnapshot {
            released_reserve_quote_lots: ph.released_reserve_quote_lots,
            released_attached_at_slot: ph.released_attached_at_slot,
            matured_pos_quote_lots: ph.matured_pos_quote_lots,
            original_reserve_at_attach: ph.original_reserve_at_attach,
        };
    }
    assert_pda(
        position_haircut,
        &[POSITION_HAIRCUT_SEED, &pos_market[..], &position.key()[..]],
        pid,
    )?;

    let (residual, matured_pos_total, dust_accrued) = {
        let d = haircut_state.try_borrow_data()?;
        let h = unsafe { &*(d.as_ptr() as *const MarketHaircutState) };
        if h.market != pos_market {
            return Err(ProgramError::InvalidArgument);
        }
        (
            u128::from_le_bytes(h.residual_quote_lots),
            u128::from_le_bytes(h.matured_pos_total_quote_lots),
            u128::from_le_bytes(h.dust_accrued_quote_lots),
        )
    };
    assert_pda(haircut_state, &[HAIRCUT_SEED, &pos_market[..]], pid)?;

    let matured_at_call = pre.matured_pos_quote_lots;
    if matured_at_call == 0 {
        return Err(ProgramError::Custom(130)); // nothing to convert
    }

    // ── pure haircut math (host-tested) ─────────────────────────────────
    let h_scaled = compute_h(residual, matured_pos_total);
    let (post, credit, dust) = apply_convert(pre, h_scaled);

    // New market accumulators (all checked).
    let new_matured_total = matured_pos_total
        .checked_sub(matured_at_call as u128)
        .ok_or(ProgramError::InsufficientFunds)?;
    let new_dust = dust_accrued
        .checked_add(dust as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    // residual −= credit: the buffer that backed the released gain.
    let new_residual = residual
        .checked_sub(credit as u128)
        .ok_or(ProgramError::InsufficientFunds)?;

    // ── writes: position_haircut, haircut_state, then the trader credit ──
    unsafe {
        let ph = &mut *(position_haircut.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut PositionHaircutState);
        ph.matured_pos_quote_lots = post.matured_pos_quote_lots;
        ph.released_reserve_quote_lots = post.released_reserve_quote_lots;
        ph.released_attached_at_slot = post.released_attached_at_slot;
        ph.original_reserve_at_attach = post.original_reserve_at_attach;
    }
    unsafe {
        let h = &mut *(haircut_state.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut MarketHaircutState);
        h.matured_pos_total_quote_lots = new_matured_total.to_le_bytes();
        h.dust_accrued_quote_lots = new_dust.to_le_bytes();
        h.residual_quote_lots = new_residual.to_le_bytes();
    }
    // Credit the SAME `credit` to the trader — value conserved (residual was just
    // debited by it). Isolated gain → the position bucket; cross gain → the pool.
    unsafe {
        if pos_isolated {
            let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut Position);
            p.collateral_quote_lots = p
                .collateral_quote_lots
                .checked_add(credit)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        } else {
            let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr()
                as *mut TraderState);
            ts.collateral_quote_lots = ts
                .collateral_quote_lots
                .checked_add(credit)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }
    Ok(())
}
