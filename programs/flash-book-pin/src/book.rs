//! Ported from programs/flash-book/src/state_v2.rs — the MarketBookHandle book
//! account wrapper + zero-copy book types (header, resting order, claimed seat,
//! free-list), de-anchored for the no_std Pinocchio program via a small compat
//! shim (local error enum + require!/error!/msg! macros). The hypertree ops
//! come from the already-ported `crate::hypertree`.
#![allow(dead_code)]
use pinocchio::program_error::ProgramError;

pub type Pubkey = [u8; 32];
type Result<T> = core::result::Result<T, ProgramError>;

pub mod errors {
    use pinocchio::program_error::ProgramError;
    #[derive(Clone, Copy)]
    pub enum FlashBookError { ArithmeticOverflow, BufferFull, OutOfRange, WrongTrader }
    impl From<FlashBookError> for ProgramError {
        fn from(e: FlashBookError) -> Self {
            match e {
                FlashBookError::ArithmeticOverflow => ProgramError::ArithmeticOverflow,
                FlashBookError::BufferFull => ProgramError::Custom(1300),
                FlashBookError::OutOfRange => ProgramError::Custom(1003),
                FlashBookError::WrongTrader => ProgramError::Custom(1100),
            }
        }
    }
}

macro_rules! require { ($c:expr, $e:expr $(,)?) => { if !($c) { return Err($e.into()); } }; }
macro_rules! error { ($e:expr) => { ProgramError::from($e) }; }
#[allow(unused_macros)]
macro_rules! msg { ($($a:tt)*) => {}; }

use crate::hypertree::{
    get_helper, get_mut_helper, DataIndex, FreeList, FreeListNode, Get,
    HyperTreeReadOperations, HyperTreeWriteOperations, Payload, RBNode, RedBlackTree,
    RedBlackTreeReadOnly, RedBlackTreeReadOperationsHelpers, NIL,
};

/// Bytes available for hypertree nodes after the fixed header **at init**.
/// 100 nodes of 96 bytes each — ~50 orders per side + 50 claimed seats.
/// This is no longer the hard ceiling: `expand_market_book` grows the
/// account in place up to `MARKET_BOOK_MAX_DATA_BYTES`. The live capacity
/// is always `MarketBookHandle::data.len()`, NOT this constant.
pub const MARKET_BOOK_DATA_BYTES: usize = 9_600;

/// Each RBNode<V> = 16-byte RBT header + V (the payload).
/// We size payloads at 80 bytes so each node = 96 bytes total — wider
/// than Manifest (64+16=80) because OUR resting orders carry the
/// trader Pubkey inline (Manifest indirects through a seat). Wave 19's
/// free-funds optimisation will move trader off the order onto the
/// seat and shrink back to 80B.
pub const NODE_PAYLOAD_BYTES: usize = 80;
pub const NODE_TOTAL_BYTES: usize = 96;

/// Node count the account holds **at init**. After `expand_market_book`
/// the live capacity is `MarketBookHandle::data.len() / NODE_TOTAL_BYTES`.
pub const MAX_NODES: usize = MARKET_BOOK_DATA_BYTES / NODE_TOTAL_BYTES;

/// Hard ceiling on nodes after expansion. 10,000 nodes keeps the data
/// region under 1 MiB — well inside DataIndex(u32) addressing and Solana's
/// 10 MiB per-account limit — while giving each side ~5,000 resting orders.
pub const MAX_NODES_EXPANDED: usize = 10_000;

/// Data region after a full expansion. = 960,000 bytes.
pub const MARKET_BOOK_MAX_DATA_BYTES: usize = MAX_NODES_EXPANDED * NODE_TOTAL_BYTES;

// ─── Header ──────────────────────────────────────────────────────────

/// 256-byte fixed header. All RBT root/best indices, the free-list
/// head, sequence counters, and identity. `#[repr(C)]` + explicit
/// padding so the struct is `bytemuck::Pod`-compatible (no implicit
/// padding bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MarketBookHeader {
    pub bump: u8,
    pub version: u8,
    pub _pad0: [u8; 6],

    pub market_pubkey: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,

    /// Root index of the bid RBT (bids ordered descending by price).
    pub bids_root_index: u32,
    /// Cached pointer to the highest-price bid for O(1) best-bid reads.
    pub bids_best_index: u32,
    /// Root index of the ask RBT (asks ordered ascending).
    pub asks_root_index: u32,
    /// Cached pointer to the lowest-price ask for O(1) best-ask reads.
    pub asks_best_index: u32,
    /// Root index of the claimed-seats RBT (per-trader state).
    pub claimed_seats_root_index: u32,
    /// Head of the free-list (linked list of evictable 80-byte slots).
    pub free_list_head_index: u32,

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

const _: () = assert!(core::mem::size_of::<MarketBookHeader>() == 256);

// ─── RestingOrderV2 ──────────────────────────────────────────────────

/// 64-byte resting-order payload. Carries everything the matcher needs
/// to clear a fill PLUS the GTT expiry + Phoenix-style `order_id` that
/// encodes side in the leading bit so a single u64 ordering serves
/// both bids and asks.
///
/// Implements `Ord` by `order_id` (which embeds price + seq + side). The
/// RBT uses this for sort.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RestingOrderV2 {
    /// `(price << 16) | (seq mod 2^16)`. For bids, the resulting u64
    /// is INVERTED (`!`) so natural ascending sort still puts the
    /// highest-price bids first. Phoenix's exact pattern.
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

    /// Trader pubkey (32 B). In wave 19's free-funds optimisation this
    /// will be replaced by `trader_index: u32` pointing at the trader's
    /// seat in the same byte array; for now we carry the pubkey inline
    /// so wave 18d can ship without seat-claim plumbing.
    pub trader: Pubkey,

    /// Anti-replay guard for off-chain replay tools. u32 saturates to
    /// u32::MAX (the unwrap_or path in call sites) once the validator's
    /// slot counter exceeds 2^32 ≈ 204 years on Solana mainnet. Replay
    /// tools that compare against this field must handle the saturation
    /// (treat `last_valid_slot == u32::MAX` as "always valid").
    pub last_valid_slot: u32,

    pub side: u8,        // 0 = long/buy/bid, 1 = short/sell/ask
    pub order_type: u8,  // 0 = limit, 1 = ioc, 2 = post_only, 3 = jit
    /// Bitfield: bit0 reduce_only, bit1 post_only-shortcut, bits 2-3 STP mode.
    pub flags: u8,
    /// Phase 2e — sub-account index this order belongs to.
    /// `0` (default) = main TraderState `[STATE_SEED, trader.as_ref()]`.
    /// `1..=255` = sub TraderState `[STATE_SEED, trader.as_ref(), &[sub_index]]`.
    /// Repurposed from the prior `_pad` byte — layout-compatible because
    /// pre-Phase-2e hypertree nodes serialised `_pad = 0`, which now
    /// reads back as `sub_index = 0` (the main-account default).
    /// ApplyFill / ApplyFlpFill use this to route fills + fees + PnL
    /// to the correct TraderState. Cancel / modify do NOT need to read
    /// it (those ixs verify the signer against `order.trader`, the
    /// wallet, regardless of sub_index).
    pub sub_index: u8,
}

const _: () = assert!(core::mem::size_of::<RestingOrderV2>() == 80);

