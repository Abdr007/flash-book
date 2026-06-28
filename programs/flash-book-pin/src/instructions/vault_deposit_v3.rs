//! vault_deposit_v3 — a depositor adds quote capital to a vault and is minted
//! shares pro-rata against the vault's pre-deposit NAV. Faithful port of the
//! Anchor `vault_deposit_v3`. Token flow mirrors `deposit_collateral` (depositor
//! ATA → protocol quote_vault), then the vault's TraderState is credited and the
//! share math runs via the host-tested/Kani-proved `vault_math::shares_to_mint`.
//!
//! NAV = the vault TraderState's collateral BEFORE this deposit. First deposit
//! (or NAV wiped to 0) mints 1:1; otherwise floor(amount * shares / nav).
//!
//! accounts: [depositor (signer, w), vault (program-owned, w),
//!            vault_trader_state (program-owned [b"trader_state", vault], w),
//!            position (program-owned [b"vault_position_v3", vault, depositor], w),
//!            insurance (PDA, r), quote_vault (w), depositor_quote_ata (w),
//!            token_program]
//! data: amount (u64 LE)

use crate::cpi::{token_transfer, TOKEN_PROGRAM_ID};
use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::INSURANCE_SEED;
use crate::state::{
    Insurance, TraderState, VaultPositionV3, VaultV3, INSURANCE_DISC, TRADER_STATE_DISC,
    VAULT_POSITION_V3_DISC, VAULT_V3_DISC,
};
use crate::vault_math::shares_to_mint;
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [depositor, vault, vault_trader_state, position, insurance, quote_vault, depositor_quote_ata, token_program, ..] =
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

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(depositor)?;
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    assert_owned_by(vault, program_id)?;
    assert_disc(vault, &VAULT_V3_DISC)?;
    assert_owned_by(vault_trader_state, program_id)?;
    assert_disc(vault_trader_state, &TRADER_STATE_DISC)?;
    assert_owned_by(position, program_id)?;
    assert_disc(position, &VAULT_POSITION_V3_DISC)?;
    assert_owned_by(insurance, program_id)?;
    assert_pda(insurance, &[INSURANCE_SEED], program_id)?;
    assert_disc(insurance, &INSURANCE_DISC)?;

    // vault accepts deposits?
    let (accept, shares_outstanding) = {
        let d = vault.try_borrow_data()?;
        let v = unsafe { &*(d.as_ptr() as *const VaultV3) };
        (v.accept_deposits, v.shares_outstanding)
    };
    if accept != 1 {
        return Err(ProgramError::InvalidArgument); // deposits closed
    }
    // the vault's TraderState must be keyed to the vault.
    let pre_deposit_nav = {
        let d = vault_trader_state.try_borrow_data()?;
        let ts = unsafe { &*(d.as_ptr() as *const TraderState) };
        if &ts.trader != vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
        ts.collateral_quote_lots
    };
    // the position record must bind (vault, depositor).
    {
        let d = position.try_borrow_data()?;
        let p = unsafe { &*(d.as_ptr() as *const VaultPositionV3) };
        if &p.vault != vault.key() || &p.depositor != depositor.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }
    // tokens must route to the canonical protocol vault.
    {
        let d = insurance.try_borrow_data()?;
        let ins = unsafe { &*(d.as_ptr() as *const Insurance) };
        if &ins.quote_vault != quote_vault.key() {
            return Err(ProgramError::InvalidArgument);
        }
    }

    // Share-mint math (pre-deposit NAV) — reject dust BEFORE moving tokens.
    let shares = shares_to_mint(amount, shares_outstanding, pre_deposit_nav)
        .map_err(|_| ProgramError::InvalidArgument)?;

    // ── transfer depositor ATA → vault, then credit + mint ──────────────
    token_transfer(token_program, depositor_quote_ata, quote_vault, depositor, amount)?;

    unsafe {
        let ts =
            &mut *(vault_trader_state.borrow_mut_data_unchecked().as_mut_ptr() as *mut TraderState);
        ts.collateral_quote_lots = ts
            .collateral_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    unsafe {
        let v = &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultV3);
        v.total_capital_quote_lots = v
            .total_capital_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        v.shares_outstanding = v
            .shares_outstanding
            .checked_add(shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    unsafe {
        let p = &mut *(position.borrow_mut_data_unchecked().as_mut_ptr() as *mut VaultPositionV3);
        p.shares = p.shares.checked_add(shares).ok_or(ProgramError::ArithmeticOverflow)?;
        p.total_deposited_quote_lots = p
            .total_deposited_quote_lots
            .checked_add(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(())
}
