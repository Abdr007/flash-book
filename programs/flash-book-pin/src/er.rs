//! MagicBlock Ephemeral Rollups (ER) shared constants.
//!
//! The `ephemeral-rollups-sdk` cannot be added to this crate (its bytemuck/borsh
//! pins conflict with pyth + the port's deps), so every ER interaction is
//! hand-rolled. This module holds the constants those hand-rolled paths share;
//! the delegate/commit/undelegate CPIs build on it.

/// The MagicBlock delegation program (base58
/// `DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh`). A delegated account is
/// re-owned by this program while it runs on the ER; ownership by it is the
/// proof that an account is currently delegated. Round-trip-verified.
pub const DELEGATION_PROGRAM_ID: [u8; 32] = [
    181, 183, 0, 225, 242, 87, 58, 192, 204, 6, 34, 1, 52, 74, 207, 151, 184, 53, 6, 235, 140, 229,
    25, 152, 204, 98, 126, 24, 147, 128, 167, 62,
];
