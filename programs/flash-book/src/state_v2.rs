//! V2 account types — orderbook over a Manifest-style hypertree.
//!
//! This is the future of the order buffer. It eliminates the BPF stack
//! overflow that bit us at CAP=64 (Borsh deserialise built a 5KB struct
//! on the 4KB stack). With this layout:
//!
//!   * The whole `MarketBookAccount` is `bytemuck::Pod` and loaded via
//!     `AccountLoader<T>` — never deserialised onto the stack.
//!   * Bids, asks, and claimed seats live in 3 overlapping red-black
//!     trees inside a single byte array. One `LinkedList` of free 80-byte
//!     blocks tracks evictable slots.
//!   * Adding/cancelling an order is an O(log n) RBT op on a 64-byte
//!     payload — no per-slot scan, no flat array.
//!   * Account size is fixed at INIT (8KB data → 100 nodes → ~50
//!     orders/side + 50 seats); realloc to grow ships in wave 18c.
//!
//! See `docs/V3_PLAN.md` § 2.1 for the full design rationale.
//!
//! Compatibility note: this lives ALONGSIDE the legacy
//! `state::OrderBufferAccount` for now. The matcher migration in wave
//! 18d/e will swap the legacy buffer for this. Wave 18b ships only the
//! types + init/expand ixs; nothing trades against it yet.

use anchor_lang::prelude::*;

use crate::hypertree::{DataIndex, Get, RBNode};

/// Bytes available for hypertree nodes after the fixed header. 100 nodes
/// of 80 bytes each. Enough for ~50 orders per side + 50 claimed seats
/// in the v1 layout. Realloc-to-grow ix follows in wave 18c.
pub const MARKET_BOOK_DATA_BYTES: usize = 8_000;

/// Each RBNode<V> = 16-byte RBT header + V (the payload).
/// We size payloads at 64 bytes so each node = 80 bytes total — same as
/// Manifest's hypertree, lets us reuse all upstream invariants.
pub const NODE_PAYLOAD_BYTES: usize = 64;
pub const NODE_TOTAL_BYTES: usize = 80;
pub const MAX_NODES: usize = MARKET_BOOK_DATA_BYTES / NODE_TOTAL_BYTES;

// ─── Header ──────────────────────────────────────────────────────────

/// 256-byte fixed header. All RBT root/best indices, the free-list
/// head, sequence counters, and identity. `#[repr(C)]` + explicit
/// padding so the struct is `bytemuck::Pod`-compatible (no implicit
/// padding bytes).
#[zero_copy]
pub struct MarketBookHeader {
    pub bump: u8,
    pub version: u8,
    pub _pad0: [u8; 6],

    pub market_pubkey: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,

    /// Root index of the bid RBT (bids ordered descending by price).
    pub bids_root_index: DataIndex,
    /// Cached pointer to the highest-price bid for O(1) best-bid reads.
    pub bids_best_index: DataIndex,
    /// Root index of the ask RBT (asks ordered ascending).
    pub asks_root_index: DataIndex,
    /// Cached pointer to the lowest-price ask for O(1) best-ask reads.
    pub asks_best_index: DataIndex,
    /// Root index of the claimed-seats RBT (per-trader state).
    pub claimed_seats_root_index: DataIndex,
    /// Head of the free-list (linked list of evictable 80-byte slots).
    pub free_list_head_index: DataIndex,

    /// Bytes of the dynamic array currently in use. Grows monotonically
    /// until `expand_market_book` is called or all slots freed and
    /// reused via the free-list.
    pub num_bytes_allocated: u32,
    /// Total resting orders across both sides. Off-chain monitors page
    /// when this approaches MAX_NODES − seats.
    pub total_orders_active: u32,

    /// Monotonic per-market order sequence counter. Encoded into
    /// `RestingOrderV2.order_id` along with the side bit.
    pub order_seq_counter: u64,

    /// Future-proofing — 112 bytes for fields we'll add in waves 19-22
    /// without breaking layout. Split into ≤32-byte chunks because
    /// bytemuck's classical `Pod for [T; N]` impl tops out at N=32.
    pub _reserved_a: [u8; 32],
    pub _reserved_b: [u8; 32],
    pub _reserved_c: [u8; 32],
    pub _reserved_d: [u8; 16],
}

const _: () = assert!(std::mem::size_of::<MarketBookHeader>() == 256);

// ─── RestingOrderV2 ──────────────────────────────────────────────────

/// 64-byte resting-order payload. Carries everything the matcher needs
/// to clear a fill PLUS the GTT expiry + Phoenix-style `order_id` that
/// encodes side in the leading bit so a single u64 ordering serves
/// both bids and asks.
///
/// Implements `Ord` by `order_id` (which embeds price + seq + side). The
/// RBT uses this for sort.
#[zero_copy]
pub struct RestingOrderV2 {
    /// `(price << 1) | side_bit`. For bids, the resulting u64 is INVERTED
    /// (`!`) so natural ascending sort still puts the highest-price
    /// bids first. Phoenix's exact pattern.
    pub order_id: u64,
    /// Per-batch monotonic sequence — used for FIFO tie-breaking within
    /// a price level (price-time priority).
    pub seq: u64,
    /// Integer price in ticks. Stored separately from `order_id` for
    /// cheap reads (don't unpack the encoded id every check).
    pub price_ticks: u64,
    /// Base lots remaining (decreases on partial fills).
    pub size_lots: u64,
    /// 0 = GTC. Otherwise the slot at which the matcher silently skips
    /// this order. Cleanup keepers reclaim rent via cancel.
    pub expires_at_slot: u64,
    /// Pointer at the trader's claimed seat node in the same byte
    /// array. O(1) seat lookup on every match.
    pub trader_index: DataIndex,
    /// Anti-replay guard for off-chain replay tools.
    pub last_valid_slot: u32,

