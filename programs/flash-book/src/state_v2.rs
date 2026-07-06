//! V2 account types — the orderbook over a hypertree slab.
//!
//! The slab layout keeps every book operation off the 4 KB BPF stack
//! (a Borsh-deserialized book struct would not fit):
//!
//!   * The book account is never deserialized onto the stack — the
//!     handle parses a 256-byte header in place and treats the rest as
//!     a raw byte slab.
//!   * Bids, asks, and claimed seats live in 3 overlapping red-black
//!     trees inside that single byte array. A free list of 96-byte
//!     slots tracks evictable nodes.
//!   * Adding/cancelling an order is an O(log n) RBT op on an 80-byte
//!     payload — no per-slot scan, no flat array.
//!   * Account size is fixed at init (~50 orders/side + 50 seats);
//!     `expand_market_book` grows it in place up to the ceiling.

use anchor_lang::prelude::*;

use crate::hypertree::{
    get_helper, get_mut_helper, DataIndex, FreeList, FreeListNode, Get, HyperTreeReadOperations,
    HyperTreeWriteOperations, Payload, RBNode, RedBlackTree, RedBlackTreeReadOnly,
    RedBlackTreeReadOperationsHelpers, NIL,
};

/// Bytes available for hypertree nodes after the fixed header **at init**.
/// 100 nodes of 96 bytes each — ~50 orders per side + 50 claimed seats.
/// This is no longer the hard ceiling: `expand_market_book` grows the
/// account in place up to `MARKET_BOOK_MAX_DATA_BYTES`. The live capacity
/// is always `MarketBookHandle::data.len()`, NOT this constant.
pub const MARKET_BOOK_DATA_BYTES: usize = 9_600;

/// Each RBNode<V> = 16-byte RBT header + V (the payload). Payloads are
/// 80 bytes so each node is 96 bytes total: resting orders carry the
/// trader Pubkey inline, so cancel/modify can verify ownership without
/// an extra seat lookup.
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

    /// Reserved — 112 bytes for future fields without a layout break.
    /// Split into ≤32-byte chunks because bytemuck's classical
    /// `Pod for [T; N]` impl tops out at N=32.
    pub _reserved_a: [u8; 32],
    pub _reserved_b: [u8; 32],
    pub _reserved_c: [u8; 32],
    pub _reserved_d: [u8; 16],
}

const _: () = assert!(std::mem::size_of::<MarketBookHeader>() == 256);

// ─── RestingOrderV2 ──────────────────────────────────────────────────

/// 80-byte resting-order payload. Carries everything the matcher needs
/// to clear a fill PLUS the GTT expiry + an `order_id` whose encoding
/// makes a single ascending u64 ordering serve both bids and asks.
///
/// Implements `Ord` by `order_id` (which embeds price + seq + side). The
/// RBT uses this for sort.
///
/// `RestingOrderV2.flags` bit1 = REDUCE_ONLY. Set ONLY by the program
/// when a reduce-only trigger/bracket leg injects its close order (never
/// settable on a user place path — those reject bit1 at intake). The
/// matcher caps a crossed reduce-only maker's fill to the maker's
/// reducible position size so it can only close, never open/flip. See
/// `matcher::reduce_only::check_reduce_only`.
pub const FLAG_REDUCE_ONLY: u8 = 0b0000_0010;

#[zero_copy]
pub struct RestingOrderV2 {
    /// Side-encoded sort key: high bits price, low bits seq. For bids
    /// the price field is inverted so natural ascending sort puts the
    /// highest-price bids first (see `encode_order_id`).
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

    /// Trader pubkey (32 B), carried inline so cancel/modify verify
    /// ownership directly against the order node.
    pub trader: Pubkey,

    /// Anti-replay guard for off-chain replay tools. u32 saturates to
    /// u32::MAX (the unwrap_or path in call sites) once the validator's
    /// slot counter exceeds 2^32 ≈ 204 years on Solana mainnet. Replay
    /// tools that compare against this field must handle the saturation
    /// (treat `last_valid_slot == u32::MAX` as "always valid").
    pub last_valid_slot: u32,

    pub side: u8,       // 0 = long/buy/bid, 1 = short/sell/ask
    pub order_type: u8, // 0 = limit, 1 = ioc, 2 = post_only, 3 = jit
    /// Bitfield (authoritative layout — see lib.rs place_limit_order_v2 docs +
    /// the matcher reads): bit0 post_only, bit1 reduce_only (program-injected
    /// only — rejected on user place paths at intake), bit2 ioc, bit3 jit,
    /// bits 4-5 stp_mode, bit6 fok.
    pub flags: u8,
    /// Sub-account index this order belongs to.
    /// `0` (default) = main TraderState `[STATE_SEED, trader.as_ref()]`.
    /// `1..=255` = sub TraderState `[STATE_SEED, trader.as_ref(), &[sub_index]]`.
    /// Occupies a former `_pad` byte — nodes serialized before the field
    /// existed carry 0 here, which reads as the main-account default.
    /// ApplyFill / ApplyFlpFill use this to route fills + fees + PnL
    /// to the correct TraderState. Cancel / modify do NOT need to read
    /// it (those ixs verify the signer against `order.trader`, the
    /// wallet, regardless of sub_index).
    pub sub_index: u8,
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

/// Fail-loud ceiling on the FIFO `seq` before it is committed to the book.
/// `encode_order_id` masks `seq` to `ORDER_ID_SEQ_BITS` (24) bits; past
/// `MAX_SEQ_ENCODABLE` the masked low bits WRAP toward 0, so a fresh order
/// would get a SMALLER `order_id` than older orders at the same price
/// (price-time-priority violation) and could even collide on `order_id`
/// with a live order (mis-resolving cancel/modify and corrupting the
/// best-index cache). Every resting order — user, FLP, trigger, TWAP,
/// iceberg, basket — funnels through `insert_bid`/`insert_ask`, so
/// enforcing the bound there is the single complete chokepoint. Fail-loud
/// (reject the order) forces a market reseat before the counter wraps,
/// rather than silently corrupting the book.
/// Pure predicate: does `seq` fit the 24-bit `order_id` field without
/// aliasing? This is the SAME bound the `encode_order_id`
/// priority/collision Kani proofs `assume()`, so the runtime guard and the
/// FV precondition can never drift.
#[inline]
pub const fn seq_is_encodable(seq: u64) -> bool {
    seq <= MAX_SEQ_ENCODABLE
}

#[inline]
pub fn require_seq_encodable(seq: u64) -> Result<()> {
    require!(
        seq_is_encodable(seq),
        crate::errors::FlashBookError::OrderSeqExhausted
    );
    Ok(())
}

/// Compose the side-encoded `order_id` from (price_ticks, seq, side).
///
/// For BIDS only the price field is inverted — not the whole word — so a
/// single ascending `order_id` walk yields correct price-TIME priority on
/// both books:
///   * higher price => better => smaller key   (price inverted for bids)
///   * older seq     => better => smaller key   (seq ascending for BOTH sides)
///
/// Inverting the entire word would also invert the seq tiebreak and make
/// bids fill LIFO at each price level (a price-time-priority violation).
///
/// Price is **saturated** to `MAX_PRICE_TICKS_ENCODABLE` — a clamp keeps
/// the ordering monotonic, where masking would wrap an out-of-range price
/// to a tiny key and mis-order the book. Seq is masked to 24 bits;
/// placement enforces the 24-bit ceiling so masking can never alias a live id.
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

// ─────────────────────────────────────────────────────────────────────
// FV: machine-checked price-TIME priority of the `order_id` encoding (Kani).
// The hypertree walks orders ascending by `order_id` ("best first"), so the
// encoding alone determines matching priority. These prove — multiply-free, so
// CBMC is fast — that a single ascending walk yields correct price-time priority
// on BOTH books, and in particular that the seq tiebreak is FIFO for bids (the
// exact property the old whole-word-inversion bug violated; see encode_order_id).
// Inputs are assumed within the encodable range (price ≤ MAX_PRICE_TICKS_ENCODABLE,
// seq ≤ MAX_SEQ_ENCODABLE), which placement enforces fail-loud.
// ─────────────────────────────────────────────────────────────────────
#[cfg(kani)]
mod order_id_priority_kani_proofs {
    use super::{encode_order_id, seq_is_encodable, MAX_PRICE_TICKS_ENCODABLE, MAX_SEQ_ENCODABLE};

