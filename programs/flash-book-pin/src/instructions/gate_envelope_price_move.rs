//! gate_envelope_price_move — READ-ONLY check that a proposed price move
//! (`old → new` over `dt_slots`) stays within the market's configured per-slot
//! envelope cap, via the host-tested `envelope::gate_price_move`. Reverts
//! `Custom(123)` on a breach. Mutates NO state — a keeper/client uses it to
//! pre-validate an oracle/mark move against the envelope band.
//!
//! accounts: [market, envelope_config (PDA, program-owned, r)]
//! data: [old_price_ticks u64][new_price_ticks u64][dt_slots u64]   — 24 bytes

use crate::envelope::gate_price_move;
use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::seeds::ENVELOPE_CONFIG_SEED;
use crate::state::{MarketEnvelopeConfig, ENVELOPE_CONFIG_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [market, envelope_config, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if data.len() < 24 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let old_price_ticks = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let new_price_ticks = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let dt_slots = u64::from_le_bytes(data[16..24].try_into().unwrap());

    assert_market(market, pid)?;
    assert_owned_by(envelope_config, pid)?;
    assert_pda(envelope_config, &[ENVELOPE_CONFIG_SEED, &market.key()[..]], pid)?;
    assert_disc(envelope_config, &ENVELOPE_CONFIG_DISC)?;

    let max_price_move_bps_per_slot = {
        let d = envelope_config.try_borrow_data()?;
        let c = unsafe { &*(d.as_ptr() as *const MarketEnvelopeConfig) };
        if &c.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        c.max_price_move_bps_per_slot
    };

    gate_price_move(old_price_ticks, new_price_ticks, dt_slots, max_price_move_bps_per_slot)
        .map_err(|_| ProgramError::Custom(123))?;
    Ok(())
}
