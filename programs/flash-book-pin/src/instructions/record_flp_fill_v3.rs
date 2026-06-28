//! record_flp_fill_v3 — book a pool-as-maker fill into a market's per-market FLP
//! exposure: update the pool's net position (open / add-weighted / reduce / flip)
//! and accrue its realized PnL. Authority-gated (the exposure's recorder). NO
//! funds move, NO book — this is the FLP's position bookkeeping, the mirror of
//! `apply_fill`'s position update for a trader.
//!
//! accounts: [authority (signer), exposure (PDA, owned, w)]
//! data: [size_lots u64][price_ticks u64][side u8][realized_pnl_delta i64]  — 25 bytes

use crate::guard::{assert_disc, assert_owned_by, assert_pda, assert_signer};
use crate::seeds::FLP_PER_MARKET_SEED;
use crate::state::{FlpExposurePerMarketV3, FLP_PER_MARKET_V3_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [authority, exposure, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 25 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let size_lots = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let price_ticks = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let side = data[16];
    let realized_pnl_delta = i64::from_le_bytes(data[17..25].try_into().unwrap());

    if side > 1 || size_lots == 0 || price_ticks == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    assert_signer(authority)?;
    assert_owned_by(exposure, program_id)?;
    assert_disc(exposure, &FLP_PER_MARKET_V3_DISC)?;
    // Auth + canonical-PDA binding BEFORE any mutation.
    let market = {
        let d = exposure.try_borrow_data()?;
        let e = unsafe { &*(d.as_ptr() as *const FlpExposurePerMarketV3) };
        if &e.authority != authority.key() {
            return Err(ProgramError::IllegalOwner);
        }
        e.market
    };
    assert_pda(exposure, &[FLP_PER_MARKET_SEED, &market[..]], program_id)?;

    unsafe {
        let e = &mut *(exposure.borrow_mut_data_unchecked().as_mut_ptr()
            as *mut FlpExposurePerMarketV3);
        let (new_side, new_size, new_entry) = FlpExposurePerMarketV3::apply_flp_fill(
            e.side,
            e.size_lots,
            e.entry_price_ticks,
            side,
            size_lots,
            price_ticks,
        );
        e.side = new_side;
        e.size_lots = new_size;
        e.entry_price_ticks = new_entry;
        e.realized_pnl = e.realized_pnl.saturating_add(realized_pnl_delta);
    }
    Ok(())
}
