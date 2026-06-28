//! On-validator integration tests for the flash-book-pin program, via
//! `solana-program-test` (loads the compiled `flash_book_pin.so` into the BPF
//! VM). These exercise the SBF-only handlers end-to-end — the verification gap
//! the host unit tests + Kani proofs can't reach. The program is loaded as
//! bytecode (no linking to pinocchio), so the harness only builds raw
//! instructions (1-byte Ix tag + data) and pre-seeds account state.
//!
//! Run: `cargo build-sbf` then `SBF_OUT_DIR=target/deploy cargo test --test integration`.

use flash_book_pin::seeds::INSURANCE_SEED;
use flash_book_pin::state::{INSURANCE_DISC, MARKET_DISC};
use solana_program_test::ProgramTest;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const IX_COVER_BAD_DEBT: u8 = 93;
const INSURANCE_LEN: usize = 200;
const MARKET_LEN: usize = 1152;

// Insurance byte offsets (repr(C), see state.rs).
const INS_BALANCE: usize = 8; // balance_quote_lots: u64
const INS_TOTAL_PAYOUTS: usize = 136; // total_payouts: u64

fn rent_account(data: Vec<u8>, owner: Pubkey) -> Account {
    Account {
        // generous rent-exempt funding; the harness rent sysvar is permissive.
        lamports: 10_000_000_000,
        data,
        owner,
        executable: false,
        rent_epoch: 0,
    }
}

fn market_account(program_id: Pubkey, sequencer: Pubkey) -> Account {
    let mut data = vec![0u8; MARKET_LEN];
    data[0..8].copy_from_slice(&MARKET_DISC);
    data[8..40].copy_from_slice(&sequencer.to_bytes()); // Market.sequencer @ offset 8
    rent_account(data, program_id)
}

fn insurance_account(program_id: Pubkey, balance: u64) -> Account {
    let mut data = vec![0u8; INSURANCE_LEN];
    data[0..8].copy_from_slice(&INSURANCE_DISC);
    data[INS_BALANCE..INS_BALANCE + 8].copy_from_slice(&balance.to_le_bytes());
    rent_account(data, program_id)
}

fn cover_bad_debt_ix(
    program_id: Pubkey,
    sequencer: Pubkey,
    market: Pubkey,
    insurance: Pubkey,
    shortfall: u64,
) -> Instruction {
    let mut data = vec![IX_COVER_BAD_DEBT];
    data.extend_from_slice(&shortfall.to_le_bytes());
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(sequencer, true), // signer
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(insurance, false), // writable
        ],
        data,
    }
}

/// Happy path: the fund covers a shortfall it can afford — balance drops by the
/// shortfall, total_payouts rises by it.
#[tokio::test]
async fn cover_bad_debt_draws_insurance() {
    let program_id = Pubkey::new_unique();
    let sequencer = Keypair::new();
    let market = Pubkey::new_unique();
    let (insurance, _b) = Pubkey::find_program_address(&[INSURANCE_SEED], &program_id);

    let mut pt = ProgramTest::new("flash_book_pin", program_id, None);
    pt.add_account(market, market_account(program_id, sequencer.pubkey()));
    pt.add_account(insurance, insurance_account(program_id, 1_000));
    let (banks, payer, blockhash) = pt.start().await;

    let ix = cover_bad_debt_ix(program_id, sequencer.pubkey(), market, insurance, 300);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer, &sequencer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let acct = banks.get_account(insurance).await.unwrap().unwrap();
    let balance = u64::from_le_bytes(acct.data[INS_BALANCE..INS_BALANCE + 8].try_into().unwrap());
    let payouts =
        u64::from_le_bytes(acct.data[INS_TOTAL_PAYOUTS..INS_TOTAL_PAYOUTS + 8].try_into().unwrap());
    assert_eq!(balance, 700, "balance should drop by the covered shortfall");
    assert_eq!(payouts, 300, "total_payouts should rise by the covered amount");
}

/// Exhausted fund: a shortfall larger than the balance drains it to 0 and pays
/// out exactly the balance (the uncovered remainder goes to ADL off-chain).
#[tokio::test]
async fn cover_bad_debt_exhausts_fund() {
    let program_id = Pubkey::new_unique();
    let sequencer = Keypair::new();
    let market = Pubkey::new_unique();
    let (insurance, _b) = Pubkey::find_program_address(&[INSURANCE_SEED], &program_id);

    let mut pt = ProgramTest::new("flash_book_pin", program_id, None);
    pt.add_account(market, market_account(program_id, sequencer.pubkey()));
    pt.add_account(insurance, insurance_account(program_id, 200));
    let (banks, payer, blockhash) = pt.start().await;

    let ix = cover_bad_debt_ix(program_id, sequencer.pubkey(), market, insurance, 500);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer, &sequencer],
        blockhash,
    );
    banks.process_transaction(tx).await.unwrap();

    let acct = banks.get_account(insurance).await.unwrap().unwrap();
    let balance = u64::from_le_bytes(acct.data[INS_BALANCE..INS_BALANCE + 8].try_into().unwrap());
    let payouts =
        u64::from_le_bytes(acct.data[INS_TOTAL_PAYOUTS..INS_TOTAL_PAYOUTS + 8].try_into().unwrap());
    assert_eq!(balance, 0, "fund drained to zero");
    assert_eq!(payouts, 200, "paid out exactly its balance");
}

/// C-1 authorization: a signer who is NOT the market's sequencer is rejected,
/// and the fund is untouched.
#[tokio::test]
async fn cover_bad_debt_rejects_wrong_sequencer() {
    let program_id = Pubkey::new_unique();
    let real_sequencer = Keypair::new();
    let impostor = Keypair::new();
    let market = Pubkey::new_unique();
    let (insurance, _b) = Pubkey::find_program_address(&[INSURANCE_SEED], &program_id);

    let mut pt = ProgramTest::new("flash_book_pin", program_id, None);
    pt.add_account(market, market_account(program_id, real_sequencer.pubkey()));
    pt.add_account(insurance, insurance_account(program_id, 1_000));
    let (banks, payer, blockhash) = pt.start().await;

    let ix = cover_bad_debt_ix(program_id, impostor.pubkey(), market, insurance, 300);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer, &impostor],
        blockhash,
    );
    let res = banks.process_transaction(tx).await;
    assert!(res.is_err(), "non-sequencer must be rejected");

    let acct = banks.get_account(insurance).await.unwrap().unwrap();
    let balance = u64::from_le_bytes(acct.data[INS_BALANCE..INS_BALANCE + 8].try_into().unwrap());
    assert_eq!(balance, 1_000, "fund untouched on a rejected draw");
}