// Make RestingOrderV2 usable as an RBT payload (Hypertree's `Payload`
// requires `Ord + Eq + Display`).
impl PartialEq for RestingOrderV2 {
    fn eq(&self, other: &Self) -> bool {
        self.order_id == other.order_id
    }
}
impl Eq for RestingOrderV2 {}
impl PartialOrd for RestingOrderV2 {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RestingOrderV2 {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.order_id.cmp(&other.order_id)
    }
}
impl core::fmt::Display for RestingOrderV2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "RestingOrderV2 {{ side={} price_ticks={} size_lots={} seq={} }}",
            self.side, self.price_ticks, self.size_lots, self.seq
        )
    }
}
impl Get for RestingOrderV2 {}

/// Bit layout of the side-encoded `order_id`: high `ORDER_ID_PRICE_BITS`
/// bits hold the price (in ticks), low `ORDER_ID_SEQ_BITS` bits hold the
/// FIFO sequence. 40 + 24 = 64.
pub const ORDER_ID_PRICE_BITS: u32 = 40;
pub const ORDER_ID_SEQ_BITS: u32 = 24;
/// Largest price (ticks) and largest seq that fit the `order_id` layout.
/// Callers must keep `price_ticks <= MAX_PRICE_TICKS_ENCODABLE` and enforce
/// the per-market `seq <= MAX_SEQ_ENCODABLE` ceiling at placement (fail-loud)
/// so two live orders can never collide on `order_id`.
pub const MAX_PRICE_TICKS_ENCODABLE: u64 = (1u64 << ORDER_ID_PRICE_BITS) - 1;
pub const MAX_SEQ_ENCODABLE: u64 = (1u64 << ORDER_ID_SEQ_BITS) - 1;

/// Compose the side-encoded `order_id` from (price_ticks, seq, side).
///
/// For BIDS we invert ONLY the price field — not the whole word — so a
/// single ascending `order_id` walk yields correct price-TIME priority on
/// both books:
///   * higher price => better => smaller key   (price inverted for bids)
///   * older seq     => better => smaller key   (seq ascending for BOTH sides)
///
/// The previous implementation inverted the entire word for bids, which
/// also inverted the seq tiebreak and made bids fill **LIFO** at each price
/// level (a price-time-priority violation). Asks were unaffected.
///
/// Price is **saturated** to `MAX_PRICE_TICKS_ENCODABLE` (clamp keeps the
/// ordering monotonic; the old code masked, which would wrap an out-of-range
/// price to a tiny key and mis-order the book). Seq is masked to 24 bits;
/// placement enforces the 24-bit ceiling so masking can never alias a live id.
/// Anti-stuffing band check: is `price_ticks` within `max_dev_bps` of `anchor`?
/// `anchor == 0` (no price anchor yet) → always true (skipped). Pure +
/// host-tested; mirrors the anchor `flp_quoter::price_within_band`. `u128`
/// intermediates so the bps comparison can't overflow.
pub fn price_within_band(anchor_ticks: u64, price_ticks: u64, max_dev_bps: u32) -> bool {
    if anchor_ticks == 0 {
        return true;
    }
    let diff = anchor_ticks.abs_diff(price_ticks) as u128;
    let allowed = (anchor_ticks as u128) * (max_dev_bps as u128);
    diff * (crate::constants::BPS_DENOM as u128) <= allowed
}

#[cfg(test)]
mod band_tests {
    use super::price_within_band;

    #[test]
    fn within_band_skips_on_zero_anchor_and_enforces_otherwise() {
        // anchor 0 → always in band (no price reference yet).
        assert!(price_within_band(0, 999_999, 1));
        // anchor 1000, 50% band → [500, 1500] inclusive.
        assert!(price_within_band(1_000, 1_500, 5_000));
        assert!(price_within_band(1_000, 500, 5_000));
        assert!(price_within_band(1_000, 1_000, 5_000));
        // just outside.
        assert!(!price_within_band(1_000, 1_501, 5_000));
        assert!(!price_within_band(1_000, 499, 5_000));
        // 0% band → only exact.
        assert!(price_within_band(1_000, 1_000, 0));
        assert!(!price_within_band(1_000, 1_001, 0));
    }
}

pub fn encode_order_id(price_ticks: u64, seq: u64, side_is_bid: bool) -> u64 {
    let price = price_ticks.min(MAX_PRICE_TICKS_ENCODABLE);
    let seq_low = seq & MAX_SEQ_ENCODABLE;
    let price_key = if side_is_bid {
        (!price) & MAX_PRICE_TICKS_ENCODABLE
    } else {
        price
    };
    (price_key << ORDER_ID_SEQ_BITS) | seq_low
}

/// Build a probe `RestingOrderV2` whose only meaningful field is `order_id`,
/// used to look up a node in an RBT by encoded id. `RestingOrderV2::cmp`
/// compares only `order_id`, so the other fields are inert for the lookup.
pub fn probe_order(order_id: u64) -> RestingOrderV2 {
    RestingOrderV2 {
        order_id,
        seq: 0,
        price_ticks: 0,
        size_lots: 0,
        expires_at_slot: 0,
        trader: Pubkey::default(),
        last_valid_slot: 0,
        side: 0,
        order_type: 0,
        flags: 0,
        sub_index: 0,
    }
}

// ─── ClaimedSeatV2 ───────────────────────────────────────────────────

/// 64-byte per-(market, trader) seat. Holds the trader pubkey + open
/// order count + free-funds balances (Phoenix-style settlement). On
/// first trade in a market the trader claims a seat (one-time rent
/// ≈ $0.0005). Subsequent trades settle into `quote_free_lots` — no
/// SPL token CPI on every fill (sub-100µs hot path, wave 19).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
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

    /// 24-byte reserved tail (split into ≤32-byte chunks for bytemuck).
    pub _reserved_a: [u8; 16],
    pub _reserved_b: [u8; 8],
}

const _: () = assert!(core::mem::size_of::<ClaimedSeatV2>() == 80);

impl PartialEq for ClaimedSeatV2 {
    fn eq(&self, other: &Self) -> bool { self.trader == other.trader }
}
impl Eq for ClaimedSeatV2 {}
impl PartialOrd for ClaimedSeatV2 {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ClaimedSeatV2 {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.trader.cmp(&other.trader)
    }
}
impl core::fmt::Display for ClaimedSeatV2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Seat {{ trader={:?} open={} }}", self.trader, self.open_orders_count)
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
// are exactly NODE_TOTAL_BYTES (96), so they fit the same FreeList
// allocator. RBNode = 16-byte RBT header + 80-byte payload = 96.
const _: () = assert!(core::mem::size_of::<RBNode<RestingOrderV2>>() == NODE_TOTAL_BYTES);
const _: () = assert!(core::mem::size_of::<RBNode<ClaimedSeatV2>>() == NODE_TOTAL_BYTES);

// ─── MarketBookHandle ────────────────────────────────────────────────
//
// Newtype over a raw `&mut [u8]` account-data slice. Exposes the header
// + dynamic byte array as separate borrowed regions so that the
// hypertree's RBT/FreeList ops can take `&mut data` without aliasing
// `&mut header`.
//
// The handle is constructed inside an ix handler from
// `ctx.accounts.market_book.try_borrow_mut_data()?` (the underlying
// account is `UncheckedAccount<'info>`, owner-checked at the context
// level). It validates the 8-byte discriminator on construction.

/// PDA seed for the market_book account: `["market_book", market.key()]`.
pub const MARKET_BOOK_SEED: &[u8] = b"market_book";

