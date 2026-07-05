//! Pyth Lazer oracle ingestion — dependency-free.
//!
//! Pyth Lazer is Pyth's low-latency push oracle: a trusted Lazer signer
//! Ed25519-signs a compact binary payload of feed prices. The signature is
//! verified via the Solana Ed25519 SigVerify precompile (instruction
//! introspection), and the payload is then parsed to extract the price for
//! the configured feed.
//!
//! The wire format is implemented directly (the `pyth-lazer-*` crates pin a
//! solana-program / borsh stack incompatible with the anchor 1.x +
//! spl-token v6 build) and matches the live Lazer Solana payload; the
//! parser is regression-pinned against a captured mainnet message below.
//!
//! SECURITY: this module does NOT itself check the Ed25519 signature math — the
//! native Ed25519 precompile does that and aborts the tx if it fails. Our job
//! is to prove, via the Instructions sysvar, that the precompile in THIS tx
//! verified (a) the trusted Lazer signer pubkey over (b) the exact payload we
//! parse. Skipping either check would let a forged payload through.

use anchor_lang::prelude::*;

/// Little-endian magic at the head of a Lazer Solana payload.
pub const LAZER_PAYLOAD_MAGIC: u32 = 0x93c7_d375;

/// `Ed25519SigVerify111111111111111111111111111` (native precompile).
pub const ED25519_PROGRAM_ID: [u8; 32] = [
    0x03, 0x7d, 0x46, 0xd6, 0x7c, 0x93, 0xfb, 0xbe, 0x12, 0xf9, 0x42, 0x8f, 0x83, 0x8d, 0x40, 0xff,
    0x05, 0x70, 0x74, 0x49, 0x27, 0xf4, 0x8a, 0x64, 0xfc, 0xca, 0x70, 0x44, 0x80, 0x00, 0x00, 0x00,
];

/// `Sysvar1nstructions1111111111111111111111111`.
pub const INSTRUCTIONS_SYSVAR_ID: [u8; 32] = [
    0x06, 0xa7, 0xd5, 0x17, 0x18, 0x7b, 0xd1, 0x66, 0x35, 0xda, 0xd4, 0x04, 0x55, 0xfd, 0xc2, 0xc0,
    0xc1, 0x24, 0xc6, 0x8f, 0x21, 0x56, 0x75, 0xa5, 0xdb, 0xba, 0xcb, 0x5f, 0x08, 0x00, 0x00, 0x00,
];

/// Property IDs inside a feed (Lazer protocol).
const PROP_PRICE: u8 = 0;
const PROP_BEST_BID: u8 = 1;
const PROP_BEST_ASK: u8 = 2;
const PROP_PUBLISHER_COUNT: u8 = 3;
const PROP_EXPONENT: u8 = 4;
const PROP_CONFIDENCE: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LazerPrice {
    /// Price scaled by 10^exponent (exponent is typically negative).
    pub price: i64,
    pub exponent: i16,
    pub confidence: u64,
    /// Payload timestamp in MICROSECONDS since the unix epoch.
    pub timestamp_us: u64,
    pub channel: u8,
}

/// Errors are mapped to `FlashBookError` at the call site; kept as a small enum
/// here so this module stays Anchor-error-free and unit-testable on the host.
#[derive(Debug, PartialEq, Eq)]
pub enum LazerError {
    BadMagic,
    Truncated,
    FeedNotFound,
    NoPrice,
    NoExponent,
}

/// Local result alias — `anchor_lang::prelude::Result` is a 1-generic alias, so
/// we use our own to carry `LazerError` on the pure (host-testable) parser.
type LzResult<T> = core::result::Result<T, LazerError>;

struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> LzResult<&'a [u8]> {
        let s = self
            .b
            .get(self.p..self.p + n)
            .ok_or(LazerError::Truncated)?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> LzResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> LzResult<i16> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> LzResult<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> LzResult<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> LzResult<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// Skip a property value whose property-id we don't care about, by size.
    fn skip_prop(&mut self, prop_id: u8) -> LzResult<()> {
        let n = match prop_id {
            PROP_PRICE | PROP_BEST_BID | PROP_BEST_ASK | PROP_CONFIDENCE => 8,
            PROP_EXPONENT | PROP_PUBLISHER_COUNT => 2,
            // Unknown / future property ids carry a u16 length-prefixed blob in
            // the Lazer extension space; be conservative and treat as 2-byte.
            _ => 2,
        };
        self.take(n)?;
        Ok(())
    }
}

