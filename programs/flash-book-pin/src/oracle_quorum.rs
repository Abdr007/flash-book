//! Pure 3-source oracle quorum aggregation — a faithful transcription of the
//! validation + median selection in the Anchor `update_oracle_quorum`. No
//! pinocchio, no syscalls → host-unit-testable.
//!
//! Accepts the MEDIAN of three submitted prices after gating each source on
//! staleness + confidence and the set on dispersion. Conservative aggregates
//! (max confidence, oldest publish time) are computed by the caller.

use crate::constants::BPS_DENOM;

#[derive(Debug, PartialEq, Eq)]
pub enum QuorumErr {
    ZeroPrice,
    Stale,
    ConfidenceTooWide,
    DispersionTooWide,
}

/// Validate three oracle submissions and return the accepted median price.
///
/// `max_*` limits of 0 disable that gate (matching anchor's `> 0` guards).
/// - staleness: each `now - published_at <= max_staleness_seconds`
/// - confidence: each `conf * BPS_DENOM / price <= max_confidence_bps`
/// - dispersion: `(max - min) * BPS_DENOM / median <= max_dispersion_bps`
pub fn aggregate_median(
    prices: [u64; 3],
    confidences: [u64; 3],
    published_at: [u64; 3],
    now_unix: u64,
    max_staleness_seconds: u32,
    max_confidence_bps: u32,
    max_dispersion_bps: u32,
) -> Result<u64, QuorumErr> {
    for &p in &prices {
        if p == 0 {
            return Err(QuorumErr::ZeroPrice);
        }
    }
    // Re-audit 2026-06-30 (LOW parity): reject a FUTURE-dated source. The staleness
    // gate below uses `now.saturating_sub(t)`, which clamps a future timestamp to
    // age 0 and would slip it — the same bug class the Pyth path fixed (O-2). Anchor
    // future-rejects each source (`require!(t <= now)`).
    for &t in &published_at {
        if t > now_unix {
            return Err(QuorumErr::Stale);
        }
    }
    if max_staleness_seconds > 0 {
        for &t in &published_at {
            if now_unix.saturating_sub(t) > max_staleness_seconds as u64 {
                return Err(QuorumErr::Stale);
            }
        }
    }
    if max_confidence_bps > 0 {
        for i in 0..3 {
            // price > 0 checked above → division is safe.
            let conf_bps = (confidences[i] as u128) * (BPS_DENOM as u128) / (prices[i] as u128);
            if conf_bps > max_confidence_bps as u128 {
                return Err(QuorumErr::ConfidenceTooWide);
            }
        }
    }
    let mut sorted = prices;
    sorted.sort_unstable();
    let (min_p, median, max_p) = (sorted[0], sorted[1], sorted[2]);
    if max_dispersion_bps > 0 {
        let dispersion_bps = ((max_p - min_p) as u128) * (BPS_DENOM as u128) / (median as u128);
        if dispersion_bps > max_dispersion_bps as u128 {
            return Err(QuorumErr::DispersionTooWide);
        }
    }
    Ok(median)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_median() {
        // unsorted in → median 100 out; all gates off.
        assert_eq!(aggregate_median([120, 100, 90], [0; 3], [0; 3], 0, 0, 0, 0), Ok(100));
        assert_eq!(aggregate_median([100, 100, 100], [0; 3], [0; 3], 0, 0, 0, 0), Ok(100));
    }

    #[test]
    fn rejects_zero_price() {
        assert_eq!(aggregate_median([0, 100, 90], [0; 3], [0; 3], 0, 0, 0, 0), Err(QuorumErr::ZeroPrice));
    }

    #[test]
    fn staleness_gate() {
        // now 1000, max age 60. source published at 930 → age 70 > 60 → stale.
        assert_eq!(
            aggregate_median([100, 100, 100], [0; 3], [1000, 1000, 930], 1000, 60, 0, 0),
            Err(QuorumErr::Stale)
        );
        // all fresh → ok.
        assert_eq!(
            aggregate_median([100, 100, 100], [0; 3], [1000, 980, 990], 1000, 60, 0, 0),
            Ok(100)
        );
    }

    #[test]
    fn confidence_gate() {
        // price 100, conf 6 → 600 bps; max 500 → too wide.
        assert_eq!(
            aggregate_median([100, 100, 100], [1, 1, 6], [0; 3], 0, 0, 500, 0),
            Err(QuorumErr::ConfidenceTooWide)
        );
        // conf 5 → 500 bps == max → ok.
        assert_eq!(
            aggregate_median([100, 100, 100], [5, 5, 5], [0; 3], 0, 0, 500, 0),
            Ok(100)
        );
    }

    #[test]
    fn dispersion_gate() {
        // prices 90/100/110 → (110-90)*10000/100 = 2000 bps; max 1000 → too wide.
        assert_eq!(
            aggregate_median([90, 100, 110], [0; 3], [0; 3], 0, 0, 0, 1_000),
            Err(QuorumErr::DispersionTooWide)
        );
        // prices 99/100/101 → 200 bps <= 1000 → ok, median 100.
        assert_eq!(
            aggregate_median([99, 100, 101], [0; 3], [0; 3], 0, 0, 0, 1_000),
            Ok(100)
        );
    }
}
