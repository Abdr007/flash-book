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

use crate::hypertree::{
    get_helper, get_mut_helper, DataIndex, FreeList, FreeListNode, Get,
    HyperTreeReadOperations, HyperTreeWriteOperations, Payload, RBNode, RedBlackTree,
    RedBlackTreeReadOnly, RedBlackTreeReadOperationsHelpers, NIL,
};

/// Bytes available for hypertree nodes after the fixed header. 100 nodes
/// of 96 bytes each. Enough for ~50 orders per side + 50 claimed seats
/// in the v1 layout. Realloc-to-grow ix follows in wave 18d.
pub const MARKET_BOOK_DATA_BYTES: usize = 9_600;

/// Each RBNode<V> = 16-byte RBT header + V (the payload).
/// We size payloads at 80 bytes so each node = 96 bytes total — wider
/// than Manifest (64+16=80) because OUR resting orders carry the
/// trader Pubkey inline (Manifest indirects through a seat). Wave 19's
/// free-funds optimisation will move trader off the order onto the
/// seat and shrink back to 80B.
pub const NODE_PAYLOAD_BYTES: usize = 80;
pub const NODE_TOTAL_BYTES: usize = 96;
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

    /// Anti-replay guard for off-chain replay tools.
    pub last_valid_slot: u32,

    pub side: u8,        // 0 = long/buy/bid, 1 = short/sell/ask
    pub order_type: u8,  // 0 = limit, 1 = ioc, 2 = post_only, 3 = jit
    /// Bitfield: bit0 reduce_only, bit1 post_only-shortcut, bits 2-3 STP mode.
    pub flags: u8,
    pub _pad: u8,
}

const _: () = assert!(std::mem::size_of::<RestingOrderV2>() == 80);

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
        _pad: 0,
    }
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

    /// 24-byte reserved tail (split into ≤32-byte chunks for bytemuck).
    pub _reserved_a: [u8; 16],
    pub _reserved_b: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<ClaimedSeatV2>() == 80);

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
// are exactly NODE_TOTAL_BYTES (96), so they fit the same FreeList
// allocator. RBNode = 16-byte RBT header + 80-byte payload = 96.
const _: () = assert!(std::mem::size_of::<RBNode<RestingOrderV2>>() == NODE_TOTAL_BYTES);
const _: () = assert!(std::mem::size_of::<RBNode<ClaimedSeatV2>>() == NODE_TOTAL_BYTES);

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

/// Total byte size of a MarketBookAccount (disc + header + data).
/// = 8 + 256 + 8000 = 8264 bytes.
pub const MARKET_BOOK_TOTAL_BYTES: usize = 8 + 256 + MARKET_BOOK_DATA_BYTES;

pub struct MarketBookHandle<'a> {
    pub header: &'a mut MarketBookHeader,
    pub data: &'a mut [u8],
}

