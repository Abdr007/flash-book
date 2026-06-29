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

// ─── set_position_cross e2e ───
const IX_SET_POSITION_CROSS: u8 = 94;

/// set_position_cross e2e: a trader with an ISOLATED long (target) + a CROSS long
/// (sibling) converts the target to cross. target isolated 100 @100/size10,
/// sibling cross size 5 @100; mark 100 (mmr 500bps), cross pool 200. After the
/// merge the pool is 200+100=300, required = mm_target(50)+mm_sibling(25) = 75 ≤
/// 300 ⇒ healthy. Asserts the pool gains the returned bucket and the target is
/// now cross (bucket 0).
#[tokio::test]
async fn set_position_cross_merges_bucket_and_stays_healthy() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let ts = Pubkey::new_unique();
    let target_market = Pubkey::new_unique();
    let target_position = Pubkey::new_unique();
    let market2 = Pubkey::new_unique();
    let sibling = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(ts, trader_state(pid, trader.pubkey(), 200, 2)); // cross pool 200, 2 open
    pt.add_account(target_market, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(target_position, position(pid, trader.pubkey(), target_market, 0, 10, 100, 100)); // isolated 100
    pt.add_account(market2, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(sibling, position(pid, trader.pubkey(), market2, 0, 5, 100, 0)); // cross sibling
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(target_market, false),
            AccountMeta::new(target_position, false),
            AccountMeta::new_readonly(market2, false),
            AccountMeta::new_readonly(sibling, false),
        ],
        data: vec![IX_SET_POSITION_CROSS],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &trader], bh))
        .await
        .unwrap();

    let ts_acct = banks.get_account(ts).await.unwrap().unwrap();
    assert_eq!(get_u64(&ts_acct.data, TS_COLLATERAL), 300, "cross pool 200 + returned 100");
    let tp = banks.get_account(target_position).await.unwrap().unwrap();
    assert_eq!(get_u64(&tp.data, POS_COLLATERAL), 0, "target is now cross (bucket 0)");
}

// ─── set_position_isolated e2e ───
const IX_SET_POSITION_ISOLATED: u8 = 95;

/// set_position_isolated e2e: a trader with two CROSS longs moves 100 from the
/// cross pool into the target's isolated bucket. target cross long 10 @100,
/// sibling cross long 5 @100; mark 100 (mmr 500bps), cross pool 200. Moving 100:
///   (a) target isolated on 100 ⇒ required mm 50 ≤ 100 ✓
///   (b) sibling on the reduced pool 100 ⇒ required mm 25 ≤ 100 ✓
/// Asserts the cross pool shrinks to 100 and the target bucket becomes 100.
#[tokio::test]
async fn set_position_isolated_splits_collateral_and_stays_healthy() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let ts = Pubkey::new_unique();
    let target_market = Pubkey::new_unique();
    let target_position = Pubkey::new_unique();
    let market2 = Pubkey::new_unique();
    let sibling = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(ts, trader_state(pid, trader.pubkey(), 200, 2));
    pt.add_account(target_market, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(target_position, position(pid, trader.pubkey(), target_market, 0, 10, 100, 0)); // cross
    pt.add_account(market2, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(sibling, position(pid, trader.pubkey(), market2, 0, 5, 100, 0)); // cross sibling
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_SET_POSITION_ISOLATED];
    data.extend_from_slice(&100u64.to_le_bytes()); // amount to isolate
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(target_market, false),
            AccountMeta::new(target_position, false),
            AccountMeta::new_readonly(market2, false),
            AccountMeta::new_readonly(sibling, false),
        ],
        data,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &trader], bh))
        .await
        .unwrap();

    let ts_acct = banks.get_account(ts).await.unwrap().unwrap();
    assert_eq!(get_u64(&ts_acct.data, TS_COLLATERAL), 100, "cross pool 200 − isolated 100");
    let tp = banks.get_account(target_position).await.unwrap().unwrap();
    assert_eq!(get_u64(&tp.data, POS_COLLATERAL), 100, "target isolated bucket = 100");
}

// ─── liquidate_portfolio_v2 e2e ───
const IX_LIQUIDATE_PORTFOLIO_V2: u8 = 96;

/// liquidate_portfolio_v2 e2e: a CROSS trader with two underwater longs (exec +
/// sibling, each 10 @200; mark 100, mmr 500bps) and a tiny cross pool (100). The
/// full-portfolio required ≈ 2·1050 = 2100 ≫ 100 ⇒ unhealthy, so liquidating
/// injects ONE forced-liquidation order (order_type 3, full exec size) into the
/// execution book. Asserts the book now holds exactly one active order.
#[tokio::test]
async fn liquidate_portfolio_v2_injects_on_unhealthy_portfolio() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let caller = Keypair::new();
    let trader = Pubkey::new_unique();
    let exec_market = Pubkey::new_unique();
    let (exec_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &exec_market.to_bytes()], &pid);
    let ts = Pubkey::new_unique();
    let exec_position = Pubkey::new_unique();
    let market2 = Pubkey::new_unique();
    let sibling = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();

    let mut exec_market_acct = market_full(pid, 100, 1, 500, 0, 0);
    put_key(&mut exec_market_acct.data, MKT_AUTHORITY, &authority.pubkey());

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(exec_market, exec_market_acct);
    pt.add_account(exec_book, init_book(pid, exec_market, base, quote, book_bump));
    pt.add_account(ts, trader_state(pid, trader, 100, 2)); // cross pool 100, 2 open
    pt.add_account(exec_position, position(pid, trader, exec_market, 0, 10, 200, 0)); // cross long
    pt.add_account(market2, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(sibling, position(pid, trader, market2, 0, 10, 200, 0)); // cross long
    let (banks, payer, bh) = pt.start().await;

    // set penalty 100bps (reward/auction/cooldown 0)
    let mut sp = vec![IX_SET_LIQ_PARAMS];
    sp.extend_from_slice(&100u32.to_le_bytes());
    sp.extend_from_slice(&0u32.to_le_bytes());
    sp.extend_from_slice(&0u64.to_le_bytes());
    sp.extend_from_slice(&0u64.to_le_bytes());
    let ix_p = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new(exec_market, false),
        ],
        data: sp,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix_p], Some(&payer.pubkey()), &[&payer, &authority], bh))
        .await
        .unwrap();

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(exec_market, false),
            AccountMeta::new(exec_book, false),
            AccountMeta::new_readonly(ts, false),
            AccountMeta::new_readonly(exec_position, false),
            AccountMeta::new_readonly(market2, false),
            AccountMeta::new_readonly(sibling, false),
        ],
        data: vec![IX_LIQUIDATE_PORTFOLIO_V2],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let mut bd = banks.get_account(exec_book).await.unwrap().unwrap().data;
    let handle = MarketBookHandle::from_account_data(&mut bd).unwrap();
    assert_eq!(handle.header.total_orders_active, 1, "one liquidation order injected");
}

// ─── JIT liquidation offer: cancel e2e (place is build-sbf-verified; its
// create_pda_account CPI can't be driven by solana-program-test 3.1 — the same
// NotEnoughAccountKeys limitation that affects all create/CPI instructions in
// the harness). cancel mutates+closes an existing account (no CPI), so its
// positive path IS testable here. ───
use flash_book_pin::seeds::JIT_LIQ_OFFER_SEED;
use flash_book_pin::state::JIT_LIQ_OFFER_DISC;
const IX_CANCEL_JIT: u8 = 98;
const JIT_NONCE: usize = 12;
const JIT_MARKET: usize = 16;
const JIT_MAKER: usize = 48;

/// cancel_jit_liquidation_offer: a pre-seeded offer is closed by its maker and
/// the rent is refunded (account gone / lamports 0).
#[tokio::test]
async fn jit_offer_cancel_closes_and_refunds() {
    let pid = Pubkey::new_unique();
    let maker = Keypair::new();
    let market = Pubkey::new_unique();
    let nonce: u32 = 7;
    let (jit_offer, _) = Pubkey::find_program_address(
        &[JIT_LIQ_OFFER_SEED, &market.to_bytes(), &maker.pubkey().to_bytes(), &nonce.to_le_bytes()],
        &pid,
    );

    // Pre-seed the offer (bound to its PDA via market/maker/nonce).
    let mut od = vec![0u8; 152];
    od[0..8].copy_from_slice(&JIT_LIQ_OFFER_DISC);
    od[JIT_NONCE..JIT_NONCE + 4].copy_from_slice(&nonce.to_le_bytes());
    put_key(&mut od, JIT_MARKET, &market);
    put_key(&mut od, JIT_MAKER, &maker.pubkey());

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(jit_offer, rent_account(od, pid));
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(maker.pubkey(), true),
            AccountMeta::new(jit_offer, false),
        ],
        data: vec![IX_CANCEL_JIT],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &maker], bh))
        .await
        .unwrap();

    let after = banks.get_account(jit_offer).await.unwrap();
    assert!(after.is_none() || after.unwrap().lamports == 0, "offer closed, rent refunded");
}

// ─── JIT auction consumption e2e (liquidate uses the best offer) ───
const JIT_REMAINING_OFF: usize = 128; // remaining_size_lots offset

fn jit_offer_acct(pid: Pubkey, market: Pubkey, side: u8, price: u64, remaining: u64) -> Account {
    let mut d = vec![0u8; 152];
    d[0..8].copy_from_slice(&flash_book_pin::state::JIT_LIQ_OFFER_DISC);
    d[9] = side; // closes positions of this side
    put_key(&mut d, 16, &market); // market @ 16
    // maker @ 48, target_trader @ 80 (zero = wildcard) — left zero
    put_u64(&mut d, 112, price); // offer_price_ticks
    put_u64(&mut d, 120, remaining); // max_size_lots
    put_u64(&mut d, JIT_REMAINING_OFF, remaining); // remaining_size_lots
    rent_account(d, pid)
}

