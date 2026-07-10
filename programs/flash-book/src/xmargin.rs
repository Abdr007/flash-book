//! Cross-domain (ER) reserved-margin attestation — opt-in, additive (#8).
//!
//! The hybrid venue splits state across two domains: a trader's COLLATERAL
//! (`TraderStateAccount`) is authoritative on L1, while their RESTING ORDERS
//! live in the order book delegated to the MagicBlock Ephemeral Rollup (ER).
//! L1 therefore cannot see the margin those resting orders will require if they
//! fill — `open_positions` only counts already-FILLED positions. Without a
//! bridge, a trader could rest large orders on the ER, withdraw the collateral
//! backing them on L1 (nothing filled yet, so the strict-withdraw gate passes),
//! and then have those orders fill into undercollateralized positions / bad debt.
//!
//! This module closes that gap with a sequencer-ATTESTED reserved-margin figure.
//! The ER's trusted margin attestor periodically writes, per trader, the total
//! initial margin reserved by that trader's live ER orders, behind a monotonic
//! epoch (the same replay-guard shape as `apply_fill`'s settlement nonce). The
//! cross-domain withdraw variants then require post-withdrawal collateral to
//! cover `max(IM_filled, floor) + er_reserved`, so collateral backing resting ER
//! orders can never be pulled.
//!
//! Everything here is a pure decision function or a plain account; the
//! instruction handlers in `lib.rs` call into it. That keeps the accounting
//! Kani-provable and host-testable independent of the (un-runnable-in-harness)
//! ER itself. Defaults are inert: a trader with no attestation / `er_active == 0`
//! behaves EXACTLY as before, so this is fully backward compatible.

use anchor_lang::prelude::*;

use crate::errors::FlashBookError;

/// PDA seed for a trader's ER reserved-margin attestation.
/// Address: `[ER_MARGIN_SEED, trader_state_key]`.
pub const ER_MARGIN_SEED: &[u8] = b"er_margin";

/// Per-trader ER reserved-margin attestation. Created once by the protocol
/// authority (which pins the trusted `attestor`), then maintained by that
/// attestor. `reserved_margin_quote_lots` is the total initial margin the
/// trader's LIVE ER orders require; `epoch` is a strictly-increasing replay
/// guard. 81 bytes + 8 disc.
#[account]
pub struct ErMarginAttestation {
    /// The `trader_state` PDA this attestation is bound to (NOT the wallet —
    /// supports sub-accounts, which key by trader_state).
    pub trader_state: Pubkey,
    /// The only key allowed to update this attestation (the ER margin
    /// sequencer). Pinned at init by the protocol authority.
    pub attestor: Pubkey,
    /// Total initial margin (quote lots) reserved by the trader's live ER
    /// resting orders. Withdrawals must leave at least this much behind.
    pub reserved_margin_quote_lots: u64,
    /// Strictly-increasing attestation epoch (monotonic replay guard).
    pub epoch: u64,
    pub bump: u8,
}

impl ErMarginAttestation {
    /// 32 + 32 + 8 + 8 + 1.
    pub const LEN: usize = 32 + 32 + 8 + 8 + 1;
}

/// Advance the attestation epoch. STRICTLY increasing only — a replayed or
/// stale attestation (epoch ≤ current) is rejected. Mirror of
/// `matcher::fill_commitment::advance_settlement_seq`, kept here so the
/// reserved-margin path has its own proven monotonic guard.
#[inline]
pub fn advance_epoch(current: u64, proposed: u64) -> Result<u64> {
    require!(proposed > current, FlashBookError::ErEpochReplay);
    Ok(proposed)
}