impl<'a> MarketBookHandle<'a> {
    /// Validate the 8-byte discriminator and split a market_book account's
    /// raw data into header + dynamic-array slices.
    pub fn from_account_data(data: &'a mut [u8]) -> Result<Self> {
        require!(
            data.len() == MARKET_BOOK_TOTAL_BYTES,
            crate::errors::FlashBookError::OutOfRange
        );
        require!(
            data[..8] == MARKET_BOOK_DISC,
            crate::errors::FlashBookError::WrongTrader,
        );
        let (header_bytes, dyn_data) = data[8..].split_at_mut(256);
        let header: &mut MarketBookHeader = bytemuck::from_bytes_mut(header_bytes);
        Ok(MarketBookHandle { header, data: dyn_data })
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
            crate::errors::FlashBookError::OutOfRange
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
        // Bump-alloc from the unused tail.
        let next_offset = self.header.num_bytes_allocated;
        let end = next_offset.saturating_add(NODE_TOTAL_BYTES as u32);
        require!(
            (end as usize) <= MARKET_BOOK_DATA_BYTES,
            crate::errors::FlashBookError::BufferFull
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
        let idx = self.alloc_node()?;
        // Write the RBNode<RestingOrderV2> payload at idx. RedBlackTree's
        // `insert` will fill in the RBT-bookkeeping fields (left/right/
        // parent/color) and call `value = order` internally.
        let new_root;
        {
            // Pass NIL for the RBT's max-tracking slot — we don't use the
            // RBT's notion of MAX. Our "best" is the MIN of the tree (see
            // `encode_order_id` for why), recomputed below.
            let mut tree = RedBlackTree::<RestingOrderV2>::new(
                self.data,
                self.header.bids_root_index,
                NIL,
            );
            tree.insert(idx, order);
            new_root = tree.get_root_index();
        }
        self.header.bids_root_index = new_root;
        self.header.bids_best_index = lookup_min_index::<RestingOrderV2>(self.data, new_root);
        self.header.total_orders_active = self.header.total_orders_active.saturating_add(1);
        Ok(idx)
    }

    /// Insert a `RestingOrderV2` into the asks RBT. Mirror of `insert_bid`.
    /// "Best" = MIN of tree = lowest-priced ask (asks are NOT inverted, so
    /// natural ascending order — smallest order_id is the best ask).
    pub fn insert_ask(&mut self, order: RestingOrderV2) -> Result<DataIndex> {
        let idx = self.alloc_node()?;
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
        self.header.asks_best_index = lookup_min_index::<RestingOrderV2>(self.data, new_root);
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
    /// Saturating sub: a delta larger than current size lands at zero (caller
    /// should then remove the node).
    pub fn decrement_size_at(&mut self, idx: DataIndex, delta: u64) -> u64 {
        let node: &mut RBNode<RestingOrderV2> =
            get_mut_helper::<RBNode<RestingOrderV2>>(self.data, idx);
        let new_size = node.get_value().size_lots.saturating_sub(delta);
        node.get_mut_value().size_lots = new_size;
        new_size
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
        self.header.bids_best_index = lookup_min_index::<RestingOrderV2>(self.data, new_root);
        self.header.total_orders_active =
            self.header.total_orders_active.saturating_sub(1);
        self.free_node(idx);
    }

    /// Mirror of `remove_bid_node` for the asks RBT.
    pub fn remove_ask_node(&mut self, idx: DataIndex) {
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
        self.header.asks_best_index = lookup_min_index::<RestingOrderV2>(self.data, new_root);
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

/// 80-byte payload for the FreeList — pure padding. Manifest's pattern.
#[zero_copy]
pub struct FreeListPadding {
    pub _padding_a: [u8; 32],
    pub _padding_b: [u8; 32],
    pub _padding_c: [u8; 16],
}
impl Get for FreeListPadding {}

const _: () = assert!(std::mem::size_of::<FreeListPadding>() == 80);
const _: () = assert!(std::mem::size_of::<FreeListNode<FreeListPadding>>() == 84);

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

    fn make_book() -> Vec<u8> {
        let mut data = vec![0u8; MARKET_BOOK_TOTAL_BYTES];
        MarketBookHandle::write_disc_and_init_header(
            &mut data,
            255,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        )
        .expect("init header");
        data
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
            _pad: 0,
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
    fn decrement_size_at_partial_fill() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let idx = handle.insert_bid(make_order(100, 1, true)).unwrap();
        assert_eq!(handle.order_at(idx).size_lots, 100);
        let new_size = handle.decrement_size_at(idx, 30);
        assert_eq!(new_size, 70);
        assert_eq!(handle.order_at(idx).size_lots, 70);
        // Saturating sub: delta > size lands at zero.
        let zeroed = handle.decrement_size_at(idx, 999);
        assert_eq!(zeroed, 0);
        assert_eq!(handle.order_at(idx).size_lots, 0);
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

    #[test]
    fn matcher_pipeline_consumes_crossed_orders() {
        // Wave 18f end-to-end: place 3 bids + 3 asks that cross, run the
        // FBA matcher, then mutate filled orders back into the hypertree.
        // Mirrors what `run_batch_v2` does in lib.rs.
        use crate::matcher::{
            fba::clear_batch,
            lot::{BaseLots, Ticks},
            order::{Order, OrderType, Side, StpMode},
        };

        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();

        // Bids at 100, 95, 90 (best = 100). Asks at 92, 96, 110.
        // Crossing region: 92..=100. Walrasian price will land somewhere
        // in that band (integer-tick). Each order gets a unique trader so
        // the matcher's self-trade prevention doesn't suppress the cross.
        let alice = Pubkey::new_unique();
        let bob = Pubkey::new_unique();
        let carol = Pubkey::new_unique();
        let dave = Pubkey::new_unique();
        let eve = Pubkey::new_unique();
        let frank = Pubkey::new_unique();
        let bid100 = handle.insert_bid(make_order_for(100, 1, true, alice)).unwrap();
        let bid95 = handle.insert_bid(make_order_for(95, 2, true, bob)).unwrap();
        let bid90 = handle.insert_bid(make_order_for(90, 3, true, carol)).unwrap();
        let ask92 = handle.insert_ask(make_order_for(92, 4, false, dave)).unwrap();
        let ask96 = handle.insert_ask(make_order_for(96, 5, false, eve)).unwrap();
        let _ask110 = handle.insert_ask(make_order_for(110, 6, false, frank)).unwrap();
        assert_eq!(handle.header.total_orders_active, 6);

        // Build matcher input by walking the book best-first.
        let mut orders: Vec<Order> = Vec::new();
        let mut sources: Vec<(u64, DataIndex, bool)> = Vec::new();
        handle.for_each_bid_best_first(|idx, o| {
            orders.push(Order {
                id: o.order_id,
                trader: o.trader,
                side: Side::Long,
                order_type: OrderType::Limit,
                size: BaseLots(o.size_lots),
                limit_price: Ticks(o.price_ticks),
                seq: o.seq,
                post_only: false,
                stp_mode: StpMode::CancelNewest,
            });
            sources.push((o.order_id, idx, true));
            true
        });
        handle.for_each_ask_best_first(|idx, o| {
            orders.push(Order {
                id: o.order_id,
                trader: o.trader,
                side: Side::Short,
                order_type: OrderType::Limit,
                size: BaseLots(o.size_lots),
                limit_price: Ticks(o.price_ticks),
                seq: o.seq,
                post_only: false,
                stp_mode: StpMode::CancelNewest,
            });
            sources.push((o.order_id, idx, false));
            true
        });

        // Clear the FBA — prior_mark = 95.
        let result = clear_batch(&orders, Ticks(95)).expect("clear_batch");
        assert!(result.clearing_volume.0 > 0, "expected at least one fill");
        assert!(!result.fills.is_empty(), "expected at least one Fill");

        // Apply fills back to the hypertree (mirror of run_batch_v2 phase 3).
        let mut consumed: Vec<(u64, DataIndex, bool, u64)> = Vec::new();
        for fill in &result.fills {
            for id in [fill.maker_id, fill.taker_id] {
                let found = sources.iter().find(|(oid, _, _)| *oid == id);
                let Some((sid, sidx, sib)) = found else { continue };
                if let Some(slot) = consumed.iter_mut().find(|(c, _, _, _)| *c == *sid)
                {
                    slot.3 = slot.3.saturating_add(fill.size.0);
                } else {
                    consumed.push((*sid, *sidx, *sib, fill.size.0));
                }
            }
        }
        for (_, idx, side_is_bid, total_filled) in &consumed {
            let new_size = handle.decrement_size_at(*idx, *total_filled);
            if new_size == 0 {
                if *side_is_bid {
                    handle.remove_bid_node(*idx);
                } else {
                    handle.remove_ask_node(*idx);
                }
            }
        }

        // Invariants: at minimum, the best bid (100) and best ask (92) crossed,
        // so each was consumed for `min(bid_size, ask_size) = 100` lots.
        // Both started at size 100 so both should be removed.
        assert_eq!(
            handle.lookup_bid_by_order_id(encode_order_id(100, 1, true)),
            NIL,
            "bid@100 must be fully filled and removed"
        );
        assert_eq!(
            handle.lookup_ask_by_order_id(encode_order_id(92, 4, false)),
            NIL,
            "ask@92 must be fully filled and removed"
        );
        // The non-crossing orders (bid@90, ask@110) must remain.
        assert_ne!(handle.lookup_bid_by_order_id(encode_order_id(90, 3, true)), NIL);
        assert_ne!(handle.lookup_ask_by_order_id(encode_order_id(110, 6, false)), NIL);
        // Total active = 6 - (consumed full fills). Minimum 2 orders fully
        // consumed (the crossing pair), so ≤ 4 remaining.
        assert!(
            handle.header.total_orders_active <= 4,
            "expected ≤4 orders remaining, got {}",
            handle.header.total_orders_active,
        );
        // Suppress unused-binding warnings (these exist to label the
        // initial book state in the test).
        let _ = (bid100, bid95, bid90, ask92, ask96);
    }
}
