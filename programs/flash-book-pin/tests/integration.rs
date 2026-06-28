//! On-validator integration tests for the flash-book-pin program, via
//! `solana-program-test` (loads the compiled `flash_book_pin.so` into the BPF
//! VM). These exercise the SBF-only handlers end-to-end — the verification gap
//! the host unit tests + Kani proofs can't reach. The program is loaded as
//! bytecode (no linking to pinocchio), so the harness only builds raw
//! instructions (1-byte Ix tag + data) and pre-seeds account state.
//!
//! Run: `cargo build-sbf` then `SBF_OUT_DIR=target/deploy cargo test --test integration`.

use flash_book_pin::seeds::INSURANCE_SEED;
use flash_book_pin::state::{INSURANCE_DISC, MARKET_DISC, POSITION_DISC, TRADER_STATE_DISC};
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
// ─── append to tests/integration.rs ───

const IX_AUTO_DELEVERAGE: u8 = 89;
const TRADER_STATE_LEN: usize = 200;
const POSITION_LEN: usize = 128;

// Market offsets
const MKT_LONG_OI: usize = 56;
const MKT_SHORT_OI: usize = 64;
const MKT_TICK: usize = 72;
const MKT_MARK: usize = 88;
const MKT_MMR_BPS: usize = 120;
// Insurance offsets
const INS_PAUSE_THRESH: usize = 128;
// TraderState offsets
const TS_TRADER: usize = 8;
const TS_COLLATERAL: usize = 40;
const TS_OPEN_POSITIONS: usize = 52;
// Position offsets
const POS_TRADER: usize = 24;
const POS_MARKET: usize = 56;
const POS_SIZE: usize = 88;
const POS_ENTRY: usize = 96;
const POS_COLLATERAL: usize = 104;
const POS_SIDE: usize = 120;