/// liquidate_position_v2 with a JIT offer in remaining_accounts: the offer (side
/// 0 = closes longs, price 100 > synthetic 99) BEATS the synthetic limit, so the
/// liquidation injects at the offer price and reserves the offer's commitment.
/// Asserts the offer's remaining decrements by the closed size.
#[tokio::test]
async fn liquidate_position_v2_consumes_jit_offer() {
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
    let jit_offer = Pubkey::new_unique();

    let mut market_acct = market_full(pid, 100, 1, 500, 0, 0);
    put_key(&mut market_acct.data, MKT_AUTHORITY, &authority.pubkey());

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_acct);
    pt.add_account(market_book, init_book(pid, market, base_mint, quote_mint, book_bump));
    pt.add_account(position_liq, position_liq_acct(pid, market, liq_position));
    pt.add_account(liq_ts, trader_state(pid, liq_trader, 0, 1));
    pt.add_account(liq_position, position(pid, liq_trader, market, 0, 10, 200, 100)); // long, isolated 100
    pt.add_account(caller_ts, trader_state(pid, caller.pubkey(), 0, 0));
    // a JIT offer that closes longs (side 0) at 100 > synthetic (mark 100 − 1% = 99).
    pt.add_account(jit_offer, jit_offer_acct(pid, market, 0, 100, 50));
    let (banks, payer, bh) = pt.start().await;

    // penalty 100bps, no reward (so the assertion isolates the JIT path)
    let mut sp = vec![IX_SET_LIQ_PARAMS];
    sp.extend_from_slice(&100u32.to_le_bytes());
    sp.extend_from_slice(&0u32.to_le_bytes());
    sp.extend_from_slice(&0u64.to_le_bytes());
    sp.extend_from_slice(&0u64.to_le_bytes());
    let ix_p = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new(market, false),
        ],
        data: sp,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix_p], Some(&payer.pubkey()), &[&payer, &authority], bh))
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
            AccountMeta::new(jit_offer, false), // remaining_accounts: the JIT offer
        ],
        data: ld,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix_liq], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let o = banks.get_account(jit_offer).await.unwrap().unwrap();
    assert_eq!(get_u64(&o.data, JIT_REMAINING_OFF), 45, "offer remaining 50 − closed 5 (JIT auction fired)");
}

// ─── execute_trigger_order e2e ───
use flash_book_pin::state::TRIGGER_ORDER_V3_DISC;
const IX_EXECUTE_TRIGGER: u8 = 99;

fn trigger_acct(pid: Pubkey, trader: Pubkey, market: Pubkey, side: u8, kind: u8, size: u64, trigger_price: u64, limit: u64) -> Account {
    let mut d = vec![0u8; 136];
    d[0..8].copy_from_slice(&TRIGGER_ORDER_V3_DISC);
    put_key(&mut d, 8, &trader);   // trader @ 8
    put_key(&mut d, 40, &market);  // market @ 40
    put_u64(&mut d, 72, size);     // size_lots
    put_u64(&mut d, 80, trigger_price); // trigger_price_ticks
    put_u64(&mut d, 88, limit);    // limit_price_ticks
    d[122] = side;
    d[123] = kind;
    d[124] = 0x01; // FLAG_ACTIVE
    rent_account(d, pid)
}

/// execute_trigger_order: an ACTIVE kind-1 trigger (fire when mark >= 100) with
/// mark 100 fires — its limit order is injected into the book and FLAG_ACTIVE is
/// cleared (one-shot).
#[tokio::test]
async fn execute_trigger_order_fires_and_injects() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let trader = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let trigger = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0)); // mark 100
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    pt.add_account(trigger, trigger_acct(pid, trader, market, 0, 1, 5, 100, 100)); // kind1, trig 100, mark 100 → fired
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(trigger, false),
        ],
        data: vec![IX_EXECUTE_TRIGGER],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let mut bd = banks.get_account(market_book).await.unwrap().unwrap().data;
    let handle = MarketBookHandle::from_account_data(&mut bd).unwrap();
    assert_eq!(handle.header.total_orders_active, 1, "trigger order injected");
    let t = banks.get_account(trigger).await.unwrap().unwrap();
    assert_eq!(t.data[124] & 0x01, 0, "FLAG_ACTIVE cleared (one-shot)");
}

// ─── execute_twap_slice e2e ───
use flash_book_pin::state::TWAP_ORDER_V3_DISC;
const IX_EXECUTE_TWAP: u8 = 100;
const TWAP_EXECUTED: usize = 88; // size_executed_lots
const TWAP_FLAGS: usize = 147;

fn twap_acct(pid: Pubkey, trader: Pubkey, market: Pubkey, side: u8, slice: u64, total: u64, limit: u64) -> Account {
    let mut d = vec![0u8; 152];
    d[0..8].copy_from_slice(&TWAP_ORDER_V3_DISC);
    put_key(&mut d, 8, &trader);
    put_key(&mut d, 40, &market);
    put_u64(&mut d, 72, slice); // slice_size_lots
    put_u64(&mut d, 80, total); // total_size_lots
    // size_executed_lots @ 88 = 0
    put_u64(&mut d, 96, limit); // limit_price_ticks
    // slot_interval @ 112 = 0 (no throttle), end_slot @ 120 = 0, last_slice @ 128 = 0
    d[146] = side;
    d[TWAP_FLAGS] = 0x01; // FLAG_ACTIVE
    rent_account(d, pid)
}

/// execute_twap_slice: an ACTIVE TWAP (total 20, slice 5, interval 0) executes
/// one slice — 5 lots are injected into the book, size_executed advances to 5,
/// and the order STAYS active (5 < 20).
#[tokio::test]
async fn execute_twap_slice_injects_and_advances() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let trader = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let twap = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0)); // min_base_lots 0
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    pt.add_account(twap, twap_acct(pid, trader, market, 0, 5, 20, 100));
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(twap, false),
        ],
        data: vec![IX_EXECUTE_TWAP],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let t = banks.get_account(twap).await.unwrap().unwrap();
    assert_eq!(get_u64(&t.data, TWAP_EXECUTED), 5, "one 5-lot slice executed");
    assert_eq!(t.data[TWAP_FLAGS] & 0x01, 0x01, "still active (5 < 20)");
    let mut bd = banks.get_account(market_book).await.unwrap().unwrap().data;
    let handle = MarketBookHandle::from_account_data(&mut bd).unwrap();
    assert_eq!(handle.header.total_orders_active, 1, "slice order injected");
}

// ─── reduce-only trigger execution e2e ───
/// A reduce-only trigger (FLAG_REDUCE_ONLY | ACTIVE, side 1 = close a long) fires
/// only with a valid opposite-side position. position long 10 vs trigger close 5
/// ⇒ valid; mark 100 ≥ trigger 100 ⇒ fired. Asserts the order is injected and
/// ACTIVE is cleared (REDUCE_ONLY retained).
#[tokio::test]
async fn execute_trigger_order_reduce_only_fires_with_position() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let trader = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let trigger = Pubkey::new_unique();
    let pos_key = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    let mut td = vec![0u8; 136];
    td[0..8].copy_from_slice(&TRIGGER_ORDER_V3_DISC);
    put_key(&mut td, 8, &trader);
    put_key(&mut td, 40, &market);
    put_u64(&mut td, 72, 5); // size
    put_u64(&mut td, 80, 100); // trigger_price
    put_u64(&mut td, 88, 100); // limit
    td[122] = 1; // side = close a long (opposite of the position)
    td[123] = 1; // kind = fire when mark >= trigger
    td[124] = 0x01 | 0x02; // ACTIVE | REDUCE_ONLY
    pt.add_account(trigger, rent_account(td, pid));
    pt.add_account(pos_key, position(pid, trader, market, 0, 10, 100, 0)); // long 10 (opposite side)
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(trigger, false),
            AccountMeta::new_readonly(pos_key, false), // reduce-only: the position to close
        ],
        data: vec![IX_EXECUTE_TRIGGER],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let mut bd = banks.get_account(market_book).await.unwrap().unwrap().data;
    let handle = MarketBookHandle::from_account_data(&mut bd).unwrap();
    assert_eq!(handle.header.total_orders_active, 1, "reduce-only trigger order injected");
    let t = banks.get_account(trigger).await.unwrap().unwrap();
    assert_eq!(t.data[124] & 0x01, 0, "ACTIVE cleared");
    assert_eq!(t.data[124] & 0x02, 0x02, "REDUCE_ONLY retained");
}

// ─── iceberg replenish + cancel e2e ───
use flash_book_pin::state::ICEBERG_ORDER_V3_DISC;
const IX_REPLENISH_ICEBERG: u8 = 102;
const IX_CANCEL_ICEBERG: u8 = 103;
const ICE_REMAINING: usize = 88; // remaining_lots
const ICE_FLAGS: usize = 131;

fn iceberg_acct(
    pid: Pubkey,
    trader: Pubkey,
    market: Pubkey,
    side: u8,
    limit: u64,
    total: u64,
    remaining: u64,
    displayed: u64,
) -> Account {
    let mut d = vec![0u8; 136];
    d[0..8].copy_from_slice(&ICEBERG_ORDER_V3_DISC);
    put_key(&mut d, 8, &trader);
    put_key(&mut d, 40, &market);
    put_u64(&mut d, 72, limit); // limit_ticks
    put_u64(&mut d, 80, total); // total_size_lots
    put_u64(&mut d, 88, remaining); // remaining_lots
    put_u64(&mut d, 96, displayed); // displayed_size_lots
    // child_order_seq @ 104, created @ 112, expires @ 120 = 0
    d[130] = side;
    d[ICE_FLAGS] = 0x01; // FLAG_ACTIVE
    rent_account(d, pid)
}

/// replenish_iceberg: an ACTIVE iceberg (total 20, displayed 5, remaining 15)
/// rests its next 5-lot chunk on the book; remaining → 10; stays ACTIVE.
#[tokio::test]
async fn replenish_iceberg_injects_next_chunk_and_advances() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let trader = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let iceberg = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    pt.add_account(iceberg, iceberg_acct(pid, trader, market, 0, 100, 20, 15, 5));
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(iceberg, false),
        ],
        data: vec![IX_REPLENISH_ICEBERG],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let a = banks.get_account(iceberg).await.unwrap().unwrap();
    assert_eq!(get_u64(&a.data, ICE_REMAINING), 10, "remaining decremented by the 5-lot chunk");
    assert_eq!(a.data[ICE_FLAGS] & 0x01, 0x01, "still active (10 left)");
    let mut bd = banks.get_account(market_book).await.unwrap().unwrap().data;
    let handle = MarketBookHandle::from_account_data(&mut bd).unwrap();
    assert_eq!(handle.header.total_orders_active, 1, "next chunk rested on the book");
}

/// replenish_iceberg: the FINAL chunk (displayed 5, remaining 5) clears
/// FLAG_ACTIVE once remaining reaches 0.
#[tokio::test]
async fn replenish_iceberg_clears_active_when_fully_placed() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let trader = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let iceberg = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    pt.add_account(iceberg, iceberg_acct(pid, trader, market, 1, 100, 20, 5, 5));
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(iceberg, false),
        ],
        data: vec![IX_REPLENISH_ICEBERG],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let a = banks.get_account(iceberg).await.unwrap().unwrap();
    assert_eq!(get_u64(&a.data, ICE_REMAINING), 0, "fully placed");
    assert_eq!(a.data[ICE_FLAGS] & 0x01, 0, "ACTIVE cleared");
}