/// 8-byte discriminator marking an account as a MarketBookAccount.
/// NOT an Anchor sighash — we own it. Picked to be visually distinct
/// in raw account dumps. "FB BK MK BK 01 …" = Flash Book / Book / Market.
pub const MARKET_BOOK_DISC: [u8; 8] = [0xFB, 0xBA, 0x00, 0x4B, 0x4D, 0x4B, 0x42, 0x01];

/// Fixed prefix every MarketBookAccount carries before the node array:
/// 8-byte discriminator + 256-byte header.
pub const MARKET_BOOK_DISC_BYTES: usize = 8;
pub const MARKET_BOOK_HEADER_BYTES: usize = 256;
pub const MARKET_BOOK_PREFIX_BYTES: usize =
    MARKET_BOOK_DISC_BYTES + MARKET_BOOK_HEADER_BYTES;

/// Total byte size of a freshly-initialised MarketBookAccount
/// (disc + header + initial data). = 8 + 256 + 9600 = 9864 bytes.
pub const MARKET_BOOK_TOTAL_BYTES: usize =
    MARKET_BOOK_PREFIX_BYTES + MARKET_BOOK_DATA_BYTES;

/// Largest a MarketBookAccount may grow to via `expand_market_book`.
/// = 264 + 960,000 = 960,264 bytes.
pub const MARKET_BOOK_MAX_TOTAL_BYTES: usize =
    MARKET_BOOK_PREFIX_BYTES + MARKET_BOOK_MAX_DATA_BYTES;

pub struct MarketBookHandle<'a> {
    pub header: &'a mut MarketBookHeader,
    pub data: &'a mut [u8],
}

impl<'a> MarketBookHandle<'a> {
    /// Validate the 8-byte discriminator and split a market_book account's
    /// raw data into header + dynamic-array slices.
    pub fn from_account_data(data: &'a mut [u8]) -> Result<Self> {
        // Accept any size from the initial layout up to the expansion cap,
        // as long as the dynamic region is a whole number of nodes. This is
        // what lets a book grown by `expand_market_book` load — the old
        // `== MARKET_BOOK_TOTAL_BYTES` check capped every book at 100 nodes.
        require!(
            data.len() >= MARKET_BOOK_TOTAL_BYTES
                && data.len() <= MARKET_BOOK_MAX_TOTAL_BYTES
                && (data.len() - MARKET_BOOK_PREFIX_BYTES) % NODE_TOTAL_BYTES == 0,
            errors::FlashBookError::OutOfRange
        );
        require!(
            data[..8] == MARKET_BOOK_DISC,
            errors::FlashBookError::WrongTrader,
        );
        let (header_bytes, dyn_data) = data[8..].split_at_mut(256);
        let header: &mut MarketBookHeader = bytemuck::from_bytes_mut(header_bytes);
        // AUDIT (re-audit 2026-06-30, ER L-2): bounds-check the header's 6 node
        // indices against the slab. A structurally-invalid book — e.g. one committed
        // by a malicious/buggy ER validator across an undelegate, or any tampered
        // account — could otherwise drive the raw `get_helper` slab accessor out of
        // bounds (panic/DoS) or misread an in-bounds-but-bogus node. `NIL` is the
        // valid empty sentinel; a well-formed book's roots/best/free-list are always
        // node-aligned offsets whose whole node fits the slab, so this NEVER rejects
        // a valid book — it fails closed only on corruption. `DataIndex` is a BYTE
        // OFFSET (`get_helper` indexes `data[idx..idx+size]`). Internal RBT links are
        // NOT walked here (O(capacity) on the hot path) — they are validated once on
        // undelegate via `validate_node_links` (AUDIT O-1).
        let slab_len = dyn_data.len();
        for idx in [
            header.bids_root_index,
            header.bids_best_index,
            header.asks_root_index,
            header.asks_best_index,
            header.claimed_seats_root_index,
            header.free_list_head_index,
        ] {
            let off = idx as usize;
            require!(
                idx == crate::hypertree::NIL
                    || (off % NODE_TOTAL_BYTES == 0
                        && off.checked_add(NODE_TOTAL_BYTES).is_some_and(|end| end <= slab_len)),
                errors::FlashBookError::OutOfRange
            );
        }
        Ok(MarketBookHandle { header, data: dyn_data })
    }

    /// AUDIT O-1 (re-audit 2026-06-30) — validate every node's INTERNAL RBT links
    /// (`left`/`right`/`parent`), not just the 6 header roots `from_account_data`
    /// checks. The hot traversal accessors use the unchecked `get_helper`, so a book
    /// carrying an out-of-range child pointer would panic-DoS the FIRST L1 traversal
    /// and brick the market (no reset ix). A well-formed book can only be produced by
    /// this program's own RBT writes, so the ONLY way a corrupt book can reach L1 is a
    /// malicious-/buggy-ER commit that is then undelegated — therefore this is called
    /// EXACTLY ONCE, from `process_undelegation`, NOT on the per-op hot path. Cost:
    /// O(capacity) once per undelegate (rare); ZERO added CU on place/cancel/match. A
    /// corrupt book fails CLOSED here (`OutOfRange` → the undelegate reverts; the book
    /// never lands corrupt on L1). `#[repr(C)] RBNode<V>` lays out `left`/`right`/
    /// `parent` as its first three `DataIndex` (u32 LE) fields at byte offsets 0/4/8
    /// of every `NODE_TOTAL_BYTES` slot regardless of payload — pinned by the compile
    /// asserts below so a field reorder fails the build, not silently validates wrong.
    pub fn validate_node_links(account_data: &[u8]) -> Result<()> {
        const _: () = assert!(core::mem::offset_of!(crate::hypertree::RBNode<RestingOrderV2>, left) == 0);
        const _: () = assert!(core::mem::offset_of!(crate::hypertree::RBNode<RestingOrderV2>, right) == 4);
        const _: () = assert!(core::mem::offset_of!(crate::hypertree::RBNode<RestingOrderV2>, parent) == 8);
        require!(
            account_data.len() >= MARKET_BOOK_PREFIX_BYTES,
            errors::FlashBookError::OutOfRange
        );
        let slab = &account_data[MARKET_BOOK_PREFIX_BYTES..];
        let slab_len = slab.len();
        require!(slab_len % NODE_TOTAL_BYTES == 0, errors::FlashBookError::OutOfRange);
        let node_count = slab_len / NODE_TOTAL_BYTES;
        for i in 0..node_count {
            let base = i * NODE_TOTAL_BYTES;
            for link_off in [0usize, 4, 8] {
                let mut b = [0u8; 4];
                b.copy_from_slice(&slab[base + link_off..base + link_off + 4]);
                let off = u32::from_le_bytes(b);
                let offu = off as usize;
                require!(
                    off == crate::hypertree::NIL
                        || (offu % NODE_TOTAL_BYTES == 0
                            && offu.checked_add(NODE_TOTAL_BYTES).is_some_and(|end| end <= slab_len)),
                    errors::FlashBookError::OutOfRange
                );
            }
        }
        Ok(())
    }