fn put_u64(d: &mut [u8], off: usize, v: u64) {
    d[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(d: &mut [u8], off: usize, v: u32) {
    d[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_key(d: &mut [u8], off: usize, k: &Pubkey) {
    d[off..off + 32].copy_from_slice(&k.to_bytes());
}
fn get_u64(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
}

fn market_full(pid: Pubkey, mark: u64, tick: u64, mmr_bps: u32, long_oi: u64, short_oi: u64) -> Account {
    let mut d = vec![0u8; MARKET_LEN];
    d[0..8].copy_from_slice(&MARKET_DISC);
    put_u64(&mut d, MKT_LONG_OI, long_oi);
    put_u64(&mut d, MKT_SHORT_OI, short_oi);
    put_u64(&mut d, MKT_TICK, tick);
    put_u64(&mut d, MKT_MARK, mark);
    put_u32(&mut d, MKT_MMR_BPS, mmr_bps);
    rent_account(d, pid)
}
fn insurance_full(pid: Pubkey, balance: u64, pause_threshold: u64) -> Account {
    let mut d = vec![0u8; INSURANCE_LEN];
    d[0..8].copy_from_slice(&INSURANCE_DISC);
    put_u64(&mut d, INS_BALANCE, balance);
    put_u64(&mut d, INS_PAUSE_THRESH, pause_threshold);
    rent_account(d, pid)
}
fn trader_state(pid: Pubkey, trader: Pubkey, collateral: u64, open: u8) -> Account {
    let mut d = vec![0u8; TRADER_STATE_LEN];
    d[0..8].copy_from_slice(&TRADER_STATE_DISC);
    put_key(&mut d, TS_TRADER, &trader);
    put_u64(&mut d, TS_COLLATERAL, collateral);
    d[TS_OPEN_POSITIONS] = open;
    rent_account(d, pid)
}
fn position(pid: Pubkey, trader: Pubkey, market: Pubkey, side: u8, size: u64, entry: u64, collateral: u64) -> Account {
    let mut d = vec![0u8; POSITION_LEN];
    d[0..8].copy_from_slice(&POSITION_DISC);
    put_key(&mut d, POS_TRADER, &trader);
    put_key(&mut d, POS_MARKET, &market);
    put_u64(&mut d, POS_SIZE, size);
    put_u64(&mut d, POS_ENTRY, entry);
    put_u64(&mut d, POS_COLLATERAL, collateral);
    d[POS_SIDE] = side;
    rent_account(d, pid)
}

/// auto_deleverage end-to-end: an UNHEALTHY isolated long is force-closed against
/// a profitable short counter at the bankruptcy price. Underwater long 10 @200,
/// isolated collateral 100; mark 100 (mmr 500bps) ⇒ available 100 < required 1050
/// ⇒ unhealthy. bp = 200 − 100/(10·1) = 190; short counter @250 (> 190) is
/// eligible. Closing 5: uw loss 100·5/10 = 50 (collat 100→50); ct gain
/// (250−190)·5 = 300 (cross pool 1000→1300); both sizes 10→5; OI 10→5 each.
#[tokio::test]
async fn auto_deleverage_settles_underwater_vs_counter() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let market = Pubkey::new_unique();
    let (insurance, _b) = Pubkey::find_program_address(&[INSURANCE_SEED], &pid);
    let uw_trader = Pubkey::new_unique();
    let ct_trader = Pubkey::new_unique();
    let uw_ts = Pubkey::new_unique();
    let uw_pos = Pubkey::new_unique();
    let ct_ts = Pubkey::new_unique();
    let ct_pos = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 100, 1, 500, 10, 10));
    pt.add_account(insurance, insurance_full(pid, 100, 1_000)); // balance < threshold ⇒ ADL eligible
    pt.add_account(uw_ts, trader_state(pid, uw_trader, 0, 1));
    pt.add_account(uw_pos, position(pid, uw_trader, market, 0, 10, 200, 100)); // long, isolated 100
    pt.add_account(ct_ts, trader_state(pid, ct_trader, 1_000, 1));
    pt.add_account(ct_pos, position(pid, ct_trader, market, 1, 10, 250, 0)); // short, cross
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_AUTO_DELEVERAGE];
    data.extend_from_slice(&5u64.to_le_bytes()); // close_size = 5
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(uw_ts, false),
            AccountMeta::new(uw_pos, false),
            AccountMeta::new(ct_ts, false),
            AccountMeta::new(ct_pos, false),
        ],
        data,
    };
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &caller], bh);
    banks.process_transaction(tx).await.unwrap();

    let uwp = banks.get_account(uw_pos).await.unwrap().unwrap();
    assert_eq!(get_u64(&uwp.data, POS_COLLATERAL), 50, "uw isolated bucket 100 − loss 50");
    assert_eq!(get_u64(&uwp.data, POS_SIZE), 5, "uw size 10 − 5");
    let ctt = banks.get_account(ct_ts).await.unwrap().unwrap();
    assert_eq!(get_u64(&ctt.data, TS_COLLATERAL), 1_300, "ct cross pool 1000 + gain 300");
    let ctp = banks.get_account(ct_pos).await.unwrap().unwrap();
    assert_eq!(get_u64(&ctp.data, POS_SIZE), 5, "ct size 10 − 5");
    let m = banks.get_account(market).await.unwrap().unwrap();
    assert_eq!(get_u64(&m.data, MKT_LONG_OI), 5, "long_oi 10 − 5");
    assert_eq!(get_u64(&m.data, MKT_SHORT_OI), 5, "short_oi 10 − 5 (long==short preserved)");
}

// ─── liquidate_position_v2 e2e (pre-seeds a valid book — no create_pda CPI) ───
use flash_book_pin::book::{MarketBookHandle, MARKET_BOOK_SEED, MARKET_BOOK_TOTAL_BYTES};
use flash_book_pin::seeds::{MARKET_SEED, POSITION_LIQ_STATE_SEED};
use flash_book_pin::state::POSITION_LIQ_STATE_DISC;

const IX_SET_LIQ_PARAMS: u8 = 91;
const IX_LIQUIDATE_V2: u8 = 92;
const PL_MARKET: usize = 8;
const PL_POSITION: usize = 40;
const PL_UNHEALTHY_SINCE: usize = 72;
const PL_LAST_LIQUIDATED: usize = 80;
const MKT_AUTHORITY: usize = 124;