/// cancel_iceberg: the trader closes their iceberg and reclaims rent — the
/// account is gone afterward and the trader's lamports increase by the rent.
#[tokio::test]
async fn cancel_iceberg_closes_and_refunds() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let market = Pubkey::new_unique();
    let iceberg = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let ice = iceberg_acct(pid, trader.pubkey(), market, 0, 100, 20, 15, 5);
    let rent_lamports = ice.lamports;
    pt.add_account(iceberg, ice);
    pt.add_account(
        trader.pubkey(),
        // owner = System Program (its id is 32 zero bytes).
        Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let before = banks.get_account(trader.pubkey()).await.unwrap().unwrap().lamports;
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new(iceberg, false),
        ],
        data: vec![IX_CANCEL_ICEBERG],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &trader], bh))
        .await
        .unwrap();

    assert!(banks.get_account(iceberg).await.unwrap().is_none(), "iceberg account closed");
    let after = banks.get_account(trader.pubkey()).await.unwrap().unwrap().lamports;
    assert_eq!(after, before + rent_lamports, "trader reclaimed the iceberg rent");
}

// ─── place_bracket_order validation e2e ───
// place_bracket creates two trigger PDAs via the create_pda CPI (build-sbf only
// under solana-program-test). But the VALIDATION gate runs BEFORE any CPI/book
// write, so invalid-param rejections ARE e2e-testable: they must fail with a
// clean InvalidArgument (custom 0), never reach the CPI, and leave the book
// untouched. The happy path is build-sbf-verified; execution reuses the
// already-tested execute_trigger_order reduce-only path.
const IX_PLACE_BRACKET: u8 = 104;

#[allow(clippy::too_many_arguments)]
fn bracket_data(
    parent_side: u8, sub_index: u8, tp_id: u8, sl_id: u8, size: u64,
    parent_limit: u64, tp_trig: u64, tp_limit: u64, sl_trig: u64, sl_limit: u64, expires: u64,
) -> Vec<u8> {
    let mut d = vec![IX_PLACE_BRACKET];
    d.push(parent_side);
    d.push(sub_index);
    d.push(tp_id);
    d.push(sl_id);
    d.extend_from_slice(&size.to_le_bytes());
    d.extend_from_slice(&parent_limit.to_le_bytes());
    d.extend_from_slice(&tp_trig.to_le_bytes());
    d.extend_from_slice(&tp_limit.to_le_bytes());
    d.extend_from_slice(&sl_trig.to_le_bytes());
    d.extend_from_slice(&sl_limit.to_le_bytes());
    d.extend_from_slice(&expires.to_le_bytes());
    d
}

async fn run_bracket(data: Vec<u8>) -> std::result::Result<(), solana_program_test::BanksClientError> {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let id0 = data[2];
    let id1 = data[3];
    let (tp, _) = Pubkey::find_program_address(
        &[b"trigger_v3", &market.to_bytes(), &trader.pubkey().to_bytes(), &[id0]], &pid);
    let (sl, _) = Pubkey::find_program_address(
        &[b"trigger_v3", &market.to_bytes(), &trader.pubkey().to_bytes(), &[id1]], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0)); // min_base_lots 0, tick 1
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    pt.add_account(
        trader.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(tp, false),
            AccountMeta::new(sl, false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8; 32]), false),
        ],
        data,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &trader], bh))
        .await
}

/// tp_trigger_id == sl_trigger_id → rejected (the two children would collide).
#[tokio::test]
async fn place_bracket_rejects_equal_trigger_ids() {
    // long: tp above (110), sl below (90); ids equal → reject before any CPI.
    let d = bracket_data(0, 0, 7, 7, 5, 100, 110, 110, 90, 90, 0);
    assert!(run_bracket(d).await.is_err(), "equal tp/sl ids must be rejected");
}

/// LONG with tp NOT above parent → rejected (a take-profit must be above entry).
#[tokio::test]
async fn place_bracket_rejects_long_tp_below_parent() {
    // long parent 100, tp 95 (below — invalid), sl 90.
    let d = bracket_data(0, 0, 1, 2, 5, 100, 95, 95, 90, 90, 0);
    assert!(run_bracket(d).await.is_err(), "long tp below parent must be rejected");
}

/// SHORT with sl NOT above parent → rejected (a short's stop is above entry).
#[tokio::test]
async fn place_bracket_rejects_short_sl_below_parent() {
    // short parent 100, tp 90 (below, ok for short), sl 95 (below — invalid).
    let d = bracket_data(1, 0, 1, 2, 5, 100, 90, 90, 95, 95, 0);
    assert!(run_bracket(d).await.is_err(), "short sl below parent must be rejected");
}

// ─── create_vault_v3 validation e2e ───
// create_vault creates the vault PDA via the create_pda CPI (build-sbf only),
// but the perf-fee cap is checked BEFORE the CPI — so an over-cap fee must fail
// with a clean InvalidArgument, never reaching account creation.
const IX_CREATE_VAULT: u8 = 105;

#[tokio::test]
async fn create_vault_rejects_perf_fee_over_cap() {
    let pid = Pubkey::new_unique();
    let strategist = Keypair::new();
    let vault_id = 0u8;
    let (vault, _) = Pubkey::find_program_address(
        &[b"vault_v3", &strategist.pubkey().to_bytes(), &[vault_id]], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(
        strategist.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_CREATE_VAULT, vault_id];
    data.extend_from_slice(&6_000u32.to_le_bytes()); // perf_fee_bps 60% > 50% cap
    data.extend_from_slice(&[0u8; 32]); // name
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(strategist.pubkey(), true),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8; 32]), false),
        ],
        data,
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &strategist], bh))
        .await;
    assert!(r.is_err(), "perf fee above the 50% cap must be rejected");
    assert!(banks.get_account(vault).await.unwrap().is_none(), "vault not created on reject");
}

// ─── vault_open_trader_state_v3 auth e2e ───
// Opening a vault's TraderState creates a PDA via the create_pda CPI (build-sbf
// only), but the strategist-auth gate runs BEFORE the CPI: a signer who is NOT
// the vault's strategist must be rejected with a clean InvalidArgument, and no
// TraderState is created.
use flash_book_pin::state::VAULT_V3_DISC;
const IX_VAULT_OPEN_TS: u8 = 106;

fn vault_acct(pid: Pubkey, strategist: Pubkey, vault_id: u8) -> Account {
    let mut d = vec![0u8; 152];
    d[0..8].copy_from_slice(&VAULT_V3_DISC);
    put_key(&mut d, 8, &strategist); // strategist @ 8
    // name @ 40, accounting (5*u64) @ 72..112, perf_fee_bps @ 112, bump @ 116
    d[117] = vault_id; // vault_id @ 117
    d[118] = 1; // accept_deposits @ 118
    rent_account(d, pid)
}

#[tokio::test]
async fn vault_open_trader_state_rejects_non_strategist() {
    let pid = Pubkey::new_unique();
    let real_strategist = Pubkey::new_unique();
    let imposter = Keypair::new();
    let vault_id = 0u8;
    let (vault, _) = Pubkey::find_program_address(
        &[b"vault_v3", &real_strategist.to_bytes(), &[vault_id]], &pid);
    let (vault_ts, _) =
        Pubkey::find_program_address(&[b"trader_state", &vault.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(vault, vault_acct(pid, real_strategist, vault_id));
    pt.add_account(
        imposter.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(imposter.pubkey(), true), // NOT the strategist
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new(vault_ts, false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8; 32]), false),
        ],
        data: vec![IX_VAULT_OPEN_TS],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh))
        .await;
    assert!(r.is_err(), "non-strategist must be rejected");
    assert!(banks.get_account(vault_ts).await.unwrap().is_none(), "no TraderState created on reject");
}

// ─── init_vault_position_v3 guard e2e ───
// Creates the position PDA via create_pda CPI (build-sbf only), but the
// vault disc check runs BEFORE the CPI: a vault account with the wrong
// discriminator must be rejected cleanly, with no position created.
const IX_INIT_VAULT_POSITION: u8 = 107;

#[tokio::test]
async fn init_vault_position_rejects_bad_vault_disc() {
    let pid = Pubkey::new_unique();
    let depositor = Keypair::new();
    let vault = Pubkey::new_unique();
    let (position, _) = Pubkey::find_program_address(
        &[b"vault_position_v3", &vault.to_bytes(), &depositor.pubkey().to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    // Vault-sized account owned by the program but with a GARBAGE discriminator.
    let mut bad = vec![0u8; 152];
    bad[0..8].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0]);
    pt.add_account(vault, rent_account(bad, pid));
    pt.add_account(
        depositor.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(depositor.pubkey(), true),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8; 32]), false),
        ],
        data: vec![IX_INIT_VAULT_POSITION],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &depositor], bh))
        .await;
    assert!(r.is_err(), "bad vault discriminator must be rejected");
    assert!(banks.get_account(position).await.unwrap().is_none(), "no position created on reject");
}

// ─── vault_deposit_v3 deposits-closed e2e ───
// The full deposit moves tokens via an SPL CPI (build-sbf only) + mints shares
// via the Kani-proved vault_math. But the `accept_deposits == 1` gate runs
// BEFORE the token CPI, so a vault with deposits closed must be rejected
// cleanly. (The share-mint correctness itself is covered by vault_math's host
// tests + proof.)
use flash_book_pin::seeds::{TRADER_STATE_SEED, VAULT_POSITION_SEED, VAULT_SEED};
use flash_book_pin::state::VAULT_POSITION_V3_DISC;
use flash_book_pin::cpi::TOKEN_PROGRAM_ID;
const IX_VAULT_DEPOSIT: u8 = 108;

fn vault_with_accept(pid: Pubkey, strategist: Pubkey, vault_id: u8, accept: u8, shares: u64) -> Account {
    let mut d = vec![0u8; 152];
    d[0..8].copy_from_slice(&VAULT_V3_DISC);
    put_key(&mut d, 8, &strategist);
    put_u64(&mut d, 72, shares); // shares_outstanding
    d[116] = 0; // bump
    d[117] = vault_id;
    d[118] = accept; // accept_deposits
    rent_account(d, pid)
}

fn vault_position(pid: Pubkey, vault: Pubkey, depositor: Pubkey) -> Account {
    let mut d = vec![0u8; 120];
    d[0..8].copy_from_slice(&VAULT_POSITION_V3_DISC);
    put_key(&mut d, 8, &vault);
    put_key(&mut d, 40, &depositor);
    rent_account(d, pid)
}

