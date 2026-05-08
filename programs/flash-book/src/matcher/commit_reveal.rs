//! Commit-reveal protocol — sequencer-proof MEV resistance.
//!
//! Phase 1 (block N):       trader submits hash(side ‖ size ‖ limit ‖ nonce ‖ trader)
//! Phase 2 (block ≤ N + K): trader reveals all parts; matcher checks hash
//! Phase 3 (next batch):    revealed order enters the FBA buffer
//!
//! Rust port uses `anchor_lang::solana_program::keccak::hashv` for the
//! commit hash — keccak is Solana-native and cheap on-chain.

use super::lot::{BaseLots, Ticks};
use super::order::{Order, OrderType, Side};
use crate::errors::FlashBookError;
use crate::state::CommitRow;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::keccak::hashv;

#[derive(Debug, Clone, Copy)]
pub struct RevealPayload {
    pub trader: Pubkey,
    pub side: Side,
    pub size: BaseLots,
    pub limit: Ticks,
    pub nonce: [u8; 32],
}

impl RevealPayload {
    /// Compute the commit hash for this payload.
    pub fn hash(&self) -> [u8; 32] {
        let side_byte: [u8; 1] = [self.side as u8];
        let size_bytes = self.size.0.to_le_bytes();
        let limit_bytes = self.limit.0.to_le_bytes();
        let trader_bytes = self.trader.to_bytes();
        let h = hashv(&[
            &trader_bytes,
            &side_byte,
            &size_bytes,
            &limit_bytes,
            &self.nonce,
        ]);
        h.0
    }
}

/// Search a commit buffer for a matching active row, identified by hash.
pub fn find_commit<'a>(
    commits: &'a [CommitRow],
    hash: &[u8; 32],
) -> Option<&'a CommitRow> {
    commits.iter().find(|r| r.valid == 1 && &r.hash == hash)
}

pub fn find_commit_mut<'a>(
    commits: &'a mut [CommitRow],
    hash: &[u8; 32],
) -> Option<&'a mut CommitRow> {
    commits
        .iter_mut()
        .find(|r| r.valid == 1 && &r.hash == hash)
}

/// Insert a commit into the first empty slot. Returns Err if buffer is full.
pub fn register_commit(
    commits: &mut [CommitRow],
    hash: [u8; 32],
    trader: Pubkey,
    bond: u64,
    current_batch: u64,
    expire_in_batches: u64,
) -> Result<()> {
    if commits.iter().any(|r| r.valid == 1 && r.hash == hash) {
        return Err(error!(FlashBookError::CommitDuplicate));
    }
    for slot in commits.iter_mut() {
        if slot.valid == 0 {
            *slot = CommitRow {
                hash,
                trader,
                bond,
                committed_at_batch: current_batch,
                expire_at_batch: current_batch.saturating_add(expire_in_batches),
                valid: 1,
            };
            return Ok(());
        }
    }
    Err(error!(FlashBookError::BufferFull))
}

/// Verify a reveal against a stored commit. On success: clears the row,
/// returns a synthesized `Order`.
pub fn redeem_reveal(
    commits: &mut [CommitRow],
    payload: &RevealPayload,
    current_batch: u64,
    seq: u64,
) -> Result<Order> {
    let h = payload.hash();
    let row = find_commit_mut(commits, &h).ok_or_else(|| error!(FlashBookError::CommitMismatch))?;
    if row.trader != payload.trader {
        return Err(error!(FlashBookError::CommitMismatch));
    }
    if current_batch > row.expire_at_batch {
        return Err(error!(FlashBookError::CommitExpired));
    }
    // Clear the slot.
    *row = CommitRow::default();
    Ok(Order {
        id: seq,
        trader: payload.trader,
        side: payload.side,
        order_type: OrderType::Taker,
        size: payload.size,
        limit_price: payload.limit,
        seq,
        post_only: false,
    })
}

/// Sweep expired commits and return the seized bonds.
pub fn sweep_expired(commits: &mut [CommitRow], current_batch: u64) -> u64 {
    let mut seized: u64 = 0;
    for slot in commits.iter_mut() {
        if slot.valid == 1 && current_batch > slot.expire_at_batch {
            seized = seized.saturating_add(slot.bond);
            *slot = CommitRow::default();
        }
    }
    seized
}

pub fn pending_count(commits: &[CommitRow]) -> usize {
    commits.iter().filter(|r| r.valid == 1).count()
}
