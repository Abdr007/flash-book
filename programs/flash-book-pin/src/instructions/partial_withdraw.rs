//! partial_withdraw_collateral (+ xdomain variants) — withdraw SOME collateral
//! while keeping open positions, gated by a post-withdraw INITIAL-margin stress
//! check + a 10%-of-notional floor (+ the ER reserved-margin floor on the
//! cross-domain path). Faithful port of the Anchor `partial_withdraw_core`.
//!
//! pin omits anchor's explicit `initial_margin_ratio_bps`, so the withdraw gate's
//! IM is derived from `max_leverage` (`BPS_DENOM / max_leverage`, floored at the
//! market's maintenance MMR) — IM >= MM, a buffer above the liquidation line.
//!
//! accounts: [trader (signer), trader_state (program-owned, w), insurance (PDA),
//!            quote_vault (w), trader_quote_ata (w), token_program,
//!            (market, position) * open_positions]
//! data: amount (u64 LE)

use crate::constants::{BPS_DENOM, WITHDRAWAL_FLOOR_BPS};
use crate::cpi::{token_transfer_signed, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::instructions::margin_probe::build_snapshot;
use crate::risk::{assess_margin, MarketSnapshot, PositionSnapshot, StressShock};
use crate::seeds::{ER_MARGIN_SEED, INSURANCE_SEED};
use crate::state::{
    ErMarginAttestation, Insurance, Market, TraderState, ER_MARGIN_DISC, INSURANCE_DISC,
    TRADER_STATE_DISC,
};
use crate::xmargin::{check_simple_withdraw, required_collateral_with_er};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    ProgramResult,
};

const MAX_PORTFOLIO: usize = 8;
const STRESS_BPS: [i32; 10] = [-3000, -2000, -1000, -500, -200, 200, 500, 1000, 2000, 3000];

/// Initial-margin bps for the withdraw gate: max(maintenance, BPS_DENOM/max_lev).
pub(crate) fn im_bps(maintenance_bps: u32, max_leverage: u32) -> u32 {
    if max_leverage == 0 {
        return maintenance_bps;
    }
    let lev_im = BPS_DENOM / max_leverage;
    if lev_im > maintenance_bps {
        lev_im
    } else {
        maintenance_bps
    }
}

/// Shared core: stress-gate a partial withdraw, then PDA-signed token release.
/// `pairs` = the trader's full open portfolio as (market, position) accounts.
#[allow(clippy::too_many_arguments)]
fn partial_withdraw_core(
    pid: &Pubkey,
    trader: &AccountInfo,
    trader_state: &AccountInfo,
    insurance: &AccountInfo,
    quote_vault: &AccountInfo,
    trader_quote_ata: &AccountInfo,
    token_program: &AccountInfo,
    pairs: &[AccountInfo],
    amount: u64,
    er_reserved: u64,
) -> ProgramResult {
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    assert_signer(trader)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(insurance, pid)?;
    let ins_bump = assert_pda(insurance, &[INSURANCE_SEED], pid)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    let (open, collateral) = {
        let d = trader_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &s.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
        (s.open_positions as usize, s.collateral_quote_lots)
    };
    if amount > collateral {
        return Err(ProgramError::InsufficientFunds);
    }
    // Vault must be the canonical protocol vault.
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    let post = collateral - amount;

    // ── build the portfolio (IM-margin snapshots) + 10%-notional floor ──
    let mut im_required: u64 = 0;
    if open > 0 {
        if open > MAX_PORTFOLIO || pairs.len() != open * 2 {
            return Err(ProgramError::InvalidArgument);
        }
        let mut positions: [PositionSnapshot; MAX_PORTFOLIO] = core::array::from_fn(|_| zero_pos());
        let mut markets: [MarketSnapshot; MAX_PORTFOLIO] = core::array::from_fn(|_| zero_mkt());
        let mut seen: [[u8; 32]; MAX_PORTFOLIO] = [[0u8; 32]; MAX_PORTFOLIO];
        let mut total_notional: u128 = 0;
        for i in 0..open {
            let m_ai = &pairs[2 * i];
            let p_ai = &pairs[2 * i + 1];
            let Some((pos, mut mkt, _c)) = build_snapshot(pid, m_ai, trader_state, p_ai, &[])? else {
                return Err(ProgramError::InvalidArgument);
            };
            if seen[..i].iter().any(|k| k == m_ai.key()) {
                return Err(ProgramError::InvalidArgument);
            }
            seen[i] = *m_ai.key();
            // RISK-2: override the snapshot's MMR with the INITIAL margin.
            let max_lev = {
                let d = m_ai.try_borrow_data()?;
                unsafe { (*(d.as_ptr() as *const Market)).max_leverage }
            };
            mkt.maintenance_margin_bps = im_bps(mkt.maintenance_margin_bps, max_lev);
            total_notional = total_notional.saturating_add(
                (pos.size_lots as u128)
                    .saturating_mul(mkt.mark_price.0 as u128)
                    .saturating_mul(mkt.tick_size as u128),
            );
            positions[i] = pos;
            markets[i] = mkt;
        }
        // Worst-case IM across the protocol-enforced correlated stress battery.
        let mut row = [StressShock { market: [0u8; 32], shock_bps: 0 }; MAX_PORTFOLIO];
        for &bps in STRESS_BPS.iter() {
            for (cell, m) in row[..open].iter_mut().zip(markets[..open].iter()) {
                *cell = StressShock { market: m.market, shock_bps: bps };
            }
            let a = assess_margin(&positions[..open], &markets[..open], &[&row[..open]], post)
                .map_err(|_| ProgramError::ArithmeticOverflow)?;
            if a.required_quote_lots > im_required {
                im_required = a.required_quote_lots;
            }
        }
        let notional_floor = (total_notional.saturating_mul(WITHDRAWAL_FLOOR_BPS as u128)
            / BPS_DENOM as u128)
            .min(u64::MAX as u128) as u64;
        let floor = required_collateral_with_er(im_required, notional_floor, er_reserved);
        if post < floor {
            return Err(ProgramError::Custom(240)); // breaches the withdraw floor
        }
    } else {
        // No positions: only the ER reserved-margin floor applies.
        if post < er_reserved {
            return Err(ProgramError::Custom(240));
        }
    }

    // ── debit + PDA-signed token release ────────────────────────────────
    unsafe {
        let s = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        s.collateral_quote_lots = post;
    }
    let bump_arr = [ins_bump];
    let seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    token_transfer_signed(token_program, quote_vault, trader_quote_ata, insurance, amount, &signer)
}