#[tokio::test]
async fn vault_deposit_rejects_when_deposits_closed() {
    let pid = Pubkey::new_unique();
    let depositor = Keypair::new();
    let strategist = Pubkey::new_unique();
    let vault_id = 0u8;
    let (vault, _) = Pubkey::find_program_address(&[VAULT_SEED, &strategist.to_bytes(), &[vault_id]], &pid);
    let (vault_ts, _) = Pubkey::find_program_address(&[TRADER_STATE_SEED, &vault.to_bytes()], &pid);
    let (position, _) = Pubkey::find_program_address(
        &[VAULT_POSITION_SEED, &vault.to_bytes(), &depositor.pubkey().to_bytes()], &pid);
    let (insurance, _) = Pubkey::find_program_address(&[INSURANCE_SEED], &pid);
    let quote_vault = Pubkey::new_unique();
    let depositor_ata = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(vault, vault_with_accept(pid, strategist, vault_id, 0, 0)); // accept=0
    pt.add_account(vault_ts, trader_state(pid, vault, 0, 0)); // TraderState keyed to vault
    pt.add_account(position, vault_position(pid, vault, depositor.pubkey()));
    pt.add_account(insurance, insurance_full(pid, 0, 0));
    pt.add_account(
        depositor.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_VAULT_DEPOSIT];
    data.extend_from_slice(&1_000u64.to_le_bytes());
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(depositor.pubkey(), true),
            AccountMeta::new(vault, false),
            AccountMeta::new(vault_ts, false),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new(depositor_ata, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(TOKEN_PROGRAM_ID), false),
        ],
        data,
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &depositor], bh))
        .await;
    assert!(r.is_err(), "deposit into a closed vault must be rejected");
}

// ─── vault_withdraw_v3 flat-gate e2e ───
// Withdraw burns shares + PDA-signed SPL release (build-sbf only). The H-6
// "redemption requires flat" gate runs BEFORE the release: a vault holding an
// open position (open_positions != 0) must reject redemptions, so an early
// depositor can't exit at an inflated NAV. (payout correctness is covered by
// vault_math.) Reuses vault_with_accept / vault_position / trader_state helpers.
const IX_VAULT_WITHDRAW: u8 = 109;

fn vault_position_shares(pid: Pubkey, vault: Pubkey, depositor: Pubkey, shares: u64) -> Account {
    let mut d = vec![0u8; 120];
    d[0..8].copy_from_slice(&VAULT_POSITION_V3_DISC);
    put_key(&mut d, 8, &vault);
    put_key(&mut d, 40, &depositor);
    put_u64(&mut d, 72, shares); // shares
    rent_account(d, pid)
}

#[tokio::test]
async fn vault_withdraw_rejects_when_not_flat() {
    let pid = Pubkey::new_unique();
    let depositor = Keypair::new();
    let strategist = Pubkey::new_unique();
    let vault_id = 0u8;
    let (vault, _) = Pubkey::find_program_address(&[VAULT_SEED, &strategist.to_bytes(), &[vault_id]], &pid);
    let (vault_ts, _) = Pubkey::find_program_address(&[TRADER_STATE_SEED, &vault.to_bytes()], &pid);
    let (position, _) = Pubkey::find_program_address(
        &[VAULT_POSITION_SEED, &vault.to_bytes(), &depositor.pubkey().to_bytes()], &pid);
    let (insurance, _) = Pubkey::find_program_address(&[INSURANCE_SEED], &pid);
    let quote_vault = Pubkey::new_unique();
    let depositor_ata = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(vault, vault_with_accept(pid, strategist, vault_id, 1, 100)); // 100 shares
    pt.add_account(vault_ts, trader_state(pid, vault, 1_000, 1)); // NAV 1000, open_positions=1 (NOT flat)
    pt.add_account(position, vault_position_shares(pid, vault, depositor.pubkey(), 50)); // owns 50
    pt.add_account(insurance, insurance_full(pid, 0, 0));
    pt.add_account(
        depositor.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_VAULT_WITHDRAW];
    data.extend_from_slice(&10u64.to_le_bytes()); // burn 10 (<= owned 50)
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(depositor.pubkey(), true),
            AccountMeta::new(vault, false),
            AccountMeta::new(vault_ts, false),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(quote_vault, false),
            AccountMeta::new(depositor_ata, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(TOKEN_PROGRAM_ID), false),
        ],
        data,
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &depositor], bh))
        .await;
    assert!(r.is_err(), "redemption from a non-flat vault must be rejected (H-6)");
    // shares untouched on reject.
    let p = banks.get_account(position).await.unwrap().unwrap();
    assert_eq!(get_u64(&p.data, 72), 50, "shares unchanged on reject");
}

// ─── settle_vault_perf_fee_v3 e2e (positive — no CPI) ───
// Pure HWM/perf-fee math + state writes, NO token CPI or PDA creation → fully
// e2e-testable. NAV doubled (2000 over 1000 shares ⇒ nav/share 2.0 vs HWM 1.0);
// 20% of the 1000-qlot gain = 200 qlots ⇒ mint 200*1000/2000 = 100 fee shares
// to the strategist, then reset HWM to the post-mint nav/share.
const IX_SETTLE_PERF_FEE: u8 = 110;

fn vault_perf(pid: Pubkey, strategist: Pubkey, shares: u64, hwm_x6: u64, perf_bps: u32) -> Account {
    let mut d = vec![0u8; 152];
    d[0..8].copy_from_slice(&VAULT_V3_DISC);
    put_key(&mut d, 8, &strategist);
    put_u64(&mut d, 72, shares); // shares_outstanding
    put_u64(&mut d, 88, hwm_x6); // hwm_nav_per_share_u64x6
    d[112..116].copy_from_slice(&perf_bps.to_le_bytes()); // perf_fee_bps @ 112
    d[118] = 1; // accept_deposits
    rent_account(d, pid)
}

#[tokio::test]
async fn settle_perf_fee_mints_on_new_high() {
    let pid = Pubkey::new_unique();
    let strategist = Keypair::new();
    let vault_id = 0u8;
    let (vault, _) = Pubkey::find_program_address(
        &[VAULT_SEED, &strategist.pubkey().to_bytes(), &[vault_id]], &pid);
    let (vault_ts, _) = Pubkey::find_program_address(&[TRADER_STATE_SEED, &vault.to_bytes()], &pid);
    let (sp, _) = Pubkey::find_program_address(
        &[VAULT_POSITION_SEED, &vault.to_bytes(), &strategist.pubkey().to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(vault, vault_perf(pid, strategist.pubkey(), 1_000, 1_000_000, 2_000)); // 1.0 HWM, 20%
    pt.add_account(vault_ts, trader_state(pid, vault, 2_000, 0)); // NAV 2000, flat
    pt.add_account(sp, vault_position_shares(pid, vault, strategist.pubkey(), 0)); // strategist record
    pt.add_account(
        strategist.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(strategist.pubkey(), true),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(vault_ts, false),
            AccountMeta::new(sp, false),
        ],
        data: vec![IX_SETTLE_PERF_FEE],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &strategist], bh))
        .await
        .unwrap();

    let sp_acct = banks.get_account(sp).await.unwrap().unwrap();
    assert_eq!(get_u64(&sp_acct.data, 72), 100, "strategist minted 100 fee shares");
    let v = banks.get_account(vault).await.unwrap().unwrap();
    assert_eq!(get_u64(&v.data, 72), 1_100, "shares_outstanding grew by the fee shares");
    // new HWM = floor(2000 * 1e6 / 1100) = 1_818_181
    assert_eq!(get_u64(&v.data, 88), 1_818_181, "HWM reset to post-mint nav/share");
}

// ─── vault_place_order_v3 + vault_cancel_order_v3 e2e (positive, no CPI) ───
// Both operate directly on the book (no token CPI / PDA creation) → fully
// e2e-testable. Place a vault-PDA order, then cancel it — the cancel only
// succeeds because the resting order's trader == the vault (binding proof).
use flash_book_pin::book::encode_order_id;
const IX_VAULT_PLACE: u8 = 111;
const IX_VAULT_CANCEL: u8 = 112;

#[tokio::test]
async fn vault_place_then_cancel_round_trip() {
    let pid = Pubkey::new_unique();
    let strategist = Keypair::new();
    let vault_id = 0u8;
    let (vault, _) = Pubkey::find_program_address(
        &[VAULT_SEED, &strategist.pubkey().to_bytes(), &[vault_id]], &pid);
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(vault, vault_with_accept(pid, strategist.pubkey(), vault_id, 1, 0));
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0)); // mark 100, tick 1, status active
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    pt.add_account(
        strategist.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    // place: bid side 0, size 5 @ 100 (== mark, within band).
    let mut pdata = vec![IX_VAULT_PLACE, 0]; // side 0
    pdata.extend_from_slice(&5u64.to_le_bytes()); // size
    pdata.extend_from_slice(&100u64.to_le_bytes()); // limit
    pdata.extend_from_slice(&0u64.to_le_bytes()); // expires
    pdata.push(0); // flags
    let place = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(strategist.pubkey(), true),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
        ],
        data: pdata,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[place], Some(&payer.pubkey()), &[&payer, &strategist], bh))
        .await
        .unwrap();

    let mut bd = banks.get_account(market_book).await.unwrap().unwrap().data;
    assert_eq!(
        MarketBookHandle::from_account_data(&mut bd).unwrap().header.total_orders_active,
        1, "vault order rested"
    );

    // cancel: first bid has seq 1 → order_id = encode_order_id(100, 1, true).
    let order_id = encode_order_id(100, 1, true);
    let mut cdata = vec![IX_VAULT_CANCEL, 0]; // side 0
    cdata.extend_from_slice(&order_id.to_le_bytes());
    let cancel = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(strategist.pubkey(), true),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
        ],
        data: cdata,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[cancel], Some(&payer.pubkey()), &[&payer, &strategist], bh))
        .await
        .unwrap();

    let mut bd2 = banks.get_account(market_book).await.unwrap().unwrap().data;
    assert_eq!(
        MarketBookHandle::from_account_data(&mut bd2).unwrap().header.total_orders_active,
        0, "vault order cancelled"
    );
}

#[tokio::test]
async fn vault_place_rejects_non_strategist() {
    let pid = Pubkey::new_unique();
    let real = Pubkey::new_unique();
    let imposter = Keypair::new();
    let vault_id = 0u8;
    let (vault, _) = Pubkey::find_program_address(&[VAULT_SEED, &real.to_bytes(), &[vault_id]], &pid);
    let market = Pubkey::new_unique();
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let (market_book, book_bump) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(vault, vault_with_accept(pid, real, vault_id, 1, 0)); // strategist = real
    pt.add_account(market, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(market_book, init_book(pid, market, base, quote, book_bump));
    pt.add_account(
        imposter.pubkey(),
        Account { lamports: 5_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let mut pdata = vec![IX_VAULT_PLACE, 0];
    pdata.extend_from_slice(&5u64.to_le_bytes());
    pdata.extend_from_slice(&100u64.to_le_bytes());
    pdata.extend_from_slice(&0u64.to_le_bytes());
    pdata.push(0);
    let place = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(imposter.pubkey(), true), // NOT the strategist
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
        ],
        data: pdata,
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[place], Some(&payer.pubkey()), &[&payer, &imposter], bh))
        .await;
    assert!(r.is_err(), "non-strategist place must be rejected");
}

