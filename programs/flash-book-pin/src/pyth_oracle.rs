//! Hand-rolled Pyth `PriceUpdateV2` reader (pin port) — in-house, byte-faithful.
//!
//! Faithful transcription of the Anchor `pyth_oracle.rs`. flash-book consumes
//! only a tiny STABLE slice of the `PriceUpdateV2` account (price / conf /
//! exponent / publish_time under FULL verification), read directly here — the
//! same "hand-roll to escape a bad dep" pattern as the ER CPIs. No `pyth-*`
//! crate at build time (its borsh-0.10 lock is incompatible with the port).
//!
//! Byte-correctness is locked by the SAME captured-fixture test as anchor: a real
//! `PriceUpdateV2` account serialized by `pyth-solana-receiver-sdk` is embedded
//! and the parser must read back the exact price/conf/exponent/publish_time.

/// Anchor account discriminator for `PriceUpdateV2`
/// (`sha256("account:PriceUpdateV2")[..8]`).
pub const PRICE_UPDATE_V2_DISCRIMINATOR: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];

/// The Pyth Solana Receiver program (base58
/// `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`) — the rightful owner of a
/// `PriceUpdateV2` account. The instruction MUST check
/// `price_update.owner() == &PYTH_RECEIVER_PROGRAM_ID`.
pub const PYTH_RECEIVER_PROGRAM_ID: [u8; 32] = [
    12, 183, 250, 187, 82, 247, 166, 72, 187, 91, 49, 125, 154, 1, 139, 144, 87, 203, 2, 71, 116,
    250, 254, 1, 230, 196, 223, 152, 204, 56, 88, 129,
];

/// The subset of a Pyth price flash-book consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PythPrice {
    pub price: i64,
    pub conf: u64,
    pub exponent: i32,
    pub publish_time: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PythErr {
    BadData,
    NotFullyVerified,
    FeedMismatch,
    Stale,
}

/// Read a Pyth `PriceUpdateV2` account under the SAME rules as the SDK's
/// `get_price_no_older_than` (FULL verification): discriminator match,
/// `verification_level == Full`, `feed_id` match, and freshness.
///
/// Byte layout (LE) after the 8-byte discriminator: `write_authority` [32] ·
/// `verification_level` (tag `1` = `Full`) · `price_message` { `feed_id` [32],
/// `price` i64, `conf` u64, `exponent` i32, `publish_time` i64, … }.
pub fn get_price_no_older_than_full(
    data: &[u8],
    feed_id: &[u8; 32],
    now_unix: i64,
    max_age_seconds: u64,
) -> Result<PythPrice, PythErr> {
    if data.len() < 8 || data[..8] != PRICE_UPDATE_V2_DISCRIMINATOR {
        return Err(PythErr::BadData);
    }
    // write_authority [8..40]; verification_level tag at byte 40.
    if data.len() < 41 {
        return Err(PythErr::BadData);
    }
    // FULL (tag 1) only; Partial (tag 0) rejected (matches SDK `.gte(Full)`).
    if data[40] != 1 {
        return Err(PythErr::NotFullyVerified);
    }

    // price_message starts right after the 1-byte `Full` tag.
    let m = 41usize;
    if data.len() < m + 32 + 8 + 8 + 4 + 8 {
        return Err(PythErr::BadData);
    }
    if &data[m..m + 32] != feed_id {
        return Err(PythErr::FeedMismatch);
    }
    let price = i64::from_le_bytes(data[m + 32..m + 40].try_into().unwrap());
    let conf = u64::from_le_bytes(data[m + 40..m + 48].try_into().unwrap());
    let exponent = i32::from_le_bytes(data[m + 48..m + 52].try_into().unwrap());
    let publish_time = i64::from_le_bytes(data[m + 52..m + 60].try_into().unwrap());

    let age = now_unix.saturating_sub(publish_time);
    if age > max_age_seconds as i64 {
        return Err(PythErr::Stale);
    }
    Ok(PythPrice { price, conf, exponent, publish_time })
}

/// Convert a Pyth price (scaled by `10^exponent`) to mark ticks:
/// `ticks = price * 10^(exponent + tick_decimals)`. Returns None on overflow /
/// non-positive result. Faithful to the Anchor conversion in
/// `update_oracle_from_pyth`.
pub fn pyth_price_to_ticks(price: i64, exponent: i32, tick_decimals: i8) -> Option<u64> {
    if price <= 0 {
        return None;
    }
    let scale_exp = exponent + tick_decimals as i32;
    let ticks: i64 = if scale_exp >= 0 {
        let mul = 10i64.checked_pow(scale_exp as u32)?;
        price.checked_mul(mul)?
    } else {
        let div = 10i64.checked_pow((-scale_exp) as u32)?;
        price.checked_div(div)?
    };
    if ticks <= 0 {
        None
    } else {
        Some(ticks as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A REAL `PriceUpdateV2` account, serialized by `pyth-solana-receiver-sdk`:
    // feed_id=[0x11;32], price=6_500_123, conf=4_242, exponent=-8,
    // publish_time=1_750_000_000, verification_level=Full. (Same captured fixture
    // as the anchor module — real data, not synthetic.)
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
        assert_eq!(&FIXTURE[..8], &PRICE_UPDATE_V2_DISCRIMINATOR);
    }

    #[test]
    fn rejects_wrong_feed_id() {
        assert_eq!(
            get_price_no_older_than_full(&FIXTURE, &[9u8; 32], PUB_TIME + 5, 30),
            Err(PythErr::FeedMismatch)
        );
    }

    #[test]
    fn rejects_stale_price() {
        assert_eq!(
            get_price_no_older_than_full(&FIXTURE, &FEED, PUB_TIME + 100, 30),
            Err(PythErr::Stale)
        );
        assert!(get_price_no_older_than_full(&FIXTURE, &FEED, PUB_TIME + 20, 30).is_ok());
    }

    #[test]
    fn rejects_partial_verification() {
        let mut b = FIXTURE;
        b[40] = 0;
        assert_eq!(
            get_price_no_older_than_full(&b, &FEED, PUB_TIME + 5, 30),
            Err(PythErr::NotFullyVerified)
        );
    }

    #[test]
    fn rejects_bad_discriminator() {
        let mut b = FIXTURE;
        b[0] ^= 0xFF;
        assert_eq!(get_price_no_older_than_full(&b, &FEED, PUB_TIME + 5, 30), Err(PythErr::BadData));
    }

    #[test]
    fn rejects_short_buffer() {
        assert_eq!(
            get_price_no_older_than_full(&FIXTURE[..40], &FEED, PUB_TIME + 5, 30),
            Err(PythErr::BadData)
        );
    }

    #[test]
    fn price_to_ticks_scales() {
        // price 6_500_123, exponent -8, tick_decimals 6 → scale -2 → /100 = 65_001.
        assert_eq!(pyth_price_to_ticks(6_500_123, -8, 6), Some(65_001));
        // exponent -8, tick_decimals 8 → scale 0 → unchanged.
        assert_eq!(pyth_price_to_ticks(6_500_123, -8, 8), Some(6_500_123));
        // positive scale multiplies: price 5, exp 0, tick_decimals 3 → *1000.
        assert_eq!(pyth_price_to_ticks(5, 0, 3), Some(5_000));
        // non-positive price → None.
        assert_eq!(pyth_price_to_ticks(0, -8, 6), None);
    }
}