    /// Stamp the discriminator + zero-init the header + data. Call this
    /// ONCE inside `init_market_book` after CreateAccount.
    pub fn write_disc_and_init_header(
        data: &mut [u8],
        bump: u8,
        market_pubkey: Pubkey,
        base_mint: Pubkey,
        quote_mint: Pubkey,
    ) -> Result<()> {
        require!(
            data.len() == MARKET_BOOK_TOTAL_BYTES,
            errors::FlashBookError::OutOfRange
        );
        data[..8].copy_from_slice(&MARKET_BOOK_DISC);
        let header_bytes = &mut data[8..8 + 256];
        let header: &mut MarketBookHeader = bytemuck::from_bytes_mut(header_bytes);
        header.bump = bump;
        header.version = 1;
        header._pad0 = [0; 6];
        header.market_pubkey = market_pubkey;
        header.base_mint = base_mint;
        header.quote_mint = quote_mint;
        header.bids_root_index = NIL;
        header.bids_best_index = NIL;
        header.asks_root_index = NIL;
        header.asks_best_index = NIL;
        header.claimed_seats_root_index = NIL;
        header.free_list_head_index = NIL;
        header.num_bytes_allocated = 0;
        header.total_orders_active = 0;
        header.order_seq_counter = 0;
        header._reserved_a = [0; 32];
        header._reserved_b = [0; 32];
        header._reserved_c = [0; 32];
        header._reserved_d = [0; 16];
        // Dynamic array stays zero — that's what Solana::CreateAccount
        // hands us, no extra zeroing needed.
        Ok(())
    }

    /// Allocate a new 80-byte node from the free-list (or grow the
    /// allocated region if the free-list is empty). Returns the byte
    /// offset where the caller can write a 64-byte payload.
    pub fn alloc_node(&mut self) -> Result<DataIndex> {
        // Try free-list first.
        let free_idx = {
            let mut fl = FreeList::<FreeListPadding>::new(
                self.data,
                self.header.free_list_head_index,
            );
            let popped = fl.remove();
            self.header.free_list_head_index = fl.get_head();
            popped
        };
        if free_idx != NIL && free_idx != FREE_LIST_END {
            return Ok(free_idx);
        }
        // Bump-alloc from the unused tail. The ceiling is the LIVE capacity
        // (`self.data` is the dynamic region after the 264-byte prefix), not
        // the init-size constant — so a book grown by `expand_market_book`
        // can allocate into the new tail.
        let next_offset = self.header.num_bytes_allocated;
        let end = next_offset.saturating_add(NODE_TOTAL_BYTES as u32);
        require!(
            (end as usize) <= self.data.len(),
            errors::FlashBookError::BufferFull
        );
        self.header.num_bytes_allocated = end;
        Ok(next_offset)
    }

    /// Return a node to the free-list. Caller is responsible for having
    /// removed the node from any tree it lived in first.
    pub fn free_node(&mut self, idx: DataIndex) {
        let mut fl = FreeList::<FreeListPadding>::new(
            self.data,
            self.header.free_list_head_index,
        );
        fl.add(idx);
        self.header.free_list_head_index = fl.get_head();
    }

    /// Insert a `RestingOrderV2` into the bids RBT. Allocates a node
    /// first, writes the payload, then inserts into the tree. Updates
    /// the bid root + best (= MIN-of-tree, which is the highest-priced
    /// bid given our inverted encoding) indices on the header.
    pub fn insert_bid(&mut self, order: RestingOrderV2) -> Result<DataIndex> {
        // Fail-loud seq ceiling — the single chokepoint every placement path
        // (place_order/taker/iceberg/twap/bracket/trigger/basket/liquidate/vault)
        // funnels through. `encode_order_id` packs `seq` into 24 bits (masking
        // `& MAX_SEQ_ENCODABLE`); a seq above the ceiling would wrap and collide,
        // silently breaking price-time priority and order-id uniqueness, so reject
        // it loudly. NOTE: pin's ceiling (2^24-1 ≈ 16.7M) is FAR tighter than
        // Anchor's `FLP_SEQ_RESERVED_OFFSET` (2^56) because pin uses a 24-bit seq
        // field in `order_id`. `order_seq_counter` is lifetime-monotonic and never
        // recycled, so a market reverts every resting insert after ~16.7M lifetime
        // placements — a known operational ceiling (see AUDIT_SCOPE residuals);
        // widening the seq field / recycling is a tracked follow-up.
        require!(order.seq <= MAX_SEQ_ENCODABLE, errors::FlashBookError::OutOfRange);
        let idx = self.alloc_node()?;
        // O(1) cache update: capture order_id now, compare against cached
        // best AFTER tree insertion. Both sides encode best = MIN-by-order_id.
        let new_order_id = order.order_id;
        let new_root;
        {
            let mut tree = RedBlackTree::<RestingOrderV2>::new(
                self.data,
                self.header.bids_root_index,
                NIL,
            );
            tree.insert(idx, order);
            new_root = tree.get_root_index();
        }
        self.header.bids_root_index = new_root;
        // O(1) best update: new is best iff tree was empty OR new order_id
        // < current best's order_id. No leftmost-walk required.
        let cur_best = self.header.bids_best_index;
        if cur_best == NIL
            || new_order_id < self.order_at(cur_best).order_id
        {
            self.header.bids_best_index = idx;
        }
        self.header.total_orders_active = self.header.total_orders_active.saturating_add(1);
        Ok(idx)
    }

    /// Insert a `RestingOrderV2` into the asks RBT. Mirror of `insert_bid`.
    /// "Best" = MIN of tree = lowest-priced ask (asks are NOT inverted, so
    /// natural ascending order — smallest order_id is the best ask).
    pub fn insert_ask(&mut self, order: RestingOrderV2) -> Result<DataIndex> {
        // Fail-loud seq ceiling — see `insert_bid`. Every placement path funnels
        // through here; `encode_order_id` requires `seq <= MAX_SEQ_ENCODABLE` or
        // the 24-bit packing wraps and collides.
        require!(order.seq <= MAX_SEQ_ENCODABLE, errors::FlashBookError::OutOfRange);
        let idx = self.alloc_node()?;
        let new_order_id = order.order_id;
        let new_root;
        {
            let mut tree = RedBlackTree::<RestingOrderV2>::new(
                self.data,
                self.header.asks_root_index,
                NIL,
            );
            tree.insert(idx, order);
            new_root = tree.get_root_index();
        }
        self.header.asks_root_index = new_root;
        let cur_best = self.header.asks_best_index;
        if cur_best == NIL
            || new_order_id < self.order_at(cur_best).order_id
        {
            self.header.asks_best_index = idx;
        }
        self.header.total_orders_active = self.header.total_orders_active.saturating_add(1);
        Ok(idx)
    }

    /// Walk the bids RBT in BEST → WORST order (highest price first). Calls
    /// `f(idx, order)` for each resting bid. Stops early if `f` returns
    /// `false`. Read-only; safe to call from a view ix.
    ///
    /// Wave 18e's read-side primitive — wave 18f's matcher walks the tree
    /// via this same helper to consume liquidity in price-time priority.
    pub fn for_each_bid_best_first<F>(&self, mut f: F)
    where
        F: FnMut(DataIndex, &RestingOrderV2) -> bool,
    {
        for_each_best_first::<RestingOrderV2, F>(
            &self.data[..],
            self.header.bids_root_index,
            self.header.bids_best_index,
            &mut f,
        );
    }

    /// Walk the asks RBT in BEST → WORST order (lowest price first). Mirror
    /// of `for_each_bid_best_first`.
    pub fn for_each_ask_best_first<F>(&self, mut f: F)
    where
        F: FnMut(DataIndex, &RestingOrderV2) -> bool,
    {
        for_each_best_first::<RestingOrderV2, F>(
            &self.data[..],
            self.header.asks_root_index,
            self.header.asks_best_index,
            &mut f,
        );
    }