// ─── update_trailing_stop e2e (positive, no CPI) ───
// Pure ratchet + state write, no CPI → fully e2e-testable. A long trailing stop
// (kind 0, 5% offset, prior anchor 1000 / trigger 950) sees the mark rise to
// 1200 ⇒ anchor ratchets to 1200 and the stop tightens to 1140 (1200 − 60).
const IX_UPDATE_TRAILING: u8 = 113;

fn trailing_trigger(pid: Pubkey, market: Pubkey, kind: u8, offset_bps: u16, anchor: u64, trigger_price: u64) -> Account {
    let mut d = vec![0u8; 136];
    d[0..8].copy_from_slice(&TRIGGER_ORDER_V3_DISC);
    put_key(&mut d, 8, &Pubkey::new_unique()); // trader
    put_key(&mut d, 40, &market);
    put_u64(&mut d, 80, trigger_price); // trigger_price_ticks
    d[123] = kind;
    d[124] = 0x01; // FLAG_ACTIVE
    d[126..128].copy_from_slice(&offset_bps.to_le_bytes()); // trailing_offset_bps @ 126
    put_u64(&mut d, 128, anchor); // trailing_anchor_ticks @ 128
    rent_account(d, pid)
}

#[tokio::test]
async fn update_trailing_stop_ratchets_long_up() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let market = Pubkey::new_unique();
    let trigger = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 1_200, 1, 500, 0, 0)); // mark 1200, tick 1
    pt.add_account(trigger, trailing_trigger(pid, market, 0, 500, 1_000, 950)); // long, 5%, anchor 1000
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(trigger, false),
        ],
        data: vec![IX_UPDATE_TRAILING],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let t = banks.get_account(trigger).await.unwrap().unwrap();
    assert_eq!(get_u64(&t.data, 80), 1_140, "stop tightened to mark − 5%");
    assert_eq!(get_u64(&t.data, 128), 1_200, "anchor ratcheted to the new high");
}

#[tokio::test]
async fn update_trailing_stop_no_progress_is_noop() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let market = Pubkey::new_unique();
    let trigger = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 900, 1, 500, 0, 0)); // mark 900 < anchor 1000
    pt.add_account(trigger, trailing_trigger(pid, market, 0, 500, 1_000, 950));
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(trigger, false),
        ],
        data: vec![IX_UPDATE_TRAILING],
    };
    // succeeds (idempotent) but changes nothing.
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let t = banks.get_account(trigger).await.unwrap().unwrap();
    assert_eq!(get_u64(&t.data, 80), 950, "trigger unchanged on no progress");
    assert_eq!(get_u64(&t.data, 128), 1_000, "anchor unchanged");
}

// ─── update_oracle_quorum e2e (positive, no CPI) ───
// Validate + median-select 3 oracle sources, then set the mark. No CPI → fully
// e2e-testable. Gates off (limits 0) ⇒ any 3 prices accepted; median of
// [120,100,90] = 100 becomes the mark (was 0).
use flash_book_pin::state::ORACLE_CONFIG_DISC;
const IX_UPDATE_ORACLE_QUORUM: u8 = 114;

fn oracle_config_acct(pid: Pubkey, market: Pubkey, max_stale: u32, max_conf: u32, max_disp: u32) -> Account {
    let mut d = vec![0u8; 88];
    d[0..8].copy_from_slice(&ORACLE_CONFIG_DISC);
    put_key(&mut d, 8, &market);
    put_u32(&mut d, 72, max_stale);   // max_staleness_seconds
    put_u32(&mut d, 76, max_conf);    // max_confidence_bps
    put_u32(&mut d, 84, max_disp);    // max_dispersion_bps
    rent_account(d, pid)
}

#[tokio::test]
async fn update_oracle_quorum_sets_median_mark() {
    let pid = Pubkey::new_unique();
    let sequencer = Keypair::new();
    let market = Pubkey::new_unique();
    let oracle_config = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_account(pid, sequencer.pubkey())); // mark 0, sequencer set
    pt.add_account(oracle_config, oracle_config_acct(pid, market, 0, 0, 0)); // gates off
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_UPDATE_ORACLE_QUORUM];
    for p in [120u64, 100, 90] { data.extend_from_slice(&p.to_le_bytes()); }  // prices
    for c in [0u64, 0, 0] { data.extend_from_slice(&c.to_le_bytes()); }       // confidences
    for t in [0u64, 0, 0] { data.extend_from_slice(&t.to_le_bytes()); }       // published_at
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(sequencer.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(oracle_config, false),
        ],
        data,
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &sequencer], bh))
        .await
        .unwrap();

    let m = banks.get_account(market).await.unwrap().unwrap();
    assert_eq!(get_u64(&m.data, MKT_MARK), 100, "mark set to the median of the 3 sources");
}

#[tokio::test]
async fn update_oracle_quorum_rejects_wide_dispersion() {
    let pid = Pubkey::new_unique();
    let sequencer = Keypair::new();
    let market = Pubkey::new_unique();
    let oracle_config = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_account(pid, sequencer.pubkey()));
    pt.add_account(oracle_config, oracle_config_acct(pid, market, 0, 0, 1_000)); // 10% dispersion cap
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_UPDATE_ORACLE_QUORUM];
    for p in [90u64, 100, 110] { data.extend_from_slice(&p.to_le_bytes()); } // 20% spread > cap
    for _ in 0..6 { data.extend_from_slice(&0u64.to_le_bytes()); }
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(sequencer.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(oracle_config, false),
        ],
        data,
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &sequencer], bh))
        .await;
    assert!(r.is_err(), "dispersion beyond the cap must be rejected");
    let m = banks.get_account(market).await.unwrap().unwrap();
    assert_eq!(get_u64(&m.data, MKT_MARK), 0, "mark untouched on reject");
}

// ─── update_oracle_from_pyth e2e (positive, real fixture) ───
// Feeds the REAL captured PriceUpdateV2 fixture (price 6_500_123, exp -8) through
// the parser + conversion. tick_decimals 6 ⇒ scale -2 ⇒ mark = 6_500_123/100 =
// 65_001. The price account is owned by the Pyth receiver program. No CPI →
// fully e2e-testable.
const IX_UPDATE_ORACLE_FROM_PYTH: u8 = 115;
const PYTH_RECEIVER_ID: [u8; 32] = [
    12, 183, 250, 187, 82, 247, 166, 72, 187, 91, 49, 125, 154, 1, 139, 144, 87, 203, 2, 71, 116,
    250, 254, 1, 230, 196, 223, 152, 204, 56, 88, 129,
];
const PYTH_FIXTURE: [u8; 133] = [
    34, 241, 35, 99, 157, 126, 244, 205, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171,
    171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171, 171,
    171, 171, 1, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
    17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 27, 47, 99, 0, 0, 0, 0, 0, 146, 16, 0, 0, 0, 0, 0,
    0, 248, 255, 255, 255, 128, 225, 78, 104, 0, 0, 0, 0, 127, 225, 78, 104, 0, 0, 0, 0, 184, 42,
    99, 0, 0, 0, 0, 0, 204, 16, 0, 0, 0, 0, 0, 0, 99, 0, 0, 0, 0, 0, 0, 0,
];

// oracle_config with source=Pyth, feed=[0x11;32], huge staleness, conf gate off.
fn pyth_oracle_config(pid: Pubkey, market: Pubkey, tick_decimals: i8) -> Account {
    let mut d = vec![0u8; 88];
    d[0..8].copy_from_slice(&ORACLE_CONFIG_DISC);
    put_key(&mut d, 8, &market);
    d[40..72].copy_from_slice(&[0x11u8; 32]); // pyth_price_feed_id
    put_u32(&mut d, 72, u32::MAX); // max_staleness_seconds (robust vs test clock)
    put_u32(&mut d, 76, 0); // max_confidence_bps = 0 (gate off)
    d[80] = tick_decimals as u8; // tick_decimals @ 80
    d[81] = 1; // source = Pyth
    rent_account(d, pid)
}

#[tokio::test]
async fn update_oracle_from_pyth_sets_converted_mark() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let market = Pubkey::new_unique();
    let oracle_config = Pubkey::new_unique();
    let price_update = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_account(pid, Pubkey::new_unique())); // mark 0
    pt.add_account(oracle_config, pyth_oracle_config(pid, market, 6));
    // Pyth price account: the real fixture, owned by the Pyth receiver program.
    pt.add_account(
        price_update,
        Account {
            lamports: 1_000_000,
            data: PYTH_FIXTURE.to_vec(),
            owner: Pubkey::new_from_array(PYTH_RECEIVER_ID),
            executable: false,
            rent_epoch: 0,
        },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(oracle_config, false),
            AccountMeta::new_readonly(price_update, false),
        ],
        data: vec![IX_UPDATE_ORACLE_FROM_PYTH],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await
        .unwrap();

    let m = banks.get_account(market).await.unwrap().unwrap();
    assert_eq!(get_u64(&m.data, MKT_MARK), 65_001, "mark = pyth price 6_500_123 * 10^(-8+6)");
}

#[tokio::test]
async fn update_oracle_from_pyth_rejects_wrong_owner() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let market = Pubkey::new_unique();
    let oracle_config = Pubkey::new_unique();
    let price_update = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_account(pid, Pubkey::new_unique()));
    pt.add_account(oracle_config, pyth_oracle_config(pid, market, 6));
    // Same fixture but owned by a RANDOM program → must be rejected.
    pt.add_account(
        price_update,
        Account {
            lamports: 1_000_000,
            data: PYTH_FIXTURE.to_vec(),
            owner: Pubkey::new_unique(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(oracle_config, false),
            AccountMeta::new_readonly(price_update, false),
        ],
        data: vec![IX_UPDATE_ORACLE_FROM_PYTH],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &caller], bh))
        .await;
    assert!(r.is_err(), "a non-Pyth-owned price account must be rejected");
    let m = banks.get_account(market).await.unwrap().unwrap();
    assert_eq!(get_u64(&m.data, MKT_MARK), 0, "mark untouched on reject");
}

// ─── stamp_book_liveness_baseline e2e (positive + negative, no CPI) ───
// Stamps the slot at which a DELEGATED book started running on the ER. No CPI —
// it only reads the book's owner (must be the delegation program) and writes the
// market. book_delegated_at_slot lives at Market offset 232.
use flash_book_pin::er::DELEGATION_PROGRAM_ID;
const IX_STAMP_LIVENESS: u8 = 116;
const MKT_BOOK_DELEGATED_AT: usize = 232;

