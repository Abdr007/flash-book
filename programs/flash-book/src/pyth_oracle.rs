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
//! Byte-correctness is proven by a DIFFERENTIAL unit test: the same account bytes
//! are read by BOTH this parser and the real `pyth-solana-receiver-sdk` (kept as a
//! dev-dependency) and asserted equal — so any offset/discriminator drift fails CI.

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
    use anchor_lang::AccountSerialize;
    use anchor_lang::solana_program::clock::Clock;
    use pyth_solana_receiver_sdk::price_update::{
        PriceFeedMessage, PriceUpdateV2, VerificationLevel,
    };

    fn serialize(acct: &PriceUpdateV2) -> Vec<u8> {
        let mut bytes = Vec::new();
        acct.try_serialize(&mut bytes).unwrap();
        bytes
    }

    fn make(feed: [u8; 32], level: VerificationLevel) -> PriceUpdateV2 {
        PriceUpdateV2 {
            write_authority: Pubkey::new_unique(),
            verification_level: level,
            price_message: PriceFeedMessage {
                feed_id: feed,
                price: 5_123_456,
                conf: 9_999,
                exponent: -8,
                publish_time: 1_700_000_000,
                prev_publish_time: 1_699_999_999,
                ema_price: 5_120_000,
                ema_conf: 10_000,
            },
            posted_slot: 12345,
        }
    }

    /// THE safety net: parse the same bytes with BOTH this parser and the real
    /// SDK, assert every field agrees. Immune to test-fixture mistakes.
    #[test]
    fn parser_matches_sdk_exactly() {
        let feed = [7u8; 32];
        let acct = make(feed, VerificationLevel::Full);
        let bytes = serialize(&acct);

        // discriminator we hard-code must equal the SDK's serialized one.
        assert_eq!(&bytes[..8], &PRICE_UPDATE_V2_DISCRIMINATOR);

        let now = 1_700_000_005i64;
        let max_age = 30u64;
        let mine = get_price_no_older_than_full(&bytes, &feed, now, max_age).unwrap();

        let clock = Clock { unix_timestamp: now, ..Default::default() };
        let sdk = acct.get_price_no_older_than(&clock, max_age, &feed).unwrap();

        assert_eq!(mine.price, sdk.price, "price");
        assert_eq!(mine.conf, sdk.conf, "conf");
        assert_eq!(mine.exponent, sdk.exponent, "exponent");
        assert_eq!(mine.publish_time, sdk.publish_time, "publish_time");
    }

    #[test]
    fn rejects_partial_verification() {
        let feed = [3u8; 32];
        let acct = make(feed, VerificationLevel::Partial { num_signatures: 5 });
        let bytes = serialize(&acct);
        // Our parser rejects Partial (Full required) — and so does the SDK's
        // Full-level reader.
        assert!(get_price_no_older_than_full(&bytes, &feed, 1_700_000_005, 30).is_err());
        let clock = Clock { unix_timestamp: 1_700_000_005, ..Default::default() };
        assert!(acct.get_price_no_older_than(&clock, 30, &feed).is_err());
    }

    #[test]
    fn rejects_wrong_feed_id() {
        let feed = [1u8; 32];
        let acct = make(feed, VerificationLevel::Full);
        let bytes = serialize(&acct);
        assert!(get_price_no_older_than_full(&bytes, &[9u8; 32], 1_700_000_005, 30).is_err());
    }

    #[test]
    fn rejects_stale_price() {
        let feed = [2u8; 32];
        let acct = make(feed, VerificationLevel::Full); // publish_time = 1_700_000_000
        let bytes = serialize(&acct);
        // now is 100s later, max_age 30 → stale.
        assert!(get_price_no_older_than_full(&bytes, &feed, 1_700_000_100, 30).is_err());
        // within window → ok.
        assert!(get_price_no_older_than_full(&bytes, &feed, 1_700_000_020, 30).is_ok());
    }

    #[test]
    fn program_id_matches_sdk() {
        assert_eq!(PYTH_RECEIVER_PROGRAM_ID, pyth_solana_receiver_sdk::ID);
    }

    #[test]
    fn rejects_bad_discriminator() {
        let feed = [4u8; 32];
        let mut bytes = serialize(&make(feed, VerificationLevel::Full));
        bytes[0] ^= 0xFF; // corrupt the discriminator
        assert!(get_price_no_older_than_full(&bytes, &feed, 1_700_000_005, 30).is_err());
    }
}