    /// Decrement the `size_lots` of the `RestingOrderV2` at `idx` by `delta`.
    /// Caller must guarantee `idx` is a live node in either the bids or asks
    /// RBT. Returns the new size_lots. Used by the matcher to apply partial
    /// fills without removing the order from the book.
    ///
    /// MATCH-H3: **checked** sub. A `delta` larger than the resting size is an
    /// over-fill accounting bug (base/quote would stop conserving) — it is now
    /// rejected instead of being silently saturated to zero, which masked the
    /// bug while the taker still recorded the full fill. Returns the new size.
    pub fn decrement_size_at(&mut self, idx: DataIndex, delta: u64) -> Result<u64> {
        let node: &mut RBNode<RestingOrderV2> =
            get_mut_helper::<RBNode<RestingOrderV2>>(self.data, idx);
        let new_size = node
            .get_value()
            .size_lots
            .checked_sub(delta)
            .ok_or_else(|| error!(errors::FlashBookError::ArithmeticOverflow))?;
        node.get_mut_value().size_lots = new_size;
        Ok(new_size)
    }

    /// Read-only access to the `RestingOrderV2` at `idx`. Caller must
    /// guarantee `idx` is a live node.
    pub fn order_at(&self, idx: DataIndex) -> &RestingOrderV2 {
        let node: &RBNode<RestingOrderV2> =
            get_helper::<RBNode<RestingOrderV2>>(&self.data[..], idx);
        node.get_value()
    }

    /// Remove a node from the bids RBT, free its slot to the free-list,
    /// and refresh the cached best/root indices. The matcher calls this
    /// when a bid is fully consumed (size hits zero) and for permissionless
    /// expired-order cleanup.
    pub fn remove_bid_node(&mut self, idx: DataIndex) {
        // Pre-capture successor BEFORE tree mutation. MIN has no left child
        // (leftmost by construction), so it has ≤1 child and remove_by_index
        // skips its swap-with-successor path — meaning the successor's
        // DataIndex remains valid after the remove. Rotations may shuffle
        // parent/child pointers but never move VALUES between slots, so
        // the successor's index still points at the in-order next-smallest
        // node — which IS the new MIN once `idx` is gone. If we're not
        // removing the best, the cached pointer is unchanged.
        let was_best = self.header.bids_best_index == idx;
        let new_best = if was_best {
            let tree = RedBlackTreeReadOnly::<RestingOrderV2>::new(
                &self.data[..],
                self.header.bids_root_index,
                NIL,
            );
            successor_index::<RestingOrderV2>(&tree, idx)
        } else {
            self.header.bids_best_index
        };

        let new_root;
        {
            let mut tree = RedBlackTree::<RestingOrderV2>::new(
                self.data,
                self.header.bids_root_index,
                NIL,
            );
            tree.remove_by_index(idx);
            new_root = tree.get_root_index();
        }
        self.header.bids_root_index = new_root;
        self.header.bids_best_index = new_best;
        self.header.total_orders_active =
            self.header.total_orders_active.saturating_sub(1);
        self.free_node(idx);
    }

    /// Mirror of `remove_bid_node` for the asks RBT.
    pub fn remove_ask_node(&mut self, idx: DataIndex) {
        let was_best = self.header.asks_best_index == idx;
        let new_best = if was_best {
            let tree = RedBlackTreeReadOnly::<RestingOrderV2>::new(
                &self.data[..],
                self.header.asks_root_index,
                NIL,
            );
            successor_index::<RestingOrderV2>(&tree, idx)
        } else {
            self.header.asks_best_index
        };

        let new_root;
        {
            let mut tree = RedBlackTree::<RestingOrderV2>::new(
                self.data,
                self.header.asks_root_index,
                NIL,
            );
            tree.remove_by_index(idx);
            new_root = tree.get_root_index();
        }
        self.header.asks_root_index = new_root;
        self.header.asks_best_index = new_best;
        self.header.total_orders_active =
            self.header.total_orders_active.saturating_sub(1);
        self.free_node(idx);
    }

    /// Find the node in the bids RBT whose `order_id` matches `order_id`.
    /// O(log n). Returns `NIL` if not found. Used by `cancel_order_v2` to
    /// translate (trader, side, seq) → DataIndex.
    pub fn lookup_bid_by_order_id(&self, order_id: u64) -> DataIndex {
        if self.header.bids_root_index == NIL {
            return NIL;
        }
        let probe = probe_order(order_id);
        let tree = RedBlackTreeReadOnly::<RestingOrderV2>::new(
            &self.data[..],
            self.header.bids_root_index,
            NIL,
        );
        tree.lookup_index::<RestingOrderV2>(&probe)
    }

    /// Mirror of `lookup_bid_by_order_id` for the asks RBT.
    pub fn lookup_ask_by_order_id(&self, order_id: u64) -> DataIndex {
        if self.header.asks_root_index == NIL {
            return NIL;
        }
        let probe = probe_order(order_id);
        let tree = RedBlackTreeReadOnly::<RestingOrderV2>::new(
            &self.data[..],
            self.header.asks_root_index,
            NIL,
        );
        tree.lookup_index::<RestingOrderV2>(&probe)
    }

    /// Insert a claimed seat into the seats RBT.
    pub fn insert_seat(&mut self, seat: ClaimedSeatV2) -> Result<DataIndex> {
        let idx = self.alloc_node()?;
        let new_root;
        {
            let mut tree = RedBlackTree::<ClaimedSeatV2>::new(
                self.data,
                self.header.claimed_seats_root_index,
                NIL,
            );
            tree.insert(idx, seat);
            new_root = tree.get_root_index();
        }
        self.header.claimed_seats_root_index = new_root;
        Ok(idx)
    }
}

/// 92-byte payload for the FreeList — pure padding. Sized so that
/// `FreeListNode<FreeListPadding>` (next_index 4B + payload 92B) is exactly
/// `NODE_TOTAL_BYTES` (96), which means `FreeList::add` scrubs the **entire**
/// freed slab slot rather than the first 84 bytes — closing HYP-C2/H-2 (a
/// freed slot used to retain 12 stale RBNode bytes: stale parent/color/high
/// value bytes that were a latent dangling-index footgun under refactor /
/// formal verification). Manifest's pattern, full-slot variant.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FreeListPadding {
    pub _padding_a: [u8; 32],
    pub _padding_b: [u8; 32],
    pub _padding_c: [u8; 28],
}
impl Get for FreeListPadding {}

const _: () = assert!(core::mem::size_of::<FreeListPadding>() == 92);
// Must equal NODE_TOTAL_BYTES so a freed slot is fully zeroed by `add`.
const _: () = assert!(core::mem::size_of::<FreeListNode<FreeListPadding>>() == NODE_TOTAL_BYTES);

/// FreeList sentinel — Manifest uses `u32::MAX` to mean "no more free
/// nodes". When `FreeList::remove()` returns this, the bump-alloc
/// fallback kicks in.
const FREE_LIST_END: DataIndex = u32::MAX;

// ─── RBT walk helpers ────────────────────────────────────────────────
//
// The vendored hypertree exposes `lookup_max_index` + `get_next_lower_index`
// (full predecessor) but only a half-case `get_next_higher_index` (used
// internally by remove). We need a real ascending iterator from the
// MIN of the tree — that's the natural walk for both books since our
// `encode_order_id` puts the BEST (highest-priced bid / lowest-priced
// ask) at the smallest order_id for both sides.