/// Cross-domain gate for the simple withdraw path (trader has NO filled
/// positions, so `open_positions == 0` is enforced separately by the handler).
/// Post-withdrawal collateral must still cover the ER reserved margin.
///
/// Returns Ok iff `amount <= collateral` AND `collateral - amount >= er_reserved`.
/// With `er_reserved == 0` this reduces to the original balance check, so the
/// non-ER path is unaffected.
#[inline]
pub fn check_simple_withdraw(collateral: u64, amount: u64, er_reserved: u64) -> Result<()> {
    require!(amount <= collateral, FlashBookError::InsufficientCollateral);
    let remaining = collateral - amount;
    require!(remaining >= er_reserved, FlashBookError::ErMarginReserved);
    Ok(())
}

/// Pure core for an INTERNAL collateral transfer between two of the same
/// trader's accounts (Track A2 extract-and-prove): move `amount` quote-lots from
/// `src` to `dst`, leaving at least `er_reserved` behind on the source. Returns
/// the new `(src, dst)` balances.
///
/// This IS the transfer handlers' code path — `transfer_main_to_sub` /
/// `transfer_sub_to_main` mutate the two `collateral_quote_lots` fields ONLY via
/// this function (enforced by `proven_wrapper_enforcement`), so the balance
/// arithmetic is proven here rather than inline. Reuses the already-proven
/// `check_simple_withdraw` gate — no duplicated guard. TOTAL collateral is
/// conserved: `src_after + dst_after == src + dst` (proven in
/// `collateral_transfer_conserves_total`).
#[inline]
pub fn apply_collateral_transfer(
    src: u64,
    dst: u64,
    amount: u64,
    er_reserved: u64,
) -> Result<(u64, u64)> {
    check_simple_withdraw(src, amount, er_reserved)?; // amount ≤ src, src − amount ≥ er_reserved
    let src_after = src - amount; // gate ⇒ no underflow
    let dst_after = dst
        .checked_add(amount)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
    Ok((src_after, dst_after))
}

/// Pure core for the CROSS→ISOLATED margin conversion (Track A2): move `amount`
/// collateral from the trader's pooled cross balance into a fresh isolated
/// position (which held 0). Returns `(cross_after, isolated_after)`. Preserves
/// the exact `ArithmeticUnderflow` error the inline path used. Conserves total:
/// `cross_after + isolated_after == cross` (proven in `split_to_isolated_conserves`).
#[inline]
pub fn split_to_isolated(cross: u64, amount: u64) -> Result<(u64, u64)> {
    let cross_after = cross
        .checked_sub(amount)
        .ok_or_else(|| error!(FlashBookError::ArithmeticUnderflow))?;
    Ok((cross_after, amount)) // fresh isolated position started at 0
}

/// Pure core for the ISOLATED→CROSS margin conversion (Track A2): return all of
/// an isolated position's `isolated` collateral to the pooled cross balance.
/// Returns `cross_after`. Preserves the exact `ArithmeticOverflow` error.
/// Conserves total: `cross_after == cross + isolated` (proven in
/// `merge_to_cross_conserves`); the isolated position is then zeroed by the caller.
#[inline]
pub fn merge_to_cross(cross: u64, isolated: u64) -> Result<u64> {
    cross
        .checked_add(isolated)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))
}

/// Pure core for a LIQUIDATION-REWARD payment (Track A2): pay the liquidator a
/// `reward`, capped at the liquidated source's available collateral, moving it
/// from `src` (the liquidated position or trader_state) to `caller` (the
/// liquidator's trader_state). Returns `(src_after, caller_after, paid)`.
/// Preserves the exact `ArithmeticOverflow` error. Conserves total collateral
/// (`src_after + caller_after == src + caller`) and never over-rewards
/// (`paid <= reward`), proven in `liquidation_reward_conserves`.
#[inline]
pub fn apply_liquidation_reward(src: u64, caller: u64, reward: u64) -> Result<(u64, u64, u64)> {
    let paid = reward.min(src); // capped at the source's available collateral
    let src_after = src - paid; // paid ≤ src ⇒ no underflow
    let caller_after = caller
        .checked_add(paid)
        .ok_or_else(|| error!(FlashBookError::ArithmeticOverflow))?;
    Ok((src_after, caller_after, paid))
}