    pub side: u8,        // 0 = long/buy/bid, 1 = short/sell/ask
    pub order_type: u8,  // 0 = limit, 1 = ioc, 2 = post_only, 3 = jit
    /// Bitfield: bit0 reduce_only, bit1 post_only-shortcut, bits 2-3 STP mode.
    pub flags: u8,
    pub _pad: u8,

    /// Client-side order id (off-chain reconciliation; zero if unset).
    pub client_order_id: u32,

    /// 8-byte reserved tail for fields we may add (HIP-3 builder ref,
    /// referrer ref, …) without breaking the 64-byte layout.
    pub _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<RestingOrderV2>() == 64);

// Make RestingOrderV2 usable as an RBT payload (Hypertree's `Payload`
// requires `Ord + Eq + Display`).
impl PartialEq for RestingOrderV2 {
    fn eq(&self, other: &Self) -> bool {
        self.order_id == other.order_id
    }
}
impl Eq for RestingOrderV2 {}
impl PartialOrd for RestingOrderV2 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RestingOrderV2 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order_id.cmp(&other.order_id)
    }
}
impl std::fmt::Display for RestingOrderV2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RestingOrderV2 {{ side={} price_ticks={} size_lots={} seq={} }}",
            self.side, self.price_ticks, self.size_lots, self.seq
        )
    }
}
impl Get for RestingOrderV2 {}

/// Compose the side-encoded `order_id` from (price_ticks, seq, side).
/// Bids invert so a single-direction sort works for both books.
pub fn encode_order_id(price_ticks: u64, seq: u64, side_is_bid: bool) -> u64 {
    // Layout: high 48 bits = price (capped), low 16 bits = seq mod 2^16
    let price = price_ticks & ((1u64 << 48) - 1);
    let seq_low = seq & ((1u64 << 16) - 1);
    let raw = (price << 16) | seq_low;
    if side_is_bid { !raw } else { raw }
}

// ─── ClaimedSeatV2 ───────────────────────────────────────────────────

/// 64-byte per-(market, trader) seat. Holds the trader pubkey + open
/// order count + free-funds balances (Phoenix-style settlement). On
/// first trade in a market the trader claims a seat (one-time rent
/// ≈ $0.0005). Subsequent trades settle into `quote_free_lots` — no
/// SPL token CPI on every fill (sub-100µs hot path, wave 19).
#[zero_copy]
pub struct ClaimedSeatV2 {
    pub trader: Pubkey,

    /// Quote lots available for trade (settled from prior fills, not
    /// yet withdrawn to the trader's ATA).
    pub quote_free_lots: u64,

    /// Quote lots locked in resting orders.
    pub quote_locked_lots: u64,

    /// Counter of trader's currently-resting orders in this book.
    pub open_orders_count: u32,

    /// Last sequence the trader was assigned. Off-chain reconciliation
    /// uses it to detect missed events.
    pub last_seq_assigned: u32,

    /// 8-byte reserved tail.
    pub _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<ClaimedSeatV2>() == 64);

impl PartialEq for ClaimedSeatV2 {
    fn eq(&self, other: &Self) -> bool { self.trader == other.trader }
}
impl Eq for ClaimedSeatV2 {}
impl PartialOrd for ClaimedSeatV2 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ClaimedSeatV2 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.trader.to_bytes().cmp(&other.trader.to_bytes())
    }
}
impl std::fmt::Display for ClaimedSeatV2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Seat {{ trader={} open={} }}", self.trader, self.open_orders_count)
    }
}
impl Get for ClaimedSeatV2 {}

// ─── MarketBookAccount ───────────────────────────────────────────────
//
// Anchor's `#[account(zero_copy)]` derives `Pod` strictly enough that
// it rejects struct fields like `[u8; 8000]` (the macro's expansion
// requires every field to satisfy a derive-time check that bytemuck's
// blanket array impl doesn't visibly satisfy in this expansion).
//
// Manifest's solution: don't use Anchor at all; raw Solana program with
// `UncheckedAccount` + manual `&[u8]` slicing. We adopt the same
// pattern in wave 18c — define a `MarketBookHandle<'a>` newtype that
// wraps a `&mut [u8]`, parses the first 256 bytes as `MarketBookHeader`
// via `bytemuck::from_bytes`, and exposes the rest as the dynamic byte
// array for the hypertree's `FreeList`/`RedBlackTree` ops.
//
// For wave 18b the header + payload structs above are enough to
// validate the layout and start writing the matcher migration.
// `init_market_book` ix lands in wave 18c.

// Compile-time sanity: RBNode<RestingOrderV2> and RBNode<ClaimedSeatV2>
// are exactly 80 bytes (= NODE_TOTAL_BYTES), so they fit the same
// FreeList allocator as Manifest's hypertree.
const _: () = assert!(std::mem::size_of::<RBNode<RestingOrderV2>>() == 80);
const _: () = assert!(std::mem::size_of::<RBNode<ClaimedSeatV2>>() == 80);