/// Walk `idx` and all left descendants until the leftmost node. Returns
/// `idx` if it has no left child. Caller passes a non-NIL starting index.
fn leftmost_descendant<V: Payload>(
    tree: &RedBlackTreeReadOnly<V>,
    mut idx: DataIndex,
) -> DataIndex {
    loop {
        let left = tree.get_left_index::<V>(idx);
        if left == NIL {
            return idx;
        }
        idx = left;
    }
}

/// Find the MIN (leftmost) node of an RBT given its root. Returns `NIL`
/// if the tree is empty. O(log n).
pub fn lookup_min_index<V: Payload>(data: &[u8], root: DataIndex) -> DataIndex {
    if root == NIL {
        return NIL;
    }
    let tree = RedBlackTreeReadOnly::<V>::new(data, root, NIL);
    leftmost_descendant::<V>(&tree, root)
}

/// In-order successor in an RBT view. General-purpose (handles both the
/// right-child case and the walk-up-while-right-child case). The vendored
/// `get_next_higher_index` only handles the first case (debug-asserts a
/// right child exists) because remove never calls it on leaves.
fn successor_index<V: Payload>(
    tree: &RedBlackTreeReadOnly<V>,
    idx: DataIndex,
) -> DataIndex {
    if idx == NIL {
        return NIL;
    }
    // Case 1: right subtree exists → leftmost of right subtree.
    let right = tree.get_right_index::<V>(idx);
    if right != NIL {
        return leftmost_descendant::<V>(tree, right);
    }
    // Case 2: walk up while we are a right child.
    let mut cur = idx;
    loop {
        let parent = tree.get_parent_index::<V>(cur);
        if parent == NIL {
            return NIL;
        }
        if tree.is_left_child::<V>(cur) {
            return parent;
        }
        cur = parent;
    }
}

