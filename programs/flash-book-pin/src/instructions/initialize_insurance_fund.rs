//! initialize_insurance_fund — create the protocol singleton Insurance PDA and
//! its quote-currency vault token account (authority = the Insurance PDA).
//!
//! PDA `[b"insurance_fund"]`. Secure-by-default: the authority must sign, the
//! Insurance + vault must be fresh, the Insurance PDA is re-derived, and the
//! token program id is checked. The vault is created as a token-program-owned
//! account then `InitializeAccount3`'d to the provided mint with the Insurance
//! PDA as authority (so withdrawals can be PDA-signed). Both the mint and vault
//! pubkeys are recorded on the Insurance account for later verification.
//!
//! accounts: [authority (signer, payer, w), insurance (PDA, w),
//!            quote_mint (r), quote_vault (signer, w, fresh keypair),
//!            token_program, system_program]
//! data: fee_contribution_bps (u32 LE)

use crate::cpi::{create_pda_account, init_token_account, TOKEN_ACCOUNT_LEN, TOKEN_PROGRAM_ID};
use crate::guard::{assert_pda, assert_signer, assert_uninitialized};
use crate::seeds::INSURANCE_SEED;
use crate::state::{Insurance, INSURANCE_DISC};
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvars::{rent::Rent, Sysvar},
    ProgramResult,
};

const INSURANCE_LEN: usize = core::mem::size_of::<Insurance>();

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, insurance, quote_mint, quote_vault, token_program, system_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 4 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let fee_contribution_bps = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if fee_contribution_bps > crate::constants::BPS_DENOM {
        return Err(ProgramError::InvalidArgument);
    }

    // ── guards ──────────────────────────────────────────────────────────
    assert_signer(authority)?;
    assert_signer(quote_vault)?; // the fresh keypair authorizes its own creation
    assert_uninitialized(insurance)?;
    assert_uninitialized(quote_vault)?;
    if token_program.key() != &TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let bump = assert_pda(insurance, &[INSURANCE_SEED], program_id)?;

    let rent = Rent::get()?;

    // ── 1. create the Insurance PDA (signed by its seeds) ───────────────
    let bump_arr = [bump];
    let ins_seeds = [Seed::from(INSURANCE_SEED), Seed::from(&bump_arr[..])];
    let ins_signer = [Signer::from(&ins_seeds[..])];
    create_pda_account(
        authority,
        insurance,
        system_program,
        rent.minimum_balance(INSURANCE_LEN),
        INSURANCE_LEN as u64,
        program_id,
        &ins_signer,
    )?;

    // ── 2. create the vault token account (signed by the vault keypair) ─
    create_pda_account(
        authority,
        quote_vault,
        system_program,
        rent.minimum_balance(TOKEN_ACCOUNT_LEN as usize),
        TOKEN_ACCOUNT_LEN,
        &TOKEN_PROGRAM_ID,
        &[], // empty seeds ⇒ the vault signs as a normal tx signer
    )?;

    // ── 3. InitializeAccount3 the vault → mint, authority = Insurance PDA ─
    init_token_account(token_program, quote_vault, quote_mint, insurance.key())?;

    // ── 4. stamp the Insurance account ──────────────────────────────────
    unsafe {
        let ins = &mut *(insurance.borrow_mut_data_unchecked().as_mut_ptr() as *mut Insurance);
        ins.disc = INSURANCE_DISC;
        ins.balance_quote_lots = 0;
        ins.total_contributions = 0;
        ins.fee_contribution_bps = fee_contribution_bps;
        ins.quote_mint = *quote_mint.key();
        ins.quote_vault = *quote_vault.key();
    }
    Ok(())
}
