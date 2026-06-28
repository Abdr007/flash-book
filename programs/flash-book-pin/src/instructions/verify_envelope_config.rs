//! verify_envelope_config — READ-ONLY defensive check that a market's STORED
//! envelope params still satisfy the envelope invariant. Re-runs the host-tested
//! `envelope::prove_envelope` over the on-chain params and reverts `Custom(120)`
//! if they no longer hold. Mutates NO state.
//!
//! `set_envelope_config` proves before writing, so this normally passes — its job
//! is to catch corruption / a prove-logic change across a program upgrade (same
//! class as `verify_market_invariants`). The config is bound to the market.
//!
//! accounts: [market, envelope_config (PDA, program-owned, r)]

use crate::envelope::{prove_envelope, EnvelopeParams};
use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::seeds::ENVELOPE_CONFIG_SEED;
use crate::state::{MarketEnvelopeConfig, ENVELOPE_CONFIG_DISC};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, envelope_config, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_market(market, pid)?;
    assert_owned_by(envelope_config, pid)?;
    assert_pda(envelope_config, &[ENVELOPE_CONFIG_SEED, &market.key()[..]], pid)?;
    assert_disc(envelope_config, &ENVELOPE_CONFIG_DISC)?;

    let params = {
        let d = envelope_config.try_borrow_data()?;
        let c = unsafe { &*(d.as_ptr() as *const MarketEnvelopeConfig) };
        if &c.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        EnvelopeParams {
            max_price_move_bps_per_slot: c.max_price_move_bps_per_slot,
            max_accrual_dt_slots: c.max_accrual_dt_slots,
            max_abs_funding_e9_per_slot: c.max_abs_funding_e9_per_slot,
            maintenance_bps: c.maintenance_bps,
            liquidation_fee_bps: c.liquidation_fee_bps,
            min_liquidation_abs_lots: c.min_liquidation_abs_lots,
            min_nonzero_mm_req_lots: c.min_nonzero_mm_req_lots,
        }
    };

    prove_envelope(&params).map_err(|_| ProgramError::Custom(120))?;
    Ok(())
}