/// Internal: walk an RBT in BEST → WORST (ascending order_id) order,
/// calling `f` on each. `cached_min` is consulted first for O(1) start;
/// if `NIL`, falls back to walking from `root`.
fn for_each_best_first<V, F>(
    data: &[u8],
    root: DataIndex,
    cached_min: DataIndex,
    f: &mut F,
) where
    V: Payload,
    F: FnMut(DataIndex, &V) -> bool,
{
    if root == NIL {
        return;
    }
    let tree = RedBlackTreeReadOnly::<V>::new(data, root, NIL);
    let mut idx = if cached_min != NIL {
        cached_min
    } else {
        leftmost_descendant::<V>(&tree, root)
    };
    while idx != NIL {
        let value: &V = tree.get_value::<V>(idx);
        if !f(idx, value) {
            return;
        }
        idx = successor_index::<V>(&tree, idx);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    fn uniq() -> Pubkey {
        use core::sync::atomic::{AtomicU8, Ordering};
        static C: AtomicU8 = AtomicU8::new(1);
        [C.fetch_add(1, Ordering::Relaxed); 32]
    }

    fn make_book() -> Vec<u8> {
        let mut data = vec![0u8; MARKET_BOOK_TOTAL_BYTES];
        MarketBookHandle::write_disc_and_init_header(
            &mut data,
            255,
            uniq(),
            uniq(),
            uniq(),
        )
        .expect("init header");
        data
    }

    // Re-audit 2026-06-30 (HIGH): the ER-undelegate book-corruption guards.
    #[test]
    fn book_validation_accepts_clean_rejects_corruption() {
        let data = make_book();
        // A freshly-initialised book validates clean on both guards.
        assert!(MarketBookHandle::validate_node_links(&data).is_ok());
        {
            let mut d = data.clone();
            assert!(MarketBookHandle::from_account_data(&mut d).is_ok());
        }
        // (1) Corrupt a HEADER node index (bids_root_index @ header off 104 → byte
        //     8+104=112) to an out-of-range, non-aligned value → from_account_data rejects.
        {
            let mut d = data.clone();
            d[112..116].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            assert!(MarketBookHandle::from_account_data(&mut d).is_err());
        }
        // (2) Corrupt a NODE's internal `left` link (first slab node, offset 0) to a
        //     non-node-aligned in-range value → validate_node_links rejects.
        {
            let mut d = data.clone();
            let base = MARKET_BOOK_PREFIX_BYTES;
            d[base..base + 4].copy_from_slice(&1u32.to_le_bytes()); // 1 not % 96
            assert!(MarketBookHandle::validate_node_links(&d).is_err());
        }
    }

    fn make_order(price: u64, seq: u64, side_is_bid: bool) -> RestingOrderV2 {
        make_order_for(price, seq, side_is_bid, Pubkey::default())
    }

    fn make_order_for(
        price: u64,
        seq: u64,
        side_is_bid: bool,
        trader: Pubkey,
    ) -> RestingOrderV2 {
        RestingOrderV2 {
            order_id: encode_order_id(price, seq, side_is_bid),
            seq,
            price_ticks: price,
            size_lots: 100,
            expires_at_slot: 0,
            trader,
            last_valid_slot: 0,
            side: if side_is_bid { 0 } else { 1 },
            order_type: 0,
            flags: 0,
            sub_index: 0,
        }
    }

    fn collect_bids(handle: &MarketBookHandle) -> Vec<u64> {
        let mut prices = Vec::new();
        handle.for_each_bid_best_first(|_idx, o| {
            prices.push(o.price_ticks);
            true
        });
        prices
    }

    fn collect_asks(handle: &MarketBookHandle) -> Vec<u64> {
        let mut prices = Vec::new();
        handle.for_each_ask_best_first(|_idx, o| {
            prices.push(o.price_ticks);
            true
        });
        prices
    }

    // Audit 2026-06 regression: the seq ceiling MUST be enforced at the insert
    // chokepoint (every placement path funnels through insert_bid/insert_ask), or
    // `encode_order_id`'s 24-bit `& MAX_SEQ_ENCODABLE` packing wraps and collides,
    // silently breaking price-time priority + order-id uniqueness.
    #[test]
    fn insert_rejects_seq_above_encoding_ceiling() {
        // seq == MAX_SEQ_ENCODABLE is the last encodable value ⇒ accepted.
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        assert!(handle.insert_bid(make_order(100, MAX_SEQ_ENCODABLE, true)).is_ok());
        assert!(handle.insert_ask(make_order(100, MAX_SEQ_ENCODABLE, false)).is_ok());

        // seq == MAX_SEQ_ENCODABLE + 1 would wrap to 0 under the mask ⇒ rejected
        // fail-loud on BOTH sides.
        let mut data2 = make_book();
        let mut handle2 = MarketBookHandle::from_account_data(&mut data2).unwrap();
        let over = MAX_SEQ_ENCODABLE + 1;
        assert!(handle2.insert_bid(make_order(100, over, true)).is_err());
        assert!(handle2.insert_ask(make_order(100, over, false)).is_err());
        // u64::MAX is also rejected (not just the boundary).
        assert!(handle2.insert_bid(make_order(100, u64::MAX, true)).is_err());
    }

    #[test]
    fn empty_book_iterates_nothing() {
        let mut data = make_book();
        let handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        assert!(collect_bids(&handle).is_empty());
        assert!(collect_asks(&handle).is_empty());
        assert_eq!(handle.header.bids_root_index, NIL);
        assert_eq!(handle.header.asks_root_index, NIL);
        assert_eq!(handle.header.bids_best_index, NIL);
        assert_eq!(handle.header.asks_best_index, NIL);
    }

    #[test]
    fn bids_iterate_highest_price_first() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        // Insert in scrambled order — iteration must still yield best first.
        handle.insert_bid(make_order(100, 1, true)).unwrap();
        handle.insert_bid(make_order(200, 2, true)).unwrap();
        handle.insert_bid(make_order(150, 3, true)).unwrap();
        handle.insert_bid(make_order(175, 4, true)).unwrap();
        handle.insert_bid(make_order(125, 5, true)).unwrap();
        assert_eq!(collect_bids(&handle), vec![200, 175, 150, 125, 100]);
        assert_eq!(handle.header.total_orders_active, 5);
    }

    #[test]
    fn asks_iterate_lowest_price_first() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        handle.insert_ask(make_order(200, 1, false)).unwrap();
        handle.insert_ask(make_order(100, 2, false)).unwrap();
        handle.insert_ask(make_order(175, 3, false)).unwrap();
        handle.insert_ask(make_order(125, 4, false)).unwrap();
        handle.insert_ask(make_order(150, 5, false)).unwrap();
        assert_eq!(collect_asks(&handle), vec![100, 125, 150, 175, 200]);
        assert_eq!(handle.header.total_orders_active, 5);
    }

    #[test]
    fn best_index_tracks_best_price_through_inserts() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();

        // First bid at 100 → that's the best.
        handle.insert_bid(make_order(100, 1, true)).unwrap();
        let best_node_first = handle.header.bids_best_index;
        assert_ne!(best_node_first, NIL);

        // Insert a HIGHER bid at 200 → best should switch to it.
        handle.insert_bid(make_order(200, 2, true)).unwrap();
        let best_node_after = handle.header.bids_best_index;
        assert_ne!(best_node_after, best_node_first);

        // Insert a LOWER bid at 50 → best stays at 200.
        handle.insert_bid(make_order(50, 3, true)).unwrap();
        assert_eq!(handle.header.bids_best_index, best_node_after);

        // Confirm walk order matches.
        assert_eq!(collect_bids(&handle), vec![200, 100, 50]);
    }

    #[test]
    fn iteration_short_circuits_when_callback_returns_false() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        for (i, p) in [100u64, 200, 150, 175, 125].iter().enumerate() {
            handle
                .insert_bid(make_order(*p, (i + 1) as u64, true))
                .unwrap();
        }
        let mut visited = Vec::new();
        handle.for_each_bid_best_first(|_idx, o| {
            visited.push(o.price_ticks);
            visited.len() < 2
        });
        assert_eq!(visited, vec![200, 175]);
    }

    #[test]
    fn bids_and_asks_share_storage_independently() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        handle.insert_bid(make_order(100, 1, true)).unwrap();
        handle.insert_ask(make_order(200, 2, false)).unwrap();
        handle.insert_bid(make_order(99, 3, true)).unwrap();
        handle.insert_ask(make_order(201, 4, false)).unwrap();
        assert_eq!(collect_bids(&handle), vec![100, 99]);
        assert_eq!(collect_asks(&handle), vec![200, 201]);
        assert_eq!(handle.header.total_orders_active, 4);
    }

    #[test]
    fn lookup_by_order_id_finds_inserted_nodes() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let bid_idx = handle.insert_bid(make_order(150, 7, true)).unwrap();
        let ask_idx = handle.insert_ask(make_order(160, 8, false)).unwrap();
        let bid_id = encode_order_id(150, 7, true);
        let ask_id = encode_order_id(160, 8, false);
        assert_eq!(handle.lookup_bid_by_order_id(bid_id), bid_idx);
        assert_eq!(handle.lookup_ask_by_order_id(ask_id), ask_idx);
        // Cross-side lookup must return NIL.
        assert_eq!(handle.lookup_bid_by_order_id(ask_id), NIL);
        assert_eq!(handle.lookup_ask_by_order_id(bid_id), NIL);
        // Unknown id returns NIL.
        assert_eq!(handle.lookup_bid_by_order_id(0xDEAD), NIL);
    }

    #[test]
    fn same_price_bids_fill_fifo_not_lifo() {
        // Regression for the bid LIFO bug: at one price level the OLDER
        // order (smaller seq) must be the best / fill first. The old
        // encode_order_id inverted the whole word, flipping the seq
        // tiebreak so the NEWEST bid filled first.
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let _older = handle.insert_bid(make_order(100, 1, true)).unwrap();
        let _newer = handle.insert_bid(make_order(100, 2, true)).unwrap();
        let best = handle.header.bids_best_index;
        assert_ne!(best, NIL);
        assert_eq!(
            handle.order_at(best).seq,
            1,
            "older bid (seq=1) must fill first at the same price (FIFO)"
        );
    }

    #[test]
    fn same_price_asks_fill_fifo() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let _o1 = handle.insert_ask(make_order(100, 1, false)).unwrap();
        let _o2 = handle.insert_ask(make_order(100, 2, false)).unwrap();
        let best = handle.header.asks_best_index;
        assert_ne!(best, NIL);
        assert_eq!(
            handle.order_at(best).seq,
            1,
            "older ask (seq=1) must fill first at the same price (FIFO)"
        );
    }

    #[test]
    fn bid_price_priority_preserved_after_fifo_fix() {
        // Higher-priced bid still beats lower-priced regardless of seq.
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        handle.insert_bid(make_order(100, 1, true)).unwrap(); // older, cheaper
        handle.insert_bid(make_order(101, 2, true)).unwrap(); // newer, pricier
        let best = handle.header.bids_best_index;
        assert_eq!(handle.order_at(best).price_ticks, 101, "highest bid is best");
    }

    #[test]
    fn decrement_size_at_partial_fill() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let idx = handle.insert_bid(make_order(100, 1, true)).unwrap();
        assert_eq!(handle.order_at(idx).size_lots, 100);
        let new_size = handle.decrement_size_at(idx, 30).unwrap();
        assert_eq!(new_size, 70);
        assert_eq!(handle.order_at(idx).size_lots, 70);
        // MATCH-H3: over-decrement (delta > size) is now REJECTED, not
        // silently saturated to zero. The size is left unchanged.
        assert!(handle.decrement_size_at(idx, 999).is_err());
        assert_eq!(handle.order_at(idx).size_lots, 70);
    }

    #[test]
    fn remove_bid_node_releases_slot_and_updates_best() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let _low = handle.insert_bid(make_order(100, 1, true)).unwrap();
        let high = handle.insert_bid(make_order(200, 2, true)).unwrap();
        let _mid = handle.insert_bid(make_order(150, 3, true)).unwrap();
        assert_eq!(collect_bids(&handle), vec![200, 150, 100]);
        assert_eq!(handle.header.total_orders_active, 3);

        // Remove the highest bid; best must now be 150.
        handle.remove_bid_node(high);
        assert_eq!(collect_bids(&handle), vec![150, 100]);
        assert_eq!(handle.header.total_orders_active, 2);
        let new_best = handle.header.bids_best_index;
        assert_ne!(new_best, NIL);
        assert_eq!(handle.order_at(new_best).price_ticks, 150);

        // The freed slot must be reusable on the next insert.
        let reused = handle.insert_bid(make_order(175, 4, true)).unwrap();
        assert_eq!(reused, high);
        assert_eq!(collect_bids(&handle), vec![175, 150, 100]);
    }

    #[test]
    fn remove_ask_node_releases_slot_and_updates_best() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let low = handle.insert_ask(make_order(100, 1, false)).unwrap();
        let _high = handle.insert_ask(make_order(200, 2, false)).unwrap();
        let _mid = handle.insert_ask(make_order(150, 3, false)).unwrap();
        assert_eq!(collect_asks(&handle), vec![100, 150, 200]);

        // Remove the best (lowest) ask; new best must be 150.
        handle.remove_ask_node(low);
        assert_eq!(collect_asks(&handle), vec![150, 200]);
        let new_best = handle.header.asks_best_index;
        assert_ne!(new_best, NIL);
        assert_eq!(handle.order_at(new_best).price_ticks, 150);
    }

    #[test]
    fn remove_last_node_clears_root_and_best() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let only = handle.insert_bid(make_order(100, 1, true)).unwrap();
        handle.remove_bid_node(only);
        assert_eq!(handle.header.bids_root_index, NIL);
        assert_eq!(handle.header.bids_best_index, NIL);
        assert_eq!(handle.header.total_orders_active, 0);
        assert!(collect_bids(&handle).is_empty());
    }

    // ─── expand_market_book (capacity growth past the 100-node cap) ──────

    /// Mirror the on-chain `realloc(.., zero_init=true)` by appending zeroed
    /// node slots to the account buffer — exactly what `expand_market_book`
    /// hands back to the matcher.
    fn grow_book(mut data: Vec<u8>, additional_nodes: usize) -> Vec<u8> {
        data.resize(data.len() + additional_nodes * NODE_TOTAL_BYTES, 0);
        data
    }

    #[test]
    fn bump_allocator_fills_initial_capacity_then_rejects() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        // The initial region holds exactly MAX_NODES (100) nodes.
        for i in 1..=MAX_NODES as u64 {
            handle.insert_bid(make_order(i, i, true)).unwrap();
        }
        assert_eq!(handle.header.total_orders_active, MAX_NODES as u32);
        // The next allocation overflows the initial region.
        assert!(
            handle.insert_bid(make_order(9_999, 9_999, true)).is_err(),
            "101st insert must hit BufferFull"
        );
    }

    #[test]
    fn from_account_data_accepts_grown_and_rejects_bad_sizes() {
        // A book grown by 50 node slots loads fine.
        let grown = grow_book(make_book(), 50);
        let mut g = grown.clone();
        assert!(MarketBookHandle::from_account_data(&mut g).is_ok());

        // One byte past a whole node → misaligned → rejected.
        let mut bad = grown.clone();
        bad.push(0);
        assert!(MarketBookHandle::from_account_data(&mut bad).is_err());

        // Smaller than the initial layout → rejected.
        let mut small = vec![0u8; MARKET_BOOK_TOTAL_BYTES - NODE_TOTAL_BYTES];
        assert!(MarketBookHandle::from_account_data(&mut small).is_err());

        // Larger than the expansion ceiling → rejected (disc stamped so only
        // the size check can trip).
        let mut huge = vec![0u8; MARKET_BOOK_MAX_TOTAL_BYTES + NODE_TOTAL_BYTES];
        huge[..8].copy_from_slice(&MARKET_BOOK_DISC);
        assert!(MarketBookHandle::from_account_data(&mut huge).is_err());
    }

    #[test]
    fn expand_lets_book_grow_past_initial_cap() {
        // Fill the initial 100-node region to the brim.
        let mut data = make_book();
        {
            let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
            for i in 1..=MAX_NODES as u64 {
                handle.insert_bid(make_order(i, i, true)).unwrap();
            }
            assert!(handle.insert_bid(make_order(7_777, 7_777, true)).is_err());
        }

        // Grow by 20 node slots — the on-chain `expand_market_book` effect.
        let mut grown = grow_book(data, 20);
        let mut handle = MarketBookHandle::from_account_data(&mut grown).unwrap();

        // The freshly-grown tail now accepts 20 more orders.
        for i in 0..20u64 {
            handle
                .insert_bid(make_order(1_000 + i, 1_000 + i, true))
                .unwrap();
        }
        assert_eq!(handle.header.total_orders_active, (MAX_NODES + 20) as u32);
        // Capacity is now 120 — the 121st overflows again.
        assert!(handle.insert_bid(make_order(2_000, 2_000, true)).is_err());

        // Integrity survives the grow: every bid still in descending order.
        let bids = collect_bids(&handle);
        assert_eq!(bids.len(), MAX_NODES + 20);
        for w in bids.windows(2) {
            assert!(w[0] > w[1], "bids must stay descending after expand");
        }
    }

}