/// Resolve the ER reserved margin a collateral-releasing path must leave
/// behind on the source trader_state. Fail-closed in both directions: an
/// ER-active source (live attested reservation) must supply its own bound
/// attestation, and a supplied attestation must belong to the source — a
/// stranger's (or stale sub-account's) attestation can never understate the
/// reservation. A source that was never attested (or attested back to zero)
/// reserves nothing.
pub fn resolve_er_reserved(
    source_state: Pubkey,
    source_er_active: u8,
    attestation: Option<&ErMarginAttestation>,
) -> Result<u64> {
    match attestation {
        Some(a) => {
            require_keys_eq!(
                a.trader_state,
                source_state,
                FlashBookError::ErMarginAccountMismatch
            );
            Ok(a.reserved_margin_quote_lots)
        }
        None => {
            require!(source_er_active == 0, FlashBookError::UseXDomainWithdraw);
            Ok(0)
        }
    }
}

/// Cross-domain required-collateral floor for the PARTIAL withdraw path (trader
/// has filled positions). The existing gate requires post-withdrawal collateral
/// `>= max(im_required, notional_floor)`; the cross-domain variant adds the ER
/// reserved margin on top (saturating), so collateral must cover BOTH the filled
/// positions' requirement AND the resting ER orders' reservation.
///
/// `required = max(im_required, notional_floor) + er_reserved` (saturating).
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
    fn epoch_must_strictly_increase() {
        assert_eq!(advance_epoch(0, 1).unwrap(), 1);
        assert_eq!(advance_epoch(5, 9).unwrap(), 9);
        // Replays and stale epochs are rejected.
        assert!(advance_epoch(5, 5).is_err());
        assert!(advance_epoch(5, 4).is_err());
        assert!(advance_epoch(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn simple_withdraw_respects_reserved_margin() {
        // No ER reservation ⇒ pure balance check (original behavior).
        assert!(check_simple_withdraw(100, 100, 0).is_ok());
        assert!(check_simple_withdraw(100, 101, 0).is_err());
        // With reservation: can pull down to exactly the reserved floor...
        assert!(check_simple_withdraw(100, 40, 60).is_ok());
        // ...but not one lot past it.
        assert!(check_simple_withdraw(100, 41, 60).is_err());
        // Reserved == full collateral ⇒ nothing withdrawable.
        assert!(check_simple_withdraw(100, 1, 100).is_err());
        assert!(check_simple_withdraw(100, 0, 100).is_ok());
    }

    #[test]
    fn resolve_er_reserved_binds_attestation_and_fails_closed() {
        let state = Pubkey::new_from_array([7u8; 32]);
        let other = Pubkey::new_from_array([8u8; 32]);
        let att = |ts: Pubkey, reserved: u64| ErMarginAttestation {
            trader_state: ts,
            attestor: Pubkey::default(),
            reserved_margin_quote_lots: reserved,
            epoch: 1,
            bump: 0,
        };
        // Bound attestation ⇒ its reservation, verbatim (even when zero).
        assert_eq!(
            resolve_er_reserved(state, 1, Some(&att(state, 42))).unwrap(),
            42
        );
        assert_eq!(
            resolve_er_reserved(state, 0, Some(&att(state, 0))).unwrap(),
            0
        );
        // A stranger's attestation can never understate the reservation.
        assert!(resolve_er_reserved(state, 1, Some(&att(other, 0))).is_err());
        // ER-active with no attestation supplied ⇒ fail closed.
        assert!(resolve_er_reserved(state, 1, None).is_err());
        // Never attested ⇒ nothing reserved.
        assert_eq!(resolve_er_reserved(state, 0, None).unwrap(), 0);
    }

    #[test]
    fn required_floor_adds_reserved_on_top_of_max() {
        // base = max(im, floor); +er_reserved.
        assert_eq!(required_collateral_with_er(30, 50, 20), 70);
        assert_eq!(required_collateral_with_er(80, 50, 20), 100);
        assert_eq!(required_collateral_with_er(0, 0, 0), 0);
        // Saturating — never wraps.
        assert_eq!(required_collateral_with_er(u64::MAX, 0, 10), u64::MAX);
    }
}

/// FV: the cross-domain gate never lets collateral fall below the ER reservation,
/// and the epoch guard is strictly monotonic — the two invariants that make the
/// reserved-margin bridge safe against bad debt and replay.
#[cfg(kani)]
mod xmargin_kani_proofs {
    use super::*;

    /// SAFETY: if the simple-withdraw gate passes, the post-withdrawal collateral
    /// is provably ≥ the ER reserved margin (collateral backing resting ER orders
    /// can never be withdrawn).
    #[kani::proof]
    fn simple_withdraw_preserves_reserved_margin() {
        let collateral: u64 = kani::any();
        let amount: u64 = kani::any();
        let er_reserved: u64 = kani::any();
        if check_simple_withdraw(collateral, amount, er_reserved).is_ok() {
            // amount <= collateral was required, so this does not underflow.
            assert!(amount <= collateral);
            assert!(collateral - amount >= er_reserved);
        }
    }

    /// CONSERVATION (Track A2): the internal collateral transfer core neither
    /// mints nor burns collateral — the total across the two accounts is
    /// invariant, the source keeps its ER reservation, and exactly `amount`
    /// moves. Proven on the REAL `apply_collateral_transfer` symbol over all
    /// `u64`, so the `transfer_*` handlers that route through it preserve
    /// V = Σ collateral + … .
    #[kani::proof]
    fn collateral_transfer_conserves_total() {
        let src: u64 = kani::any();
        let dst: u64 = kani::any();
        let amount: u64 = kani::any();
        let er_reserved: u64 = kani::any();
        if let Ok((src_after, dst_after)) = apply_collateral_transfer(src, dst, amount, er_reserved)
        {
            // total collateral conserved — nothing minted or burned
            assert!(src_after as u128 + dst_after as u128 == src as u128 + dst as u128);
            // exactly `amount` moved from src to dst
            assert!(src_after == src - amount);
            assert!(dst_after == dst + amount);
            // the source still covers its ER reservation
            assert!(src_after >= er_reserved);
        }
    }

    /// CONSERVATION (Track A2): the cross→isolated margin conversion moves
    /// `amount` from the cross pool to a fresh isolated position without minting
    /// or burning — `cross_after + isolated_after == cross`, on the real
    /// `split_to_isolated` symbol over all `u64`.
    #[kani::proof]
    fn split_to_isolated_conserves() {
        let cross: u64 = kani::any();
        let amount: u64 = kani::any();
        if let Ok((cross_after, isolated_after)) = split_to_isolated(cross, amount) {
            assert!(cross_after as u128 + isolated_after as u128 == cross as u128);
            assert!(cross_after == cross - amount);
            assert!(isolated_after == amount);
        }
    }

    /// CONSERVATION (Track A2): the isolated→cross margin conversion returns all
    /// isolated collateral to the cross pool without minting or burning —
    /// `cross_after == cross + isolated`, on the real `merge_to_cross` symbol.
    #[kani::proof]
    fn merge_to_cross_conserves() {
        let cross: u64 = kani::any();
        let isolated: u64 = kani::any();
        if let Ok(cross_after) = merge_to_cross(cross, isolated) {
            assert!(cross_after as u128 == cross as u128 + isolated as u128);
        }
    }

    /// CONSERVATION (Track A2): the liquidation-reward payment moves the capped
    /// reward from the liquidated source to the liquidator without minting or
    /// burning — `src_after + caller_after == src + caller`, `paid <= reward`,
    /// on the real `apply_liquidation_reward` symbol over all `u64`.
    #[kani::proof]
    fn liquidation_reward_conserves() {
        let src: u64 = kani::any();
        let caller: u64 = kani::any();
        let reward: u64 = kani::any();
        if let Ok((src_after, caller_after, paid)) = apply_liquidation_reward(src, caller, reward) {
            assert!(src_after as u128 + caller_after as u128 == src as u128 + caller as u128);
            assert!(paid <= reward); // never over-rewards
            assert!(src_after == src - paid); // exact debit, capped at src
        }
    }

    /// MONOTONIC: the epoch guard only accepts a strictly-greater epoch, so a
    /// replayed/stale attestation can never be applied.
    #[kani::proof]
    fn epoch_strictly_increases() {
        let current: u64 = kani::any();
        let proposed: u64 = kani::any();
        if let Ok(next) = advance_epoch(current, proposed) {
            assert!(next > current);
            assert!(next == proposed);
        }
    }

    /// FAIL-CLOSED: the reservation resolver never understates the attested
    /// reserve — with an attestation it returns the bound reservation verbatim
    /// (or rejects a foreign one), and without one it only ever returns 0 for a
    /// source that is provably not ER-active.
    #[kani::proof]
    fn resolve_er_reserved_never_understates() {
        let state = Pubkey::new_from_array(kani::any());
        let er_active: u8 = kani::any();
        let supplied: bool = kani::any();
        let att = ErMarginAttestation {
            trader_state: Pubkey::new_from_array(kani::any()),
            attestor: Pubkey::new_from_array([0u8; 32]),
            reserved_margin_quote_lots: kani::any(),
            epoch: kani::any(),
            bump: kani::any(),
        };
        let attestation = if supplied { Some(&att) } else { None };
        if let Ok(reserved) = resolve_er_reserved(state, er_active, attestation) {
            match attestation {
                Some(a) => {
                    assert!(a.trader_state == state);
                    assert!(reserved == a.reserved_margin_quote_lots);
                }
                None => {
                    assert!(er_active == 0);
                    assert!(reserved == 0);
                }
            }
        }
    }

    /// CONSERVATIVE: the cross-domain required floor is never LESS than either the
    /// filled-position requirement or the ER reservation alone — adding ER margin
    /// only ever tightens the withdraw gate, never loosens it.
    #[kani::proof]
    fn required_floor_is_conservative() {
        let im: u64 = kani::any();
        let floor: u64 = kani::any();
        let er: u64 = kani::any();
        let req = required_collateral_with_er(im, floor, er);
        let base = core::cmp::max(im, floor);
        assert!(req >= base);
        assert!(req >= er);
    }

    /// ANTI-SELF-LIQUIDATION (the Hyperliquid self-liquidation attack, blocked by
    /// construction): NO withdrawal the reserve-margin gate allows can leave the
    /// account below maintenance margin — so a trader can never withdraw
    /// themselves into a liquidatable state and dump the resulting loss onto the
    /// insurance fund. The partial-withdraw gate admits `amount` only if the
    /// remainder covers `required_collateral_with_er(im, floor, er) =
    /// max(im,floor)+er`. Because initial margin is never below maintenance margin
    /// (`im >= mm`, a protocol invariant), the remainder is provably `>= mm`, and
    /// it still covers the ER reservation. This is the adversarial theorem behind
    /// the launch claim.
    #[kani::proof]
    fn withdraw_cannot_self_liquidate_below_maintenance() {
        let collateral: u64 = kani::any();
        let amount: u64 = kani::any();
        let im: u64 = kani::any();
        let floor: u64 = kani::any();
        let mm: u64 = kani::any();
        let er: u64 = kani::any();
        // Initial margin dominates maintenance margin — always true on-chain.
        kani::assume(im >= mm);
        let required = required_collateral_with_er(im, floor, er);
        // The gate an adversary must pass to release `amount`.
        kani::assume(amount <= collateral);
        kani::assume(collateral - amount >= required);
        let remaining = collateral - amount;
        // The account cannot be pushed below maintenance margin...
        assert!(remaining >= mm);
        // ...nor below the ER reservation (no cross-domain loss dump).
        assert!(remaining >= er);
    }
}
