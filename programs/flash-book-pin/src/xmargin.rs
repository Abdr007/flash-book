//! Cross-domain (#8) margin math — pin port of the Anchor `xmargin`. Pure +
//! host-testable + Kani-provable. The ER holds a trader's resting orders, so L1
//! collateral must leave behind the sequencer-attested reserved margin those
//! orders lock. These helpers gate the withdraw paths against it.

/// Advance the attestation epoch — STRICTLY increasing (replay/stale guard).
#[inline]
pub fn advance_epoch(current: u64, proposed: u64) -> Result<u64, ()> {
    if proposed > current {
        Ok(proposed)
    } else {
        Err(())
    }
}

/// Simple-withdraw gate (no filled positions): post-withdraw collateral must
/// still cover the ER reserved margin. `er_reserved == 0` ⇒ pure balance check.
/// Ok iff `amount <= collateral && collateral - amount >= er_reserved`.
#[inline]
pub fn check_simple_withdraw(collateral: u64, amount: u64, er_reserved: u64) -> Result<(), ()> {
    if amount > collateral {
        return Err(());
    }
    if collateral - amount < er_reserved {
        return Err(());
    }
    Ok(())
}

/// Partial-withdraw required-collateral floor: `max(im_required, notional_floor)
/// + er_reserved` (saturating). `er_reserved == 0` ⇒ the original non-ER floor.
#[inline]
pub fn required_collateral_with_er(im_required: u64, notional_floor: u64, er_reserved: u64) -> u64 {
    let base = if im_required > notional_floor {
        im_required
    } else {
        notional_floor
    };
    base.saturating_add(er_reserved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_strictly_increasing() {
        assert_eq!(advance_epoch(0, 1), Ok(1));
        assert_eq!(advance_epoch(5, 9), Ok(9));
        assert_eq!(advance_epoch(5, 5), Err(()));
        assert_eq!(advance_epoch(5, 4), Err(()));
        assert_eq!(advance_epoch(u64::MAX, u64::MAX), Err(()));
    }

    #[test]
    fn simple_withdraw_respects_reserved() {
        assert!(check_simple_withdraw(100, 100, 0).is_ok());
        assert!(check_simple_withdraw(100, 101, 0).is_err());
        assert!(check_simple_withdraw(100, 40, 60).is_ok()); // down to the floor
        assert!(check_simple_withdraw(100, 41, 60).is_err()); // one past it
    }

    #[test]
    fn er_floor_is_max_plus_reserved() {
        assert_eq!(required_collateral_with_er(100, 50, 0), 100); // im wins
        assert_eq!(required_collateral_with_er(50, 100, 0), 100); // floor wins
        assert_eq!(required_collateral_with_er(100, 50, 30), 130); // + reserved
        assert_eq!(required_collateral_with_er(u64::MAX, 0, 10), u64::MAX); // saturating
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// The ER floor never UNDERSTATES either component: the result is >= both the
    /// margin requirement and the reserved margin (so a withdraw can't leave the
    /// trader unable to cover their ER orders or their filled positions).
    #[kani::proof]
    fn proof_floor_covers_both() {
        let im: u64 = kani::any();
        let nf: u64 = kani::any();
        let er: u64 = kani::any();
        let r = required_collateral_with_er(im, nf, er);
        // r >= er (unless saturated, in which case r == u64::MAX >= er too).
        assert!(r >= er || r == u64::MAX);
        // r >= max(im, nf) (saturating add only grows the base).
        let base = if im > nf { im } else { nf };
        assert!(r >= base);
    }

    /// advance_epoch admits a proposed epoch IFF it strictly exceeds current —
    /// no replay (==) or rollback (<) is ever accepted.
    #[kani::proof]
    fn proof_epoch_monotonic() {
        let cur: u64 = kani::any();
        let prop: u64 = kani::any();
        match advance_epoch(cur, prop) {
            Ok(v) => assert!(v == prop && prop > cur),
            Err(()) => assert!(prop <= cur),
        }
    }
}