    /// H1: the insert-time guard `seq_is_encodable` admits EXACTLY the seqs every
    /// proof in this module `assume()`s — so runtime enforcement and the FV
    /// precondition are provably the same bound, not two constants that can drift.
    #[kani::proof]
    fn seq_guard_matches_encoding_precondition() {
        let seq: u64 = kani::any();
        assert_eq!(seq_is_encodable(seq), seq <= MAX_SEQ_ENCODABLE);
    }

    /// H1: composing the guard with the encoding — any two DISTINCT orders the
    /// guard admits never collide on `order_id` (the book key stays injective).
    /// This restates `distinct_orders_never_collide` through the ACTUAL runtime
    /// predicate (`seq_is_encodable`) instead of a free `assume`, closing the loop
    /// between what `insert_bid`/`insert_ask` enforce and what the proofs need.
    #[kani::proof]
    fn guard_admitted_orders_never_collide() {
        let side: bool = kani::any();
        let p1: u64 = kani::any();
        let p2: u64 = kani::any();
        let s1: u64 = kani::any();
        let s2: u64 = kani::any();
        kani::assume(p1 <= MAX_PRICE_TICKS_ENCODABLE && p2 <= MAX_PRICE_TICKS_ENCODABLE);
        kani::assume(seq_is_encodable(s1) && seq_is_encodable(s2));
        kani::assume(p1 != p2 || s1 != s2);
        assert!(encode_order_id(p1, s1, side) != encode_order_id(p2, s2, side));
    }

    /// ASK price priority: a LOWER-priced ask has a smaller `order_id`, so it
    /// fills first — regardless of either order's seq (price dominates the key).
    #[kani::proof]
    fn ask_lower_price_fills_first() {
        let p1: u64 = kani::any();
        let p2: u64 = kani::any();
        let s1: u64 = kani::any();
        let s2: u64 = kani::any();
        kani::assume(p1 <= MAX_PRICE_TICKS_ENCODABLE && p2 <= MAX_PRICE_TICKS_ENCODABLE);
        kani::assume(s1 <= MAX_SEQ_ENCODABLE && s2 <= MAX_SEQ_ENCODABLE);
        kani::assume(p1 < p2);
        assert!(encode_order_id(p1, s1, false) < encode_order_id(p2, s2, false));
    }

    /// BID price priority: a HIGHER-priced bid has a smaller `order_id` (the price
    /// field is inverted for bids), so the best bid fills first — regardless of seq.
    #[kani::proof]
    fn bid_higher_price_fills_first() {
        let p1: u64 = kani::any();
        let p2: u64 = kani::any();
        let s1: u64 = kani::any();
        let s2: u64 = kani::any();
        kani::assume(p1 <= MAX_PRICE_TICKS_ENCODABLE && p2 <= MAX_PRICE_TICKS_ENCODABLE);
        kani::assume(s1 <= MAX_SEQ_ENCODABLE && s2 <= MAX_SEQ_ENCODABLE);
        kani::assume(p1 > p2);
        assert!(encode_order_id(p1, s1, true) < encode_order_id(p2, s2, true));
    }

    /// TIME priority within a price level (FIFO), for BOTH sides: at the same
    /// price, the EARLIER seq has a smaller `order_id` and fills first. (Bids must
    /// NOT invert the seq — the old bug made bids LIFO; this rules it out.)
    #[kani::proof]
    fn earlier_seq_fills_first_at_same_price() {
        let side: bool = kani::any();
        let p: u64 = kani::any();
        let s1: u64 = kani::any();
        let s2: u64 = kani::any();
        kani::assume(p <= MAX_PRICE_TICKS_ENCODABLE);
        kani::assume(s1 <= MAX_SEQ_ENCODABLE && s2 <= MAX_SEQ_ENCODABLE);
        kani::assume(s1 < s2);
        assert!(encode_order_id(p, s1, side) < encode_order_id(p, s2, side));
    }

