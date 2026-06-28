//! verify_oracle_config — READ-ONLY consistency check on a market's oracle
//! config. Re-asserts the same bounds `init_market_oracle_config` enforced at
//! write time: `max_staleness_seconds > 0`, `max_confidence_bps ∈ (0, 1000]`, and
//! `source` is a known value (0 = trusted, 1 = Pyth). Reverts `Custom(125)` on a
//! breach. Mutates NO state.
//!
//! Port-addition (no standalone oracle-config verify in anchor): catches a
//! corrupted config across an upgrade, same enforcing-probe shape as the other
//! `verify_*` instructions.
//!
//! accounts: [market, oracle_config (PDA, program-owned, r)]

use crate::guard::{assert_disc, assert_market, assert_owned_by, assert_pda};
use crate::seeds::ORACLE_CONFIG_SEED;
use crate::state::{
    MarketOracleConfig, ORACLE_CONFIG_DISC, ORACLE_SOURCE_PYTH, ORACLE_SOURCE_TRUSTED,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Confidence cap ceiling (bps) — mirrors `init_market_oracle_config`.
const MAX_CONFIDENCE_BPS: u32 = 1_000;

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [market, oracle_config, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_market(market, pid)?;
    assert_owned_by(oracle_config, pid)?;
    assert_pda(oracle_config, &[ORACLE_CONFIG_SEED, &market.key()[..]], pid)?;
    assert_disc(oracle_config, &ORACLE_CONFIG_DISC)?;

    let ok = {
        let d = oracle_config.try_borrow_data()?;
        let c = unsafe { &*(d.as_ptr() as *const MarketOracleConfig) };
        if &c.market != market.key() {
            return Err(ProgramError::InvalidArgument);
        }
        c.max_staleness_seconds > 0
            && c.max_confidence_bps > 0
            && c.max_confidence_bps <= MAX_CONFIDENCE_BPS
            && (c.source == ORACLE_SOURCE_TRUSTED || c.source == ORACLE_SOURCE_PYTH)
    };

    if !ok {
        return Err(ProgramError::Custom(125));
    }
    Ok(())
}