// ── Formal verification (Kani) — order-id encoding under the seq ceiling ──────
// Audit 2026-06: the `insert_bid`/`insert_ask` chokepoint now enforces
// `seq <= MAX_SEQ_ENCODABLE`. These proofs are the *reason* that ceiling is the
// right precondition — they show that UNDER it, `encode_order_id` is collision-
// free and price-time-priority-correct (mirrors the Anchor H1 seq-guard proof).
#[cfg(kani)]
mod kani_proofs {
    use super::{encode_order_id, MAX_PRICE_TICKS_ENCODABLE, MAX_SEQ_ENCODABLE, ORDER_ID_SEQ_BITS};

    /// Collision-freedom: on one side, at one price, two DISTINCT in-range seqs
    /// always produce DISTINCT order_ids. (Above the ceiling the `& MAX_SEQ`
    /// mask wraps — exactly what the insert guard forbids.)
    #[kani::proof]
    fn encode_order_id_collision_free_under_ceiling() {
        let price: u64 = kani::any();
        let s1: u64 = kani::any();
        let s2: u64 = kani::any();
        kani::assume(price <= MAX_PRICE_TICKS_ENCODABLE);
        kani::assume(s1 <= MAX_SEQ_ENCODABLE && s2 <= MAX_SEQ_ENCODABLE);
        kani::assume(s1 != s2);
        let bid: bool = kani::any();
        assert!(encode_order_id(price, s1, bid) != encode_order_id(price, s2, bid));
    }

    /// Price-time priority: the seq occupies only the low `ORDER_ID_SEQ_BITS`, so
    /// a strictly better price key always sorts ahead REGARDLESS of seq. For asks
    /// (natural order) a lower price ⇒ smaller order_id; bids invert the price, so
    /// a higher price ⇒ smaller order_id. We prove the ask direction here: same
    /// side, p1 < p2 ⇒ id(p1) < id(p2) for ANY two in-range seqs.
    #[kani::proof]
    fn encode_order_id_price_dominates_seq_asks() {
        let p1: u64 = kani::any();
        let p2: u64 = kani::any();
        let s1: u64 = kani::any();
        let s2: u64 = kani::any();
        kani::assume(p1 <= MAX_PRICE_TICKS_ENCODABLE && p2 <= MAX_PRICE_TICKS_ENCODABLE);
        kani::assume(s1 <= MAX_SEQ_ENCODABLE && s2 <= MAX_SEQ_ENCODABLE);
        kani::assume(p1 < p2);
        // ask side (side_is_bid = false): price key is the raw price.
        assert!(encode_order_id(p1, s1, false) < encode_order_id(p2, s2, false));
        // sanity: the seq lives strictly below the price field.
        assert!((1u64 << ORDER_ID_SEQ_BITS) > MAX_SEQ_ENCODABLE);
    }
}

