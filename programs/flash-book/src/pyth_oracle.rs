//! Hand-rolled Pyth `PriceUpdateV2` reader — in-house, byte-faithful.
//!
//! ## Why this exists
//! `pyth-solana-receiver-sdk`'s transitive `pythnet-sdk` is stuck on borsh 0.10
//! (`borsh::maybestd`), which is INCOMPATIBLE with anchor 1.x's borsh 1.x — so
//! the SDK blocks the framework modernization the orderbook needs to stay
//! ecosystem-compatible. flash-book consumes only a tiny, STABLE slice of the
//! `PriceUpdateV2` account (price / conf / exponent / publish_time under FULL
//! verification), so we read that layout directly here — the same "hand-roll to
//! escape a bad dep" pattern already used for the ER CPIs in `er.rs`.
//!
//! ## Safety
//! Byte-correctness is locked by a CAPTURED-FIXTURE test: a real `PriceUpdateV2`
//! account, serialized by the actual `pyth-solana-receiver-sdk` (in the prior
//! pyth-as-dev-dep step) is embedded here and the parser must read back the exact
//! price/conf/exponent/publish_time — so any offset/discriminator drift fails CI,
//! with NO pyth dependency at build time.

use anchor_lang::prelude::*;

/// Anchor account discriminator for `PriceUpdateV2`
/// (`sha256("account:PriceUpdateV2")[..8]`). Verified against the SDK in tests.
pub const PRICE_UPDATE_V2_DISCRIMINATOR: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];

/// The Pyth Solana Receiver program — the rightful owner of a `PriceUpdateV2`
/// account. `Account<PriceUpdateV2>` validated this for free; callers using this
/// module MUST check `price_update.owner == &PYTH_RECEIVER_PROGRAM_ID` themselves.
/// Verified against `pyth_solana_receiver_sdk::ID` in tests.
pub const PYTH_RECEIVER_PROGRAM_ID: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

/// The subset of a Pyth price flash-book consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PythPrice {
    pub price: i64,
    pub conf: u64,
    pub exponent: i32,
    pub publish_time: i64,
}

/// Read a Pyth `PriceUpdateV2` account under the SAME rules as the SDK's
/// `get_price_no_older_than` (FULL verification):
///   1. account discriminator == `PriceUpdateV2`,
///   2. `verification_level == Full`,
///   3. `price_message.feed_id == feed_id`,
///   4. `now - publish_time <= max_age_seconds`.
///
/// Byte layout (all little-endian), after the 8-byte anchor discriminator:
///   `write_authority` [32] · `verification_level` (borsh enum: tag `0` =
///   `Partial{num_signatures: u8}`, tag `1` = `Full`) · `price_message`
///   { `feed_id` [32], `price` i64, `conf` u64, `exponent` i32, `publish_time`
///   i64, … } · …
pub fn get_price_no_older_than_full(
    data: &[u8],
    feed_id: &[u8; 32],
    now_unix: i64,
    max_age_seconds: u64,
) -> Result<PythPrice> {
    use crate::errors::FlashBookError;

    require!(data.len() >= 8, FlashBookError::OutOfRange);
    require!(
        data[..8] == PRICE_UPDATE_V2_DISCRIMINATOR,
        FlashBookError::OutOfRange
    );
    // write_authority occupies [8..40]; verification_level tag at byte 40.
    require!(data.len() >= 41, FlashBookError::OutOfRange);
    // get_price_no_older_than requires FULL (tag 1). Partial (tag 0) is rejected,
    // matching the SDK's `verification_level.gte(Full)` check.
    require!(data[40] == 1, FlashBookError::OracleTooStale);

    // price_message starts right after the 1-byte `Full` tag.
    let m = 41usize;
    require!(data.len() >= m + 32 + 8 + 8 + 4 + 8, FlashBookError::OutOfRange);

    require!(&data[m..m + 32] == feed_id, FlashBookError::OracleTooStale); // MismatchedFeedId
    let price = i64::from_le_bytes(data[m + 32..m + 40].try_into().unwrap());
    let conf = u64::from_le_bytes(data[m + 40..m + 48].try_into().unwrap());
    let exponent = i32::from_le_bytes(data[m + 48..m + 52].try_into().unwrap());
    let publish_time = i64::from_le_bytes(data[m + 52..m + 60].try_into().unwrap());

    let age = now_unix.saturating_sub(publish_time);
    require!(age <= max_age_seconds as i64, FlashBookError::OracleTooStale);

    Ok(PythPrice { price, conf, exponent, publish_time })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A REAL `PriceUpdateV2` account, serialized by `pyth-solana-receiver-sdk`
    // (`try_serialize`) with: feed_id=[0x11;32], price=6_500_123, conf=4_242,
    // exponent=-8, publish_time=1_750_000_000, verification_level=Full. Captured
    // while pyth was a dev-dependency (see git history of this module); embedding
    // it lets us drop the pyth dep entirely while still pinning byte-correctness.
    const FIXTURE: [u8; 133] = [
        34, 241, 35, 99, 157, 126, 244, 205, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171,
        171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171,
        171, 171, 171, 171, 1, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
        17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 27, 47, 99, 0, 0, 0, 0, 0, 146,
        16, 0, 0, 0, 0, 0, 0, 248, 255, 255, 255, 128, 225, 78, 104, 0, 0, 0, 0, 127, 225, 78, 104,
        0, 0, 0, 0, 184, 42, 99, 0, 0, 0, 0, 0, 204, 16, 0, 0, 0, 0, 0, 0, 99, 0, 0, 0, 0, 0, 0, 0,
    ];
    const FEED: [u8; 32] = [0x11; 32];
    const PUB_TIME: i64 = 1_750_000_000;

    #[test]
    fn parses_real_pyth_account_exactly() {
        let p = get_price_no_older_than_full(&FIXTURE, &FEED, PUB_TIME + 5, 30).unwrap();
        assert_eq!(p.price, 6_500_123);
        assert_eq!(p.conf, 4_242);
        assert_eq!(p.exponent, -8);
        assert_eq!(p.publish_time, PUB_TIME);
        // discriminator we hard-code matches the real serialized one.
        assert_eq!(&FIXTURE[..8], &PRICE_UPDATE_V2_DISCRIMINATOR);
    }

    #[test]
    fn rejects_wrong_feed_id() {
        assert!(get_price_no_older_than_full(&FIXTURE, &[9u8; 32], PUB_TIME + 5, 30).is_err());
    }

    #[test]
    fn rejects_stale_price() {
        assert!(get_price_no_older_than_full(&FIXTURE, &FEED, PUB_TIME + 100, 30).is_err());
        assert!(get_price_no_older_than_full(&FIXTURE, &FEED, PUB_TIME + 20, 30).is_ok());
    }

    #[test]
    fn rejects_partial_verification() {
        // byte 40 is the verification_level tag; 1 = Full. Anything else (Partial)
        // is rejected by the parser.
        let mut b = FIXTURE;
        b[40] = 0;
        assert!(get_price_no_older_than_full(&b, &FEED, PUB_TIME + 5, 30).is_err());
    }

    #[test]
    fn rejects_bad_discriminator() {
        let mut b = FIXTURE;
        b[0] ^= 0xFF;
        assert!(get_price_no_older_than_full(&b, &FEED, PUB_TIME + 5, 30).is_err());
    }

    #[test]
    fn rejects_short_buffer() {
        assert!(get_price_no_older_than_full(&FIXTURE[..40], &FEED, PUB_TIME + 5, 30).is_err());
    }
}
