//! Per-trader volume rate limit (Wave 54).
//!
//! Bounds the maximum aggressor notional a single trader can transact
//! in a sliding window. Anti-griefing primitive: a hostile market-
//! taker can't drain the matcher's compute budget by sending a flood
//! of large aggressor orders.
//!
//! Sliding-window token-bucket. On each fill, the trader's bucket
//! consumes `fill_notional` tokens; the bucket refills at
//! `refill_rate_per_slot` per slot, capped at `bucket_capacity`.

/// Per-trader bucket state. On TraderState (Wave 54b layout change).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VolumeBucket {
    pub tokens_remaining: u64,
    pub last_refill_slot: u64,
}

/// Refill the bucket based on slot elapsed; clamp to capacity.
#[inline]
pub fn refill(
    bucket: VolumeBucket,
    refill_per_slot: u64,
    capacity: u64,
    now_slot: u64,
) -> VolumeBucket {
    if refill_per_slot == 0 || capacity == 0 {
        return bucket;
    }
    let dt = now_slot.saturating_sub(bucket.last_refill_slot);
    let added = (refill_per_slot as u128).saturating_mul(dt as u128);
    let new_tokens = (bucket.tokens_remaining as u128).saturating_add(added);
    let clamped = new_tokens.min(capacity as u128).min(u64::MAX as u128) as u64;
    VolumeBucket {
        tokens_remaining: clamped,
        last_refill_slot: now_slot,
    }
}

/// Try to consume `cost` tokens. Returns `(success, new_bucket)`.
/// If the bucket has insufficient tokens, returns `(false, bucket)`
/// unchanged.
#[inline]
pub fn try_consume(bucket: VolumeBucket, cost: u64) -> (bool, VolumeBucket) {
    if bucket.tokens_remaining < cost {
        return (false, bucket);
    }
    (
        true,
        VolumeBucket {
            tokens_remaining: bucket.tokens_remaining - cost,
            last_refill_slot: bucket.last_refill_slot,
        },
    )
}

/// Convenience: refill + try_consume in one call.
#[inline]
pub fn try_charge(
    bucket: VolumeBucket,
    cost: u64,
    refill_per_slot: u64,
    capacity: u64,
    now_slot: u64,
) -> (bool, VolumeBucket) {
    let refilled = refill(bucket, refill_per_slot, capacity, now_slot);
    try_consume(refilled, cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bucket_rejects() {
        let b = VolumeBucket::default();
        let (ok, _) = try_consume(b, 100);
        assert!(!ok);
    }

    #[test]
    fn full_bucket_admits_up_to_capacity() {
        let b = VolumeBucket { tokens_remaining: 1_000, last_refill_slot: 0 };
        let (ok, b2) = try_consume(b, 500);
        assert!(ok);
        assert_eq!(b2.tokens_remaining, 500);
    }

    #[test]
    fn refill_adds_tokens_over_time() {
        let b = VolumeBucket { tokens_remaining: 0, last_refill_slot: 100 };
        let b2 = refill(b, 10, 1_000, 200); // dt=100, +1000, capped at 1000.
        assert_eq!(b2.tokens_remaining, 1_000);
        assert_eq!(b2.last_refill_slot, 200);
    }

    #[test]
    fn refill_caps_at_capacity() {
        let b = VolumeBucket { tokens_remaining: 900, last_refill_slot: 0 };
        let b2 = refill(b, 10, 1_000, 1_000); // would add 10_000 but capped.
        assert_eq!(b2.tokens_remaining, 1_000);
    }

    #[test]
    fn try_charge_combines_refill_consume() {
        let b = VolumeBucket { tokens_remaining: 0, last_refill_slot: 0 };
        // After 100 slots × 10/slot = 1000 tokens. Consume 600 → 400 left.
        let (ok, b2) = try_charge(b, 600, 10, 1_000, 100);
        assert!(ok);
        assert_eq!(b2.tokens_remaining, 400);
    }

    #[test]
    fn rate_limit_blocks_when_drained() {
        let b = VolumeBucket { tokens_remaining: 1_000, last_refill_slot: 0 };
        let (ok1, b) = try_charge(b, 800, 0, 1_000, 0);
        assert!(ok1);
        // Bucket has 200 left, refill rate 0 → can't recover.
        let (ok2, _) = try_charge(b, 500, 0, 1_000, 1_000);
        assert!(!ok2);
    }

    #[test]
    fn zero_refill_no_recovery() {
        let b = VolumeBucket { tokens_remaining: 0, last_refill_slot: 0 };
        let b2 = refill(b, 0, 1_000, 1_000);
        assert_eq!(b2.tokens_remaining, 0);
    }

    #[test]
    fn zero_capacity_no_op() {
        let b = VolumeBucket { tokens_remaining: 100, last_refill_slot: 0 };
        let b2 = refill(b, 10, 0, 100);
        assert_eq!(b2.tokens_remaining, 100);
    }
}