    /// No collision: two orders with distinct (price, seq) within range never
    /// encode to the same `order_id` (so the RBT key is injective on live orders).
    #[kani::proof]
    fn distinct_orders_never_collide() {
        let side: bool = kani::any();
        let p1: u64 = kani::any();
        let p2: u64 = kani::any();
        let s1: u64 = kani::any();
        let s2: u64 = kani::any();
        kani::assume(p1 <= MAX_PRICE_TICKS_ENCODABLE && p2 <= MAX_PRICE_TICKS_ENCODABLE);
        kani::assume(s1 <= MAX_SEQ_ENCODABLE && s2 <= MAX_SEQ_ENCODABLE);
        kani::assume(p1 != p2 || s1 != s2);
        assert!(encode_order_id(p1, s1, side) != encode_order_id(p2, s2, side));
    }
}

// ─── ClaimedSeatV2 ───────────────────────────────────────────────────

/// 80-byte per-(market, trader) seat. Holds the trader pubkey + open
/// order count + free-funds balances. On first trade in a market the
/// trader claims a seat (one-time rent ≈ $0.0005). Subsequent trades
/// settle into `quote_free_lots` — no SPL token CPI on every fill.
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
    fn eq(&self, other: &Self) -> bool {
        self.trader == other.trader
    }
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
        write!(
            f,
            "Seat {{ trader={} open={} }}",
            self.trader, self.open_orders_count
        )
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
// The book account therefore bypasses Anchor account types entirely:
// it is an `UncheckedAccount` (owner-checked at the context level)
// accessed through `MarketBookHandle<'a>` — a newtype over `&mut [u8]`
// that parses the first 256 bytes as `MarketBookHeader` via
// `bytemuck::from_bytes` and exposes the rest as the dynamic byte
// array for the hypertree's `FreeList`/`RedBlackTree` ops.

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

/// Fixed prefix every MarketBookAccount carries before the node array:
/// 8-byte discriminator + 256-byte header.
pub const MARKET_BOOK_DISC_BYTES: usize = 8;
pub const MARKET_BOOK_HEADER_BYTES: usize = 256;
pub const MARKET_BOOK_PREFIX_BYTES: usize = MARKET_BOOK_DISC_BYTES + MARKET_BOOK_HEADER_BYTES;

/// Total byte size of a freshly-initialised MarketBookAccount
/// (disc + header + initial data). = 8 + 256 + 9600 = 9864 bytes.
pub const MARKET_BOOK_TOTAL_BYTES: usize = MARKET_BOOK_PREFIX_BYTES + MARKET_BOOK_DATA_BYTES;

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
            crate::errors::FlashBookError::OutOfRange
        );
        require!(
            data[..8] == MARKET_BOOK_DISC,
            crate::errors::FlashBookError::WrongTrader,
        );
        let (header_bytes, dyn_data) = data[8..].split_at_mut(256);
        let header: &mut MarketBookHeader = bytemuck::from_bytes_mut(header_bytes);
        // Bounds-check the header's node indices against the slab capacity.
        // A structurally-invalid book — e.g. one committed by a
        // malicious or buggy ER sequencer across an undelegate, or any tampered
        // account — could otherwise drive a raw slab accessor out of bounds
        // (panic/DoS) or misread an in-bounds-but-bogus node. `NIL` is the valid
        // empty sentinel; a well-formed book's roots/best/free-list are always
        // slab node ids < capacity, so this NEVER rejects a valid book — it fails
        // closed only on corruption.
        // `DataIndex` is a BYTE OFFSET into the slab (`get_helper` indexes
        // `data[idx..idx+size]`), so a valid node index is `NIL` or a node-aligned
        // offset whose whole node fits the slab.
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
                idx == NIL
                    || (off % NODE_TOTAL_BYTES == 0
                        && off
                            .checked_add(NODE_TOTAL_BYTES)
                            .is_some_and(|end| end <= slab_len)),
                crate::errors::FlashBookError::OutOfRange
            );
        }
        // The bump allocator returns `num_bytes_allocated` as the next
        // fresh node offset. If a malicious-/buggy-ER commit leaves it
        // non-node-aligned or past the slab, the next `alloc_node` yields a
        // misaligned slice (bytemuck alignment panic → placement brick) or an
        // in-bounds offset that overlaps a live node (silent corruption). Validate
        // it here on the per-op hot gate so a tampered bump pointer fails closed.
        require!(
            header.num_bytes_allocated as usize % NODE_TOTAL_BYTES == 0
                && header.num_bytes_allocated as usize <= slab_len,
            crate::errors::FlashBookError::OutOfRange
        );
        // NOTE: internal RBT node-links (left/right/parent) are NOT walked here — that
        // would add O(capacity) CU to EVERY book op on the hot path. Instead they are
        // validated ONCE, when a book re-enters L1 via `process_undelegation` (the
        // only point a malicious-ER-committed book can arrive), by
        // `MarketBookHandle::validate_node_links` below. Zero hot-path cost.
        Ok(MarketBookHandle {
            header,
            data: dyn_data,
        })
    }

    /// Validate every node's INTERNAL RBT links
    /// (`left`/`right`/`parent`), not just the 6 header roots `from_account_data`
    /// checks. The hot traversal accessors use the unchecked `get_helper`, so a book
    /// carrying an out-of-range child pointer would panic-DoS the FIRST L1 traversal
    /// and brick the market (no reset ix). A well-formed book can only be produced by
    /// this program's own (Kani-proven) RBT writes, so the ONLY way a corrupt book can
    /// reach L1 is a malicious-/buggy-ER commit that is then undelegated — therefore
    /// this is called EXACTLY ONCE, from `process_undelegation` (the single choke
    /// point a returning book passes through), NOT on the per-op `from_account_data`
    /// hot path. Cost: O(capacity) once per undelegate (a rare op); ZERO added CU on
    /// place/cancel/match. A corrupt book fails CLOSED here (`OutOfRange` → the
    /// undelegate reverts, the book never lands corrupt on L1) instead of panicking
    /// later — with no change to the proven RBT internals.
    ///
    /// `#[repr(C)] RBNode<V>` lays out `left`/`right`/`parent` as its first three
    /// `DataIndex` (= u32 LE) fields, so they sit at byte offsets 0/4/8 of every
    /// `NODE_TOTAL_BYTES` slot regardless of the payload `V`. Pinned by compile-time
    /// asserts so a field reorder fails the build, not silently validates wrong bytes.
    pub fn validate_node_links(account_data: &[u8]) -> Result<()> {
        const _: () = assert!(core::mem::offset_of!(RBNode<RestingOrderV2>, left) == 0);
        const _: () = assert!(core::mem::offset_of!(RBNode<RestingOrderV2>, right) == 4);
        const _: () = assert!(core::mem::offset_of!(RBNode<RestingOrderV2>, parent) == 8);
        require!(
            account_data.len() >= MARKET_BOOK_PREFIX_BYTES,
            crate::errors::FlashBookError::OutOfRange
        );
        let slab = &account_data[MARKET_BOOK_PREFIX_BYTES..];
        let slab_len = slab.len();
        require!(
            slab_len % NODE_TOTAL_BYTES == 0,
            crate::errors::FlashBookError::OutOfRange
        );
        let node_count = slab_len / NODE_TOTAL_BYTES;
        for i in 0..node_count {
            let base = i * NODE_TOTAL_BYTES;
            for link_off in [0usize, 4, 8] {
                let mut b = [0u8; 4];
                b.copy_from_slice(&slab[base + link_off..base + link_off + 4]);
                let off = u32::from_le_bytes(b) as usize;
                require!(
                    off == NIL as usize
                        || (off % NODE_TOTAL_BYTES == 0
                            && off
                                .checked_add(NODE_TOTAL_BYTES)
                                .is_some_and(|end| end <= slab_len)),
                    crate::errors::FlashBookError::OutOfRange
                );
            }
            // `RBNode.color` (byte offset 12) is a `#[repr(u8)]`
            // enum with only 0=Black / 1=Red valid; reading any other discriminant as
            // the enum is UB. `unsafe impl Pod` bypasses bytemuck's variant check, so
            // validate the byte here. Free/unused slots are zeroed (0=Black), so a
            // well-formed book always passes; only a tampered color byte fails closed.
            require!(
                slab[base + 12] <= 1,
                crate::errors::FlashBookError::OutOfRange
            );
        }

        // The per-link bounds check above stops an OOB panic but NOT an
        // infinite loop: a malicious-/buggy-ER commit can plant a book whose
        // links are all in-bounds yet form a CYCLE (e.g. A.right=B and
        // B.right=A), and the L1 traversals (lookup_max / get_next_higher)
        // are unbounded `while` loops — a cycle would spin to compute
        // exhaustion and brick the market (there is no reset ix). The guard
        // is a bounded-reachability walk: DFS each tree
        // from its root with a SHARED visited bitmap, rejecting a revisit (cycle
        // or cross-tree aliasing) or a child whose `parent` link does not point
        // back (broken symmetry). Verifying child→parent symmetry along an
        // acyclic DFS also makes every parent-chain (up-walk) acyclic. Runs ONCE
        // per undelegate, off the hot path.
        let header: &MarketBookHeader = bytemuck::from_bytes(
            &account_data
                [MARKET_BOOK_DISC_BYTES..MARKET_BOOK_DISC_BYTES + MARKET_BOOK_HEADER_BYTES],
        );
        let read_link = |off: usize, link_off: usize| -> u32 {
            let mut b = [0u8; 4];
            b.copy_from_slice(&slab[off + link_off..off + link_off + 4]);
            u32::from_le_bytes(b)
        };
        let mut visited = vec![false; node_count];
        let mut stack: Vec<usize> = Vec::new();
        for root in [
            header.bids_root_index,
            header.asks_root_index,
            header.claimed_seats_root_index,
        ] {
            if root == NIL {
                continue;
            }
            // The root index comes straight from the untrusted header and is read
            // via the unchecked `read_link` on the very next line, and again as a
            // slab offset by the DFS. `from_account_data` bounds- and
            // alignment-checks the header roots before use; this gate must mirror
            // that BEFORE its first deref, or a committed root of `9600`
            // (node-count boundary) or `NIL − 1` slices out of range (panic, not
            // the clean reject this gate exists to guarantee), and a misaligned
            // in-bounds root is accepted here yet rejected by every later
            // `from_account_data` — bricking the market. A well-formed root is
            // always node-aligned and wholly in-slab, so this never rejects a
            // valid book.
            let root_off = root as usize;
            require!(
                root_off % NODE_TOTAL_BYTES == 0
                    && root_off
                        .checked_add(NODE_TOTAL_BYTES)
                        .is_some_and(|end| end <= slab_len),
                crate::errors::FlashBookError::OutOfRange
            );
            // A tree root has no parent. The child→parent symmetry check
            // below validates every non-root node's parent link, but never the
            // root's own. `successor_index`'s up-walk terminates ONLY on
            // `parent == NIL`, so a root whose parent points back into the tree
            // makes the max-node up-walk an unbounded cycle → permanent brick.
            // Pin every live root's parent to NIL.
            require!(
                read_link(root as usize, 8) == NIL,
                crate::errors::FlashBookError::OutOfRange
            );
            stack.push(root as usize);
            let mut steps = 0usize;
            while let Some(off) = stack.pop() {
                steps += 1;
                require!(
                    steps <= node_count,
                    crate::errors::FlashBookError::OutOfRange
                );
                let ord = off / NODE_TOTAL_BYTES;
                require!(
                    ord < node_count && !visited[ord],
                    crate::errors::FlashBookError::OutOfRange
                );
                visited[ord] = true;
                // left (offset 0) and right (offset 4) children.
                for child_link_off in [0usize, 4] {
                    let child = read_link(off, child_link_off);
                    if child != NIL {
                        // `child` was validated in-bounds/aligned by the loop
                        // above; its parent (offset 8) must point back here.
                        require!(
                            read_link(child as usize, 8) == off as u32,
                            crate::errors::FlashBookError::OutOfRange
                        );
                        stack.push(child as usize);
                    }
                }
            }
        }

        // `for_each_best_first` starts its ascending walk at the cached best
        // pointer, so the best MUST be the tree minimum (the leftmost descendant
        // of the root): a best that points elsewhere makes matching skip better
        // liquidity, misreads top-of-book, or — if detached from the tree —
        // spins the successor walk forever (brick). An empty tree (root == NIL)
        // has no best. Requiring best == leftmost also pins it in-bounds and
        // root-reachable, so this subsumes the visited check. Every validly
        // matching book already satisfies this, so no well-formed book is
        // rejected.
        for (best, root) in [
            (header.bids_best_index, header.bids_root_index),
            (header.asks_best_index, header.asks_root_index),
        ] {
            if root == NIL {
                require!(best == NIL, crate::errors::FlashBookError::OutOfRange);
            } else {
                // Walk left from the root to the minimum. Links were bounds-checked
                // above; the step bound is defensive (the DFS already proved the
                // tree acyclic, so the left-chain terminates).
                let mut cur = root as usize;
                let mut steps = 0usize;
                loop {
                    steps += 1;
                    require!(
                        steps <= node_count,
                        crate::errors::FlashBookError::OutOfRange
                    );
                    let left = read_link(cur, 0);
                    if left == NIL {
                        break;
                    }
                    cur = left as usize;
                }
                require!(
                    best as usize == cur,
                    crate::errors::FlashBookError::OutOfRange
                );
            }
        }

        // The free list is not walked by the tree DFS. A commit
        // can plant a free list that (a) cycles or (b) aliases a live tree node, so a
        // later `alloc_node` pops a slot that is still linked in a tree → one physical
        // slot in two logical positions (use-after-free / type confusion / eventual
        // brick). Walk the free list from its head with the SHARED `visited` bitmap:
        // any slot already marked (tree-live, or a free-list revisit = cycle) fails
        // closed, and the walk is step-bounded. This proves the tree-live and free
        // sets are disjoint and the free list is acyclic. `FreeListNode.next_index`
        // sits at byte offset 0 (same slot the DFS read as `left`).
        {
            let mut free = header.free_list_head_index;
            let mut steps = 0usize;
            while free != NIL {
                steps += 1;
                require!(
                    steps <= node_count,
                    crate::errors::FlashBookError::OutOfRange
                );
                let off = free as usize;
                require!(
                    off % NODE_TOTAL_BYTES == 0
                        && off
                            .checked_add(NODE_TOTAL_BYTES)
                            .is_some_and(|end| end <= slab_len),
                    crate::errors::FlashBookError::OutOfRange
                );
                let ord = off / NODE_TOTAL_BYTES;
                require!(!visited[ord], crate::errors::FlashBookError::OutOfRange);
                visited[ord] = true;
                free = read_link(off, 0);
            }
        }

        // A bump allocation hands out the slot at `num_bytes_allocated`, so every
        // LIVE node (tree or free-list — all marked in `visited`) must lie
        // strictly below it; otherwise the next `alloc_node` returns a slot that
        // overlaps a live node (aliasing / type confusion / balance corruption).
        // The alignment/`<= slab_len` checks never tie the bump pointer to the
        // live set, so a commit could plant a live node above it with an empty
        // free list. Require the bump pointer to cover every live node.
        let live_end = visited
            .iter()
            .rposition(|&v| v)
            .map_or(0, |max_ord| (max_ord + 1) * NODE_TOTAL_BYTES);
        require!(
            live_end <= header.num_bytes_allocated as usize,
            crate::errors::FlashBookError::OutOfRange
        );

        // Fail closed at undelegation on a corrupt bump pointer
        // too (from_account_data validates it on every subsequent op, but reverting
        // the undelegate keeps the corrupt book off L1 in the first place).
        require!(
            header.num_bytes_allocated as usize % NODE_TOTAL_BYTES == 0
                && header.num_bytes_allocated as usize <= slab_len,
            crate::errors::FlashBookError::OutOfRange
        );

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
            let mut fl =
                FreeList::<FreeListPadding>::new(self.data, self.header.free_list_head_index);
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
            crate::errors::FlashBookError::BufferFull
        );
        self.header.num_bytes_allocated = end;
        Ok(next_offset)
    }

    /// Return a node to the free-list. Caller is responsible for having
    /// removed the node from any tree it lived in first.
    pub fn free_node(&mut self, idx: DataIndex) {
        let mut fl = FreeList::<FreeListPadding>::new(self.data, self.header.free_list_head_index);
        fl.add(idx);
        self.header.free_list_head_index = fl.get_head();
    }

    /// Insert a `RestingOrderV2` into the bids RBT. Allocates a node
    /// first, writes the payload, then inserts into the tree. Updates
    /// the bid root + best (= MIN-of-tree, which is the highest-priced
    /// bid given our inverted encoding) indices on the header.
    pub fn insert_bid(&mut self, order: RestingOrderV2) -> Result<DataIndex> {
        require_seq_encodable(order.seq)?;
        let idx = self.alloc_node()?;
        // O(1) cache update: capture order_id now, compare against cached
        // best AFTER tree insertion. Both sides encode best = MIN-by-order_id.
        let new_order_id = order.order_id;
        let new_root;
        {
            let mut tree =
                RedBlackTree::<RestingOrderV2>::new(self.data, self.header.bids_root_index, NIL);
            tree.insert(idx, order);
            new_root = tree.get_root_index();
        }
        self.header.bids_root_index = new_root;
        // O(1) best update: new is best iff tree was empty OR new order_id
        // < current best's order_id. No leftmost-walk required.
        let cur_best = self.header.bids_best_index;
        if cur_best == NIL || new_order_id < self.order_at(cur_best).order_id {
            self.header.bids_best_index = idx;
        }
        self.header.total_orders_active = self.header.total_orders_active.saturating_add(1);
        Ok(idx)
    }

    /// Insert a `RestingOrderV2` into the asks RBT. Mirror of `insert_bid`.
    /// "Best" = MIN of tree = lowest-priced ask (asks are NOT inverted, so
    /// natural ascending order — smallest order_id is the best ask).
    pub fn insert_ask(&mut self, order: RestingOrderV2) -> Result<DataIndex> {
        require_seq_encodable(order.seq)?;
        let idx = self.alloc_node()?;
        let new_order_id = order.order_id;
        let new_root;
        {
            let mut tree =
                RedBlackTree::<RestingOrderV2>::new(self.data, self.header.asks_root_index, NIL);
            tree.insert(idx, order);
            new_root = tree.get_root_index();
        }
        self.header.asks_root_index = new_root;
        let cur_best = self.header.asks_best_index;
        if cur_best == NIL || new_order_id < self.order_at(cur_best).order_id {
            self.header.asks_best_index = idx;
        }
        self.header.total_orders_active = self.header.total_orders_active.saturating_add(1);
        Ok(idx)
    }

    /// Walk the bids RBT in BEST → WORST order (highest price first). Calls
    /// `f(idx, order)` for each resting bid. Stops early if `f` returns
    /// `false`. Read-only; safe to call from a view ix.
    ///
    /// The matcher walks the tree via this same helper to consume
    /// liquidity in price-time priority.
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
            .ok_or_else(|| error!(crate::errors::FlashBookError::ArithmeticOverflow))?;
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
            let mut tree =
                RedBlackTree::<RestingOrderV2>::new(self.data, self.header.bids_root_index, NIL);
            tree.remove_by_index(idx);
            new_root = tree.get_root_index();
        }
        self.header.bids_root_index = new_root;
        self.header.bids_best_index = new_best;
        self.header.total_orders_active = self.header.total_orders_active.saturating_sub(1);
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
            let mut tree =
                RedBlackTree::<RestingOrderV2>::new(self.data, self.header.asks_root_index, NIL);
            tree.remove_by_index(idx);
            new_root = tree.get_root_index();
        }
        self.header.asks_root_index = new_root;
        self.header.asks_best_index = new_best;
        self.header.total_orders_active = self.header.total_orders_active.saturating_sub(1);
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
}

