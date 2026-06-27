//! cancel_all_v2 — remove every resting order owned by the signer, both sides,
//! bounded by MAX_CANCELS_PER_IX (a tx that would exceed it cancels the first
//! N and returns; the caller re-invokes to drain the rest).
//!
//! Faithful port of the Anchor `cancel_all_v2`. no_std: collected indices live
//! in fixed-size stack buffers (no Vec).
use crate::book::MarketBookHandle;
use crate::guard::{assert_market, assert_market_book};
use crate::hypertree::{DataIndex, NIL};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

const MAX_CANCELS_PER_IX: usize = 24;

/// data: (none)
/// accounts: [trader(signer), market, market_book]
pub fn process(pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let trader = &accounts[0];
    if !trader.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let trader_pk = *trader.key();

    assert_market(&accounts[1], pid)?;
    assert_market_book(&accounts[2], &accounts[1], pid)?;
    unsafe {
        let book_data = accounts[2].borrow_mut_data_unchecked();
        let mut handle = MarketBookHandle::from_account_data(book_data)?;

        // Collect this trader's node indices first (the walk closure borrows
        // &handle immutably; removal needs &mut handle), capped at MAX_CANCELS.
        let mut bid_idx = [NIL; MAX_CANCELS_PER_IX];
        let mut n_bid: usize = 0;
        let mut ask_idx = [NIL; MAX_CANCELS_PER_IX];
        let mut n_ask: usize = 0;

        {
            let mut on_bid = |idx: DataIndex, o: &crate::book::RestingOrderV2| -> bool {
                if n_bid + n_ask >= MAX_CANCELS_PER_IX {
                    return false;
                }
                if o.trader == trader_pk {
                    bid_idx[n_bid] = idx;
                    n_bid += 1;
                }
                true
            };
            handle.for_each_bid_best_first(&mut on_bid);
        }
        {
            let mut on_ask = |idx: DataIndex, o: &crate::book::RestingOrderV2| -> bool {
                if n_bid + n_ask >= MAX_CANCELS_PER_IX {
                    return false;
                }
                if o.trader == trader_pk {
                    ask_idx[n_ask] = idx;
                    n_ask += 1;
                }
                true
            };
            handle.for_each_ask_best_first(&mut on_ask);
        }

        for i in 0..n_bid {
            handle.remove_bid_node(bid_idx[i]);
        }
        for i in 0..n_ask {
            handle.remove_ask_node(ask_idx[i]);
        }
    }
    Ok(())
}