/// Parse a Lazer Solana payload and return the price for `feed_id`.
///
/// Wire format (all integers little-endian):
///   magic:        u32   (== LAZER_PAYLOAD_MAGIC)
///   timestamp_us: u64
///   channel:      u8
///   feeds_len:    u8
///   repeated feeds_len times:
///     feed_id:        u32
///     num_properties: u8
///     repeated num_properties times:
///       prop_id: u8
///       value:   (Price=i64, Exponent=i16, Confidence=u64, ...)
pub fn parse_lazer_price(
    payload: &[u8],
    feed_id: u32,
) -> core::result::Result<LazerPrice, LazerError> {
    let mut c = Cursor::new(payload);
    if c.u32()? != LAZER_PAYLOAD_MAGIC {
        return Err(LazerError::BadMagic);
    }
    let timestamp_us = c.u64()?;
    let channel = c.u8()?;
    let feeds_len = c.u8()?;

    for _ in 0..feeds_len {
        let fid = c.u32()?;
        let num_props = c.u8()?;
        let mut price: Option<i64> = None;
        let mut exponent: Option<i16> = None;
        let mut confidence: u64 = 0;
        for _ in 0..num_props {
            let prop_id = c.u8()?;
            match prop_id {
                PROP_PRICE => price = Some(c.i64()?),
                PROP_EXPONENT => exponent = Some(c.i16()?),
                PROP_CONFIDENCE => confidence = c.u64()?,
                other => c.skip_prop(other)?,
            }
        }
        if fid == feed_id {
            let price = price.ok_or(LazerError::NoPrice)?;
            let exponent = exponent.ok_or(LazerError::NoExponent)?;
            return Ok(LazerPrice {
                price,
                exponent,
                confidence,
                timestamp_us,
                channel,
            });
        }
    }
    Err(LazerError::FeedNotFound)
}

