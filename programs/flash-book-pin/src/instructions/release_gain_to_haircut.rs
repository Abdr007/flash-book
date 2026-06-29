//! release_gain_to_haircut — the haircut ENTRY point. The market authority moves
//! a realized gain OUT of the trader's collateral and INTO their position's
//! warmup reserve, where it matures over `[h_min, h_max]` and later converts at
//! `h ≤ 1` (`mature_position` → `convert_position`). Residual is unchanged at
//! release (it is only debited at convert, by the credit actually paid out).
//!
//! The gain leaves the ISOLATED bucket if the position is isolated
//! (`collateral > 0`), else the trader's CROSS pool — the same bucket the gain
//! would have landed in.
//!
//! accounts: [authority (signer), market (program-owned, r), trader_state (owned, w),
//!            position (owned, w), position_haircut (PDA, owned, w),
//!            haircut_state (PDA, owned, r)]
//! data: gain_quote_lots (u64 LE)

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::guard::assert_signer;
use crate::haircut::{apply_release, PositionHaircutSnapshot};
use crate::seeds::{HAIRCUT_SEED, POSITION_HAIRCUT_SEED};
use crate::state::{
    Market, MarketHaircutState, Position, PositionHaircutState, TraderState, HAIRCUT_STATE_DISC,
    POSITION_DISC, POSITION_HAIRCUT_DISC, TRADER_STATE_DISC,
};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, market, trader_state, position, position_haircut, haircut_state, ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let gain = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if gain == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(authority)?;
    assert_market(market, pid)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(position, pid)?;
    assert_disc(position, &POSITION_DISC)?;
    assert_owned_by(position_haircut, pid)?;
    assert_disc(position_haircut, &POSITION_HAIRCUT_DISC)?;
    assert_owned_by(haircut_state, pid)?;
    assert_disc(haircut_state, &HAIRCUT_STATE_DISC)?;

    let market_key = *market.key();

    // ── auth: market authority ──────────────────────────────────────────
    {
        let d = market.try_borrow_data()?;
        let m = unsafe { &*(d.as_ptr() as *const Market) };
        if &m.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
    }
    let (ts_trader, ts_sub) = {
        let d = trader_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        (ts.trader, ts.sub_index)
    };
    let isolated = {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const Position) };
        // Bind by sub_index too (see convert_position) — defence-in-depth even
        // though this ix is market-authority gated.
        if p.trader != ts_trader || p.market != market_key || p.sub_index != ts_sub {
            return Err(ProgramError::InvalidArgument);
        }
        p.collateral_quote_lots > 0
    };

    // position_haircut: bound to this position + market, canonical PDA.
    let pre = {
        let d = position_haircut.try_borrow_data()?;
        let ph = unsafe { &*(d.as_ptr() as *const PositionHaircutState) };
        if &ph.position != position.key() || ph.market != market_key {
            return Err(ProgramError::InvalidArgument);
        }
        PositionHaircutSnapshot {
            released_reserve_quote_lots: ph.released_reserve_quote_lots,
            released_attached_at_slot: ph.released_attached_at_slot,
            matured_pos_quote_lots: ph.matured_pos_quote_lots,
            original_reserve_at_attach: ph.original_reserve_at_attach,
        }
    };
    assert_pda(
        position_haircut,
        &[POSITION_HAIRCUT_SEED, &market_key[..], &position.key()[..]],
        pid,
    )?;
    // haircut_state: the market's haircut engine must exist (gate + binding).
    {
        let d = haircut_state.try_borrow_data()?;
        let h = unsafe { &*(d.as_ptr() as *const MarketHaircutState) };
        if h.market != market_key {
            return Err(ProgramError::InvalidArgument);
        }
    }
    assert_pda(haircut_state, &[HAIRCUT_SEED, &market_key[..]], pid)?;

    // ── compute the post-release reserve (host-tested) ──────────────────
    let now = Clock::get()?.slot;
    let post = apply_release(pre, gain, now).map_err(|_| ProgramError::InvalidArgument)?;

    // ── debit the gain from the bucket it would have landed in ──────────
    unsafe {
        if isolated {
            let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut Position);
            p.collateral_quote_lots = p
                .collateral_quote_lots
                .checked_sub(gain)
                .ok_or(ProgramError::InsufficientFunds)?;
        } else {
            let ts = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr()
                as *mut TraderState);
            ts.collateral_quote_lots = ts
                .collateral_quote_lots
                .checked_sub(gain)
                .ok_or(ProgramError::InsufficientFunds)?;
        }
    }
    // ── write the warmup reserve ────────────────────────────────────────
    unsafe {
        let ph = &mut *(position_haircut.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut PositionHaircutState);
        ph.released_reserve_quote_lots = post.released_reserve_quote_lots;
        ph.released_attached_at_slot = post.released_attached_at_slot;
        ph.matured_pos_quote_lots = post.matured_pos_quote_lots;
        ph.original_reserve_at_attach = post.original_reserve_at_attach;
    }
    Ok(())
}