#[tokio::test]
async fn stamp_liveness_records_baseline_for_delegated_book() {
    let pid = Pubkey::new_unique();
    let payer_signer = Keypair::new();
    let market = Pubkey::new_unique();
    let (market_book, _) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_account(pid, Pubkey::new_unique())); // book_delegated_at_slot 0
    // The book PDA, currently DELEGATED → owned by the delegation program.
    pt.add_account(
        market_book,
        Account {
            lamports: 1_000_000,
            data: vec![0u8; 16],
            owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID),
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        payer_signer.pubkey(),
        Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(payer_signer.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(market_book, false),
        ],
        data: vec![IX_STAMP_LIVENESS],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &payer_signer], bh))
        .await
        .unwrap();

    let m = banks.get_account(market).await.unwrap().unwrap();
    assert_ne!(get_u64(&m.data, MKT_BOOK_DELEGATED_AT), 0, "baseline slot stamped");
}

#[tokio::test]
async fn stamp_liveness_rejects_undelegated_book() {
    let pid = Pubkey::new_unique();
    let payer_signer = Keypair::new();
    let market = Pubkey::new_unique();
    let (market_book, _) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_account(pid, Pubkey::new_unique()));
    // Book owned by OUR program (i.e. NOT delegated) → no clock to start.
    pt.add_account(
        market_book,
        Account { lamports: 1_000_000, data: vec![0u8; 16], owner: pid, executable: false, rent_epoch: 0 },
    );
    pt.add_account(
        payer_signer.pubkey(),
        Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8; 32]), executable: false, rent_epoch: 0 },
    );
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(payer_signer.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(market_book, false),
        ],
        data: vec![IX_STAMP_LIVENESS],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &payer_signer], bh))
        .await;
    assert!(r.is_err(), "an undelegated book must be rejected");
    let m = banks.get_account(market).await.unwrap().unwrap();
    assert_eq!(get_u64(&m.data, MKT_BOOK_DELEGATED_AT), 0, "no baseline on reject");
}

// ─── book_permission e2e ───
// init/set/close call the MagicBlock permission CPI (build-sbf only). But the
// account validation + the idempotent-init no-op run BEFORE any CPI, so those
// paths ARE e2e-testable: full validation passes and init returns cleanly when
// the permission already exists (no CPI hit).
use flash_book_pin::er_permission::{
    EPHEMERAL_VAULT_ID, MAGIC_PROGRAM_ID, PERMISSION_PROGRAM_ID, PERMISSION_SEED,
};
const IX_INIT_BOOK_PERM: u8 = 117;
const IX_SET_BOOK_PRIVACY: u8 = 118;

fn book_perm_accounts(
    pid: Pubkey,
    authority: Pubkey,
    perm_lamports: u64,
) -> (Pubkey, Pubkey, Pubkey) {
    // returns (market, market_book, permission)
    let market = Pubkey::new_unique();
    let (market_book, _) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);
    let (permission, _) = Pubkey::find_program_address(
        &[PERMISSION_SEED, &market_book.to_bytes()],
        &Pubkey::new_from_array(PERMISSION_PROGRAM_ID),
    );
    let _ = (authority, perm_lamports);
    (market, market_book, permission)
}

#[tokio::test]
async fn init_book_permission_idempotent_noop_when_exists() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let (market, market_book, permission) = book_perm_accounts(pid, authority.pubkey(), 1);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    pt.add_account(market_book, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: pid, executable: false, rent_epoch: 0 });
    // permission ALREADY exists (lamports > 0) → init is a no-op (no CPI).
    pt.add_account(permission, Account { lamports: 1_000_000, data: vec![], owner: Pubkey::new_from_array(PERMISSION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    pt.add_account(Pubkey::new_from_array(EPHEMERAL_VAULT_ID), Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    pt.add_account(Pubkey::new_from_array(MAGIC_PROGRAM_ID), Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: true, rent_epoch: 0 });
    pt.add_account(Pubkey::new_from_array(PERMISSION_PROGRAM_ID), Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: true, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(permission, false),
            AccountMeta::new(Pubkey::new_from_array(EPHEMERAL_VAULT_ID), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(MAGIC_PROGRAM_ID), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(PERMISSION_PROGRAM_ID), false),
        ],
        data: vec![IX_INIT_BOOK_PERM],
    };
    banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &authority], bh))
        .await
        .unwrap(); // succeeds via the idempotent no-op, no CPI
}

#[tokio::test]
async fn init_book_permission_rejects_wrong_authority() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let (market, market_book, permission) = book_perm_accounts(pid, authority.pubkey(), 1);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey()); // real authority
    pt.add_account(market, m);
    pt.add_account(market_book, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: pid, executable: false, rent_epoch: 0 });
    pt.add_account(permission, Account { lamports: 1_000_000, data: vec![], owner: Pubkey::new_from_array(PERMISSION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(imposter.pubkey(), true), // NOT the market authority
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new(permission, false),
            AccountMeta::new(Pubkey::new_from_array(EPHEMERAL_VAULT_ID), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(MAGIC_PROGRAM_ID), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(PERMISSION_PROGRAM_ID), false),
        ],
        data: vec![IX_INIT_BOOK_PERM],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh))
        .await;
    assert!(r.is_err(), "non-authority must be rejected");
}

#[tokio::test]
async fn set_book_privacy_rejects_too_many_members() {
    // n_members = 33 > MAX (32) is rejected at data-parse, before any account work.
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let market = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(authority.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    pt.add_account(market, market_account(pid, Pubkey::new_unique()));
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_SET_BOOK_PRIVACY, 1, 33]; // is_private=1, n=33
    data.extend_from_slice(&[0u8; 33 * 32]); // 33 pubkeys
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market, false), // placeholders; rejected before use
            AccountMeta::new(market, false),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(market, false),
        ],
        data,
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix], Some(&payer.pubkey()), &[&payer, &authority], bh))
        .await;
    assert!(r.is_err(), "more than MAX_PRIVACY_MEMBERS must be rejected");
}

// ─── delegate / undelegate market_book e2e ───
// The Delegate/Undelegate CPIs hit the MagicBlock delegation program (build-sbf
// only). The auth + ownership validation runs BEFORE the CPI and IS testable.
const IX_DELEGATE_MARKET_BOOK: u8 = 120;
const IX_UNDELEGATE_MARKET_BOOK: u8 = 121;

fn nine_accounts(market: Pubkey, market_book: Pubkey, authority: Pubkey, pid: Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new(market, false),
        AccountMeta::new(market_book, false),
        AccountMeta::new_readonly(pid, false),                  // owner_program
        AccountMeta::new(Pubkey::new_unique(), false),          // delegate_buffer
        AccountMeta::new(Pubkey::new_unique(), false),          // delegation_record
        AccountMeta::new(Pubkey::new_unique(), false),          // delegation_metadata
        AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false), // system
        AccountMeta::new_readonly(Pubkey::new_from_array(DELEGATION_PROGRAM_ID), false),
    ]
}

#[tokio::test]
async fn delegate_market_book_rejects_already_delegated() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let market = Pubkey::new_unique();
    let (market_book, _) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    // market_book already owned by the delegation program → "already delegated".
    pt.add_account(market_book, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    pt.add_account(authority.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_DELEGATE_MARKET_BOOK];
    data.extend_from_slice(&30_000u32.to_le_bytes());
    data.push(0); // no validator
    let ix = Instruction { program_id: pid, accounts: nine_accounts(market, market_book, authority.pubkey(), pid), data };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &authority], bh))
        .await;
    assert!(r.is_err(), "delegating an already-delegated book must be rejected");
}

#[tokio::test]
async fn delegate_market_book_rejects_wrong_authority() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let market = Pubkey::new_unique();
    let (market_book, _) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    pt.add_account(market_book, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: pid, executable: false, rent_epoch: 0 });
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_DELEGATE_MARKET_BOOK];
    data.extend_from_slice(&30_000u32.to_le_bytes());
    data.push(0);
    let ix = Instruction { program_id: pid, accounts: nine_accounts(market, market_book, imposter.pubkey(), pid), data };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh))
        .await;
    assert!(r.is_err(), "non-authority delegate must be rejected");
}

#[tokio::test]
async fn undelegate_market_book_rejects_wrong_authority() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let market = Pubkey::new_unique();
    let (market_book, _) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    pt.add_account(market_book, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(imposter.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new_readonly(pid, false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(DELEGATION_PROGRAM_ID), false),
        ],
        data: vec![IX_UNDELEGATE_MARKET_BOOK],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh))
        .await;
    assert!(r.is_err(), "non-authority undelegate must be rejected");
}

// ─── delegate / undelegate market e2e ───
const IX_DELEGATE_MARKET: u8 = 122;
const IX_UNDELEGATE_MARKET: u8 = 123;

#[tokio::test]
async fn delegate_market_rejects_already_delegated() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let (market, _) = Pubkey::find_program_address(&[b"market", &base_mint.to_bytes(), &quote_mint.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    // market owned by the delegation program → assert_owned_by(pid) fails.
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    m.owner = Pubkey::new_from_array(DELEGATION_PROGRAM_ID);
    pt.add_account(market, m);
    pt.add_account(authority.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_DELEGATE_MARKET];
    data.extend_from_slice(&30_000u32.to_le_bytes());
    data.push(0);
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new_readonly(pid, false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(DELEGATION_PROGRAM_ID), false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &authority], bh)).await;
    assert!(r.is_err(), "delegating an already-delegated market must be rejected");
}

#[tokio::test]
async fn undelegate_market_rejects_wrong_authority() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let (market, _) = Pubkey::find_program_address(&[b"market", &base_mint.to_bytes(), &quote_mint.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    // delegated market: owned by the delegation program, data preserved (authority @ 124).
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    m.owner = Pubkey::new_from_array(DELEGATION_PROGRAM_ID);
    pt.add_account(market, m);
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(imposter.pubkey(), true), // not the authority
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new_readonly(pid, false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(DELEGATION_PROGRAM_ID), false),
        ],
        data: vec![IX_UNDELEGATE_MARKET],
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh)).await;
    assert!(r.is_err(), "non-authority undelegate must be rejected");
}

// ─── force_undelegate_market_book e2e (liveness gate, no CPI) ───
// Permissionless escape: the liveness gate runs BEFORE the undelegate CPI, so a
// premature force-undelegate (ER still live) is cleanly rejected. A recently
// delegated book (book_delegated_at_slot ≈ current slot) is "live".
const IX_FORCE_UNDELEGATE: u8 = 124;

#[tokio::test]
async fn force_undelegate_rejects_while_live() {
    let pid = Pubkey::new_unique();
    let payer_signer = Keypair::new();
    let market = Pubkey::new_unique();
    let (market_book, _) =
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    // market owned by us; book delegated recently (slot 1) → within the stall
    // window vs the test validator's low current slot → "live" → reject.
    let mut m = market_account(pid, Pubkey::new_unique());
    put_u64(&mut m.data, MKT_BOOK_DELEGATED_AT, 1);
    pt.add_account(market, m);
    pt.add_account(market_book, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    pt.add_account(payer_signer.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(payer_signer.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(market_book, false),
            AccountMeta::new_readonly(pid, false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(DELEGATION_PROGRAM_ID), false),
        ],
        data: vec![IX_FORCE_UNDELEGATE],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &payer_signer], bh))
        .await;
    assert!(r.is_err(), "force-undelegate must be rejected while the ER is live");
}