/// Proof — read from the Instructions sysvar — that an Ed25519 SigVerify
/// precompile instruction earlier in THIS transaction verified `signer` over
/// exactly `expected_msg`. The precompile guarantees the signature is valid;
/// we only confirm the (pubkey, message) it bound. Returns Ok(()) on a match.
///
/// `ed25519_ix_index` is the index of the Ed25519 instruction in the tx (the
/// client places it immediately before this instruction, conventionally 0).
pub fn verify_ed25519_precompile(
    instructions_sysvar: &AccountInfo,
    ed25519_ix_index: usize,
    signer: &[u8; 32],
    expected_msg: &[u8],
) -> core::result::Result<(), ProgramError> {
    // Confirm the account passed really is the Instructions sysvar.
    if instructions_sysvar.key.to_bytes() != INSTRUCTIONS_SYSVAR_ID {
        return Err(ProgramError::InvalidArgument);
    }
    let sysvar = instructions_sysvar
        .try_borrow_data()
        .map_err(|_| ProgramError::InvalidArgument)?;
    // Manual Instructions-sysvar parse (avoids the version-fragile
    // solana-instructions-sysvar dep; same byte layout it serializes):
    //   [0..2]                num_instructions: u16 LE
    //   [2 + 2*i ..]          offset_i: u16 LE  (start of instruction i)
    //   at offset_i:
    //     [0..2]              num_accounts: u16 LE
    //     per account:        [flags:u8][pubkey:32]   (33 bytes)
    //     [.. +32]            program_id: 32
    //     [.. +2]             data_len: u16 LE
    //     [.. +data_len]      instruction data
    let rd_u16 = |buf: &[u8], o: usize| -> core::result::Result<usize, ProgramError> {
        buf.get(o..o + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
            .ok_or(ProgramError::InvalidArgument)
    };
    let num_ix = rd_u16(&sysvar, 0)?;
    if ed25519_ix_index >= num_ix {
        return Err(ProgramError::InvalidArgument);
    }
    let ix_start = rd_u16(&sysvar, 2 + ed25519_ix_index * 2)?;
    let num_accounts = rd_u16(&sysvar, ix_start)?;
    let pid_off = ix_start + 2 + num_accounts * 33;
    let program_id = sysvar
        .get(pid_off..pid_off + 32)
        .ok_or(ProgramError::InvalidArgument)?;
    if program_id != ED25519_PROGRAM_ID.as_slice() {
        return Err(ProgramError::InvalidArgument); // not the Ed25519 precompile
    }
    let data_len = rd_u16(&sysvar, pid_off + 32)?;
    let data_off = pid_off + 34;
    let d = sysvar
        .get(data_off..data_off + data_len)
        .ok_or(ProgramError::InvalidArgument)?;

    // Ed25519 precompile instruction layout:
    //   [0]      num_signatures (u8)
    //   [1]      padding
    //   [2..16]  Ed25519SignatureOffsets { sig_off:u16, sig_ix:u16,
    //            pk_off:u16, pk_ix:u16, msg_off:u16, msg_size:u16, msg_ix:u16 }
    // Offsets with ix == u16::MAX reference this same instruction's data; we
    // require the single-signature, self-contained form (msg/pk inline).
    if d.len() < 16 || d[0] != 1 {
        return Err(ProgramError::InvalidArgument);
    }
    let rd16 = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
    let sig_off = rd16(2);
    let sig_ix = rd16(4);
    let pk_off = rd16(6);
    let pk_ix = rd16(8);
    let msg_off = rd16(10);
    let msg_size = rd16(12);
    let msg_ix = rd16(14);
    let cur = u16::MAX as usize;
    // All three regions must live inside this Ed25519 instruction's own data.
    if sig_ix != cur || pk_ix != cur || msg_ix != cur {
        return Err(ProgramError::InvalidArgument);
    }
    let _ = sig_off;
    let pk = d
        .get(pk_off..pk_off + 32)
        .ok_or(ProgramError::InvalidArgument)?;
    let msg = d
        .get(msg_off..msg_off + msg_size)
        .ok_or(ProgramError::InvalidArgument)?;
    if pk != signer.as_slice() {
        return Err(ProgramError::InvalidArgument); // wrong / untrusted signer
    }
    if msg != expected_msg {
        return Err(ProgramError::InvalidArgument); // signature bound a different message
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // REAL Pyth Lazer Solana payload, extracted from a Jupiter-Perps mainnet
    // transaction and Ed25519-verified against the trusted Lazer signer
    // 9gKEEcFzSd1PDYBKWAKZi4Sq4ZCUaVX5oTr8kEjdwsfR. No synthetic data.
    const REAL_MSG: &[u8] = &[
        0x75, 0xd3, 0xc7, 0x93, 0x00, 0x47, 0x5f, 0x2b, 0x46, 0x55, 0x06, 0x00, 0x03, 0x01, 0x06,
        0x00, 0x00, 0x00, 0x04, 0x00, 0x38, 0xcf, 0xfd, 0xa4, 0x01, 0x00, 0x00, 0x00, 0x04, 0xf8,
        0xff, 0x05, 0xd7, 0x0b, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x01, 0x00, 0x47, 0x5f,
        0x2b, 0x46, 0x55, 0x06, 0x00,
    ];

    #[test]
    fn parses_real_lazer_message() {
        // feed_id 6 is the one present in the real message.
        let p = parse_lazer_price(REAL_MSG, 6).expect("parse");
        assert_eq!(p.price, 7_063_064_376, "price (≈ $70.63 at exp -8)");
        assert_eq!(p.exponent, -8);
        assert_eq!(p.confidence, 1_182_679);
        assert_eq!(p.channel, 3);
        assert_eq!(p.timestamp_us, 1_782_609_724_000_000);
        // sanity: real_usd = price * 10^exp
        let usd = p.price as f64 * 10f64.powi(p.exponent as i32);
        assert!((usd - 70.63064376).abs() < 1e-6, "got {usd}");
    }

    #[test]
    fn bad_magic_rejected() {
        let mut m = REAL_MSG.to_vec();
        m[0] ^= 0xff;
        assert_eq!(parse_lazer_price(&m, 6), Err(LazerError::BadMagic));
    }

    #[test]
    fn missing_feed_rejected() {
        assert_eq!(
            parse_lazer_price(REAL_MSG, 999),
            Err(LazerError::FeedNotFound)
        );
    }

    #[test]
    fn truncated_rejected() {
        assert_eq!(
            parse_lazer_price(&REAL_MSG[..10], 6),
            Err(LazerError::Truncated)
        );
    }
}
