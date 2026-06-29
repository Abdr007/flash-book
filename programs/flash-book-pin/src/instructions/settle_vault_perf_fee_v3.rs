//! settle_vault_perf_fee_v3 — crystallize the strategist's performance fee when
//! the vault's NAV-per-share sets a new high-water mark. Faithful port of the
//! Anchor `settle_vault_perf_fee_v3`. The fee is paid by MINTING new shares to
//! the strategist (diluting holders by exactly perf_fee_bps of the gain), then
//! the HWM is reset to the post-mint NAV/share. No tokens move.
//!
//! The strategist's share record is a VaultPositionV3 keyed on (vault,
//! strategist); it must already exist (init_vault_position_v3) — pin uses an
//! explicit init rather than anchor's init_if_needed.
//!
//! accounts: [strategist (signer), vault (program-owned, w),
//!            vault_trader_state (program-owned [b"trader_state", vault], r),
//!            strategist_position (program-owned [b"vault_position_v3", vault,
//!            strategist], w)]
//! data: (none)

use crate::constants::USD_UNIT;
use crate::guard::{assert_disc, assert_owned_by, assert_signer};
use crate::state::{
    TraderState, VaultPositionV3, VaultV3, TRADER_STATE_DISC, VAULT_POSITION_V3_DISC, VAULT_V3_DISC,
};
use crate::vault_math::{nav_per_share_x6, perf_fee_shares};
use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [strategist, vault, vault_trader_state, strategist_position, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_signer(strategist)?;
    assert_owned_by(vault, program_id)?;
    assert_disc(vault, &VAULT_V3_DISC)?;
    assert_owned_by(vault_trader_state, program_id)?;
    assert_disc(vault_trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(strategist_position, program_id)?;
    assert_disc(strategist_position, &VAULT_POSITION_V3_DISC)?;

    let (v_strategist, shares_outstanding, prev_hwm, perf_fee_bps) = {
        let d = vault.try_borrow_data()?;
        let v = unsafe { &*(d.as_ptr() as *const VaultV3) };
        (v.strategist, v.shares_outstanding, v.hwm_nav_per_share_u64x6, v.perf_fee_bps)
    };
    if &v_strategist != strategist.key() {
        return Err(ProgramError::InvalidArgument); // only the strategist
    }

    let now_unix = Clock::get()?.unix_timestamp.max(0) as u64;

    // No shares yet → just (re)anchor the HWM at par.
    if shares_outstanding == 0 {
        unsafe {
            let v = &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultV3);
            v.hwm_nav_per_share_u64x6 = USD_UNIT;
            v.last_perf_settlement_unix = now_unix;
        }
        return Ok(());
    }

    // Live NAV from the vault's TraderState (must be keyed to the vault).
    let nav = {
        let d = vault_trader_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
        // Re-audit 2026-06 (MED): flat-gate the settle (parity with deposit/withdraw,
        // audit H-6). NAV = `collateral_quote_lots` ignores an open position's
        // unrealized PnL; settling a perf fee while the vault holds an open WINNING
        // position mints fee shares against a high that a later realized loss erases,
        // diluting the remaining depositors who also bore the fee. Require flat.
        if ts.open_positions != 0 {
            return Err(ProgramError::InvalidArgument); // vault has an open position
        }
        ts.collateral_quote_lots
    };
    if nav == 0 {
        return Err(ProgramError::InvalidArgument); // can't price shares
    }

    let nps = nav_per_share_x6(nav, shares_outstanding);

    // HWM unset → bootstrap it to the current NAV/share, no fee.
    if prev_hwm == 0 {
        unsafe {
            let v = &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultV3);
            v.hwm_nav_per_share_u64x6 = nps;
            v.last_perf_settlement_unix = now_unix;
        }
        return Ok(());
    }

    // Must be a new high to settle.
    if nps <= prev_hwm {
        return Err(ProgramError::Custom(170)); // no new high-water mark
    }

    let mint = perf_fee_shares(nav, shares_outstanding, nps, prev_hwm, perf_fee_bps);

    // New high but the fee rounds to dust → just advance the HWM.
    if mint == 0 {
        unsafe {
            let v = &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultV3);
            v.hwm_nav_per_share_u64x6 = nps;
            v.last_perf_settlement_unix = now_unix;
        }
        return Ok(());
    }

    // Credit the strategist's share record (must bind vault + strategist).
    unsafe {
        let p =
            &mut *(strategist_position.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultPositionV3);
        if &p.vault != vault.key() || &p.depositor != strategist.key() {
            return Err(ProgramError::InvalidArgument);
        }
        p.shares = p.shares.checked_add(mint).ok_or(ProgramError::ArithmeticOverflow)?;
    }

    // Mint the fee shares + reset the HWM to the post-mint NAV/share.
    unsafe {
        let v = &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultV3);
        v.shares_outstanding = v
            .shares_outstanding
            .checked_add(mint)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        v.total_perf_shares_minted = v.total_perf_shares_minted.saturating_add(mint);
        v.hwm_nav_per_share_u64x6 = nav_per_share_x6(nav, v.shares_outstanding);
        v.last_perf_settlement_unix = now_unix;
    }
    Ok(())
}