fn zero_pos() -> PositionSnapshot {
    PositionSnapshot {
        market: [0u8; 32], side: crate::order::Side::Long, size_lots: 0,
        entry_price: crate::lot::Ticks(0), cum_funding_index_at_entry: 0, collateral_quote_lots: 0,
    }
}
fn zero_mkt() -> MarketSnapshot {
    MarketSnapshot {
        market: [0u8; 32], mark_price: crate::lot::Ticks(0), cum_funding_index: 0,
        maintenance_margin_bps: 0, tick_size: 0, concentration_threshold_lots: 0,
        concentration_extra_mmr_bps: 0, side_oi_lots: 0, oi_mmr_slope_bps_per_million_lots: 0,
        oi_mmr_max_extra_bps: 0,
    }
}

const TS_ER_ACTIVE: usize = 156;

/// Strict partial withdraw — rejects ER-active traders (they must use xdomain).
pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, trader_state, insurance, quote_vault, trader_quote_ata, token_program, pairs @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());

    // Strict path: ER-active traders MUST use the xdomain variant.
    {
        let d = trader_state.try_borrow_data()?;
        if d.len() > TS_ER_ACTIVE && d[TS_ER_ACTIVE] != 0 {
            return Err(ProgramError::Custom(241)); // use xdomain withdraw
        }
    }
    partial_withdraw_core(
        pid, trader, trader_state, insurance, quote_vault, trader_quote_ata, token_program, pairs,
        amount, 0,
    )
}


/// Read + bind the trader's ER reserved-margin attestation, returning the
/// reserved margin. The attestation MUST be the canonical PDA for THIS
/// trader_state (blocks substituting another trader's / a stale one to
/// understate the reservation).
fn read_er_reserved(
    pid: &Pubkey,
    er_margin: &AccountInfo,
    trader_state_key: &Pubkey,
) -> Result<u64, ProgramError> {
    assert_owned_by(er_margin, pid)?;
    assert_disc(er_margin, &ER_MARGIN_DISC)?;
    assert_pda(er_margin, &[ER_MARGIN_SEED, &trader_state_key[..]], pid)?;
    let d = er_margin.try_borrow_data()?;
    let a = unsafe { &*(d.as_ptr() as *const ErMarginAttestation) };
    if &a.trader_state != trader_state_key {
        return Err(ProgramError::InvalidArgument);
    }
    Ok(a.reserved_margin_quote_lots)
}

/// Cross-domain PARTIAL withdraw — identical to the strict path but honors the
/// trader's ER reserved margin (the variant ER-active traders MUST use). Reuses
/// `partial_withdraw_core` with `er_reserved` from the attestation.
///
/// accounts: [trader (signer), trader_state (w), insurance, quote_vault (w),
///            trader_quote_ata (w), token_program, er_margin, (market, position)*]
pub fn xdomain(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, trader_state, insurance, quote_vault, trader_quote_ata, token_program, er_margin, pairs @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let er_reserved = read_er_reserved(pid, er_margin, trader_state.key())?;
    partial_withdraw_core(
        pid, trader, trader_state, insurance, quote_vault, trader_quote_ata, token_program, pairs,
        amount, er_reserved,
    )
}

/// Cross-domain FULL withdraw — the trader is FLAT (no filled positions), so only
/// the ER reserved-margin floor applies (`check_simple_withdraw`). Faithful port
/// of the Anchor `withdraw_collateral_xdomain`.
///
/// accounts: [trader (signer), trader_state (w), insurance, quote_vault (w),
///            trader_quote_ata (w), token_program, er_margin]
pub fn withdraw_xdomain(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [trader, trader_state, insurance, quote_vault, trader_quote_ata, token_program, er_margin, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    assert_signer(trader)?;
    assert_owned_by(trader_state, pid)?;
    assert_disc(trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(insurance, pid)?;
    let ins_bump = assert_pda(insurance, &[INSURANCE_SEED], pid)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    let er_reserved = read_er_reserved(pid, er_margin, trader_state.key())?;

    let collateral = {
        let d = trader_state.try_borrow_data()?;
        let s = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &s.trader != trader.key() {
            return Err(ProgramError::InvalidArgument);
        }
        if s.open_positions != 0 {
            return Err(ProgramError::Custom(242)); // full withdraw requires flat
        }
        s.collateral_quote_lots
    };
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }
    // Post-withdraw collateral must still cover the ER reservation.
    check_simple_withdraw(collateral, amount, er_reserved).map_err(|_| ProgramError::Custom(240))?;

    unsafe {
        let s = &mut *(trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        s.collateral_quote_lots = collateral - amount;
    }
    let bump_arr = [ins_bump];
    let seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let signer = [Signer::from(&seeds[..])];
    token_transfer_signed(token_program, quote_vault, trader_quote_ata, insurance, amount, &signer)
}