fn init_book(pid: Pubkey, market: Pubkey, base: Pubkey, quote: Pubkey, bump: u8) -> Account {
    let mut data = vec![0u8; MARKET_BOOK_TOTAL_BYTES];
    MarketBookHandle::write_disc_and_init_header(
        &mut data, bump, market.to_bytes(), base.to_bytes(), quote.to_bytes(),
    )
    .unwrap();
    rent_account(data, pid)
}
fn position_liq_acct(pid: Pubkey, market: Pubkey, position: Pubkey) -> Account {
    let mut d = vec![0u8; 120];
    d[0..8].copy_from_slice(&POSITION_LIQ_STATE_DISC);
    put_key(&mut d, PL_MARKET, &market);
    put_key(&mut d, PL_POSITION, &position);
    rent_account(d, pid)
}

/// liquidate_position_v2 e2e: inject a forced-liquidation order for an unhealthy
/// isolated long and pay the flat (auction=0) liquidator reward. Same unhealthy
/// scenario (long 10 @200, isolated 100; mark 100, mmr 500bps). penalty 100bps,
/// reward 500bps. Closing 5: reward = (5·100·1)·500/10000 = 25 (≤ 100 bucket).
/// Asserts: liquidatee bucket 100→75, caller pool 0→25, size UNCHANGED (10 — the
/// close is deferred to the matcher), liq-state stamped (both slots > 0).
#[tokio::test]
async fn liquidate_position_v2_injects_and_rewards() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let caller = Keypair::new();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let (market, _) = Pubkey::find_program_address(
        &[MARKET_SEED, &base_mint.to_bytes(), &quote_mint.to_bytes()], &pid);
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let liq_trader = Pubkey::new_unique();
    let liq_ts = Pubkey::new_unique();
    let liq_position = Pubkey::new_unique();
    let (position_liq, _) = Pubkey::find_program_address(
        &[POSITION_LIQ_STATE_SEED, &market.to_bytes(), &liq_position.to_bytes()], &pid);
    let caller_ts = Pubkey::new_unique();

    let mut market_acct = market_full(pid, 100, 1, 500, 10, 10);
    put_key(&mut market_acct.data, MKT_AUTHORITY, &authority.pubkey());

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_acct);
    pt.add_account(market_book, init_book(pid, market, base_mint, quote_mint, book_bump));
    pt.add_account(position_liq, position_liq_acct(pid, market, liq_position));
    pt.add_account(liq_ts, trader_state(pid, liq_trader, 0, 1));
    pt.add_account(liq_position, position(pid, liq_trader, market, 0, 10, 200, 100));
    pt.add_account(caller_ts, trader_state(pid, caller.pubkey(), 0, 0));
    let (banks, payer, bh) = pt.start().await;

    let mut sp = vec![IX_SET_LIQ_PARAMS];
    sp.extend_from_slice(&100u32.to_le_bytes()); // penalty
    sp.extend_from_slice(&500u32.to_le_bytes()); // reward
    sp.extend_from_slice(&0u64.to_le_bytes()); // auction (flat)
    sp.extend_from_slice(&0u64.to_le_bytes()); // cooldown
    let ix_params = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new(market, false),
        ],
        data: sp,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix_params], Some(&payer.pubkey()), &[&payer, &authority], bh))
        .await
        .unwrap();

    let mut ld = vec![IX_LIQUIDATE_V2];
    ld.extend_from_slice(&5u64.to_le_bytes());
    let ix_liq = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(liq_ts, false),
            AccountMeta::new(caller_ts, false),
            AccountMeta::new(liq_position, false),
            AccountMeta::new(position_liq, false),
        ],
        data: ld,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix_liq], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let pos = banks.get_account(liq_position).await.unwrap().unwrap();
    assert_eq!(get_u64(&pos.data, POS_COLLATERAL), 75, "bucket 100 − reward 25");
    assert_eq!(get_u64(&pos.data, POS_SIZE), 10, "size unchanged (close deferred to matcher)");
    let cts = banks.get_account(caller_ts).await.unwrap().unwrap();
    assert_eq!(get_u64(&cts.data, TS_COLLATERAL), 25, "liquidator reward credited");
    let pl = banks.get_account(position_liq).await.unwrap().unwrap();
    assert!(get_u64(&pl.data, PL_LAST_LIQUIDATED) > 0, "last_liquidated stamped");
    assert!(get_u64(&pl.data, PL_UNHEALTHY_SINCE) > 0, "unhealthy_since stamped");
}