// ─── commit_market_book e2e (magic-program pin, before CPI) ───
// The commit ScheduleCommit CPI hits the magic program (build-sbf only). The
// magic_program / magic_context address pins run before the invoke, so a wrong
// magic program is cleanly rejected.
use flash_book_pin::er::MAGIC_CONTEXT_ID;
const IX_COMMIT_MARKET_BOOK: u8 = 125;

#[tokio::test]
async fn commit_market_book_rejects_wrong_magic_program() {
    let pid = Pubkey::new_unique();
    let payer_signer = Keypair::new();
    let market_book = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(payer_signer.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    pt.add_account(market_book, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(payer_signer.pubkey(), true),
            AccountMeta::new(market_book, false),
            AccountMeta::new(Pubkey::new_from_array(MAGIC_CONTEXT_ID), false),
            AccountMeta::new_readonly(Pubkey::new_unique(), false), // WRONG magic program
        ],
        data: vec![IX_COMMIT_MARKET_BOOK],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &payer_signer], bh))
        .await;
    assert!(r.is_err(), "commit with a wrong magic program must be rejected");
}

// ─── process_undelegation e2e (8-byte dispatch + buffer guard, before CPI) ───
// The delegation program CPIs this back with an 8-byte discriminator (not a
// 1-byte tag); the entrypoint routes it to process_undelegation. The buffer must
// be the delegation program's SIGNER — a non-signer buffer is rejected before
// the re-open CPI. Exercises the special dispatch path + the guard.
use flash_book_pin::er::EXTERNAL_UNDELEGATE_DISCRIMINATOR;

#[tokio::test]
async fn process_undelegation_rejects_non_signer_buffer() {
    let pid = Pubkey::new_unique();
    let payer_signer = Keypair::new();
    let delegated = Pubkey::new_unique();
    let buffer = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(payer_signer.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    pt.add_account(delegated, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: pid, executable: false, rent_epoch: 0 });
    // buffer owned by the delegation program but NOT passed as a signer.
    pt.add_account(buffer, Account { lamports: 1_000_000, data: vec![0u8; 8], owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    // data = 8-byte callback disc ++ borsh Vec<Vec<u8>> seeds (2 seeds).
    let mut data = EXTERNAL_UNDELEGATE_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&4u32.to_le_bytes());
    data.extend_from_slice(b"book");
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&[0xAA, 0xBB]);

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(delegated, false),
            AccountMeta::new(buffer, false), // NOT a signer
            AccountMeta::new(payer_signer.pubkey(), true),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
        ],
        data,
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &payer_signer], bh))
        .await;
    assert!(r.is_err(), "a non-signer buffer must be rejected (only the delegation program may finalize)");
}

// ─── init_fill_commitment e2e (auth gate, before CPI) ───
// Creates the ring PDA via create_pda CPI (build-sbf only); the market-authority
// gate runs before the CPI, so a wrong authority is cleanly rejected.
use flash_book_pin::fill_commitment::FILL_COMMIT_SEED;
const IX_INIT_FILL_COMMITMENT: u8 = 127;

#[tokio::test]
async fn init_fill_commitment_rejects_wrong_authority() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let market = Pubkey::new_unique();
    let (fill_commit, _) =
        Pubkey::find_program_address(&[FILL_COMMIT_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey()); // real authority
    pt.add_account(market, m);
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(imposter.pubkey(), true), // NOT the market authority
            AccountMeta::new(market, false),
            AccountMeta::new(fill_commit, false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
        ],
        data: vec![IX_INIT_FILL_COMMITMENT],
    };
    let r = banks
        .process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh))
        .await;
    assert!(r.is_err(), "non-authority init_fill_commitment must be rejected");
}

// ─── delegate / undelegate fill_commitment e2e ───
const IX_DELEGATE_FILL: u8 = 128;
const IX_UNDELEGATE_FILL: u8 = 131;

#[tokio::test]
async fn delegate_fill_commitment_rejects_already_delegated() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let market = Pubkey::new_unique();
    let (fc, _) = Pubkey::find_program_address(&[FILL_COMMIT_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    // ring already delegated → owned by the delegation program.
    pt.add_account(fc, Account { lamports: 1_000_000, data: vec![0u8; 64], owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    pt.add_account(authority.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_DELEGATE_FILL];
    data.extend_from_slice(&30_000u32.to_le_bytes());
    data.push(0);
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(fc, false),
            AccountMeta::new_readonly(pid, false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(DELEGATION_PROGRAM_ID), false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &authority], bh)).await;
    assert!(r.is_err(), "delegating an already-delegated ring must be rejected");
}

#[tokio::test]
async fn undelegate_fill_commitment_rejects_wrong_authority() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let market = Pubkey::new_unique();
    let (fc, _) = Pubkey::find_program_address(&[FILL_COMMIT_SEED, &market.to_bytes()], &pid);

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    pt.add_account(fc, Account { lamports: 1_000_000, data: vec![0u8; 64], owner: Pubkey::new_from_array(DELEGATION_PROGRAM_ID), executable: false, rent_epoch: 0 });
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(imposter.pubkey(), true), // not the authority
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(fc, false),
            AccountMeta::new_readonly(pid, false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array([0u8;32]), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(DELEGATION_PROGRAM_ID), false),
        ],
        data: vec![IX_UNDELEGATE_FILL],
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh)).await;
    assert!(r.is_err(), "non-authority undelegate must be rejected");
}

// ─── migrations + views e2e (no CPI → fully testable) ───
const IX_MIGRATE_MARKET: u8 = 132;
const IX_VIEW_PREDICTED_FUNDING: u8 = 134;
const IX_VIEW_PORTFOLIO_RISK: u8 = 138;

#[tokio::test]
async fn migrate_market_to_v3_is_noop_on_canonical_account() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let market = Pubkey::new_unique();
    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    pt.add_account(authority.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;
    let ix = Instruction {
        program_id: pid,
        accounts: vec![AccountMeta::new_readonly(authority.pubkey(), true), AccountMeta::new(market, false)],
        data: vec![IX_MIGRATE_MARKET],
    };
    banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &authority], bh)).await.unwrap();
}

#[tokio::test]
async fn migrate_market_to_v3_rejects_wrong_authority() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let market = Pubkey::new_unique();
    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut m = market_account(pid, Pubkey::new_unique());
    put_key(&mut m.data, MKT_AUTHORITY, &authority.pubkey());
    pt.add_account(market, m);
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;
    let ix = Instruction {
        program_id: pid,
        accounts: vec![AccountMeta::new_readonly(imposter.pubkey(), true), AccountMeta::new(market, false)],
        data: vec![IX_MIGRATE_MARKET],
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh)).await;
    assert!(r.is_err(), "wrong authority rejected");
}

#[tokio::test]
async fn view_predicted_funding_emits_and_succeeds() {
    let pid = Pubkey::new_unique();
    let caller = Keypair::new();
    let market = Pubkey::new_unique();
    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(market, market_full(pid, 1_234, 1, 500, 0, 0));
    pt.add_account(caller.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;
    let ix = Instruction { program_id: pid, accounts: vec![AccountMeta::new_readonly(market, false)], data: vec![IX_VIEW_PREDICTED_FUNDING] };
    banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh)).await.unwrap();
}

#[tokio::test]
async fn view_portfolio_risk_emits_and_succeeds() {
    let pid = Pubkey::new_unique();
    let trader = Pubkey::new_unique();
    let ts = Pubkey::new_unique();
    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(ts, trader_state(pid, trader, 5_000, 2));
    let (banks, payer, bh) = pt.start().await;
    let ix = Instruction { program_id: pid, accounts: vec![AccountMeta::new_readonly(ts, false)], data: vec![IX_VIEW_PORTFOLIO_RISK] };
    banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh)).await.unwrap();
}

// ─── sweep_collateral e2e (non-CPI → fully testable) ───
const IX_SWEEP_COLLATERAL: u8 = 139;
const TS_DELEGATE: usize = 86;

#[tokio::test]
async fn sweep_collateral_moves_between_flat_states() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let other = Pubkey::new_unique();
    let from = Pubkey::new_unique();
    let to = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    // from: trader == signer (authorized), flat.
    pt.add_account(from, trader_state(pid, authority.pubkey(), 1_000, 0));
    // to: different trader, but signer is its delegate (authorized), flat.
    let mut to_ts = trader_state(pid, other, 500, 0);
    put_key(&mut to_ts.data, TS_DELEGATE, &authority.pubkey());
    pt.add_account(to, to_ts);
    pt.add_account(authority.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_SWEEP_COLLATERAL];
    data.extend_from_slice(&300u64.to_le_bytes());
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(authority.pubkey(), true),
            AccountMeta::new(from, false),
            AccountMeta::new(to, false),
        ],
        data,
    };
    banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &authority], bh)).await.unwrap();

    let f = banks.get_account(from).await.unwrap().unwrap();
    let t = banks.get_account(to).await.unwrap().unwrap();
    assert_eq!(get_u64(&f.data, TS_COLLATERAL), 700, "source debited");
    assert_eq!(get_u64(&t.data, TS_COLLATERAL), 800, "dest credited");
}

#[tokio::test]
async fn sweep_collateral_rejects_unauthorized() {
    let pid = Pubkey::new_unique();
    let authority = Keypair::new();
    let imposter = Keypair::new();
    let other = Pubkey::new_unique();
    let from = Pubkey::new_unique();
    let to = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(from, trader_state(pid, authority.pubkey(), 1_000, 0)); // owned by authority, NOT imposter
    let mut to_ts = trader_state(pid, other, 500, 0);
    put_key(&mut to_ts.data, TS_DELEGATE, &authority.pubkey());
    pt.add_account(to, to_ts);
    pt.add_account(imposter.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_SWEEP_COLLATERAL];
    data.extend_from_slice(&300u64.to_le_bytes());
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(imposter.pubkey(), true), // not authorized for `from`
            AccountMeta::new(from, false),
            AccountMeta::new(to, false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &imposter], bh)).await;
    assert!(r.is_err(), "unauthorized sweep must be rejected");
}

// ─── partial_withdraw_collateral e2e (strict gate + core entry, before CPI) ───
const IX_PARTIAL_WITHDRAW: u8 = 140;
const TS_ER_ACTIVE: usize = 156;