/// 92-byte payload for the FreeList — pure padding. Sized so that
/// `FreeListNode<FreeListPadding>` (next_index 4B + payload 92B) is exactly
/// `NODE_TOTAL_BYTES` (96), which means `FreeList::add` scrubs the **entire**
/// freed slab slot: a shorter payload would leave stale RBNode bytes
/// (parent/color/high value bytes) in freed slots — a latent
/// dangling-index footgun.
#[zero_copy]
pub struct FreeListPadding {
    pub _padding_a: [u8; 32],
    pub _padding_b: [u8; 32],
    pub _padding_c: [u8; 28],
}
impl Get for FreeListPadding {}

const _: () = assert!(std::mem::size_of::<FreeListPadding>() == 92);
// Must equal NODE_TOTAL_BYTES so a freed slot is fully zeroed by `add`.
const _: () = assert!(std::mem::size_of::<FreeListNode<FreeListPadding>>() == NODE_TOTAL_BYTES);

/// FreeList sentinel: `u32::MAX` means "no more free nodes". When
/// `FreeList::remove()` returns this, the bump-alloc fallback kicks in.
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

/// In-order successor in an RBT view. General-purpose (handles both the
/// right-child case and the walk-up-while-right-child case). The vendored
/// `get_next_higher_index` only handles the first case (debug-asserts a
/// right child exists) because remove never calls it on leaves.
fn successor_index<V: Payload>(tree: &RedBlackTreeReadOnly<V>, idx: DataIndex) -> DataIndex {
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
fn for_each_best_first<V, F>(data: &[u8], root: DataIndex, cached_min: DataIndex, f: &mut F)
where
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

    fn make_order_for(price: u64, seq: u64, side_is_bid: bool, trader: Pubkey) -> RestingOrderV2 {
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

    // Validates the cumulative reduce-only capacity scan + clamp used by
    // `execute_trigger_order_v3` — the exact `for_each_ask_best_first`
    // predicate (reduce-only flag + same trader + same sub_index) and the
    // `min(req, position − Σexisting)` math — on a REAL MarketBookHandle.
    // Proves the scan sums only THIS position's resting reduce-only orders
    // (not other traders, other sub_indices, or non-reduce-only orders),
    // preserves scale-out (partials summing ≤ position all fit), and trims
    // genuine over-capacity to 0 (no setup can flip a position across the
    // match→settle gap).
    fn insert_ro_ask(
        handle: &mut MarketBookHandle,
        seq: u64,
        trader: Pubkey,
        sub: u8,
        size: u64,
        reduce_only: bool,
    ) {
        let mut o = make_order_for(100_000 + seq * 1_000, seq, false /*ask*/, trader);
        o.size_lots = size;
        o.sub_index = sub;
        o.flags = if reduce_only { FLAG_REDUCE_ONLY } else { 0 };
        handle.insert_ask(o).unwrap();
    }

    fn scan_ro(handle: &MarketBookHandle, trader: Pubkey, sub: u8) -> u64 {
        let mut existing = 0u64;
        handle.for_each_ask_best_first(|_i, o: &RestingOrderV2| {
            if o.flags & FLAG_REDUCE_ONLY != 0 && o.trader == trader && o.sub_index == sub {
                existing = existing.saturating_add(o.size_lots);
            }
            true
        });
        existing
    }

    fn clamp(position: u64, existing: u64, requested: u64) -> u64 {
        requested.min(position.saturating_sub(existing))
    }

    #[test]
    fn reduce_only_capacity_clamp_scan() {
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();
        let trader = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let position = 100u64;

        // Empty book ⇒ full capacity.
        assert_eq!(scan_ro(&handle, trader, 0), 0);
        assert_eq!(clamp(position, 0, 100), 100);

        // Scale-out is preserved: first 50-lot exit fits, second 50 still fits (Σ=100).
        insert_ro_ask(&mut handle, 1, trader, 0, 50, true);
        assert_eq!(scan_ro(&handle, trader, 0), 50);
        assert_eq!(
            clamp(position, 50, 50),
            50,
            "scale-out second partial exit fits"
        );

        insert_ro_ask(&mut handle, 2, trader, 0, 50, true);
        assert_eq!(scan_ro(&handle, trader, 0), 100);
        // Over-capacity: a THIRD reduce-only exit is trimmed to 0 — this is exactly
        // what stops two orders from summing past the position and flipping it.
        assert_eq!(
            clamp(position, 100, 50),
            0,
            "over-capacity reduce-only trimmed to 0"
        );

        // Must NOT count: a different trader, a different sub_index, or a
        // non-reduce-only order — otherwise the clamp would wrongly block legit orders.
        insert_ro_ask(&mut handle, 3, other, 0, 100, true); // other trader
        insert_ro_ask(&mut handle, 4, trader, 1, 100, true); // other position (sub 1)
        insert_ro_ask(&mut handle, 5, trader, 0, 100, false); // NOT reduce-only
        assert_eq!(
            scan_ro(&handle, trader, 0),
            100,
            "only this (trader, sub, reduce-only) position's orders are summed"
        );
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
    fn insert_rejects_seq_beyond_encoding_ceiling() {
        // A seq past the 24-bit encoding ceiling would wrap
        // the low bits of order_id (price-time-priority break + id collision).
        // Both insert paths must fail loud, and a seq exactly at the ceiling
        // must still be accepted (boundary).
        let mut data = make_book();
        let mut handle = MarketBookHandle::from_account_data(&mut data).unwrap();

        assert!(handle
            .insert_bid(make_order(100, MAX_SEQ_ENCODABLE, true))
            .is_ok());
        assert!(handle
            .insert_ask(make_order(100, MAX_SEQ_ENCODABLE, false))
            .is_ok());
        assert!(handle
            .insert_bid(make_order(100, MAX_SEQ_ENCODABLE + 1, true))
            .is_err());
        assert!(handle.insert_ask(make_order(100, u64::MAX, false)).is_err());
        // Only the two in-range orders ever joined the book.
        assert_eq!(handle.header.total_orders_active, 2);
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
        assert_eq!(
            handle.order_at(best).price_ticks,
            101,
            "highest bid is best"
        );
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

    // from_account_data must reject a structurally-corrupt
    // header node index (OOB or misaligned) — never feed it to a raw slab
    // accessor — while still accepting a well-formed book.
    #[test]
    fn from_account_data_rejects_corrupt_node_index() {
        // out-of-bounds byte offset
        let mut data = make_book();
        {
            let h = MarketBookHandle::from_account_data(&mut data).unwrap();
            h.header.bids_root_index = 9_999_999;
        }
        assert!(
            MarketBookHandle::from_account_data(&mut data).is_err(),
            "OOB node index must be rejected"
        );
        // in-bounds but not node-aligned
        let mut data = make_book();
        {
            let h = MarketBookHandle::from_account_data(&mut data).unwrap();
            h.header.asks_best_index = 1;
        }
        assert!(
            MarketBookHandle::from_account_data(&mut data).is_err(),
            "misaligned node index must be rejected"
        );
        // a well-formed (empty, all-NIL) book still loads
        let mut data = make_book();
        assert!(
            MarketBookHandle::from_account_data(&mut data).is_ok(),
            "valid book must still load"
        );
    }

    // A corrupt INTERNAL node link (left/right/parent) must also be rejected,
    // not just the header roots — a malicious-ER-committed book with an OOB
    // child pointer would otherwise panic the first L1 traversal (book DoS).
    // `#[repr(C)] RBNode` puts left/right/parent at byte offsets 0/4/8 of
    // node 0 (the first slab slot).
    // `validate_node_links` is the once-per-undelegate corruption gate
    // (called from `process_undelegation`, NOT the per-op hot path). It must reject an
    // out-of-range or misaligned internal RBT link and pass a well-formed book.
    #[test]
    fn validate_node_links_rejects_corrupt_internal_link() {
        let p = MARKET_BOOK_PREFIX_BYTES; // node 0 starts here; its `left` is at +0
                                          // OOB internal left-link
        let mut data = make_book();
        data[p..p + 4].copy_from_slice(&9_999_999u32.to_le_bytes());
        assert!(
            MarketBookHandle::validate_node_links(&data).is_err(),
            "OOB internal left-link must be rejected"
        );
        // in-bounds but misaligned internal parent-link (node 0, offset +8)
        let mut data = make_book();
        data[p + 8..p + 12].copy_from_slice(&1u32.to_le_bytes());
        assert!(
            MarketBookHandle::validate_node_links(&data).is_err(),
            "misaligned internal parent-link must be rejected"
        );
        // a valid book (free-list links are all node-aligned or NIL) passes
        let data = make_book();
        assert!(MarketBookHandle::validate_node_links(&data).is_ok());
        // and the per-op hot path still loads it (no link walk there now)
        let mut data = make_book();
        assert!(MarketBookHandle::from_account_data(&mut data).is_ok());
    }

    /// A malicious-/buggy-ER commit can plant a book whose links are all
    /// IN-BOUNDS yet form a CYCLE; per-link bounds checks alone accept it and
    /// the first L1 traversal would infinite-loop (compute-exhaustion →
    /// market brick). Build a 2-node cycle rooted at
    /// node0 (node0.right=node1, node1.right=node0, parents point back so the
    /// symmetry check passes) and assert the bounded-reachability walk rejects.
    #[test]
    fn validate_node_links_rejects_cyclic_book() {
        // RBT indices are SLAB-relative byte offsets: node 0 = 0, node 1 = 96.
        let n0: u32 = 0;
        let n1: u32 = NODE_TOTAL_BYTES as u32;

        let mut data = make_book();
        {
            let h = MarketBookHandle::from_account_data(&mut data).unwrap();
            h.header.bids_root_index = n0; // root = node 0
        }
        // Byte writes are at ACCOUNT offset = PREFIX + slab_offset + link_off.
        let write = |data: &mut [u8], slab_off: u32, link_off: usize, val: u32| {
            let base = MARKET_BOOK_PREFIX_BYTES + slab_off as usize + link_off;
            data[base..base + 4].copy_from_slice(&val.to_le_bytes());
        };
        // node0: left=NIL, right=n1, parent=n1
        write(&mut data, n0, 0, NIL);
        write(&mut data, n0, 4, n1);
        write(&mut data, n0, 8, n1);
        // node1: left=NIL, right=n0, parent=n0  → 2-cycle, symmetry satisfied
        write(&mut data, n1, 0, NIL);
        write(&mut data, n1, 4, n0);
        write(&mut data, n1, 8, n0);

        assert!(
            MarketBookHandle::validate_node_links(&data).is_err(),
            "cyclic (but in-bounds) book must be rejected by bounded reachability"
        );
    }

    /// A tree root has no parent, but the child→parent symmetry walk never
    /// inspects the root's own parent link. A commit can point `root.parent`
    /// back into the tree; `successor_index`'s up-walk terminates only on
    /// `parent == NIL`, so the max-node up-walk then cycles forever → brick.
    /// The validator must pin every live root's parent to NIL.
    #[test]
    fn validate_node_links_rejects_nonnil_root_parent() {
        let mut data = make_book();
        let (root_off, other_off) = {
            let mut h = MarketBookHandle::from_account_data(&mut data).unwrap();
            let a = h.insert_bid(make_order(100, 1, true)).unwrap();
            let b = h.insert_bid(make_order(200, 2, true)).unwrap();
            let root = h.header.bids_root_index;
            let other = if root == a { b } else { a };
            (root, other)
        };
        // The well-formed book (root.parent == NIL) passes.
        assert!(MarketBookHandle::validate_node_links(&data).is_ok());
        // Corrupt root.parent (offset +8) to another in-tree node.
        let base = MARKET_BOOK_PREFIX_BYTES + root_off as usize + 8;
        data[base..base + 4].copy_from_slice(&other_off.to_le_bytes());
        assert!(
            MarketBookHandle::validate_node_links(&data).is_err(),
            "non-NIL root parent must be rejected"
        );
    }

    /// A committed root index that is out of range or misaligned must be
    /// CLEANLY REJECTED, never dereferenced. The root is read via the unchecked
    /// `read_link` (and later as a slab offset), so without a bounds+alignment
    /// gate an out-of-range root slices past the slab (panic → validator brick),
    /// and a misaligned-but-in-bounds root is accepted here yet rejected by every
    /// subsequent `from_account_data` (permanent market brick). A well-formed root
    /// is always node-aligned and wholly in-slab, so neither is a valid book.
    #[test]
    fn validate_node_links_rejects_out_of_range_root() {
        let slab_len = make_book().len() - MARKET_BOOK_PREFIX_BYTES;

        // A node-aligned root exactly at the slab end: `off + NODE_TOTAL_BYTES`
        // exceeds the slab, so `read_link(root, 8)` would slice out of range.
        let mut oob = make_book();
        {
            let h = MarketBookHandle::from_account_data(&mut oob).unwrap();
            h.header.bids_root_index = slab_len as u32;
        }
        assert!(
            MarketBookHandle::validate_node_links(&oob).is_err(),
            "an out-of-range root must be rejected, not panic"
        );

        // A wildly out-of-range root (NIL − 1, not the empty sentinel).
        let mut huge = make_book();
        {
            let h = MarketBookHandle::from_account_data(&mut huge).unwrap();
            h.header.bids_root_index = NIL - 1;
        }
        assert!(
            MarketBookHandle::validate_node_links(&huge).is_err(),
            "a huge out-of-range root must be rejected, not panic"
        );

        // A misaligned, in-bounds root (offset 4): would be accepted without the
        // alignment gate, then rejected by from_account_data → brick.
        let mut misaligned = make_book();
        {
            let h = MarketBookHandle::from_account_data(&mut misaligned).unwrap();
            h.header.bids_root_index = 4;
        }
        assert!(
            MarketBookHandle::validate_node_links(&misaligned).is_err(),
            "a misaligned root must be rejected"
        );
    }

    /// Every live node must sit below `num_bytes_allocated`; otherwise the next
    /// bump `alloc_node` returns a slot overlapping a live node (aliasing /
    /// type confusion). A commit can under-count the bump pointer with an empty
    /// free list — the alignment/`<= slab_len` checks accept it. Reject it.
    #[test]
    fn validate_node_links_rejects_undercounted_bump_pointer() {
        let mut data = make_book();
        {
            let mut h = MarketBookHandle::from_account_data(&mut data).unwrap();
            h.insert_bid(make_order(100, 1, true)).unwrap();
            h.insert_bid(make_order(200, 2, true)).unwrap();
        }
        // Two live nodes ⇒ bump pointer covers both; the book passes.
        assert!(MarketBookHandle::validate_node_links(&data).is_ok());
        // Shrink the bump pointer so it covers only the first node — the second
        // live node now sits at/above it and would be re-handed by alloc_node.
        {
            let h = MarketBookHandle::from_account_data(&mut data).unwrap();
            h.header.num_bytes_allocated = NODE_TOTAL_BYTES as u32;
        }
        assert!(
            MarketBookHandle::validate_node_links(&data).is_err(),
            "a live node at/above the bump pointer must be rejected"
        );
    }

    // `for_each_best_first` starts at the cached best, so the best must be the
    // tree minimum (leftmost). A commit that plants a non-minimal best makes
    // matching skip better liquidity / misread top-of-book. The check must accept
    // every honestly-built book (best == leftmost) and reject a tampered best.
    #[test]
    fn validate_node_links_requires_best_is_tree_minimum() {
        let read_u32 = |data: &[u8], off: u32, link: usize| -> u32 {
            let base = MARKET_BOOK_PREFIX_BYTES + off as usize + link;
            let mut b = [0u8; 4];
            b.copy_from_slice(&data[base..base + 4]);
            u32::from_le_bytes(b)
        };

        // Empty book (both roots NIL, both bests NIL) — accepted.
        let empty = make_book();
        assert!(MarketBookHandle::validate_node_links(&empty).is_ok());
        // A best on an empty tree — rejected.
        let mut bad_empty = make_book();
        {
            let h = MarketBookHandle::from_account_data(&mut bad_empty).unwrap();
            h.header.bids_best_index = 0;
        }
        assert!(MarketBookHandle::validate_node_links(&bad_empty).is_err());

        // Single-node book — best == root == leftmost, accepted.
        let mut one = make_book();
        {
            let mut h = MarketBookHandle::from_account_data(&mut one).unwrap();
            h.insert_bid(make_order(100, 1, true)).unwrap();
        }
        assert!(MarketBookHandle::validate_node_links(&one).is_ok());

        // Multi-node unbalanced book — accepted (insert keeps best == leftmost).
        let mut data = make_book();
        {
            let mut h = MarketBookHandle::from_account_data(&mut data).unwrap();
            for (p, s) in [(100u64, 1u64), (200, 2), (150, 3), (175, 4), (125, 5)] {
                h.insert_bid(make_order(p, s, true)).unwrap();
            }
        }
        assert!(
            MarketBookHandle::validate_node_links(&data).is_ok(),
            "an honestly-built multi-node book must be accepted"
        );

        // Tamper: point best at the root's right child (strictly greater than the
        // root, so never the minimum). Must be rejected.
        let root = {
            let h = MarketBookHandle::from_account_data(&mut data).unwrap();
            h.header.bids_root_index
        };
        let right_child = read_u32(&data, root, 4);
        assert!(
            right_child != NIL,
            "test needs a right subtree to tamper with"
        );
        {
            let h = MarketBookHandle::from_account_data(&mut data).unwrap();
            h.header.bids_best_index = right_child;
        }
        assert!(
            MarketBookHandle::validate_node_links(&data).is_err(),
            "a best that is not the tree minimum must be rejected"
        );
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