#[tokio::test]
async fn partial_withdraw_rejects_er_active_trader() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let ts = Pubkey::new_unique();
    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut ts_acct = trader_state(pid, trader.pubkey(), 1_000, 0);
    ts_acct.data[TS_ER_ACTIVE] = 1; // ER-active → strict path fails closed
    pt.add_account(ts, ts_acct);
    pt.add_account(trader.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let d = Pubkey::new_unique();
    pt.add_account(d, Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;
    let mut data = vec![IX_PARTIAL_WITHDRAW];
    data.extend_from_slice(&100u64.to_le_bytes());
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(d, false), AccountMeta::new(d, false), AccountMeta::new(d, false),
            AccountMeta::new_readonly(d, false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &trader], bh)).await;
    assert!(r.is_err(), "ER-active trader must use xdomain withdraw (strict rejected)");
}

// Audit 2026-06 regression: the STRICT full `withdraw_collateral` (ix 12) must
// ALSO fail closed for an ER-active trader (parity with the Anchor
// `require!(s.er_active == 0, UseXDomainWithdraw)`). Before the fix it had no ER
// check at all, so an ER-active trader with 0 mainnet `open_positions` could pull
// collateral that backs their ER resting orders → ER bad debt.
const IX_WITHDRAW_COLLATERAL: u8 = 12;
#[tokio::test]
async fn withdraw_collateral_rejects_er_active_trader() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let ts = Pubkey::new_unique();
    let (insurance, _) = Pubkey::find_program_address(&[INSURANCE_SEED], &pid);
    let d = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    let mut ts_acct = trader_state(pid, trader.pubkey(), 1_000, 0); // flat (open=0)
    ts_acct.data[TS_ER_ACTIVE] = 1; // but ER-active ⇒ must route via xdomain
    pt.add_account(ts, ts_acct);
    pt.add_account(insurance, insurance_full(pid, 0, 0));
    pt.add_account(d, Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    pt.add_account(trader.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;
    let mut data = vec![IX_WITHDRAW_COLLATERAL];
    data.extend_from_slice(&100u64.to_le_bytes());
    let ix = Instruction {
        program_id: pid,
        // [trader(signer), trader_state, insurance, quote_vault, trader_ata, token_program]
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(d, false), AccountMeta::new(d, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(flash_book_pin::cpi::TOKEN_PROGRAM_ID), false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &trader], bh)).await;
    let err = format!("{:?}", r.expect_err("ER-active trader must not use the strict full withdraw"));
    // Custom(241) = "use xdomain withdraw" — proves the er_active gate (which runs
    // BEFORE the vault-binding check) is what rejected, not some later failure.
    assert!(err.contains("Custom(241)"), "expected er_active gate Custom(241), got: {err}");
}

#[tokio::test]
async fn partial_withdraw_rejects_zero_amount() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let ts = Pubkey::new_unique();
    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(ts, trader_state(pid, trader.pubkey(), 1_000, 0)); // er_active = 0
    pt.add_account(trader.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let d = Pubkey::new_unique();
    pt.add_account(d, Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;
    let mut data = vec![IX_PARTIAL_WITHDRAW];
    data.extend_from_slice(&0u64.to_le_bytes()); // zero amount
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(d, false), AccountMeta::new(d, false), AccountMeta::new(d, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(flash_book_pin::cpi::TOKEN_PROGRAM_ID), false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &trader], bh)).await;
    assert!(r.is_err(), "zero amount rejected");
}

// ─── xdomain withdraw e2e (attestation bind + flat gate, before CPI) ───
use flash_book_pin::seeds::ER_MARGIN_SEED;
use flash_book_pin::state::ER_MARGIN_DISC;
const IX_PARTIAL_WITHDRAW_XDOMAIN: u8 = 141;
const IX_WITHDRAW_XDOMAIN: u8 = 142;

fn er_margin_acct(pid: Pubkey, bound_ts: Pubkey) -> Account {
    let mut d = vec![0u8; 96];
    d[0..8].copy_from_slice(&ER_MARGIN_DISC);
    put_key(&mut d, 8, &bound_ts); // trader_state @ 8
    rent_account(d, pid)
}

#[tokio::test]
async fn withdraw_xdomain_rejects_not_flat() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let ts = Pubkey::new_unique();
    let (er_margin, _) = Pubkey::find_program_address(&[ER_MARGIN_SEED, &ts.to_bytes()], &pid);
    let (insurance, _) = Pubkey::find_program_address(&[INSURANCE_SEED], &pid);
    let d = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(ts, trader_state(pid, trader.pubkey(), 1_000, 1)); // open_positions = 1
    pt.add_account(er_margin, er_margin_acct(pid, ts));
    pt.add_account(insurance, insurance_full(pid, 0, 0));
    pt.add_account(d, Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    pt.add_account(trader.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_WITHDRAW_XDOMAIN];
    data.extend_from_slice(&100u64.to_le_bytes());
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(d, false), AccountMeta::new(d, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(flash_book_pin::cpi::TOKEN_PROGRAM_ID), false),
            AccountMeta::new_readonly(er_margin, false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &trader], bh)).await;
    assert!(r.is_err(), "full xdomain withdraw requires flat");
}

#[tokio::test]
async fn partial_withdraw_xdomain_rejects_attestation_mismatch() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let ts = Pubkey::new_unique();
    let (er_margin, _) = Pubkey::find_program_address(&[ER_MARGIN_SEED, &ts.to_bytes()], &pid);
    let (insurance, _) = Pubkey::find_program_address(&[INSURANCE_SEED], &pid);
    let d = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(ts, trader_state(pid, trader.pubkey(), 1_000, 0));
    // er_margin at the right PDA but BOUND to a different trader_state.
    pt.add_account(er_margin, er_margin_acct(pid, Pubkey::new_unique()));
    pt.add_account(insurance, insurance_full(pid, 0, 0));
    pt.add_account(d, Account { lamports: 1, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    pt.add_account(trader.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_PARTIAL_WITHDRAW_XDOMAIN];
    data.extend_from_slice(&100u64.to_le_bytes());
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(d, false), AccountMeta::new(d, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(flash_book_pin::cpi::TOKEN_PROGRAM_ID), false),
            AccountMeta::new_readonly(er_margin, false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &trader], bh)).await;
    assert!(r.is_err(), "attestation bound to a different trader_state must be rejected");
}

// ─── place_basket_order_v2 e2e (non-CPI → fully testable) ───
const IX_BASKET_V2: u8 = 143;

fn basket_v2_setup(pid: Pubkey, trader: Pubkey, collateral: u64)
    -> (Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, ProgramTest)
{
    // (ts, mkt_a, book_a, pos_a, mkt_b, book_b, pos_b)
    let ts = Pubkey::new_unique();
    let mkt_a = Pubkey::new_unique();
    let mkt_b = Pubkey::new_unique();
    let (book_a, ba) = Pubkey::find_program_address(&[MARKET_BOOK_SEED, &mkt_a.to_bytes()], &pid);
    let (book_b, bb) = Pubkey::find_program_address(&[MARKET_BOOK_SEED, &mkt_b.to_bytes()], &pid);
    let base = Pubkey::new_unique();
    let quote = Pubkey::new_unique();
    let pos_a = Pubkey::new_unique();
    let pos_b = Pubkey::new_unique();

    let mut pt = ProgramTest::new("flash_book_pin", pid, None);
    pt.add_account(ts, trader_state(pid, trader, collateral, 0));
    pt.add_account(mkt_a, market_full(pid, 100, 1, 500, 0, 0)); // 5% MMR, status Active(0), min_lot 0
    pt.add_account(mkt_b, market_full(pid, 100, 1, 500, 0, 0));
    pt.add_account(book_a, init_book(pid, mkt_a, base, quote, ba));
    pt.add_account(book_b, init_book(pid, mkt_b, base, quote, bb));
    pt.add_account(pos_a, position(pid, trader, mkt_a, 0, 0, 0, 0)); // flat
    pt.add_account(pos_b, position(pid, trader, mkt_b, 0, 0, 0, 0));
    (ts, mkt_a, book_a, pos_a, mkt_b, book_b, pos_b, pt)
}

fn basket_leg(side: u8, size: u64, limit: u64) -> Vec<u8> {
    let mut d = vec![side];
    d.extend_from_slice(&size.to_le_bytes());
    d.extend_from_slice(&limit.to_le_bytes());
    d.push(0); // post_only
    d
}

#[tokio::test]
async fn basket_v2_places_both_legs_when_healthy() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let (ts, mkt_a, book_a, pos_a, mkt_b, book_b, pos_b, mut pt) =
        basket_v2_setup(pid, trader.pubkey(), 1_000_000); // ample collateral
    pt.add_account(trader.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_BASKET_V2];
    data.extend_from_slice(&basket_leg(0, 5, 100)); // leg a: bid
    data.extend_from_slice(&basket_leg(1, 5, 100)); // leg b: ask (diff market)
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new_readonly(ts, false),
            AccountMeta::new_readonly(mkt_a, false), AccountMeta::new(book_a, false), AccountMeta::new_readonly(pos_a, false),
            AccountMeta::new_readonly(mkt_b, false), AccountMeta::new(book_b, false), AccountMeta::new_readonly(pos_b, false),
        ],
        data,
    };
    banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &trader], bh)).await.unwrap();

    let mut da = banks.get_account(book_a).await.unwrap().unwrap().data;
    let mut db = banks.get_account(book_b).await.unwrap().unwrap().data;
    assert_eq!(MarketBookHandle::from_account_data(&mut da).unwrap().header.total_orders_active, 1, "leg a rested");
    assert_eq!(MarketBookHandle::from_account_data(&mut db).unwrap().header.total_orders_active, 1, "leg b rested");
}

#[tokio::test]
async fn basket_v2_rejects_underwater_projection() {
    let pid = Pubkey::new_unique();
    let trader = Keypair::new();
    let (ts, mkt_a, book_a, pos_a, mkt_b, book_b, pos_b, mut pt) =
        basket_v2_setup(pid, trader.pubkey(), 1); // ~zero collateral → IM breached
    pt.add_account(trader.pubkey(), Account { lamports: 1_000_000_000, data: vec![], owner: Pubkey::new_from_array([0u8;32]), executable: false, rent_epoch: 0 });
    let (banks, payer, bh) = pt.start().await;

    let mut data = vec![IX_BASKET_V2];
    data.extend_from_slice(&basket_leg(0, 100, 100)); // large legs
    data.extend_from_slice(&basket_leg(1, 100, 100));
    let ix = Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new_readonly(ts, false),
            AccountMeta::new_readonly(mkt_a, false), AccountMeta::new(book_a, false), AccountMeta::new_readonly(pos_a, false),
            AccountMeta::new_readonly(mkt_b, false), AccountMeta::new(book_b, false), AccountMeta::new_readonly(pos_b, false),
        ],
        data,
    };
    let r = banks.process_transaction(Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer, &trader], bh)).await;
    assert!(r.is_err(), "underwater basket projection must be rejected");
}
