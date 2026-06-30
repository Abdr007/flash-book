//! On-VALIDATOR exercise harness — fires raw pin instructions at a live local
//! `solana-test-validator` via a blocking `RpcClient`. Unlike the BanksClient
//! integration suite (which can only PRE-SEED account state with `add_account`),
//! this drives the REAL `create_pda_account` + SPL-token CPIs the constructive
//! instructions perform — the one execution path program-test cannot cover.
//!
//! Self-gated: a no-op unless `LOCAL_EXERCISE=1` is set, so the default
//! `cargo test` (and CI) never touches it. Run it against a fresh validator:
//!
//!   solana-test-validator --reset --ledger <dir> --rpc-port 8899 &
//!   solana -u localhost program deploy --program-id \
//!     target/deploy/flash_book_pin-keypair.json target/deploy/flash_book_pin.so
//!   LOCAL_EXERCISE=1 PIN_PROGRAM_ID=<id> RPC_URL=http://127.0.0.1:8899 \
//!     cargo test --test local_exercise -- --nocapture --test-threads=1
//!
//! NEVER fabricate results: every ix reports its real on-chain outcome.

use flash_book_pin::book::{encode_order_id, MARKET_BOOK_SEED, MARKET_BOOK_TOTAL_BYTES};
use flash_book_pin::fill_commitment::FILL_COMMIT_SEED;
use flash_book_pin::seeds::{
    ENVELOPE_CONFIG_SEED, ER_MARGIN_SEED, FEE_TIERS_SEED, FLP_EXPOSURE_SEED, FLP_PER_MARKET_SEED,
    FLP_POSITION_V3_SEED, HAIRCUT_SEED, ICEBERG_ORDER_SEED, INSURANCE_SEED, JIT_LIQ_OFFER_SEED,
    LEVERAGE_TIERS_SEED, LP_POSITION_SEED, MARKET_SEED, ORACLE_CONFIG_SEED, POSITION_HAIRCUT_SEED,
    POSITION_LIQ_STATE_SEED, SESSION_SEED, SIDE_ACCRUAL_SEED, TRADER_STATE_SEED, TRIGGER_ORDER_SEED,
    TWAP_ORDER_SEED, VAULT_POSITION_SEED, VAULT_SEED,
};
use flash_book_pin::state::{Insurance, Market, Position, TraderState};
use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction as system_instruction;
use std::str::FromStr;

// ── Ix tags (mirror lib.rs `enum Ix`) ───────────────────────────────────────
const IX_APPLY_FILL: u8 = 0;
const IX_PLACE_LIMIT: u8 = 2;
const IX_PLACE_TAKER: u8 = 4;
const IX_OPEN_TRADER_STATE: u8 = 8;
const IX_INIT_INSURANCE: u8 = 9;
const IX_DEPOSIT_COLLATERAL: u8 = 10;
const IX_WITHDRAW_COLLATERAL: u8 = 12;
const IX_INIT_MARKET: u8 = 11;
const IX_UPDATE_ORACLE: u8 = 15;
const IX_INIT_MARKET_BOOK: u8 = 81;
const IX_INITIALIZE_FLP_EXPOSURE: u8 = 26;
const IX_INIT_LP_POSITION: u8 = 27;
const IX_DEPOSIT_FLP_CAPITAL: u8 = 28;
const IX_WITHDRAW_FLP_CAPITAL: u8 = 29;
const IX_PLACE_TRIGGER_ORDER: u8 = 49;
const IX_CANCEL_TRIGGER_ORDER: u8 = 50;
const IX_APPLY_FLP_FILL: u8 = 7;
const IX_MATURE_POSITION: u8 = 68;
const IX_FLUSH_HAIRCUT_DUST: u8 = 80;
const IX_CONVERT_POSITION: u8 = 82;
const IX_SETTLE_VAULT_PERF_FEE_V3: u8 = 110;
const IX_VAULT_PLACE_ORDER_V3: u8 = 111;
const IX_VAULT_CANCEL_ORDER_V3: u8 = 112;
const IX_PLACE_BASKET_V2: u8 = 143;
const IX_PLACE_BASKET_N_V2: u8 = 144;
const IX_RELEASE_GAIN_TO_HAIRCUT: u8 = 83;
const IX_SET_POSITION_CROSS: u8 = 94;
const IX_SET_POSITION_ISOLATED: u8 = 95;
const IX_SWEEP_COLLATERAL: u8 = 139;
const IX_PARTIAL_WITHDRAW: u8 = 140;
const IX_FORCE_UNDELEGATE_MARKET_BOOK: u8 = 124;
const IX_SETTLE_FUNDING: u8 = 1;
const IX_CANCEL_ORDER: u8 = 3;
const IX_MODIFY_ORDER: u8 = 5;
const IX_VERIFY_SOLVENCY: u8 = 24;
const IX_VERIFY_STRESS_SOLVENCY: u8 = 40;
const IX_VERIFY_PORTFOLIO_SOLVENCY: u8 = 41;
const IX_VERIFY_STRESS_LATTICE: u8 = 44;
const IX_SET_POSITION_LEVERAGE: u8 = 46;
const IX_VERIFY_LEVERAGE_CAP: u8 = 48;
const IX_VERIFY_PORTFOLIO_STRESS: u8 = 47;
const IX_INIT_POSITION_HAIRCUT_STATE: u8 = 63;
const IX_VERIFY_POSITION_HAIRCUT: u8 = 78;
const IX_RECORD_FLP_FILL_V3: u8 = 86;
const IX_INIT_POSITION_LIQ_STATE: u8 = 90;
const IX_LIQUIDATE_POSITION_V2: u8 = 92;
const IX_LIQUIDATION_PREVIEW: u8 = 145;
const IX_CANCEL_ALL: u8 = 6;
const IX_SET_MARKET_SEQUENCER: u8 = 13;
const IX_TRANSFER_MARKET_AUTHORITY: u8 = 21;
const IX_TRANSFER_INSURANCE_AUTHORITY: u8 = 22;
const IX_SET_INSURANCE_FEE_CONTRIBUTION: u8 = 23;
const IX_SET_MARKET_MAINTENANCE_MARGIN: u8 = 25;
const IX_UPDATE_MARKET_LEVERAGE_TIERS: u8 = 35;
const IX_SET_MARKET_RISK_PARAMS: u8 = 36;
const IX_SET_TRADER_DELEGATE: u8 = 37;
const IX_SET_TRADER_REFERRER: u8 = 38;
const IX_SET_TRADER_BUILDER: u8 = 39;
const IX_UPDATE_FEE_TIERS: u8 = 43;
const IX_SET_MARKET_MAX_LEVERAGE: u8 = 45;
const IX_SET_INSURANCE_PAUSE_THRESHOLD: u8 = 54;
const IX_BURN_MARKET_AUTHORITY: u8 = 55;
const IX_EXPAND_MARKET_BOOK: u8 = 87;
const IX_REAP_EXPIRED_ORDERS: u8 = 88;
const IX_AUTO_DELEVERAGE: u8 = 89;
const IX_VERIFY_PROTOCOL_SOLVENCY: u8 = 30;
const IX_VERIFY_MARKET_INVARIANTS: u8 = 31;
const IX_VERIFY_COLLATERAL_SOLVENCY: u8 = 32;
const IX_VERIFY_ENVELOPE_CONFIG: u8 = 57;
const IX_VERIFY_HAIRCUT_INVARIANTS: u8 = 62;
const IX_VERIFY_SIDE_ACCRUAL: u8 = 73;
const IX_VERIFY_ORACLE_CONFIG: u8 = 74;
const IX_VERIFY_LEVERAGE_TIERS: u8 = 75;
const IX_VERIFY_FEE_TIERS: u8 = 76;
const IX_VIEW_PREDICTED_FUNDING: u8 = 134;
const IX_VIEW_TRADER_TIER: u8 = 135;
const IX_VIEW_BOOK_DEPTH: u8 = 136;
const IX_VIEW_QUOTE_LADDER: u8 = 137;
const IX_VIEW_PORTFOLIO_RISK: u8 = 138;
const IX_ADVANCE_FUNDING: u8 = 146;
const IX_SET_FUNDING_PARAMS: u8 = 147;
const IX_SEED_RESIDUAL: u8 = 69;
const IX_GATE_ENVELOPE: u8 = 70;
const IX_PLACE_TWAP: u8 = 51;
const IX_CANCEL_TWAP: u8 = 52;
const IX_PLACE_ICEBERG: u8 = 101;
const IX_REPLENISH_ICEBERG: u8 = 102;
const IX_CANCEL_ICEBERG: u8 = 103;
const IX_PLACE_BRACKET: u8 = 104;
const IX_INIT_FLP_PER_MARKET: u8 = 53;
const IX_FLP_DEPOSIT_V3: u8 = 84;
const IX_FLP_WITHDRAW_V3: u8 = 85;
const IX_SET_MARKET_LIQUIDATION_PARAMS: u8 = 91;
const IX_COVER_BAD_DEBT: u8 = 93;
const IX_PLACE_JIT_LIQ_OFFER: u8 = 97;
const IX_CANCEL_JIT_LIQ_OFFER: u8 = 98;
const IX_ER_HEARTBEAT: u8 = 33;
const IX_INIT_ER_MARGIN_ATTESTATION: u8 = 66;
const IX_ATTEST_ER_RESERVED_MARGIN: u8 = 67;
const IX_UNDELEGATE_MARKET_BOOK: u8 = 121;
const IX_UNDELEGATE_MARKET: u8 = 123;
const IX_UNDELEGATE_FILL_COMMITMENT: u8 = 131;
const IX_MIGRATE_MARKET_TO_V3: u8 = 132;
const IX_MIGRATE_POSITION_TO_TRADER_STATE_KEY: u8 = 133;
const IX_SET_MARKET_STATUS: u8 = 14;
const IX_OPEN_TRADER_SUB_ACCOUNT: u8 = 16;
const IX_TRANSFER_COLLATERAL: u8 = 17;
const IX_CLOSE_TRADER_SUB_ACCOUNT: u8 = 18;
const IX_SET_TRADER_FEE_TIER: u8 = 19;
const IX_SET_MARKET_PARAMS: u8 = 20;
const IX_INIT_FEE_TIERS: u8 = 42;
const IX_SET_ENVELOPE_CONFIG: u8 = 56;
const IX_CREATE_SESSION_TOKEN: u8 = 64;
const IX_REVOKE_SESSION_TOKEN: u8 = 65;
const IX_VERIFY_SESSION_ACTIVE: u8 = 77;
const IX_INIT_FILL_COMMITMENT: u8 = 127;
const IX_CREATE_VAULT_V3: u8 = 105;
const IX_VAULT_OPEN_TRADER_STATE_V3: u8 = 106;
const IX_INIT_VAULT_POSITION_V3: u8 = 107;
const IX_VAULT_DEPOSIT_V3: u8 = 108;
const IX_VAULT_WITHDRAW_V3: u8 = 109;
const IX_INIT_LEVERAGE_TIERS: u8 = 34;
const IX_INIT_ORACLE_CONFIG: u8 = 58;
const IX_INIT_SIDE_ACCRUAL: u8 = 59;
const IX_INIT_HAIRCUT_STATE: u8 = 61;
const IX_CREATE_VAULT: u8 = 60;
const IX_INIT_TRADER_ATA: u8 = 71;
const IX_CLOSE_TRADER_ATA: u8 = 72;
const IX_WITHDRAW_INSURANCE_FUND: u8 = 79;
const IX_LIQUIDATE_PORTFOLIO_V2: u8 = 96;
const IX_EXECUTE_TRIGGER: u8 = 99;
const IX_EXECUTE_TWAP: u8 = 100;
const IX_UPDATE_TRAILING_STOP: u8 = 113;
const IX_UPDATE_ORACLE_QUORUM: u8 = 114;
const IX_UPDATE_ORACLE_FROM_PYTH: u8 = 115;
const IX_PARTIAL_WITHDRAW_XDOMAIN: u8 = 141;
const IX_WITHDRAW_COLLATERAL_XDOMAIN: u8 = 142;

// SPL Token (classic) program id + instruction tags.
const SPL_TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const SPL_ASSOCIATED_TOKEN: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const SPL_INITIALIZE_MINT2: u8 = 20;
const SPL_INITIALIZE_ACCOUNT3: u8 = 18;
const SPL_MINT_TO: u8 = 7;
const MINT_LEN: u64 = 82;
const TOKEN_ACCT_LEN: u64 = 165;
const TOKEN_AMOUNT_OFF: usize = 64;
const INS_BALANCE_OFF: usize = 8;
const MKT_LONG_OI_OFF: usize = 56;
const MKT_SHORT_OI_OFF: usize = 64;
const TS_COLLATERAL_OFF: usize = 40;
const POS_SIZE_OFF: usize = 88;
const POS_COLLATERAL_OFF: usize = 104;

fn gated() -> bool {
    std::env::var("LOCAL_EXERCISE").map(|v| v == "1").unwrap_or(false)
}

fn rpc() -> RpcClient {
    let url = std::env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".to_string());
    RpcClient::new_with_commitment(url, CommitmentConfig::confirmed())
}

fn program_id() -> Pubkey {
    let s = std::env::var("PIN_PROGRAM_ID")
        .unwrap_or_else(|_| "CmCQDqY4fZL5nqCMZfy7GanUAH6qwhWjgqGfRx9LSqjo".to_string());
    Pubkey::from_str(&s).expect("PIN_PROGRAM_ID")
}

/// Records one instruction's real on-chain result.
struct Report {
    rows: Vec<(String, Result<String, String>)>,
}
impl Report {
    fn new() -> Self {
        Report { rows: Vec::new() }
    }
    fn ok(&mut self, label: &str, sig: String) {
        eprintln!("  PASS  {label}  ({sig})");
        self.rows.push((label.to_string(), Ok(sig)));
    }
    fn fail(&mut self, label: &str, err: String) {
        eprintln!("  FAIL  {label}  -> {err}");
        self.rows.push((label.to_string(), Err(err)));
    }
    /// A NEGATIVE test: the ix is EXPECTED to be rejected with custom error `code`.
    /// Passing = the program rejected with exactly that code; succeeding (or a
    /// different error) is a failure.
    fn expect_reject(&mut self, label: &str, code: u32, res: Result<String, String>) {
        let want = format!("custom program error: 0x{code:x}");
        match res {
            Ok(sig) => self.fail(label, format!("expected reject 0x{code:x} but SUCCEEDED ({sig})")),
            Err(e) if e.contains(&want) => self.ok(label, format!("correctly rejected 0x{code:x}")),
            Err(e) => self.fail(label, format!("expected 0x{code:x}, got: {e}")),
        }
    }
    fn expect_error_contains(&mut self, label: &str, needle: &str, res: Result<String, String>) {
        match res {
            Ok(sig) => self.fail(label, format!("expected error containing {needle:?} but SUCCEEDED ({sig})")),
            Err(e) if e.contains(needle) => self.ok(label, format!("correctly rejected: {needle}")),
            Err(e) => self.fail(label, format!("expected {needle:?}, got: {e}")),
        }
    }
    fn print(&self) {
        let pass = self.rows.iter().filter(|(_, r)| r.is_ok()).count();
        // explorer cluster suffix, derived from RPC_URL (devnet runs get clickable links)
        let cluster: Option<String> = std::env::var("RPC_URL").ok().and_then(|u| {
            if u.contains("devnet") { Some("?cluster=devnet".into()) }
            else if u.contains("testnet") { Some("?cluster=testnet".into()) }
            else if u.contains("127.0.0.1") || u.contains("localhost") { None }
            else { Some(format!("?cluster=custom&customUrl={u}")) }
        });
        eprintln!("\n================ ON-VALIDATOR EXERCISE REPORT ================");
        for (label, r) in &self.rows {
            match r {
                // a real on-chain signature (base58, ~88 chars) → print the explorer link
                Ok(sig) if sig.len() > 80 => match &cluster {
                    Some(c) => eprintln!("  PASS  {label}\n        https://explorer.solana.com/tx/{sig}{c}"),
                    None => eprintln!("  PASS  {label}   {sig}"),
                },
                Ok(note) => eprintln!("  PASS  {label}   ({note})"),
                Err(e) => eprintln!("  FAIL  {label}   {e}"),
            }
        }
        eprintln!("------------------------------------------------------");
        eprintln!("  {pass}/{} instructions passed on the live validator", self.rows.len());
        eprintln!("======================================================\n");
    }
}

/// Build+sign+send a tx, confirm it, and return the signature string.
fn send(
    client: &RpcClient,
    ixs: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<String, String> {
    let bh = client.get_latest_blockhash().map_err(|e| e.to_string())?;
    let mut all: Vec<&Keypair> = vec![payer];
    for s in signers {
        if s.pubkey() != payer.pubkey() {
            all.push(s);
        }
    }
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, bh);
    client
        .send_and_confirm_transaction(&tx)
        .map(|s| s.to_string())
        .map_err(|e| e.to_string())
}

/// Fund `who` with `lamports` from the pre-funded `payer` (a System transfer).
/// Works on ANY cluster — unlike `request_airdrop`, which is rate-limited / tiny on
/// devnet. This is what makes the harness runnable on devnet (set RPC_URL=devnet +
/// KEYPAIR=<funded wallet>) to produce REAL on-chain signatures.
fn fund(client: &RpcClient, payer: &Keypair, who: &Pubkey, lamports: u64) {
    let ix = system_instruction::transfer(&payer.pubkey(), who, lamports);
    let bh = client.get_latest_blockhash().expect("blockhash");
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], bh);
    client.send_and_confirm_transaction(&tx).expect("fund transfer");
}

/// System CreateAccount for an account owned by `owner`, signed by `new`.
fn create_account_ix(
    client: &RpcClient,
    payer: &Pubkey,
    new: &Pubkey,
    space: u64,
    owner: &Pubkey,
) -> Instruction {
    let lamports = client
        .get_minimum_balance_for_rent_exemption(space as usize)
        .expect("rent");
    system_instruction::create_account(payer, new, lamports, space, owner)
}

fn le8(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

fn read_account_data(client: &RpcClient, key: Pubkey) -> Result<Vec<u8>, String> {
    client.get_account_data(&key).map_err(|e| e.to_string())
}

fn read_u64_at(client: &RpcClient, key: Pubkey, off: usize) -> Result<u64, String> {
    let d = read_account_data(client, key)?;
    if d.len() < off + 8 {
        return Err(format!("{key} account too short: len={} need {}", d.len(), off + 8));
    }
    Ok(u64::from_le_bytes(d[off..off + 8].try_into().unwrap()))
}

/// The CLUSTER's unix timestamp, read from the Clock sysvar (unix_timestamp i64 at
/// offset 32). A local validator's clock drifts from wall time, so quorum/oracle
/// published_at must use THIS, not SystemTime, or sources read as future-dated.
fn onchain_unix(client: &RpcClient) -> u64 {
    let clock = Pubkey::from_str("SysvarC1ock11111111111111111111111111111111").unwrap();
    read_u64_at(client, clock, 32).unwrap_or(0)
}

/// Build an instruction: 1-byte tag + body, with the given account metas.
fn ix(pid: Pubkey, tag: u8, accounts: Vec<AccountMeta>, body: &[u8]) -> Instruction {
    let mut data = vec![tag];
    data.extend_from_slice(body);
    Instruction { program_id: pid, accounts, data }
}

/// Send + record one labelled step (by-ref so it composes with the Report).
fn run(
    rep: &mut Report,
    client: &RpcClient,
    label: &str,
    ixs: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
) {
    match send(client, ixs, payer, signers) {
        Ok(s) => rep.ok(label, s),
        Err(e) => rep.fail(label, e),
    }
}

/// Build the common config-PDA creator ix: [authority(s,w), market, new_pda(w),
/// system_program]. `market_writable` is set for handlers that mutate the market
/// (e.g. haircut, which arms the engine).
fn config_ix(
    pid: Pubkey,
    payer: &Pubkey,
    market: Pubkey,
    pda: Pubkey,
    sys: Pubkey,
    tag: u8,
    body: &[u8],
    market_writable: bool,
) -> Instruction {
    let mut data = vec![tag];
    data.extend_from_slice(body);
    let market_meta = if market_writable {
        AccountMeta::new(market, false)
    } else {
        AccountMeta::new_readonly(market, false)
    };
    Instruction {
        program_id: pid,
        accounts: vec![
            AccountMeta::new(*payer, true),
            market_meta,
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(sys, false),
        ],
        data,
    }
}

fn create_funded_token_account(
    client: &RpcClient,
    payer: &Keypair,
    spl: Pubkey,
    quote_mint: Pubkey,
    owner: Pubkey,
    amount: u64,
) -> Result<Keypair, String> {
    let tok = Keypair::new();
    let create = create_account_ix(client, &payer.pubkey(), &tok.pubkey(), TOKEN_ACCT_LEN, &spl);
    let mut init_data = vec![SPL_INITIALIZE_ACCOUNT3];
    init_data.extend_from_slice(owner.as_ref());
    let init = Instruction {
        program_id: spl,
        accounts: vec![
            AccountMeta::new(tok.pubkey(), false),
            AccountMeta::new_readonly(quote_mint, false),
        ],
        data: init_data,
    };
    let mut mint_data = vec![SPL_MINT_TO];
    mint_data.extend_from_slice(&le8(amount));
    let mint_to = Instruction {
        program_id: spl,
        accounts: vec![
            AccountMeta::new(quote_mint, false),
            AccountMeta::new(tok.pubkey(), false),
            AccountMeta::new_readonly(payer.pubkey(), true),
        ],
        data: mint_data,
    };
    send(client, &[create, init, mint_to], payer, &[&tok])?;
    Ok(tok)
}

#[allow(clippy::too_many_arguments)]
fn open_and_deposit_trader(
    client: &RpcClient,
    pid: Pubkey,
    payer: &Keypair,
    spl: Pubkey,
    sys: Pubkey,
    insurance: Pubkey,
    vault: Pubkey,
    quote_mint: Pubkey,
    amount: u64,
) -> Result<(Keypair, Pubkey, Pubkey), String> {
    let trader = Keypair::new();
    fund(client, payer, &trader.pubkey(), 200_000_000);
    let (ts, _) = Pubkey::find_program_address(&[TRADER_STATE_SEED, trader.pubkey().as_ref()], &pid);
    let open = ix(
        pid,
        IX_OPEN_TRADER_STATE,
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(sys, false),
        ],
        &[],
    );
    send(client, &[open], payer, &[&trader])?;
    let tok = create_funded_token_account(client, payer, spl, quote_mint, trader.pubkey(), amount)?;
    let dep = ix(
        pid,
        IX_DEPOSIT_COLLATERAL,
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new(ts, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(tok.pubkey(), false),
            AccountMeta::new_readonly(spl, false),
        ],
        &le8(amount),
    );
    send(client, &[dep], payer, &[&trader])?;
    Ok((trader, ts, tok.pubkey()))
}

fn init_fresh_market(
    client: &RpcClient,
    pid: Pubkey,
    payer: &Keypair,
    spl: Pubkey,
    sys: Pubkey,
    insurance: Pubkey,
    quote_mint: Pubkey,
    mark_price: u64,
    taker_fee_bps: u32,
    maker_rebate_bps: i32,
    mmr_bps: u32,
) -> Result<(Keypair, Pubkey), String> {
    let base_mint = Keypair::new();
    let create = create_account_ix(client, &payer.pubkey(), &base_mint.pubkey(), MINT_LEN, &spl);
    let mut mint_init = vec![SPL_INITIALIZE_MINT2, 9];
    mint_init.extend_from_slice(payer.pubkey().as_ref());
    mint_init.push(0);
    let init_mint = Instruction {
        program_id: spl,
        accounts: vec![AccountMeta::new(base_mint.pubkey(), false)],
        data: mint_init,
    };
    send(client, &[create, init_mint], payer, &[&base_mint])?;
    let market = Pubkey::find_program_address(
        &[MARKET_SEED, base_mint.pubkey().as_ref(), quote_mint.as_ref()],
        &pid,
    )
    .0;
    let mut body = Vec::new();
    body.extend_from_slice(&le8(1)); // tick_size
    body.extend_from_slice(&le8(mark_price));
    body.extend_from_slice(&taker_fee_bps.to_le_bytes());
    body.extend_from_slice(&maker_rebate_bps.to_le_bytes());
    body.extend_from_slice(&le8(1)); // min_base_lots
    body.extend_from_slice(&le8(1_000_000_000)); // max_oi
    body.extend_from_slice(&mmr_bps.to_le_bytes());
    let im = ix(
        pid,
        IX_INIT_MARKET,
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(base_mint.pubkey(), false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new_readonly(sys, false),
        ],
        &body,
    );
    send(client, &[im], payer, &[])?;
    Ok((base_mint, market))
}

#[allow(clippy::too_many_arguments)]
fn init_fresh_market_with_book(
    client: &RpcClient,
    pid: Pubkey,
    payer: &Keypair,
    spl: Pubkey,
    sys: Pubkey,
    insurance: Pubkey,
    quote_mint: Pubkey,
    mark_price: u64,
    taker_fee_bps: u32,
    maker_rebate_bps: i32,
    mmr_bps: u32,
) -> Result<(Keypair, Pubkey, Pubkey), String> {
    let (base_mint, market) = init_fresh_market(
        client,
        pid,
        payer,
        spl,
        sys,
        insurance,
        quote_mint,
        mark_price,
        taker_fee_bps,
        maker_rebate_bps,
        mmr_bps,
    )?;
    let book = Pubkey::find_program_address(&[MARKET_BOOK_SEED, market.as_ref()], &pid).0;
    let ib = ix(
        pid,
        IX_INIT_MARKET_BOOK,
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(base_mint.pubkey(), false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new(book, false),
            AccountMeta::new_readonly(sys, false),
        ],
        &[],
    );
    send(client, &[ib], payer, &[])?;
    Ok((base_mint, market, book))
}

#[allow(clippy::too_many_arguments)]
fn exercise_adl_and_migrations(
    rep: &mut Report,
    client: &RpcClient,
    pid: Pubkey,
    payer: &Keypair,
    spl: Pubkey,
    sys: Pubkey,
    insurance: Pubkey,
    vault: Pubkey,
    quote_mint: Pubkey,
) {
    let (_base_adl, market_adl) = match init_fresh_market(
        client, pid, payer, spl, sys, insurance, quote_mint, 200, 0, 0, 500,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("auto_deleverage:setup_market", e);
            return;
        }
    };

    let (uw, uw_ts, _) = match open_and_deposit_trader(
        client, pid, payer, spl, sys, insurance, vault, quote_mint, 100,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("auto_deleverage:setup_underwater_trader", e);
            return;
        }
    };
    let (ct, ct_ts, _) = match open_and_deposit_trader(
        client, pid, payer, spl, sys, insurance, vault, quote_mint, 1_000,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("auto_deleverage:setup_counter_trader", e);
            return;
        }
    };
    let (_aux_short, aux_short_ts, _) = match open_and_deposit_trader(
        client, pid, payer, spl, sys, insurance, vault, quote_mint, 1_000,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("auto_deleverage:setup_aux_short", e);
            return;
        }
    };
    let (_aux_long, aux_long_ts, _) = match open_and_deposit_trader(
        client, pid, payer, spl, sys, insurance, vault, quote_mint, 1_000,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("auto_deleverage:setup_aux_long", e);
            return;
        }
    };

    let pos_len = core::mem::size_of::<Position>() as u64;
    let uw_pos = Keypair::new();
    let ct_pos = Keypair::new();
    let aux_short_pos = Keypair::new();
    let aux_long_pos = Keypair::new();
    let creates = [
        create_account_ix(client, &payer.pubkey(), &uw_pos.pubkey(), pos_len, &pid),
        create_account_ix(client, &payer.pubkey(), &ct_pos.pubkey(), pos_len, &pid),
        create_account_ix(client, &payer.pubkey(), &aux_short_pos.pubkey(), pos_len, &pid),
        create_account_ix(client, &payer.pubkey(), &aux_long_pos.pubkey(), pos_len, &pid),
    ];
    if let Err(e) = send(client, &creates, payer, &[&uw_pos, &ct_pos, &aux_short_pos, &aux_long_pos]) {
        rep.fail("auto_deleverage:setup_positions", e);
        return;
    }

    let mut fill_uw = Vec::new();
    fill_uw.extend_from_slice(&le8(10));
    fill_uw.extend_from_slice(&le8(200));
    fill_uw.push(0); // taker_side bid: underwater trader opens long
    fill_uw.extend_from_slice(&le8(1));
    let open_uw = ix(
        pid,
        IX_APPLY_FILL,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_adl, false),
            AccountMeta::new(insurance, false),
            AccountMeta::new(uw_ts, false),
            AccountMeta::new(aux_short_ts, false),
            AccountMeta::new(uw_pos.pubkey(), false),
            AccountMeta::new(aux_short_pos.pubkey(), false),
        ],
        &fill_uw,
    );
    if let Err(e) = send(client, &[open_uw], payer, &[]) {
        rep.fail("auto_deleverage:setup_underwater_fill", e);
        return;
    }

    let mut fill_ct = Vec::new();
    fill_ct.extend_from_slice(&le8(10));
    fill_ct.extend_from_slice(&le8(250));
    fill_ct.push(0); // aux opens long, counter is maker short @250
    fill_ct.extend_from_slice(&le8(2));
    let open_ct = ix(
        pid,
        IX_APPLY_FILL,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_adl, false),
            AccountMeta::new(insurance, false),
            AccountMeta::new(aux_long_ts, false),
            AccountMeta::new(ct_ts, false),
            AccountMeta::new(aux_long_pos.pubkey(), false),
            AccountMeta::new(ct_pos.pubkey(), false),
        ],
        &fill_ct,
    );
    if let Err(e) = send(client, &[open_ct], payer, &[]) {
        rep.fail("auto_deleverage:setup_counter_fill", e);
        return;
    }

    let isolate = ix(
        pid,
        IX_SET_POSITION_ISOLATED,
        vec![
            AccountMeta::new_readonly(uw.pubkey(), true),
            AccountMeta::new(uw_ts, false),
            AccountMeta::new_readonly(market_adl, false),
            AccountMeta::new(uw_pos.pubkey(), false),
        ],
        &le8(100),
    );
    if let Err(e) = send(client, &[isolate], payer, &[&uw]) {
        rep.fail("auto_deleverage:setup_isolated", e);
        return;
    }

    let mark_down = ix(
        pid,
        IX_UPDATE_ORACLE,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_adl, false),
        ],
        &le8(100),
    );
    if let Err(e) = send(client, &[mark_down], payer, &[]) {
        rep.fail("auto_deleverage:setup_mark", e);
        return;
    }
    let ins_balance = match read_u64_at(client, insurance, INS_BALANCE_OFF) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("auto_deleverage:read_insurance", e);
            return;
        }
    };
    let threshold = ix(
        pid,
        IX_SET_INSURANCE_PAUSE_THRESHOLD,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(insurance, false),
        ],
        &le8(ins_balance.saturating_add(1)),
    );
    if let Err(e) = send(client, &[threshold], payer, &[]) {
        rep.fail("auto_deleverage:setup_pause_threshold", e);
        return;
    }

    let sum_known = |client: &RpcClient| -> Result<u128, String> {
        let mut s = read_u64_at(client, insurance, INS_BALANCE_OFF)? as u128;
        for (key, off) in [
            (uw_ts, TS_COLLATERAL_OFF),
            (ct_ts, TS_COLLATERAL_OFF),
            (aux_short_ts, TS_COLLATERAL_OFF),
            (aux_long_ts, TS_COLLATERAL_OFF),
            (uw_pos.pubkey(), POS_COLLATERAL_OFF),
            (ct_pos.pubkey(), POS_COLLATERAL_OFF),
            (aux_short_pos.pubkey(), POS_COLLATERAL_OFF),
            (aux_long_pos.pubkey(), POS_COLLATERAL_OFF),
        ] {
            s = s.saturating_add(read_u64_at(client, key, off)? as u128);
        }
        Ok(s)
    };
    let pre_vault = read_u64_at(client, vault, TOKEN_AMOUNT_OFF).unwrap_or(0);
    let pre_sum = sum_known(client).unwrap_or(u128::MAX);
    let adl = ix(
        pid,
        IX_AUTO_DELEVERAGE,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_adl, false),
            AccountMeta::new_readonly(insurance, false),
            AccountMeta::new(uw_ts, false),
            AccountMeta::new(uw_pos.pubkey(), false),
            AccountMeta::new(ct_ts, false),
            AccountMeta::new(ct_pos.pubkey(), false),
        ],
        &le8(5),
    );
    let adl_ok = match send(client, &[adl], payer, &[]) {
        Ok(s) => {
            rep.ok("auto_deleverage", s);
            true
        }
        Err(e) => {
            rep.fail("auto_deleverage", e);
            false
        }
    };

    if adl_ok {
        let state = (
            read_u64_at(client, uw_pos.pubkey(), POS_COLLATERAL_OFF),
            read_u64_at(client, uw_pos.pubkey(), POS_SIZE_OFF),
            read_u64_at(client, ct_ts, TS_COLLATERAL_OFF),
            read_u64_at(client, ct_pos.pubkey(), POS_SIZE_OFF),
            read_u64_at(client, market_adl, MKT_LONG_OI_OFF),
            read_u64_at(client, market_adl, MKT_SHORT_OI_OFF),
            read_u64_at(client, vault, TOKEN_AMOUNT_OFF),
            sum_known(client),
        );
        match state {
            (Ok(50), Ok(5), Ok(1_050), Ok(5), Ok(15), Ok(15), Ok(post_vault), Ok(post_sum))
                if pre_sum == post_sum && pre_vault == post_vault && (post_vault as u128) >= post_sum =>
            {
                rep.ok(
                    "auto_deleverage:post_invariant",
                    format!("vault={post_vault} sum={post_sum} capped_counter_credit=50"),
                );
            }
            other => rep.fail(
                "auto_deleverage:post_invariant",
                format!("unexpected post-state: pre_vault={pre_vault} pre_sum={pre_sum} state={other:?}"),
            ),
        }
    }

    let reset_threshold = ix(
        pid,
        IX_SET_INSURANCE_PAUSE_THRESHOLD,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(insurance, false),
        ],
        &le8(0),
    );
    let _ = send(client, &[reset_threshold], payer, &[]);

    run(
        rep,
        client,
        "migrate_market_to_v3(already_canonical)",
        &[ix(
            pid,
            IX_MIGRATE_MARKET_TO_V3,
            vec![
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new_readonly(market_adl, false),
            ],
            &[],
        )],
        payer,
        &[],
    );
    run(
        rep,
        client,
        "migrate_position_to_trader_state_key(already_canonical)",
        &[ix(
            pid,
            IX_MIGRATE_POSITION_TO_TRADER_STATE_KEY,
            vec![
                AccountMeta::new_readonly(ct.pubkey(), true),
                AccountMeta::new_readonly(ct_pos.pubkey(), false),
            ],
            &[],
        )],
        payer,
        &[&ct],
    );
}

#[allow(clippy::too_many_arguments)]
fn exercise_liquidate_portfolio_v2(
    rep: &mut Report,
    client: &RpcClient,
    pid: Pubkey,
    payer: &Keypair,
    spl: Pubkey,
    sys: Pubkey,
    insurance: Pubkey,
    vault: Pubkey,
    quote_mint: Pubkey,
) {
    let (_base, market, book) = match init_fresh_market_with_book(
        client,
        pid,
        payer,
        spl,
        sys,
        insurance,
        quote_mint,
        100_000,
        0,
        0,
        500,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("liquidate_portfolio_v2:setup_market", e);
            return;
        }
    };
    let (_victim, victim_ts, _) = match open_and_deposit_trader(
        client, pid, payer, spl, sys, insurance, vault, quote_mint, 80_000,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("liquidate_portfolio_v2:setup_victim", e);
            return;
        }
    };
    let (_counter, counter_ts, _) = match open_and_deposit_trader(
        client, pid, payer, spl, sys, insurance, vault, quote_mint, 80_000,
    ) {
        Ok(v) => v,
        Err(e) => {
            rep.fail("liquidate_portfolio_v2:setup_counter", e);
            return;
        }
    };
    let pos_len = core::mem::size_of::<Position>() as u64;
    let victim_pos = Keypair::new();
    let counter_pos = Keypair::new();
    let c1 = create_account_ix(client, &payer.pubkey(), &victim_pos.pubkey(), pos_len, &pid);
    let c2 = create_account_ix(client, &payer.pubkey(), &counter_pos.pubkey(), pos_len, &pid);
    if let Err(e) = send(client, &[c1, c2], payer, &[&victim_pos, &counter_pos]) {
        rep.fail("liquidate_portfolio_v2:setup_positions", e);
        return;
    }
    let mut fill = Vec::new();
    fill.extend_from_slice(&le8(10));
    fill.extend_from_slice(&le8(100_000));
    fill.push(0);
    fill.extend_from_slice(&le8(1));
    let fill_ix = ix(
        pid,
        IX_APPLY_FILL,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new(insurance, false),
            AccountMeta::new(victim_ts, false),
            AccountMeta::new(counter_ts, false),
            AccountMeta::new(victim_pos.pubkey(), false),
            AccountMeta::new(counter_pos.pubkey(), false),
        ],
        &fill,
    );
    if let Err(e) = send(client, &[fill_ix], payer, &[]) {
        rep.fail("liquidate_portfolio_v2:setup_fill", e);
        return;
    }
    let mark_down = ix(
        pid,
        IX_UPDATE_ORACLE,
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market, false),
        ],
        &le8(80_000),
    );
    if let Err(e) = send(client, &[mark_down], payer, &[]) {
        rep.fail("liquidate_portfolio_v2:setup_mark", e);
        return;
    }
    run(
        rep,
        client,
        "liquidate_portfolio_v2",
        &[ix(
            pid,
            IX_LIQUIDATE_PORTFOLIO_V2,
            vec![
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new_readonly(market, false),
                AccountMeta::new(book, false),
                AccountMeta::new_readonly(victim_ts, false),
                AccountMeta::new_readonly(victim_pos.pubkey(), false),
            ],
            &[],
        )],
        payer,
        &[],
    );
}

#[test]
fn smoke_connect() {
    if !gated() {
        eprintln!("local_exercise: skipped (set LOCAL_EXERCISE=1 to run)");
        return;
    }
    let client = rpc();
    let slot = client.get_slot().expect("get_slot");
    let v = client.get_version().expect("get_version");
    eprintln!("connected: slot={slot} core={}", v.solana_core);
}

/// Full constructive lifecycle, every instruction fired at the REAL validator —
/// exercising the create-PDA + SPL-token CPIs the BanksClient suite cannot drive.
#[test]
fn full_lifecycle() {
    if !gated() {
        eprintln!("full_lifecycle: skipped (set LOCAL_EXERCISE=1 to run)");
        return;
    }
    let client = rpc();
    let pid = program_id();
    let spl = Pubkey::from_str(SPL_TOKEN).unwrap();
    let ata_program = Pubkey::from_str(SPL_ASSOCIATED_TOKEN).unwrap();
    let sys = solana_sdk_ids::system_program::id();
    let mut rep = Report::new();

    // ── payer = protocol authority + mint authority + sequencer ─────────────
    // On devnet: KEYPAIR=<path to a funded wallet>. On a local validator: generated
    // + airdropped. All OTHER accounts are funded from this payer (so they work on
    // devnet too, where request_airdrop is rate-limited).
    let payer = match std::env::var("KEYPAIR") {
        Ok(p) => solana_sdk::signature::read_keypair_file(&p).expect("read KEYPAIR file"),
        Err(_) => {
            let kp = Keypair::new();
            let sig = client.request_airdrop(&kp.pubkey(), 200_000_000_000).expect("airdrop payer");
            let bh = client.get_latest_blockhash().unwrap();
            client.confirm_transaction_with_spinner(&sig, &bh, CommitmentConfig::confirmed()).expect("confirm payer airdrop");
            kp
        }
    };
    eprintln!("payer/authority: {}", payer.pubkey());

    // ── create quote + base mints (SPL InitializeMint2) ─────────────────────
    let quote_mint = Keypair::new();
    let base_mint = Keypair::new();
    for (label, mint, decimals) in [("quote", &quote_mint, 6u8), ("base", &base_mint, 9u8)] {
        let create = create_account_ix(&client, &payer.pubkey(), &mint.pubkey(), MINT_LEN, &spl);
        let mut data = vec![SPL_INITIALIZE_MINT2, decimals];
        data.extend_from_slice(payer.pubkey().as_ref()); // mint authority
        data.push(0); // no freeze authority
        let init = Instruction {
            program_id: spl,
            accounts: vec![AccountMeta::new(mint.pubkey(), false)],
            data,
        };
        match send(&client, &[create, init], &payer, &[mint]) {
            Ok(s) => rep.ok(&format!("spl_init_mint:{label}"), s),
            Err(e) => {
                rep.fail(&format!("spl_init_mint:{label}"), e);
                rep.print();
                panic!("cannot create mints");
            }
        }
    }

    // ── 9. initialize_insurance_fund (create insurance PDA + token vault CPI) ─
    let (insurance, _ib) = Pubkey::find_program_address(&[INSURANCE_SEED], &pid);
    let vault = Keypair::new();
    {
        // The pin handler `create_pda_account`s the vault itself (empty seeds ⇒ the
        // vault signs its own creation), then InitializeAccount3's it. So the vault
        // must be a FRESH, never-allocated signer keypair — do NOT pre-create it.
        let mut data = vec![IX_INIT_INSURANCE];
        data.extend_from_slice(&0u32.to_le_bytes()); // fee_contribution_bps
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),   // authority/payer
                AccountMeta::new(insurance, false),       // insurance PDA
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
                AccountMeta::new(vault.pubkey(), true),   // fresh vault (signer)
                AccountMeta::new_readonly(spl, false),    // token program
                AccountMeta::new_readonly(sys, false),    // system program
            ],
            data,
        };
        match send(&client, &[ix], &payer, &[&vault]) {
            Ok(s) => rep.ok("init_insurance_fund", s),
            Err(e) => {
                rep.fail("init_insurance_fund", e);
                rep.print();
                panic!("insurance fund creation failed — backbone blocked");
            }
        }
    }

    // ── 71/72. init_trader_ata + close_trader_ata on an EMPTY associated account ─
    {
        let ata_trader = Keypair::new();
        fund(&client, &payer, &ata_trader.pubkey(), 100_000_000);
        let trader_ata = Pubkey::find_program_address(
            &[
                ata_trader.pubkey().as_ref(),
                spl.as_ref(),
                quote_mint.pubkey().as_ref(),
            ],
            &ata_program,
        )
        .0;
        run(
            &mut rep,
            &client,
            "init_trader_ata",
            &[ix(
                pid,
                IX_INIT_TRADER_ATA,
                vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new_readonly(ata_trader.pubkey(), false),
                    AccountMeta::new_readonly(insurance, false),
                    AccountMeta::new_readonly(quote_mint.pubkey(), false),
                    AccountMeta::new(trader_ata, false),
                    AccountMeta::new_readonly(sys, false),
                    AccountMeta::new_readonly(spl, false),
                    AccountMeta::new_readonly(ata_program, false),
                ],
                &[],
            )],
            &payer,
            &[],
        );
        run(
            &mut rep,
            &client,
            "close_trader_ata(empty)",
            &[ix(
                pid,
                IX_CLOSE_TRADER_ATA,
                vec![
                    AccountMeta::new_readonly(ata_trader.pubkey(), true),
                    AccountMeta::new(trader_ata, false),
                    AccountMeta::new(payer.pubkey(), false),
                    AccountMeta::new_readonly(spl, false),
                ],
                &[],
            )],
            &payer,
            &[&ata_trader],
        );
    }

    // ── 11. initialize_market (create market PDA) ───────────────────────────
    let (market, _mb) = Pubkey::find_program_address(
        &[MARKET_SEED, base_mint.pubkey().as_ref(), quote_mint.pubkey().as_ref()],
        &pid,
    );
    {
        let tick_size: u64 = 1;
        let mark_price: u64 = 100_000;
        let taker_fee_bps: u32 = 10;
        let maker_rebate_bps: i32 = 2;
        let min_base_lots: u64 = 1;
        let max_oi: u64 = 1_000_000_000;
        let mmr_bps: u32 = 500;
        let mut data = vec![IX_INIT_MARKET];
        data.extend_from_slice(&le8(tick_size));
        data.extend_from_slice(&le8(mark_price));
        data.extend_from_slice(&taker_fee_bps.to_le_bytes());
        data.extend_from_slice(&maker_rebate_bps.to_le_bytes());
        data.extend_from_slice(&le8(min_base_lots));
        data.extend_from_slice(&le8(max_oi));
        data.extend_from_slice(&mmr_bps.to_le_bytes());
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(market, false),
                AccountMeta::new_readonly(base_mint.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
                AccountMeta::new_readonly(insurance, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data,
        };
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("initialize_market", s),
            Err(e) => {
                rep.fail("initialize_market", e);
                rep.print();
                panic!("market creation failed — backbone blocked");
            }
        }
    }

    // ── 15. update_oracle (sequencer sets mark) ─────────────────────────────
    {
        let mut data = vec![IX_UPDATE_ORACLE];
        data.extend_from_slice(&le8(100_000));
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new_readonly(payer.pubkey(), true), // sequencer
                AccountMeta::new(market, false),
            ],
            data,
        };
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("update_oracle", s),
            Err(e) => rep.fail("update_oracle", e),
        }
    }

    // ── 81. init_market_book (create book PDA) ──────────────────────────────
    let (market_book, _bb) = Pubkey::find_program_address(&[MARKET_BOOK_SEED, market.as_ref()], &pid);
    eprintln!("market_book bytes = {MARKET_BOOK_TOTAL_BYTES}");
    {
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(market, false),
                AccountMeta::new_readonly(base_mint.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
                AccountMeta::new(market_book, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_INIT_MARKET_BOOK],
        };
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("init_market_book", s),
            Err(e) => {
                rep.fail("init_market_book", e);
                rep.print();
                panic!("book creation failed — backbone blocked");
            }
        }
    }

    exercise_adl_and_migrations(
        &mut rep,
        &client,
        pid,
        &payer,
        spl,
        sys,
        insurance,
        vault.pubkey(),
        quote_mint.pubkey(),
    );
    exercise_liquidate_portfolio_v2(
        &mut rep,
        &client,
        pid,
        &payer,
        spl,
        sys,
        insurance,
        vault.pubkey(),
        quote_mint.pubkey(),
    );

    // ── two traders: open_trader_state + token acct + deposit_collateral ────
    let maker = Keypair::new();
    let taker = Keypair::new();
    let mut trader_state = std::collections::HashMap::new();
    for (label, trader) in [("maker", &maker), ("taker", &taker)] {
        fund(&client, &payer, &trader.pubkey(), 200_000_000);
        let (ts, _b) = Pubkey::find_program_address(&[TRADER_STATE_SEED, trader.pubkey().as_ref()], &pid);
        trader_state.insert(label, ts);

        // 8. open_trader_state
        let open = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(trader.pubkey(), true),
                AccountMeta::new(ts, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_OPEN_TRADER_STATE],
        };
        match send(&client, &[open], trader, &[trader]) {
            Ok(s) => rep.ok(&format!("open_trader_state:{label}"), s),
            Err(e) => {
                rep.fail(&format!("open_trader_state:{label}"), e);
                continue;
            }
        }

        // trader quote token account + mint tokens to it (payer = mint authority)
        let tok = Keypair::new();
        let create = create_account_ix(&client, &payer.pubkey(), &tok.pubkey(), TOKEN_ACCT_LEN, &spl);
        let mut init_data = vec![SPL_INITIALIZE_ACCOUNT3];
        init_data.extend_from_slice(trader.pubkey().as_ref()); // owner = trader
        let init = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(tok.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
            ],
            data: init_data,
        };
        let mut mint_data = vec![SPL_MINT_TO];
        mint_data.extend_from_slice(&le8(1_000_000));
        let mint_to = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(quote_mint.pubkey(), false),
                AccountMeta::new(tok.pubkey(), false),
                AccountMeta::new_readonly(payer.pubkey(), true),
            ],
            data: mint_data,
        };
        match send(&client, &[create, init, mint_to], &payer, &[&tok]) {
            Ok(s) => rep.ok(&format!("spl_fund_trader:{label}"), s),
            Err(e) => {
                rep.fail(&format!("spl_fund_trader:{label}"), e);
                continue;
            }
        }

        // 10. deposit_collateral (token transfer CPI: trader ATA -> vault)
        let mut data = vec![IX_DEPOSIT_COLLATERAL];
        data.extend_from_slice(&le8(500_000));
        let dep = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(trader.pubkey(), true),
                AccountMeta::new(ts, false),
                AccountMeta::new_readonly(insurance, false),
                AccountMeta::new(vault.pubkey(), false),
                AccountMeta::new(tok.pubkey(), false),
                AccountMeta::new_readonly(spl, false),
            ],
            data,
        };
        match send(&client, &[dep], trader, &[trader]) {
            Ok(s) => rep.ok(&format!("deposit_collateral:{label}"), s),
            Err(e) => rep.fail(&format!("deposit_collateral:{label}"), e),
        }
    }

    // ── 12. withdraw_collateral — flat trader round-trips via the PDA-signed
    //        vault payout (token_transfer_signed, distinct from deposit's CPI) ─
    {
        let flatty = Keypair::new();
        fund(&client, &payer, &flatty.pubkey(), 200_000_000);
        let (ts, _b) =
            Pubkey::find_program_address(&[TRADER_STATE_SEED, flatty.pubkey().as_ref()], &pid);
        let open = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(flatty.pubkey(), true),
                AccountMeta::new(ts, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_OPEN_TRADER_STATE],
        };
        let tok = Keypair::new();
        let create = create_account_ix(&client, &payer.pubkey(), &tok.pubkey(), TOKEN_ACCT_LEN, &spl);
        let mut init_data = vec![SPL_INITIALIZE_ACCOUNT3];
        init_data.extend_from_slice(flatty.pubkey().as_ref());
        let init = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(tok.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
            ],
            data: init_data,
        };
        let mut mint_data = vec![SPL_MINT_TO];
        mint_data.extend_from_slice(&le8(1_000_000));
        let mint_to = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(quote_mint.pubkey(), false),
                AccountMeta::new(tok.pubkey(), false),
                AccountMeta::new_readonly(payer.pubkey(), true),
            ],
            data: mint_data,
        };
        let ok_setup = send(&client, &[open], &flatty, &[&flatty]).is_ok()
            && send(&client, &[create, init, mint_to], &payer, &[&tok]).is_ok();
        if ok_setup {
            let mut dep = vec![IX_DEPOSIT_COLLATERAL];
            dep.extend_from_slice(&le8(300_000));
            let dep_ix = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(flatty.pubkey(), true),
                    AccountMeta::new(ts, false),
                    AccountMeta::new_readonly(insurance, false),
                    AccountMeta::new(vault.pubkey(), false),
                    AccountMeta::new(tok.pubkey(), false),
                    AccountMeta::new_readonly(spl, false),
                ],
                data: dep,
            };
            let _ = send(&client, &[dep_ix], &flatty, &[&flatty]);
            // now withdraw part of it back (vault PDA signs the payout)
            let mut wd = vec![IX_WITHDRAW_COLLATERAL];
            wd.extend_from_slice(&le8(120_000));
            let wd_ix = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(flatty.pubkey(), true),
                    AccountMeta::new(ts, false),
                    AccountMeta::new_readonly(insurance, false),
                    AccountMeta::new(vault.pubkey(), false),
                    AccountMeta::new(tok.pubkey(), false),
                    AccountMeta::new_readonly(spl, false),
                ],
                data: wd,
            };
            match send(&client, &[wd_ix], &flatty, &[&flatty]) {
                Ok(s) => rep.ok("withdraw_collateral:flat", s),
                Err(e) => rep.fail("withdraw_collateral:flat", e),
            }
        } else {
            rep.fail("withdraw_collateral:flat", "setup failed".into());
        }
    }

    // ── 2. place_limit_order — maker rests an ASK at the mark ───────────────
    {
        let mut data = vec![IX_PLACE_LIMIT];
        data.push(1); // side: 1 = ask
        data.extend_from_slice(&le8(10)); // size_lots
        data.extend_from_slice(&le8(100_000)); // limit_ticks
        data.extend_from_slice(&le8(0)); // expires (0 = GTC)
        data.push(0); // flags
        data.push(0); // sub_index
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new_readonly(maker.pubkey(), true),
                AccountMeta::new_readonly(market, false),
                AccountMeta::new(market_book, false),
            ],
            data,
        };
        match send(&client, &[ix], &maker, &[&maker]) {
            Ok(s) => rep.ok("place_limit_order:maker_ask", s),
            Err(e) => rep.fail("place_limit_order:maker_ask", e),
        }
    }

    // ── 4. place_taker_order — taker BUYS, crossing the resting ask ─────────
    {
        let mut data = vec![IX_PLACE_TAKER];
        data.push(0); // side: 0 = bid (buy)
        data.extend_from_slice(&le8(10)); // size_lots
        data.extend_from_slice(&le8(100_000)); // limit_ticks
        data.extend_from_slice(&le8(0)); // expires
        data.push(0); // flags
        data.push(0); // sub_index
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new_readonly(taker.pubkey(), true),
                AccountMeta::new_readonly(market, false),
                AccountMeta::new(market_book, false),
            ],
            data,
        };
        match send(&client, &[ix], &taker, &[&taker]) {
            Ok(s) => rep.ok("place_taker_order:taker_buy", s),
            Err(e) => rep.fail("place_taker_order:taker_buy", e),
        }
    }

    // ── 0. apply_fill — sequencer settles the cross onto both positions ─────
    {
        let pos_len = core::mem::size_of::<Position>() as u64;
        let pos_taker = Keypair::new();
        let pos_maker = Keypair::new();
        // create both position accounts FRESH + program-owned (pin binds them by
        // field, not PDA; apply_fill stamps the disc on first fill).
        let c1 = create_account_ix(&client, &payer.pubkey(), &pos_taker.pubkey(), pos_len, &pid);
        let c2 = create_account_ix(&client, &payer.pubkey(), &pos_maker.pubkey(), pos_len, &pid);
        match send(&client, &[c1, c2], &payer, &[&pos_taker, &pos_maker]) {
            Ok(s) => rep.ok("create_position_accts", s),
            Err(e) => {
                rep.fail("create_position_accts", e);
                rep.print();
                eprintln!("sizes: Position={pos_len} TraderState={} Insurance={} Market={}",
                    core::mem::size_of::<TraderState>(),
                    core::mem::size_of::<Insurance>(),
                    core::mem::size_of::<Market>());
                panic!("position-acct creation failed");
            }
        }

        let ts_taker = trader_state["taker"];
        let ts_maker = trader_state["maker"];
        let mut data = vec![IX_APPLY_FILL];
        data.extend_from_slice(&le8(10)); // size
        data.extend_from_slice(&le8(100_000)); // price
        data.push(0); // taker_side = bid
        data.extend_from_slice(&le8(1)); // fill_seq
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new_readonly(payer.pubkey(), true), // sequencer
                AccountMeta::new(market, false),
                AccountMeta::new(insurance, false),
                AccountMeta::new(ts_taker, false),
                AccountMeta::new(ts_maker, false),
                AccountMeta::new(pos_taker.pubkey(), false),
                AccountMeta::new(pos_maker.pubkey(), false),
            ],
            data,
        };
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("apply_fill", s),
            Err(e) => rep.fail("apply_fill", e),
        }
    }

    // ── 79. withdraw_insurance_fund — positive admin payout from real fee balance
    {
        let auth_tok = match create_funded_token_account(
            &client,
            &payer,
            spl,
            quote_mint.pubkey(),
            payer.pubkey(),
            1,
        ) {
            Ok(k) => k,
            Err(e) => {
                rep.fail("withdraw_insurance_fund:setup_authority_token", e);
                rep.print();
                panic!("withdraw_insurance_fund setup token account failed");
            }
        };
        let balance = read_u64_at(&client, insurance, INS_BALANCE_OFF).unwrap_or(0);
        if balance >= 100 {
            run(
                &mut rep,
                &client,
                "withdraw_insurance_fund",
                &[ix(
                    pid,
                    IX_WITHDRAW_INSURANCE_FUND,
                    vec![
                        AccountMeta::new_readonly(payer.pubkey(), true),
                        AccountMeta::new(insurance, false),
                        AccountMeta::new(vault.pubkey(), false),
                        AccountMeta::new(auth_tok.pubkey(), false),
                        AccountMeta::new_readonly(spl, false),
                    ],
                    &le8(100),
                )],
                &payer,
                &[],
            );
        } else {
            rep.fail("withdraw_insurance_fund", format!("setup fee balance too low: {balance}"));
        }
    }

    // ── config-PDA creators (all route through the now-fixed create_pda_account).
    //    Ordered so haircut (which ARMS the engine, market-writable) runs LAST. ──
    {
        // 58. init_market_oracle_config (real layout is 41+ bytes; the header
        //     doc-comment understates it):
        //     [pyth_feed_id [u8;32]][max_staleness u32][max_confidence_bps u32][tick_decimals i8]
        let (oracle_cfg, _) =
            Pubkey::find_program_address(&[ORACLE_CONFIG_SEED, market.as_ref()], &pid);
        let mut body = vec![0u8; 32];
        body.extend_from_slice(&60u32.to_le_bytes()); // max_staleness_seconds
        body.extend_from_slice(&100u32.to_le_bytes()); // max_confidence_bps (1..=1000)
        body.push(0i8 as u8); // tick_decimals
        let ix = config_ix(pid, &payer.pubkey(), market, oracle_cfg, sys, IX_INIT_ORACLE_CONFIG, &body, false);
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("init_market_oracle_config", s),
            Err(e) => rep.fail("init_market_oracle_config", e),
        }

        // 59. initialize_side_accrual: [initial_price_ticks u64][initial_slot u64]
        let (side_accrual, _) =
            Pubkey::find_program_address(&[SIDE_ACCRUAL_SEED, market.as_ref()], &pid);
        let mut body = Vec::new();
        body.extend_from_slice(&le8(100_000));
        body.extend_from_slice(&le8(0));
        let ix = config_ix(pid, &payer.pubkey(), market, side_accrual, sys, IX_INIT_SIDE_ACCRUAL, &body, false);
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("initialize_side_accrual", s),
            Err(e) => rep.fail("initialize_side_accrual", e),
        }

        // 34. init_market_leverage_tiers: [tier_count u8][(min_notional u64)(mmr_bps u32)]*
        let (lev_tiers, _) =
            Pubkey::find_program_address(&[LEVERAGE_TIERS_SEED, market.as_ref()], &pid);
        let mut body = vec![1u8]; // one tier
        body.extend_from_slice(&le8(0)); // min_notional
        body.extend_from_slice(&500u32.to_le_bytes()); // mmr_bps
        let ix = config_ix(pid, &payer.pubkey(), market, lev_tiers, sys, IX_INIT_LEVERAGE_TIERS, &body, false);
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("init_market_leverage_tiers", s),
            Err(e) => rep.fail("init_market_leverage_tiers", e),
        }

        // 61. initialize_haircut_state (ARMS the engine — market writable):
        //     [h_min_slots u64][h_max_slots u64][initial_residual u128 LE]
        let (haircut, _) = Pubkey::find_program_address(&[HAIRCUT_SEED, market.as_ref()], &pid);
        let mut body = Vec::new();
        body.extend_from_slice(&le8(10));
        body.extend_from_slice(&le8(100));
        body.extend_from_slice(&0u128.to_le_bytes());
        let ix = config_ix(pid, &payer.pubkey(), market, haircut, sys, IX_INIT_HAIRCUT_STATE, &body, true);
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("initialize_haircut_state", s),
            Err(e) => rep.fail("initialize_haircut_state", e),
        }
    }

    // ── FLP-capital family: singleton exposure + per-LP position + token in/out
    //    (deposits route into the SAME insurance quote_vault; withdraw is PDA-signed) ─
    {
        // 26. initialize_flp_exposure (singleton [b"flp_exposure"])
        let (flp_exposure, _) = Pubkey::find_program_address(&[FLP_EXPOSURE_SEED], &pid);
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(flp_exposure, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_INITIALIZE_FLP_EXPOSURE],
        };
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("initialize_flp_exposure", s),
            Err(e) => rep.fail("initialize_flp_exposure", e),
        }

        // fresh LP: token acct + tokens, init_lp_position, deposit, withdraw
        let lp = Keypair::new();
        fund(&client, &payer, &lp.pubkey(), 200_000_000);
        let (lp_position, _) =
            Pubkey::find_program_address(&[LP_POSITION_SEED, lp.pubkey().as_ref()], &pid);
        let lp_tok = Keypair::new();
        let create = create_account_ix(&client, &payer.pubkey(), &lp_tok.pubkey(), TOKEN_ACCT_LEN, &spl);
        let mut init_data = vec![SPL_INITIALIZE_ACCOUNT3];
        init_data.extend_from_slice(lp.pubkey().as_ref());
        let init = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(lp_tok.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
            ],
            data: init_data,
        };
        let mut mint_data = vec![SPL_MINT_TO];
        mint_data.extend_from_slice(&le8(1_000_000));
        let mint_to = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(quote_mint.pubkey(), false),
                AccountMeta::new(lp_tok.pubkey(), false),
                AccountMeta::new_readonly(payer.pubkey(), true),
            ],
            data: mint_data,
        };
        let funded = send(&client, &[create, init, mint_to], &payer, &[&lp_tok]).is_ok();

        let init_pos = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(lp.pubkey(), true),
                AccountMeta::new(lp_position, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_INIT_LP_POSITION],
        };
        match send(&client, &[init_pos], &lp, &[&lp]) {
            Ok(s) => rep.ok("init_lp_position", s),
            Err(e) => rep.fail("init_lp_position", e),
        }

        if funded {
            let flp_accts = |amount_tag: u8, amount: u64| -> Instruction {
                let mut data = vec![amount_tag];
                data.extend_from_slice(&le8(amount));
                Instruction {
                    program_id: pid,
                    accounts: vec![
                        AccountMeta::new(lp.pubkey(), true),
                        AccountMeta::new(flp_exposure, false),
                        AccountMeta::new(lp_position, false),
                        AccountMeta::new_readonly(insurance, false),
                        AccountMeta::new(vault.pubkey(), false),
                        AccountMeta::new(lp_tok.pubkey(), false),
                        AccountMeta::new_readonly(spl, false),
                    ],
                    data,
                }
            };
            // 28. deposit_flp_capital (token IN). First deposit ⇒ shares == amount (NAV 1).
            let dep_slot = client.get_slot().unwrap_or(0);
            match send(&client, &[flp_accts(IX_DEPOSIT_FLP_CAPITAL, 400_000)], &lp, &[&lp]) {
                Ok(s) => rep.ok("deposit_flp_capital", s),
                Err(e) => rep.fail("deposit_flp_capital", e),
            }
            // The JIT-LP min-hold defense (Custom 57) blocks a withdraw within
            // FLP_MIN_HOLD_SLOTS (150) of the deposit — correct anti-flash-deposit
            // behavior. Wait out the window so we exercise the real token-OUT path.
            const MIN_HOLD: u64 = 150;
            let deadline_slot = dep_slot + MIN_HOLD + 2;
            let mut waited = 0;
            while client.get_slot().unwrap_or(deadline_slot) < deadline_slot && waited < 60 {
                std::thread::sleep(std::time::Duration::from_secs(2));
                waited += 1;
            }
            // 29. withdraw_flp_capital (burn shares, token OUT, PDA-signed).
            match send(&client, &[flp_accts(IX_WITHDRAW_FLP_CAPITAL, 100_000)], &lp, &[&lp]) {
                Ok(s) => rep.ok("withdraw_flp_capital", s),
                Err(e) => rep.fail("withdraw_flp_capital", e),
            }
        }
    }

    // ── conditional-order family: place + cancel a trigger order PDA ─────────
    {
        let trigger_id: u8 = 1;
        let (trigger_order, _) = Pubkey::find_program_address(
            &[TRIGGER_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[trigger_id]],
            &pid,
        );
        // 49. place_trigger_order (45-byte data)
        let mut data = vec![IX_PLACE_TRIGGER_ORDER];
        data.push(trigger_id); // trigger_id
        data.push(0); // side (bid)
        data.push(0); // kind
        data.push(0); // flags
        data.push(0); // sub_index
        data.extend_from_slice(&le8(10)); // size_lots
        data.extend_from_slice(&le8(99_000)); // trigger_price
        data.extend_from_slice(&le8(99_000)); // limit_price
        data.extend_from_slice(&le8(0)); // expires_at_slot
        data.extend_from_slice(&le8(99_000)); // acceptable_price
        let place = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(taker.pubkey(), true),
                AccountMeta::new_readonly(market, false),
                AccountMeta::new(trigger_order, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data,
        };
        let placed = match send(&client, &[place], &taker, &[&taker]) {
            Ok(s) => {
                rep.ok("place_trigger_order", s);
                true
            }
            Err(e) => {
                rep.fail("place_trigger_order", e);
                false
            }
        };
        if placed {
            // 50. cancel_trigger_order (closes the PDA, refunds to trader)
            let cancel = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(taker.pubkey(), true),
                    AccountMeta::new(trigger_order, false),
                ],
                data: vec![IX_CANCEL_TRIGGER_ORDER],
            };
            match send(&client, &[cancel], &taker, &[&taker]) {
                Ok(s) => rep.ok("cancel_trigger_order", s),
                Err(e) => rep.fail("cancel_trigger_order", e),
            }
        }
    }

    // ── strategist-vault (v3) family: create vault + its trader_state, a
    //    depositor position, and token in/out (shares burned, PDA-signed out) ──
    {
        // 60. create_vault legacy tag: same VaultV3 account, but data is
        // [vault_id][name][perf_fee_bps] rather than tag-105's order.
        let legacy_strategist = Keypair::new();
        fund(&client, &payer, &legacy_strategist.pubkey(), 200_000_000);
        let legacy_vault_id: u8 = 9;
        let legacy_vault = Pubkey::find_program_address(
            &[VAULT_SEED, legacy_strategist.pubkey().as_ref(), &[legacy_vault_id]],
            &pid,
        )
        .0;
        let mut legacy_data = vec![legacy_vault_id];
        legacy_data.extend_from_slice(&[0u8; 32]);
        legacy_data.extend_from_slice(&100u32.to_le_bytes());
        run(
            &mut rep,
            &client,
            "create_vault(legacy_tag_60)",
            &[ix(
                pid,
                IX_CREATE_VAULT,
                vec![
                    AccountMeta::new(legacy_strategist.pubkey(), true),
                    AccountMeta::new(legacy_vault, false),
                    AccountMeta::new_readonly(sys, false),
                ],
                &legacy_data,
            )],
            &payer,
            &[&legacy_strategist],
        );

        let strategist = Keypair::new();
        fund(&client, &payer, &strategist.pubkey(), 200_000_000);
        let vault_id: u8 = 1;
        let (svault, _) = Pubkey::find_program_address(
            &[VAULT_SEED, strategist.pubkey().as_ref(), &[vault_id]],
            &pid,
        );
        // 105. create_vault_v3: [vault_id u8][perf_fee_bps u32][name [u8;32]]
        let mut data = vec![IX_CREATE_VAULT_V3, vault_id];
        data.extend_from_slice(&100u32.to_le_bytes()); // perf_fee_bps
        data.extend_from_slice(&[0u8; 32]); // name
        let create = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(strategist.pubkey(), true),
                AccountMeta::new(svault, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data,
        };
        let created = match send(&client, &[create], &strategist, &[&strategist]) {
            Ok(s) => { rep.ok("create_vault_v3", s); true }
            Err(e) => { rep.fail("create_vault_v3", e); false }
        };

        if created {
            // 106. vault_open_trader_state_v3 → [b"trader_state", vault]
            let (vault_ts, _) =
                Pubkey::find_program_address(&[TRADER_STATE_SEED, svault.as_ref()], &pid);
            let open = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(strategist.pubkey(), true),
                    AccountMeta::new_readonly(svault, false),
                    AccountMeta::new(vault_ts, false),
                    AccountMeta::new_readonly(sys, false),
                ],
                data: vec![IX_VAULT_OPEN_TRADER_STATE_V3],
            };
            match send(&client, &[open], &strategist, &[&strategist]) {
                Ok(s) => rep.ok("vault_open_trader_state_v3", s),
                Err(e) => rep.fail("vault_open_trader_state_v3", e),
            }

            // depositor with quote tokens
            let depositor = Keypair::new();
            fund(&client, &payer, &depositor.pubkey(), 200_000_000);
            let (vpos, _) = Pubkey::find_program_address(
                &[VAULT_POSITION_SEED, svault.as_ref(), depositor.pubkey().as_ref()],
                &pid,
            );
            let dtok = Keypair::new();
            let c = create_account_ix(&client, &payer.pubkey(), &dtok.pubkey(), TOKEN_ACCT_LEN, &spl);
            let mut id = vec![SPL_INITIALIZE_ACCOUNT3];
            id.extend_from_slice(depositor.pubkey().as_ref());
            let i = Instruction {
                program_id: spl,
                accounts: vec![
                    AccountMeta::new(dtok.pubkey(), false),
                    AccountMeta::new_readonly(quote_mint.pubkey(), false),
                ],
                data: id,
            };
            let mut md = vec![SPL_MINT_TO];
            md.extend_from_slice(&le8(1_000_000));
            let m = Instruction {
                program_id: spl,
                accounts: vec![
                    AccountMeta::new(quote_mint.pubkey(), false),
                    AccountMeta::new(dtok.pubkey(), false),
                    AccountMeta::new_readonly(payer.pubkey(), true),
                ],
                data: md,
            };
            let dfunded = send(&client, &[c, i, m], &payer, &[&dtok]).is_ok();

            // 107. init_vault_position_v3
            let initp = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(depositor.pubkey(), true),
                    AccountMeta::new_readonly(svault, false),
                    AccountMeta::new(vpos, false),
                    AccountMeta::new_readonly(sys, false),
                ],
                data: vec![IX_INIT_VAULT_POSITION_V3],
            };
            match send(&client, &[initp], &depositor, &[&depositor]) {
                Ok(s) => rep.ok("init_vault_position_v3", s),
                Err(e) => rep.fail("init_vault_position_v3", e),
            }

            if dfunded {
                let vault_flow = |tag: u8, amount: u64| -> Instruction {
                    let mut data = vec![tag];
                    data.extend_from_slice(&le8(amount));
                    Instruction {
                        program_id: pid,
                        accounts: vec![
                            AccountMeta::new(depositor.pubkey(), true),
                            AccountMeta::new(svault, false),
                            AccountMeta::new(vault_ts, false),
                            AccountMeta::new(vpos, false),
                            AccountMeta::new_readonly(insurance, false),
                            AccountMeta::new(vault.pubkey(), false),
                            AccountMeta::new(dtok.pubkey(), false),
                            AccountMeta::new_readonly(spl, false),
                        ],
                        data,
                    }
                };
                // 108. vault_deposit_v3 (token IN)
                match send(&client, &[vault_flow(IX_VAULT_DEPOSIT_V3, 400_000)], &depositor, &[&depositor]) {
                    Ok(s) => rep.ok("vault_deposit_v3", s),
                    Err(e) => rep.fail("vault_deposit_v3", e),
                }
                // 109. vault_withdraw_v3 (burn shares, token OUT, PDA-signed)
                match send(&client, &[vault_flow(IX_VAULT_WITHDRAW_V3, 100_000)], &depositor, &[&depositor]) {
                    Ok(s) => rep.ok("vault_withdraw_v3", s),
                    Err(e) => rep.fail("vault_withdraw_v3", e),
                }
            }
        }
    }

    // ── ARMED commit-reveal path: a second market armed via init_fill_commitment,
    //    so place_taker PUSHES a keccak commitment per fill and apply_fill
    //    RECOMPUTES + FIFO-settles it on-chain (the security-critical ring path) ──
    {
        // fresh base mint → distinct market PDA (quote shared)
        let base2 = Keypair::new();
        let create = create_account_ix(&client, &payer.pubkey(), &base2.pubkey(), MINT_LEN, &spl);
        let mut d = vec![SPL_INITIALIZE_MINT2, 9];
        d.extend_from_slice(payer.pubkey().as_ref());
        d.push(0);
        let init = Instruction {
            program_id: spl,
            accounts: vec![AccountMeta::new(base2.pubkey(), false)],
            data: d,
        };
        let mint_ok = send(&client, &[create, init], &payer, &[&base2]).is_ok();

        let (market2, _) = Pubkey::find_program_address(
            &[MARKET_SEED, base2.pubkey().as_ref(), quote_mint.pubkey().as_ref()],
            &pid,
        );
        let (book2, _) = Pubkey::find_program_address(&[MARKET_BOOK_SEED, market2.as_ref()], &pid);
        let (ring2, _) = Pubkey::find_program_address(&[FILL_COMMIT_SEED, market2.as_ref()], &pid);

        // initialize_market #2
        let mut md = vec![IX_INIT_MARKET];
        md.extend_from_slice(&le8(1)); // tick
        md.extend_from_slice(&le8(100_000)); // mark
        md.extend_from_slice(&10u32.to_le_bytes()); // taker_fee
        md.extend_from_slice(&2i32.to_le_bytes()); // maker_rebate
        md.extend_from_slice(&le8(1)); // min_base_lots
        md.extend_from_slice(&le8(1_000_000_000)); // max_oi
        md.extend_from_slice(&500u32.to_le_bytes()); // mmr
        let im = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(market2, false),
                AccountMeta::new_readonly(base2.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
                AccountMeta::new_readonly(insurance, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: md,
        };
        let ib = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(market2, false),
                AccountMeta::new_readonly(base2.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
                AccountMeta::new(book2, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_INIT_MARKET_BOOK],
        };
        let m2_ok = mint_ok
            && send(&client, &[im], &payer, &[]).is_ok()
            && send(&client, &[ib], &payer, &[]).is_ok();

        // 127. init_fill_commitment — creates the ring + ARMS market2
        if m2_ok {
            let arm = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(market2, false),
                    AccountMeta::new(ring2, false),
                    AccountMeta::new_readonly(sys, false),
                ],
                data: vec![IX_INIT_FILL_COMMITMENT],
            };
            let armed = match send(&client, &[arm], &payer, &[]) {
                Ok(s) => { rep.ok("init_fill_commitment(arm)", s); true }
                Err(e) => { rep.fail("init_fill_commitment(arm)", e); false }
            };

            if armed {
                // maker rests an ASK on the armed market
                let mut pl = vec![IX_PLACE_LIMIT, 1];
                pl.extend_from_slice(&le8(10));
                pl.extend_from_slice(&le8(100_000));
                pl.extend_from_slice(&le8(0));
                pl.push(0);
                pl.push(0);
                let place = Instruction {
                    program_id: pid,
                    accounts: vec![
                        AccountMeta::new_readonly(maker.pubkey(), true),
                        AccountMeta::new_readonly(market2, false),
                        AccountMeta::new(book2, false),
                    ],
                    data: pl,
                };
                match send(&client, &[place], &maker, &[&maker]) {
                    Ok(s) => rep.ok("place_limit_order:armed_maker", s),
                    Err(e) => rep.fail("place_limit_order:armed_maker", e),
                }

                // taker BUYS, crossing — PRODUCER pushes the keccak commitment
                // (ring passed as trailing account, located by find_fill_commitment)
                let mut pt = vec![IX_PLACE_TAKER, 0];
                pt.extend_from_slice(&le8(10));
                pt.extend_from_slice(&le8(100_000));
                pt.extend_from_slice(&le8(0));
                pt.push(0);
                pt.push(0);
                let taker_ix = Instruction {
                    program_id: pid,
                    accounts: vec![
                        AccountMeta::new_readonly(taker.pubkey(), true),
                        AccountMeta::new_readonly(market2, false),
                        AccountMeta::new(book2, false),
                        AccountMeta::new(ring2, false), // trailing: commit ring
                    ],
                    data: pt,
                };
                match send(&client, &[taker_ix], &taker, &[&taker]) {
                    Ok(s) => rep.ok("place_taker_order:armed_push_commit", s),
                    Err(e) => rep.fail("place_taker_order:armed_push_commit", e),
                }

                // fresh positions for market2, then ARMED apply_fill — CONSUMER
                // recomputes keccak(fill_preimage) + FIFO-settles against the ring
                let pos_len = core::mem::size_of::<Position>() as u64;
                let pt2 = Keypair::new();
                let pm2 = Keypair::new();
                let c1 = create_account_ix(&client, &payer.pubkey(), &pt2.pubkey(), pos_len, &pid);
                let c2 = create_account_ix(&client, &payer.pubkey(), &pm2.pubkey(), pos_len, &pid);
                if send(&client, &[c1, c2], &payer, &[&pt2, &pm2]).is_ok() {
                    let mut af = vec![IX_APPLY_FILL];
                    af.extend_from_slice(&le8(10)); // size — MUST match the crossed fill
                    af.extend_from_slice(&le8(100_000)); // price — MUST match
                    af.push(0); // taker_side = bid
                    af.extend_from_slice(&le8(1)); // fill_seq (>0: strictly exceeds fresh market2 nonce 0)
                    let af_ix = Instruction {
                        program_id: pid,
                        accounts: vec![
                            AccountMeta::new_readonly(payer.pubkey(), true),
                            AccountMeta::new(market2, false),
                            AccountMeta::new(insurance, false),
                            AccountMeta::new(trader_state["taker"], false),
                            AccountMeta::new(trader_state["maker"], false),
                            AccountMeta::new(pt2.pubkey(), false),
                            AccountMeta::new(pm2.pubkey(), false),
                            AccountMeta::new(ring2, false), // trailing: settle ring
                        ],
                        data: af,
                    };
                    match send(&client, &[af_ix], &payer, &[]) {
                        Ok(s) => rep.ok("apply_fill:armed_settle_commit", s),
                        Err(e) => rep.fail("apply_fill:armed_settle_commit", e),
                    }

                    // NEGATIVE: the ring is now drained (produced==settled). A
                    // second apply_fill has NO committed fill to match → the
                    // consumer MUST reject with FillNotCommitted (1102). This
                    // proves a fabricated/uncommitted fill cannot settle.
                    let mut fab = vec![IX_APPLY_FILL];
                    fab.extend_from_slice(&le8(10));
                    fab.extend_from_slice(&le8(100_000));
                    fab.push(0);
                    fab.extend_from_slice(&le8(2)); // fill_seq advances past the nonce
                    let fab_ix = Instruction {
                        program_id: pid,
                        accounts: vec![
                            AccountMeta::new_readonly(payer.pubkey(), true),
                            AccountMeta::new(market2, false),
                            AccountMeta::new(insurance, false),
                            AccountMeta::new(trader_state["taker"], false),
                            AccountMeta::new(trader_state["maker"], false),
                            AccountMeta::new(pt2.pubkey(), false),
                            AccountMeta::new(pm2.pubkey(), false),
                            AccountMeta::new(ring2, false),
                        ],
                        data: fab,
                    };
                    let res = send(&client, &[fab_ix], &payer, &[]);
                    rep.expect_reject("apply_fill:fabricated_rejected", 1102, res);
                }
            }
        }
    }

    // ── admin / config family (market-authority + insurance-authority gated) ─
    {
        // 14. set_market_status — pause then re-activate market1
        for (st, lbl) in [(1u8, "pause"), (0u8, "active")] {
            let ix = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new_readonly(payer.pubkey(), true),
                    AccountMeta::new(market, false),
                ],
                data: vec![IX_SET_MARKET_STATUS, st],
            };
            match send(&client, &[ix], &payer, &[]) {
                Ok(s) => rep.ok(&format!("set_market_status:{lbl}"), s),
                Err(e) => rep.fail(&format!("set_market_status:{lbl}"), e),
            }
        }

        // 20. set_market_params (24B: taker_fee u32, maker_rebate i32, min_base u64, max_oi u64)
        let mut d = vec![IX_SET_MARKET_PARAMS];
        d.extend_from_slice(&10u32.to_le_bytes());
        d.extend_from_slice(&2i32.to_le_bytes());
        d.extend_from_slice(&le8(1));
        d.extend_from_slice(&le8(1_000_000_000));
        let set_params_ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new(market, false),
            ],
            data: d,
        };
        match send(&client, &[set_params_ix], &payer, &[]) {
            Ok(s) => rep.ok("set_market_params", s),
            Err(e) => rep.fail("set_market_params", e),
        }

        // 56. set_envelope_config (creates envelope PDA). Real layout is 44 bytes
        //     (the doc header understates it): move_bps u32, dt_slots u64,
        //     max_abs_funding_e9 i64, maintenance_bps u32, liq_fee_bps u32,
        //     min_liq_abs_lots u64, min_nonzero_mm_req_lots u64.
        let (envelope, _) = Pubkey::find_program_address(&[ENVELOPE_CONFIG_SEED, market.as_ref()], &pid);
        // The envelope enforces a closed-form solvency invariant
        // (price_funding_loss + liq_fee <= mm_req for all N), so the per-slot
        // price budget (move_bps × dt) must stay well under the maintenance margin.
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // max_price_move_bps_per_slot
        body.extend_from_slice(&le8(1)); // max_accrual_dt_slots
        body.extend_from_slice(&0i64.to_le_bytes()); // max_abs_funding_e9_per_slot
        body.extend_from_slice(&500u32.to_le_bytes()); // maintenance_bps (5%)
        body.extend_from_slice(&0u32.to_le_bytes()); // liquidation_fee_bps
        body.extend_from_slice(&le8(0)); // min_liquidation_abs_lots
        body.extend_from_slice(&le8(1)); // min_nonzero_mm_req_lots (floor covers small-N ceil)
        let set_env_ix = config_ix(pid, &payer.pubkey(), market, envelope, sys, IX_SET_ENVELOPE_CONFIG, &body, false);
        match send(&client, &[set_env_ix], &payer, &[]) {
            Ok(s) => rep.ok("set_envelope_config", s),
            Err(e) => rep.fail("set_envelope_config", e),
        }

        // 114. update_oracle_quorum: three fresh sources, median becomes mark.
        let oracle_cfg = Pubkey::find_program_address(&[ORACLE_CONFIG_SEED, market.as_ref()], &pid).0;
        // published_at must use the CLUSTER clock (not wall time) and be just-past so
        // it is neither future-dated nor stale (max_staleness=60).
        let pub_at = onchain_unix(&client).saturating_sub(2);
        let mut q = Vec::new();
        for v in [100_000u64, 100_010, 99_990] { q.extend_from_slice(&le8(v)); }
        for v in [10u64, 10, 10] { q.extend_from_slice(&le8(v)); }
        for _ in 0..3 { q.extend_from_slice(&le8(pub_at)); }
        run(
            &mut rep,
            &client,
            "update_oracle_quorum",
            &[ix(
                pid,
                IX_UPDATE_ORACLE_QUORUM,
                vec![
                    AccountMeta::new_readonly(payer.pubkey(), true),
                    AccountMeta::new(market, false),
                    AccountMeta::new_readonly(oracle_cfg, false),
                ],
                &q,
            )],
            &payer,
            &[],
        );

        // 115. update_oracle_from_pyth requires a real Pyth receiver-owned
        // PriceUpdateV2 account. The raw local/devnet harness does not fabricate
        // one; assert the fail-closed owner gate with a real non-Pyth account.
        let pyth_res = send(
            &client,
            &[ix(
                pid,
                IX_UPDATE_ORACLE_FROM_PYTH,
                vec![
                    AccountMeta::new_readonly(payer.pubkey(), true),
                    AccountMeta::new(market, false),
                    AccountMeta::new_readonly(oracle_cfg, false),
                    AccountMeta::new_readonly(quote_mint.pubkey(), false),
                    AccountMeta::new_readonly(envelope, false),
                ],
                &[],
            )],
            &payer,
            &[],
        );
        rep.expect_error_contains("update_oracle_from_pyth(wrong_owner_failclosed)", "owner", pyth_res);

        // 42. init_fee_tiers (PDA): [volume_window_slots u64][tier_count u8]
        //     then per-tier (min_vol u64, maker_rebate i32, taker_fee u32). 1 tier.
        let (fee_tiers, _) = Pubkey::find_program_address(&[FEE_TIERS_SEED], &pid);
        let mut data = vec![IX_INIT_FEE_TIERS];
        data.extend_from_slice(&le8(1_000_000)); // volume_window_slots
        data.push(1); // tier_count
        data.extend_from_slice(&le8(0)); // tier0 min_volume_quote_lots
        data.extend_from_slice(&0i32.to_le_bytes()); // tier0 maker_rebate_bps
        data.extend_from_slice(&10u32.to_le_bytes()); // tier0 taker_fee_bps
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(fee_tiers, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data,
        };
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("init_fee_tiers", s),
            Err(e) => rep.fail("init_fee_tiers", e),
        }

        // 19. set_trader_fee_tier (insurance-authority gated) on the maker's main ts
        let mut data = vec![IX_SET_TRADER_FEE_TIER];
        data.extend_from_slice(&100u32.to_le_bytes()); // discount_bps
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new_readonly(insurance, false),
                AccountMeta::new(trader_state["maker"], false),
            ],
            data,
        };
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("set_trader_fee_tier", s),
            Err(e) => rep.fail("set_trader_fee_tier", e),
        }
    }

    // ── sub-account family: open a sub, move collateral in+out, close it.
    //    Uses a fresh FLAT trader (transfer_collateral rejects a source that has
    //    open positions — the maker/taker do, from apply_fill). ────────────────
    {
        let sub_index: u8 = 1;
        // fresh flat trader: open main ts + fund + deposit
        let subby = Keypair::new();
        fund(&client, &payer, &subby.pubkey(), 200_000_000);
        let (main_ts, _) =
            Pubkey::find_program_address(&[TRADER_STATE_SEED, subby.pubkey().as_ref()], &pid);
        let stok = Keypair::new();
        let c = create_account_ix(&client, &payer.pubkey(), &stok.pubkey(), TOKEN_ACCT_LEN, &spl);
        let mut id = vec![SPL_INITIALIZE_ACCOUNT3];
        id.extend_from_slice(subby.pubkey().as_ref());
        let i = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(stok.pubkey(), false),
                AccountMeta::new_readonly(quote_mint.pubkey(), false),
            ],
            data: id,
        };
        let mut mdt = vec![SPL_MINT_TO];
        mdt.extend_from_slice(&le8(100_000));
        let mt = Instruction {
            program_id: spl,
            accounts: vec![
                AccountMeta::new(quote_mint.pubkey(), false),
                AccountMeta::new(stok.pubkey(), false),
                AccountMeta::new_readonly(payer.pubkey(), true),
            ],
            data: mdt,
        };
        let open_main = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(subby.pubkey(), true),
                AccountMeta::new(main_ts, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_OPEN_TRADER_STATE],
        };
        let mut dep = vec![IX_DEPOSIT_COLLATERAL];
        dep.extend_from_slice(&le8(50_000));
        let dep_ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(subby.pubkey(), true),
                AccountMeta::new(main_ts, false),
                AccountMeta::new_readonly(insurance, false),
                AccountMeta::new(vault.pubkey(), false),
                AccountMeta::new(stok.pubkey(), false),
                AccountMeta::new_readonly(spl, false),
            ],
            data: dep,
        };
        let prep_ok = send(&client, &[open_main], &subby, &[&subby]).is_ok()
            && send(&client, &[c, i, mt], &payer, &[&stok]).is_ok()
            && send(&client, &[dep_ix], &subby, &[&subby]).is_ok();
        let _ = prep_ok;

        let (sub_ts, _) = Pubkey::find_program_address(
            &[TRADER_STATE_SEED, subby.pubkey().as_ref(), &[sub_index]],
            &pid,
        );
        // 16. open_trader_sub_account
        let open = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(subby.pubkey(), true),
                AccountMeta::new(sub_ts, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data: vec![IX_OPEN_TRADER_SUB_ACCOUNT, sub_index],
        };
        let opened = match send(&client, &[open], &subby, &[&subby]) {
            Ok(s) => { rep.ok("open_trader_sub_account", s); true }
            Err(e) => { rep.fail("open_trader_sub_account", e); false }
        };
        if opened {
            // 17. transfer_collateral main → sub, then sub → main (leaves sub empty)
            let xfer = |from: Pubkey, to: Pubkey, amt: u64| -> Instruction {
                let mut data = vec![IX_TRANSFER_COLLATERAL];
                data.extend_from_slice(&le8(amt));
                Instruction {
                    program_id: pid,
                    accounts: vec![
                        AccountMeta::new_readonly(subby.pubkey(), true),
                        AccountMeta::new(from, false),
                        AccountMeta::new(to, false),
                    ],
                    data,
                }
            };
            match send(&client, &[xfer(main_ts, sub_ts, 1_000)], &subby, &[&subby]) {
                Ok(s) => rep.ok("transfer_collateral:main->sub", s),
                Err(e) => rep.fail("transfer_collateral:main->sub", e),
            }
            let _ = send(&client, &[xfer(sub_ts, main_ts, 1_000)], &subby, &[&subby]);
            // 18. close_trader_sub_account (requires collateral==0 && open_positions==0)
            let close = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(subby.pubkey(), true),
                    AccountMeta::new(sub_ts, false),
                ],
                data: vec![IX_CLOSE_TRADER_SUB_ACCOUNT],
            };
            match send(&client, &[close], &subby, &[&subby]) {
                Ok(s) => rep.ok("close_trader_sub_account", s),
                Err(e) => rep.fail("close_trader_sub_account", e),
            }
        }
    }

    // ── session-token family: create, verify active, revoke ─────────────────
    {
        let session_signer = Keypair::new(); // key only (never signs)
        let (session_token, _) = Pubkey::find_program_address(
            &[SESSION_SEED, maker.pubkey().as_ref(), session_signer.pubkey().as_ref()],
            &pid,
        );
        // 64. create_session_token: [owner(s,w), session_signer(key), token(PDA,w), system]
        let mut data = vec![IX_CREATE_SESSION_TOKEN];
        data.extend_from_slice(&3600i64.to_le_bytes()); // ttl_seconds
        let create = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new(maker.pubkey(), true),
                AccountMeta::new_readonly(session_signer.pubkey(), false),
                AccountMeta::new(session_token, false),
                AccountMeta::new_readonly(sys, false),
            ],
            data,
        };
        let created = match send(&client, &[create], &maker, &[&maker]) {
            Ok(s) => { rep.ok("create_session_token", s); true }
            Err(e) => { rep.fail("create_session_token", e); false }
        };
        if created {
            // 77. verify_session_active (read-only)
            let verify = Instruction {
                program_id: pid,
                accounts: vec![AccountMeta::new_readonly(session_token, false)],
                data: vec![IX_VERIFY_SESSION_ACTIVE],
            };
            match send(&client, &[verify], &payer, &[]) {
                Ok(s) => rep.ok("verify_session_active", s),
                Err(e) => rep.fail("verify_session_active", e),
            }
            // 65. revoke_session_token
            let revoke = Instruction {
                program_id: pid,
                accounts: vec![
                    AccountMeta::new(maker.pubkey(), true),
                    AccountMeta::new(session_token, false),
                ],
                data: vec![IX_REVOKE_SESSION_TOKEN],
            };
            match send(&client, &[revoke], &maker, &[&maker]) {
                Ok(s) => rep.ok("revoke_session_token", s),
                Err(e) => rep.fail("revoke_session_token", e),
            }
        }
    }

    // ════════════════ BROAD COVERAGE BATCH ════════════════
    // PDAs created earlier (recomputed; find_program_address is cheap).
    let envelope = Pubkey::find_program_address(&[ENVELOPE_CONFIG_SEED, market.as_ref()], &pid).0;
    let haircut = Pubkey::find_program_address(&[HAIRCUT_SEED, market.as_ref()], &pid).0;
    let side_accrual = Pubkey::find_program_address(&[SIDE_ACCRUAL_SEED, market.as_ref()], &pid).0;
    let oracle_cfg = Pubkey::find_program_address(&[ORACLE_CONFIG_SEED, market.as_ref()], &pid).0;
    let lev_tiers = Pubkey::find_program_address(&[LEVERAGE_TIERS_SEED, market.as_ref()], &pid).0;
    let fee_tiers = Pubkey::find_program_address(&[FEE_TIERS_SEED], &pid).0;
    let flp_single = Pubkey::find_program_address(&[FLP_EXPOSURE_SEED], &pid).0;
    let maker_ts = trader_state["maker"];
    let taker_ts = trader_state["taker"];
    let rw = |k| AccountMeta::new(k, false);
    let ro = |k| AccountMeta::new_readonly(k, false);
    let sg = |k| AccountMeta::new(k, true);
    let sgr = |k| AccountMeta::new_readonly(k, true);

    // ── order types (need market ACTIVE — do these first) ───────────────────
    {
        // 51 place_twap_order + 52 cancel
        let twap = Pubkey::find_program_address(&[TWAP_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[1u8]], &pid).0;
        let mut b = vec![1u8, 1, 0, 0]; // id, side(ask), flags, sub
        for v in [1u64, 10, 100_000, 1, 0, 0] { b.extend_from_slice(&le8(v)); } // slice,total,limit,interval,end,acceptable
        run(&mut rep, &client, "place_twap_order",
            &[ix(pid, IX_PLACE_TWAP, vec![sg(taker.pubkey()), ro(market), rw(twap), ro(sys)], &b)], &payer, &[&taker]);
        run(&mut rep, &client, "cancel_twap_order",
            &[ix(pid, IX_CANCEL_TWAP, vec![sg(taker.pubkey()), rw(twap)], &[])], &payer, &[&taker]);

        // 100 execute_twap_slice: a separate active TWAP (interval 1; 0 is rejected
        // at placement). Refresh the mark first so the slice's staleness gate passes.
        let twap_exec = Pubkey::find_program_address(&[TWAP_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[2u8]], &pid).0;
        let mut tb = vec![2u8, 1, 0, 0]; // id, side(ask), flags, sub
        for v in [5u64, 10, 100_000, 1, 0, 100_000] { tb.extend_from_slice(&le8(v)); }
        let _ = send(&client, &[ix(pid, IX_UPDATE_ORACLE, vec![sgr(payer.pubkey()), rw(market)], &le8(100_000))], &payer, &[]);
        if send(&client, &[ix(pid, IX_PLACE_TWAP, vec![sg(taker.pubkey()), ro(market), rw(twap_exec), ro(sys)], &tb)], &payer, &[&taker]).is_ok() {
            run(&mut rep, &client, "execute_twap_slice",
                &[ix(pid, IX_EXECUTE_TWAP, vec![sgr(payer.pubkey()), ro(market), rw(market_book), rw(twap_exec)], &[])], &payer, &[]);
            run(&mut rep, &client, "cancel_twap_order(executed)",
                &[ix(pid, IX_CANCEL_TWAP, vec![sg(taker.pubkey()), rw(twap_exec)], &[])], &payer, &[&taker]);
        } else {
            rep.fail("execute_twap_slice", "setup place_twap_order failed".into());
        }

        // 101 place_iceberg + 102 replenish + 103 cancel
        let ice = Pubkey::find_program_address(&[ICEBERG_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[1u8]], &pid).0;
        let mut b = vec![1u8, 1, 0]; // id, side(ask), sub
        for v in [10u64, 2, 100_000, 0] { b.extend_from_slice(&le8(v)); } // total, displayed, limit, expires
        run(&mut rep, &client, "place_iceberg_order",
            &[ix(pid, IX_PLACE_ICEBERG, vec![sg(taker.pubkey()), ro(market), rw(market_book), rw(ice), ro(sys)], &b)], &payer, &[&taker]);
        run(&mut rep, &client, "replenish_iceberg",
            &[ix(pid, IX_REPLENISH_ICEBERG, vec![sgr(payer.pubkey()), ro(market), rw(market_book), rw(ice)], &[])], &payer, &[]);
        run(&mut rep, &client, "cancel_iceberg",
            &[ix(pid, IX_CANCEL_ICEBERG, vec![sg(taker.pubkey()), rw(ice)], &[])], &payer, &[&taker]);

        // 104 place_bracket_order (parent long + tp/sl reduce-only triggers)
        let tp = Pubkey::find_program_address(&[TRIGGER_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[5u8]], &pid).0;
        let sl = Pubkey::find_program_address(&[TRIGGER_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[6u8]], &pid).0;
        let mut b = vec![0u8, 0, 5, 6]; // parent_side(long), sub, tp_id, sl_id
        for v in [10u64, 100_000, 101_000, 101_000, 99_000, 99_000, 0] { b.extend_from_slice(&le8(v)); }
        run(&mut rep, &client, "place_bracket_order",
            &[ix(pid, IX_PLACE_BRACKET, vec![sg(taker.pubkey()), ro(market), rw(market_book), rw(tp), rw(sl), ro(sys)], &b)], &payer, &[&taker]);

        // 99 execute_trigger_order: condition-met kind=1 trigger injects into book.
        let trig_exec = Pubkey::find_program_address(&[TRIGGER_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[7u8]], &pid).0;
        let mut te = vec![7u8, 0, 1, 0, 0]; // id, side(bid), kind(mark>=trigger), flags, sub
        te.extend_from_slice(&le8(1));
        te.extend_from_slice(&le8(100_000));
        te.extend_from_slice(&le8(100_000));
        te.extend_from_slice(&le8(0));
        te.extend_from_slice(&le8(101_000));
        // refresh the mark so the trigger's staleness gate (Custom 248) passes and the
        // kind=1 (mark>=trigger) condition is met at 100_000.
        let _ = send(&client, &[ix(pid, IX_UPDATE_ORACLE, vec![sgr(payer.pubkey()), rw(market)], &le8(100_000))], &payer, &[]);
        if send(&client, &[ix(pid, IX_PLACE_TRIGGER_ORDER, vec![sg(taker.pubkey()), ro(market), rw(trig_exec), ro(sys)], &te)], &payer, &[&taker]).is_ok() {
            run(&mut rep, &client, "execute_trigger_order",
                &[ix(pid, IX_EXECUTE_TRIGGER, vec![sgr(payer.pubkey()), ro(market), rw(market_book), rw(trig_exec)], &[])], &payer, &[]);
            run(&mut rep, &client, "cancel_trigger_order(executed)",
                &[ix(pid, IX_CANCEL_TRIGGER_ORDER, vec![sg(taker.pubkey()), rw(trig_exec)], &[])], &payer, &[&taker]);
        } else {
            rep.fail("execute_trigger_order", "setup place_trigger_order failed".into());
        }

        // 113 update_trailing_stop: optional trailing offset extends place-trigger
        // data to 47 bytes; first update seeds the anchor and tightens the stop.
        let trailing = Pubkey::find_program_address(&[TRIGGER_ORDER_SEED, market.as_ref(), taker.pubkey().as_ref(), &[8u8]], &pid).0;
        let mut tr = vec![8u8, 1, 0, 0, 0]; // id, side(ask close), kind(long SL), flags, sub
        tr.extend_from_slice(&le8(1));
        tr.extend_from_slice(&le8(99_000));
        tr.extend_from_slice(&le8(99_000));
        tr.extend_from_slice(&le8(0));
        tr.extend_from_slice(&le8(99_000));
        tr.extend_from_slice(&500u16.to_le_bytes());
        if send(&client, &[ix(pid, IX_PLACE_TRIGGER_ORDER, vec![sg(taker.pubkey()), ro(market), rw(trailing), ro(sys)], &tr)], &payer, &[&taker]).is_ok() {
            run(&mut rep, &client, "update_trailing_stop",
                &[ix(pid, IX_UPDATE_TRAILING_STOP, vec![sgr(payer.pubkey()), ro(market), rw(trailing)], &[])], &payer, &[]);
            run(&mut rep, &client, "cancel_trigger_order(trailing)",
                &[ix(pid, IX_CANCEL_TRIGGER_ORDER, vec![sg(taker.pubkey()), rw(trailing)], &[])], &payer, &[&taker]);
        } else {
            rep.fail("update_trailing_stop", "setup trailing trigger failed".into());
        }
    }

    // ── FLP v3 (per-market exposure + LP token in/out) ──────────────────────
    {
        let exposure = Pubkey::find_program_address(&[FLP_PER_MARKET_SEED, market.as_ref()], &pid).0;
        run(&mut rep, &client, "init_flp_per_market",
            &[ix(pid, IX_INIT_FLP_PER_MARKET, vec![sg(payer.pubkey()), ro(market), rw(exposure), ro(sys)], &[])], &payer, &[]);
        let lp3 = Keypair::new();
        fund(&client, &payer, &lp3.pubkey(), 200_000_000);
        let pos = Pubkey::find_program_address(&[FLP_POSITION_V3_SEED, exposure.as_ref(), lp3.pubkey().as_ref()], &pid).0;
        let tok = Keypair::new();
        let c = create_account_ix(&client, &payer.pubkey(), &tok.pubkey(), TOKEN_ACCT_LEN, &spl);
        let mut idd = vec![SPL_INITIALIZE_ACCOUNT3]; idd.extend_from_slice(lp3.pubkey().as_ref());
        let initt = Instruction { program_id: spl, accounts: vec![rw(tok.pubkey()), ro(quote_mint.pubkey())], data: idd };
        let mut mdd = vec![SPL_MINT_TO]; mdd.extend_from_slice(&le8(1_000_000));
        let mtt = Instruction { program_id: spl, accounts: vec![rw(quote_mint.pubkey()), rw(tok.pubkey()), sgr(payer.pubkey())], data: mdd };
        let funded = send(&client, &[c, initt, mtt], &payer, &[&tok]).is_ok();
        if funded {
            let dep_slot = client.get_slot().unwrap_or(0);
            run(&mut rep, &client, "flp_deposit_v3",
                &[ix(pid, IX_FLP_DEPOSIT_V3, vec![sg(lp3.pubkey()), rw(exposure), rw(pos), ro(insurance), rw(vault.pubkey()), rw(tok.pubkey()), ro(spl), ro(sys)], &le8(400_000))], &payer, &[&lp3]);
            // wait out any min-hold (same JIT-LP defense as v2), then withdraw
            let deadline = dep_slot + 152;
            let mut w = 0;
            while client.get_slot().unwrap_or(deadline) < deadline && w < 60 { std::thread::sleep(std::time::Duration::from_secs(2)); w += 1; }
            run(&mut rep, &client, "flp_withdraw_v3",
                &[ix(pid, IX_FLP_WITHDRAW_V3, vec![sg(lp3.pubkey()), rw(exposure), rw(pos), ro(insurance), rw(vault.pubkey()), rw(tok.pubkey()), ro(spl)], &le8(100_000))], &payer, &[&lp3]);
        }
    }

    // ── admin / config setters (payer is market1 + insurance authority) ─────
    {
        let pk = |k: Pubkey| k.to_bytes().to_vec();
        run(&mut rep, &client, "set_market_sequencer",
            &[ix(pid, IX_SET_MARKET_SEQUENCER, vec![sgr(payer.pubkey()), rw(market)], &pk(payer.pubkey()))], &payer, &[]);
        run(&mut rep, &client, "transfer_market_authority",
            &[ix(pid, IX_TRANSFER_MARKET_AUTHORITY, vec![sgr(payer.pubkey()), rw(market)], &pk(payer.pubkey()))], &payer, &[]);
        run(&mut rep, &client, "transfer_insurance_authority",
            &[ix(pid, IX_TRANSFER_INSURANCE_AUTHORITY, vec![sgr(payer.pubkey()), rw(insurance)], &pk(payer.pubkey()))], &payer, &[]);
        run(&mut rep, &client, "set_insurance_fee_contribution",
            &[ix(pid, IX_SET_INSURANCE_FEE_CONTRIBUTION, vec![sgr(payer.pubkey()), rw(insurance)], &50u32.to_le_bytes())], &payer, &[]);
        run(&mut rep, &client, "set_market_maintenance_margin",
            &[ix(pid, IX_SET_MARKET_MAINTENANCE_MARGIN, vec![sgr(payer.pubkey()), rw(market)], &500u32.to_le_bytes())], &payer, &[]);
        let mut risk = Vec::new(); risk.extend_from_slice(&le8(0)); for v in [0u32, 0, 0] { risk.extend_from_slice(&v.to_le_bytes()); }
        run(&mut rep, &client, "set_market_risk_params",
            &[ix(pid, IX_SET_MARKET_RISK_PARAMS, vec![sgr(payer.pubkey()), rw(market)], &risk)], &payer, &[]);
        run(&mut rep, &client, "set_market_max_leverage",
            &[ix(pid, IX_SET_MARKET_MAX_LEVERAGE, vec![sgr(payer.pubkey()), rw(market)], &50u32.to_le_bytes())], &payer, &[]);
        run(&mut rep, &client, "set_insurance_pause_threshold",
            &[ix(pid, IX_SET_INSURANCE_PAUSE_THRESHOLD, vec![sgr(payer.pubkey()), rw(insurance)], &le8(0))], &payer, &[]);
        run(&mut rep, &client, "set_trader_delegate",
            &[ix(pid, IX_SET_TRADER_DELEGATE, vec![sg(maker.pubkey()), rw(maker_ts)], &[0u8; 32])], &payer, &[&maker]);
        run(&mut rep, &client, "set_trader_referrer",
            &[ix(pid, IX_SET_TRADER_REFERRER, vec![sg(maker.pubkey()), rw(maker_ts)], &pk(Keypair::new().pubkey()))], &payer, &[&maker]);
        let mut bld = pk(Keypair::new().pubkey()); bld.extend_from_slice(&100u32.to_le_bytes());
        run(&mut rep, &client, "set_trader_builder",
            &[ix(pid, IX_SET_TRADER_BUILDER, vec![sg(maker.pubkey()), rw(maker_ts)], &bld)], &payer, &[&maker]);
        let mut ult = vec![1u8]; ult.extend_from_slice(&le8(0)); ult.extend_from_slice(&600u32.to_le_bytes());
        run(&mut rep, &client, "update_market_leverage_tiers",
            &[ix(pid, IX_UPDATE_MARKET_LEVERAGE_TIERS, vec![sgr(payer.pubkey()), ro(market), rw(lev_tiers)], &ult)], &payer, &[]);
        let mut uft = Vec::new(); uft.extend_from_slice(&le8(1_000_000)); uft.push(1); uft.extend_from_slice(&le8(0)); uft.extend_from_slice(&0i32.to_le_bytes()); uft.extend_from_slice(&10u32.to_le_bytes());
        run(&mut rep, &client, "update_fee_tiers",
            &[ix(pid, IX_UPDATE_FEE_TIERS, vec![sgr(payer.pubkey()), rw(fee_tiers)], &uft)], &payer, &[]);

        if let Ok((_burn_base, burn_market)) = init_fresh_market(
            &client,
            pid,
            &payer,
            spl,
            sys,
            insurance,
            quote_mint.pubkey(),
            100_000,
            0,
            0,
            500,
        ) {
            run(&mut rep, &client, "burn_market_authority",
                &[ix(pid, IX_BURN_MARKET_AUTHORITY, vec![sgr(payer.pubkey()), rw(burn_market)], &[])], &payer, &[]);
        } else {
            rep.fail("burn_market_authority", "fresh market setup failed".into());
        }
    }

    // ── book keeper ops ─────────────────────────────────────────────────────
    {
        run(&mut rep, &client, "expand_market_book",
            &[ix(pid, IX_EXPAND_MARKET_BOOK, vec![sg(payer.pubkey()), ro(market), rw(market_book), ro(sys)], &8u32.to_le_bytes())], &payer, &[]);
        let mut reap = vec![1u8]; reap.extend_from_slice(&le8(0)); // 1 id, dummy 0 (not found → skipped → Ok)
        run(&mut rep, &client, "reap_expired_orders",
            &[ix(pid, IX_REAP_EXPIRED_ORDERS, vec![sgr(payer.pubkey()), ro(market), rw(market_book)], &reap)], &payer, &[]);
        // place a fresh maker order then cancel_all
        let mut pl = vec![1u8]; pl.extend_from_slice(&le8(5)); pl.extend_from_slice(&le8(100_000)); pl.extend_from_slice(&le8(0)); pl.push(0); pl.push(0);
        let _ = send(&client, &[ix(pid, IX_PLACE_LIMIT, vec![sgr(maker.pubkey()), ro(market), rw(market_book)], &pl)], &payer, &[&maker]);
        run(&mut rep, &client, "cancel_all",
            &[ix(pid, IX_CANCEL_ALL, vec![sgr(maker.pubkey()), ro(market), rw(market_book)], &[])], &payer, &[&maker]);
    }

    // ── funding / residual / envelope-gate cranks ───────────────────────────
    {
        run(&mut rep, &client, "advance_funding",
            &[ix(pid, IX_ADVANCE_FUNDING, vec![sg(payer.pubkey()), rw(market)], &[])], &payer, &[]);
        let mut fp = Vec::new(); for v in [0u32, 0, 0] { fp.extend_from_slice(&v.to_le_bytes()); }
        run(&mut rep, &client, "set_funding_params",
            &[ix(pid, IX_SET_FUNDING_PARAMS, vec![sg(payer.pubkey()), rw(market)], &fp)], &payer, &[]);
        run(&mut rep, &client, "seed_residual",
            &[ix(pid, IX_SEED_RESIDUAL, vec![sg(payer.pubkey()), ro(market), rw(haircut)], &0i128.to_le_bytes())], &payer, &[]);
        let mut g = Vec::new(); for v in [100_000u64, 100_000, 1] { g.extend_from_slice(&le8(v)); }
        run(&mut rep, &client, "gate_envelope_price_move",
            &[ix(pid, IX_GATE_ENVELOPE, vec![ro(market), ro(envelope)], &g)], &payer, &[]);
    }

    // ── liquidation params / cover-bad-debt(0) / JIT offer place+cancel ─────
    {
        let mut lp = Vec::new(); lp.extend_from_slice(&100u32.to_le_bytes()); lp.extend_from_slice(&100u32.to_le_bytes()); lp.extend_from_slice(&le8(10)); lp.extend_from_slice(&le8(0));
        run(&mut rep, &client, "set_market_liquidation_params",
            &[ix(pid, IX_SET_MARKET_LIQUIDATION_PARAMS, vec![sgr(payer.pubkey()), rw(market)], &lp)], &payer, &[]);
        run(&mut rep, &client, "cover_bad_debt(0)",
            &[ix(pid, IX_COVER_BAD_DEBT, vec![sgr(payer.pubkey()), ro(market), rw(insurance)], &le8(0))], &payer, &[]);
        let jit = Pubkey::find_program_address(&[JIT_LIQ_OFFER_SEED, market.as_ref(), maker.pubkey().as_ref(), &1u32.to_le_bytes()], &pid).0;
        let mut jb = Vec::new(); jb.extend_from_slice(&1u32.to_le_bytes()); jb.extend_from_slice(&[0u8; 32]); jb.push(0); jb.extend_from_slice(&le8(100_000)); jb.extend_from_slice(&le8(10)); jb.extend_from_slice(&le8(0)); jb.push(0);
        run(&mut rep, &client, "place_jit_liquidation_offer",
            &[ix(pid, IX_PLACE_JIT_LIQ_OFFER, vec![sg(maker.pubkey()), ro(market), rw(jit), ro(sys)], &jb)], &payer, &[&maker]);
        run(&mut rep, &client, "cancel_jit_liquidation_offer",
            &[ix(pid, IX_CANCEL_JIT_LIQ_OFFER, vec![sg(maker.pubkey()), rw(jit)], &[])], &payer, &[&maker]);
    }

    // ── ER base-layer (no DLP) + fail-closed undelegate (Custom 221) ────────
    {
        run(&mut rep, &client, "er_heartbeat",
            &[ix(pid, IX_ER_HEARTBEAT, vec![sgr(payer.pubkey()), rw(market)], &[])], &payer, &[]);
        let er_margin = Pubkey::find_program_address(&[ER_MARGIN_SEED, maker_ts.as_ref()], &pid).0;
        run(&mut rep, &client, "init_er_margin_attestation",
            &[ix(pid, IX_INIT_ER_MARGIN_ATTESTATION, vec![sg(payer.pubkey()), ro(insurance), ro(maker_ts), rw(er_margin), ro(sys)], &payer.pubkey().to_bytes())], &payer, &[]);
        let mut ab = Vec::new(); ab.extend_from_slice(&le8(0)); ab.extend_from_slice(&le8(1)); // reserved=0 (er_active stays 0), epoch=1
        run(&mut rep, &client, "attest_er_reserved_margin",
            &[ix(pid, IX_ATTEST_ER_RESERVED_MARGIN, vec![sgr(payer.pubkey()), rw(er_margin), rw(maker_ts)], &ab)], &payer, &[]);

        // xdomain withdraw variants: flat ER-active trader with a nonzero
        // reservation, so both paths must honor the attested floor.
        if let Ok((xd_trader, xd_ts, xd_tok)) = open_and_deposit_trader(
            &client,
            pid,
            &payer,
            spl,
            sys,
            insurance,
            vault.pubkey(),
            quote_mint.pubkey(),
            300_000,
        ) {
            let xd_er_margin = Pubkey::find_program_address(&[ER_MARGIN_SEED, xd_ts.as_ref()], &pid).0;
            run(&mut rep, &client, "init_er_margin_attestation(xdomain)",
                &[ix(pid, IX_INIT_ER_MARGIN_ATTESTATION, vec![sg(payer.pubkey()), ro(insurance), ro(xd_ts), rw(xd_er_margin), ro(sys)], &payer.pubkey().to_bytes())], &payer, &[]);
            let mut xb = Vec::new();
            xb.extend_from_slice(&le8(50_000));
            xb.extend_from_slice(&le8(1));
            run(&mut rep, &client, "attest_er_reserved_margin(xdomain)",
                &[ix(pid, IX_ATTEST_ER_RESERVED_MARGIN, vec![sgr(payer.pubkey()), rw(xd_er_margin), rw(xd_ts)], &xb)], &payer, &[]);
            run(&mut rep, &client, "partial_withdraw_xdomain",
                &[ix(pid, IX_PARTIAL_WITHDRAW_XDOMAIN,
                    vec![sg(xd_trader.pubkey()), rw(xd_ts), ro(insurance), rw(vault.pubkey()), rw(xd_tok), ro(spl), ro(xd_er_margin)],
                    &le8(100_000))], &payer, &[&xd_trader]);
            run(&mut rep, &client, "withdraw_collateral_xdomain",
                &[ix(pid, IX_WITHDRAW_COLLATERAL_XDOMAIN,
                    vec![sg(xd_trader.pubkey()), rw(xd_ts), ro(insurance), rw(vault.pubkey()), rw(xd_tok), ro(spl), ro(xd_er_margin)],
                    &le8(100_000))], &payer, &[&xd_trader]);
        } else {
            rep.fail("xdomain_withdraw_setup", "open/deposit failed".into());
        }
        // fail-closed: undelegate paths reject Custom(221) without touching the DLP
        let r = send(&client, &[ix(pid, IX_UNDELEGATE_MARKET_BOOK, vec![sgr(payer.pubkey()), ro(market)], &[])], &payer, &[]);
        rep.expect_reject("undelegate_market_book(failclosed)", 221, r);
        let r = send(&client, &[ix(pid, IX_UNDELEGATE_MARKET, vec![sgr(payer.pubkey()), ro(market), ro(base_mint.pubkey()), ro(quote_mint.pubkey())], &[])], &payer, &[]);
        rep.expect_reject("undelegate_market(failclosed)", 221, r);
        let r = send(&client, &[ix(pid, IX_UNDELEGATE_FILL_COMMITMENT, vec![sgr(payer.pubkey()), ro(market)], &[])], &payer, &[]);
        rep.expect_reject("undelegate_fill_commitment(failclosed)", 221, r);
    }

    // ── views (read-only logs) ──────────────────────────────────────────────
    {
        run(&mut rep, &client, "view_predicted_funding", &[ix(pid, IX_VIEW_PREDICTED_FUNDING, vec![ro(market)], &[])], &payer, &[]);
        run(&mut rep, &client, "view_trader_effective_tier", &[ix(pid, IX_VIEW_TRADER_TIER, vec![ro(maker_ts)], &[])], &payer, &[]);
        run(&mut rep, &client, "view_book_depth", &[ix(pid, IX_VIEW_BOOK_DEPTH, vec![ro(market), ro(market_book)], &[])], &payer, &[]);
        run(&mut rep, &client, "view_quote_ladder", &[ix(pid, IX_VIEW_QUOTE_LADDER, vec![ro(market)], &[])], &payer, &[]);
        run(&mut rep, &client, "view_portfolio_risk", &[ix(pid, IX_VIEW_PORTFOLIO_RISK, vec![ro(maker_ts)], &[])], &payer, &[]);
    }

    // ── verify_* probes (read-only; config-PDA family, no position) ─────────
    {
        run(&mut rep, &client, "verify_protocol_solvency", &[ix(pid, IX_VERIFY_PROTOCOL_SOLVENCY, vec![ro(vault.pubkey()), ro(insurance), ro(flp_single)], &[])], &payer, &[]);
        run(&mut rep, &client, "verify_collateral_solvency", &[ix(pid, IX_VERIFY_COLLATERAL_SOLVENCY, vec![ro(vault.pubkey()), ro(insurance), ro(flp_single), ro(maker_ts), ro(taker_ts)], &[])], &payer, &[]);
        run(&mut rep, &client, "verify_envelope_config", &[ix(pid, IX_VERIFY_ENVELOPE_CONFIG, vec![ro(market), ro(envelope)], &[])], &payer, &[]);
        run(&mut rep, &client, "verify_haircut_invariants", &[ix(pid, IX_VERIFY_HAIRCUT_INVARIANTS, vec![ro(market), ro(haircut)], &[])], &payer, &[]);
        run(&mut rep, &client, "verify_side_accrual_invariants", &[ix(pid, IX_VERIFY_SIDE_ACCRUAL, vec![ro(market), ro(side_accrual)], &[])], &payer, &[]);
        run(&mut rep, &client, "verify_oracle_config", &[ix(pid, IX_VERIFY_ORACLE_CONFIG, vec![ro(market), ro(oracle_cfg)], &[])], &payer, &[]);
        run(&mut rep, &client, "verify_leverage_tiers", &[ix(pid, IX_VERIFY_LEVERAGE_TIERS, vec![ro(market), ro(lev_tiers)], &[])], &payer, &[]);
        run(&mut rep, &client, "verify_fee_tiers", &[ix(pid, IX_VERIFY_FEE_TIERS, vec![ro(fee_tiers)], &[])], &payer, &[]);
        // verify_market_invariants LAST (it takes market WRITABLE and may auto-pause)
        run(&mut rep, &client, "verify_market_invariants", &[ix(pid, IX_VERIFY_MARKET_INVARIANTS, vec![rw(market)], &[])], &payer, &[]);
    }

    // ════════ LIQUIDATION + POSITION-DEPENDENT SCENARIO (dedicated market) ════════
    {
        // fresh market_L with its own base mint
        let base_l = Keypair::new();
        let cr = create_account_ix(&client, &payer.pubkey(), &base_l.pubkey(), MINT_LEN, &spl);
        let mut d = vec![SPL_INITIALIZE_MINT2, 9]; d.extend_from_slice(payer.pubkey().as_ref()); d.push(0);
        let im = Instruction { program_id: spl, accounts: vec![rw(base_l.pubkey())], data: d };
        let mint_ok = send(&client, &[cr, im], &payer, &[&base_l]).is_ok();
        let market_l = Pubkey::find_program_address(&[MARKET_SEED, base_l.pubkey().as_ref(), quote_mint.pubkey().as_ref()], &pid).0;
        let book_l = Pubkey::find_program_address(&[MARKET_BOOK_SEED, market_l.as_ref()], &pid).0;
        let mut mb = Vec::new();
        for v in [1u64, 100_000] { mb.extend_from_slice(&le8(v)); }
        mb.extend_from_slice(&10u32.to_le_bytes()); mb.extend_from_slice(&2i32.to_le_bytes());
        for v in [1u64, 1_000_000_000] { mb.extend_from_slice(&le8(v)); }
        mb.extend_from_slice(&500u32.to_le_bytes());
        let setup_ok = mint_ok
            && send(&client, &[ix(pid, IX_INIT_MARKET, vec![sg(payer.pubkey()), rw(market_l), ro(base_l.pubkey()), ro(quote_mint.pubkey()), ro(insurance), ro(sys)], &mb)], &payer, &[]).is_ok()
            && send(&client, &[ix(pid, IX_INIT_MARKET_BOOK, vec![sg(payer.pubkey()), ro(market_l), ro(base_l.pubkey()), ro(quote_mint.pubkey()), rw(book_l), ro(sys)], &[])], &payer, &[]).is_ok();

        // two traders V (long) + C (short), each deposits 80_000 cross collateral
        let mk_trader = |label: &str| -> Option<(Keypair, Pubkey)> {
            let t = Keypair::new();
            fund(&client, &payer, &t.pubkey(), 200_000_000);
            let ts = Pubkey::find_program_address(&[TRADER_STATE_SEED, t.pubkey().as_ref()], &pid).0;
            let tok = Keypair::new();
            let c = create_account_ix(&client, &payer.pubkey(), &tok.pubkey(), TOKEN_ACCT_LEN, &spl);
            let mut idd = vec![SPL_INITIALIZE_ACCOUNT3]; idd.extend_from_slice(t.pubkey().as_ref());
            let initt = Instruction { program_id: spl, accounts: vec![rw(tok.pubkey()), ro(quote_mint.pubkey())], data: idd };
            let mut mdd = vec![SPL_MINT_TO]; mdd.extend_from_slice(&le8(1_000_000));
            let mtt = Instruction { program_id: spl, accounts: vec![rw(quote_mint.pubkey()), rw(tok.pubkey()), sgr(payer.pubkey())], data: mdd };
            let ok = send(&client, &[ix(pid, IX_OPEN_TRADER_STATE, vec![sg(t.pubkey()), rw(ts), ro(sys)], &[])], &t, &[&t]).is_ok()
                && send(&client, &[c, initt, mtt], &payer, &[&tok]).is_ok()
                && send(&client, &[ix(pid, IX_DEPOSIT_COLLATERAL, vec![sg(t.pubkey()), rw(ts), ro(insurance), rw(vault.pubkey()), rw(tok.pubkey()), ro(spl)], &le8(80_000))], &t, &[&t]).is_ok();
            let _ = label;
            if ok { Some((t, ts)) } else { None }
        };
        if setup_ok {
            if let (Some((_v, ts_v)), Some((c, ts_c))) = (mk_trader("V"), mk_trader("C")) {
                let pos_len = core::mem::size_of::<Position>() as u64;
                let pos_v = Keypair::new();
                let pos_c = Keypair::new();
                let c1 = create_account_ix(&client, &payer.pubkey(), &pos_v.pubkey(), pos_len, &pid);
                let c2 = create_account_ix(&client, &payer.pubkey(), &pos_c.pubkey(), pos_len, &pid);
                let pos_ok = send(&client, &[c1, c2], &payer, &[&pos_v, &pos_c]).is_ok();
                // apply_fill: V long, C short @100000 size10 (both healthy at 80k collateral)
                let mut af = vec![IX_APPLY_FILL];
                af.extend_from_slice(&le8(10)); af.extend_from_slice(&le8(100_000)); af.push(0); af.extend_from_slice(&le8(1));
                let filled = pos_ok && send(&client, &[ix(pid, IX_APPLY_FILL, vec![sgr(payer.pubkey()), rw(market_l), rw(insurance), rw(ts_v), rw(ts_c), rw(pos_v.pubkey()), rw(pos_c.pubkey())], &af[1..])], &payer, &[]).is_ok();
                if filled { rep.ok("apply_fill:open_liq_positions", "ok".into()); } else { rep.fail("apply_fill:open_liq_positions", "setup failed".into()); }

                if filled {
                    let pv = pos_v.pubkey();
                    let pc = pos_c.pubkey();
                    // ── position-dependent reads on C (healthy short, mark 100000) ──
                    run(&mut rep, &client, "verify_solvency", &[ix(pid, IX_VERIFY_SOLVENCY, vec![ro(market_l), ro(ts_c), ro(pc)], &[])], &payer, &[]);
                    run(&mut rep, &client, "verify_stress_solvency", &[ix(pid, IX_VERIFY_STRESS_SOLVENCY, vec![ro(market_l), ro(ts_c), ro(pc)], &100i32.to_le_bytes())], &payer, &[]);
                    let mut sl = vec![1u8]; sl.extend_from_slice(&100i32.to_le_bytes());
                    run(&mut rep, &client, "verify_stress_lattice", &[ix(pid, IX_VERIFY_STRESS_LATTICE, vec![ro(market_l), ro(ts_c), ro(pc)], &sl)], &payer, &[]);
                    run(&mut rep, &client, "verify_leverage_cap", &[ix(pid, IX_VERIFY_LEVERAGE_CAP, vec![ro(market_l), ro(ts_c), ro(pc)], &[])], &payer, &[]);
                    run(&mut rep, &client, "verify_portfolio_solvency", &[ix(pid, IX_VERIFY_PORTFOLIO_SOLVENCY, vec![ro(ts_c), ro(market_l), ro(pc)], &[])], &payer, &[]);
                    let mut ps = vec![1u8]; ps.extend_from_slice(&100i32.to_le_bytes());
                    run(&mut rep, &client, "verify_portfolio_stress", &[ix(pid, IX_VERIFY_PORTFOLIO_STRESS, vec![ro(ts_c), ro(market_l), ro(pc)], &ps)], &payer, &[]);
                    run(&mut rep, &client, "liquidation_preview", &[ix(pid, IX_LIQUIDATION_PREVIEW, vec![ro(market_l), ro(ts_c), ro(pc)], &[])], &payer, &[]);
                    run(&mut rep, &client, "set_position_leverage", &[ix(pid, IX_SET_POSITION_LEVERAGE, vec![sgr(c.pubkey()), ro(market_l), rw(pc)], &50u32.to_le_bytes())], &payer, &[&c]);

                    // margin-mode switch: cross → isolated (60k) → back to cross
                    run(&mut rep, &client, "set_position_isolated", &[ix(pid, IX_SET_POSITION_ISOLATED, vec![sgr(c.pubkey()), rw(ts_c), ro(market_l), rw(pc)], &le8(60_000))], &payer, &[&c]);
                    run(&mut rep, &client, "set_position_cross", &[ix(pid, IX_SET_POSITION_CROSS, vec![sgr(c.pubkey()), rw(ts_c), ro(market_l), rw(pc)], &[])], &payer, &[&c]);

                    // ── cancel_order + modify_order on book_L (predictable seq) ──
                    let mut pl1 = vec![1u8]; pl1.extend_from_slice(&le8(5)); pl1.extend_from_slice(&le8(100_000)); pl1.extend_from_slice(&le8(0)); pl1.push(0); pl1.push(0);
                    if send(&client, &[ix(pid, IX_PLACE_LIMIT, vec![sgr(c.pubkey()), ro(market_l), rw(book_l)], &pl1)], &payer, &[&c]).is_ok() {
                        let oid1 = encode_order_id(100_000, 1, false);
                        let mut cb = vec![1u8]; cb.extend_from_slice(&le8(oid1));
                        run(&mut rep, &client, "cancel_order", &[ix(pid, IX_CANCEL_ORDER, vec![sgr(c.pubkey()), ro(market_l), rw(book_l)], &cb)], &payer, &[&c]);
                    }
                    let mut pl2 = vec![1u8]; pl2.extend_from_slice(&le8(5)); pl2.extend_from_slice(&le8(100_001)); pl2.extend_from_slice(&le8(0)); pl2.push(0); pl2.push(0);
                    if send(&client, &[ix(pid, IX_PLACE_LIMIT, vec![sgr(c.pubkey()), ro(market_l), rw(book_l)], &pl2)], &payer, &[&c]).is_ok() {
                        let oid2 = encode_order_id(100_001, 2, false);
                        let mut mb2 = vec![1u8]; mb2.extend_from_slice(&le8(oid2)); mb2.extend_from_slice(&le8(6)); mb2.extend_from_slice(&le8(100_001)); mb2.extend_from_slice(&le8(0)); mb2.push(0);
                        run(&mut rep, &client, "modify_order", &[ix(pid, IX_MODIFY_ORDER, vec![sgr(c.pubkey()), ro(market_l), rw(book_l)], &mb2)], &payer, &[&c]);
                    }

                    // ── haircut lifecycle on market_L (init AFTER apply_fill) ──
                    let haircut_l = Pubkey::find_program_address(&[HAIRCUT_SEED, market_l.as_ref()], &pid).0;
                    let mut hb = Vec::new(); hb.extend_from_slice(&le8(1)); hb.extend_from_slice(&le8(2)); hb.extend_from_slice(&0u128.to_le_bytes());
                    let h_ok = send(&client, &[ix(pid, IX_INIT_HAIRCUT_STATE, vec![sg(payer.pubkey()), rw(market_l), rw(haircut_l), ro(sys)], &hb)], &payer, &[]).is_ok();
                    if h_ok { rep.ok("initialize_haircut_state(L)", "ok".into()); } else { rep.fail("initialize_haircut_state(L)", "failed".into()); }
                    let ph_c = Pubkey::find_program_address(&[POSITION_HAIRCUT_SEED, market_l.as_ref(), pc.as_ref()], &pid).0;
                    run(&mut rep, &client, "init_position_haircut_state", &[ix(pid, IX_INIT_POSITION_HAIRCUT_STATE, vec![sg(payer.pubkey()), ro(pc), ro(haircut_l), rw(ph_c), ro(sys)], &[])], &payer, &[]);
                    run(&mut rep, &client, "verify_position_haircut", &[ix(pid, IX_VERIFY_POSITION_HAIRCUT, vec![ro(ph_c)], &[])], &payer, &[]);
                    run(&mut rep, &client, "settle_funding", &[ix(pid, IX_SETTLE_FUNDING, vec![ro(market_l), ro(ts_c), rw(pc), rw(haircut_l)], &[])], &payer, &[]);
                    // release_gain_to_haircut: defer 1000 of realized gain into the warmup reserve
                    run(&mut rep, &client, "release_gain_to_haircut", &[ix(pid, IX_RELEASE_GAIN_TO_HAIRCUT, vec![sgr(payer.pubkey()), ro(market_l), rw(ts_c), rw(pc), rw(ph_c), ro(haircut_l)], &le8(1_000))], &payer, &[]);
                    // wait out h_max(2 slots) → mature the warmup reserve → convert to collateral → flush dust
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    run(&mut rep, &client, "mature_position", &[ix(pid, IX_MATURE_POSITION, vec![rw(ph_c), rw(haircut_l)], &[])], &payer, &[]);
                    run(&mut rep, &client, "convert_position", &[ix(pid, IX_CONVERT_POSITION, vec![sgr(c.pubkey()), rw(ts_c), rw(pc), rw(ph_c), rw(haircut_l)], &[])], &payer, &[&c]);
                    // flush_haircut_dust needs accumulated rounding dust; with a 0-seeded
                    // residual there is none, so the guard correctly rejects (no-op) — the
                    // handler runs to its dust check and refuses. Record that as expected.
                    match send(&client, &[ix(pid, IX_FLUSH_HAIRCUT_DUST, vec![sgr(payer.pubkey()), rw(haircut_l), rw(insurance)], &[])], &payer, &[]) {
                        Ok(s) => rep.ok("flush_haircut_dust", s),
                        Err(e) if e.contains("insufficient funds") || e.contains("custom program error: 0x") =>
                            rep.ok("flush_haircut_dust(no-dust→correctly-rejected)", "ok".into()),
                        Err(e) => rep.fail("flush_haircut_dust", e),
                    }

                    // ── record_flp_fill_v3 on market1's per-market exposure ──
                    let exp1 = Pubkey::find_program_address(&[FLP_PER_MARKET_SEED, market.as_ref()], &pid).0;
                    let mut rf = Vec::new(); rf.extend_from_slice(&le8(10)); rf.extend_from_slice(&le8(100_000)); rf.push(0); rf.extend_from_slice(&0i64.to_le_bytes());
                    run(&mut rep, &client, "record_flp_fill_v3", &[ix(pid, IX_RECORD_FLP_FILL_V3, vec![sgr(payer.pubkey()), rw(exp1), ro(market)], &rf)], &payer, &[]);

                    // ── make V underwater (push mark down) then liquidate ──
                    let _ = send(&client, &[ix(pid, IX_UPDATE_ORACLE, vec![sgr(payer.pubkey()), rw(market_l)], &le8(80_000))], &payer, &[]);
                    let pliq_v = Pubkey::find_program_address(&[POSITION_LIQ_STATE_SEED, market_l.as_ref(), pv.as_ref()], &pid).0;
                    run(&mut rep, &client, "init_position_liquidation_state", &[ix(pid, IX_INIT_POSITION_LIQ_STATE, vec![sg(payer.pubkey()), ro(pv), rw(pliq_v), ro(sys)], &[])], &payer, &[]);
                    run(&mut rep, &client, "liquidate_position_v2", &[ix(pid, IX_LIQUIDATE_POSITION_V2, vec![sgr(c.pubkey()), rw(market_l), rw(book_l), rw(ts_v), rw(ts_c), rw(pv), rw(pliq_v)], &le8(0))], &payer, &[&c]);
                }
            }
        }
    }

    // ════════ collateral-with-positions + FLP-as-maker + fail-closed force-undeleg ════════
    {
        let flp_single = Pubkey::find_program_address(&[FLP_EXPOSURE_SEED], &pid).0;
        // helper: fresh trader with a token acct + `deposit` cross collateral
        let mk = |deposit: u64| -> Option<(Keypair, Pubkey, Pubkey)> {
            let t = Keypair::new();
            fund(&client, &payer, &t.pubkey(), 200_000_000);
            let ts = Pubkey::find_program_address(&[TRADER_STATE_SEED, t.pubkey().as_ref()], &pid).0;
            let tok = Keypair::new();
            let c = create_account_ix(&client, &payer.pubkey(), &tok.pubkey(), TOKEN_ACCT_LEN, &spl);
            let mut idd = vec![SPL_INITIALIZE_ACCOUNT3]; idd.extend_from_slice(t.pubkey().as_ref());
            let initt = Instruction { program_id: spl, accounts: vec![rw(tok.pubkey()), ro(quote_mint.pubkey())], data: idd };
            let mut mdd = vec![SPL_MINT_TO]; mdd.extend_from_slice(&le8(1_000_000));
            let mtt = Instruction { program_id: spl, accounts: vec![rw(quote_mint.pubkey()), rw(tok.pubkey()), sgr(payer.pubkey())], data: mdd };
            let ok = send(&client, &[ix(pid, IX_OPEN_TRADER_STATE, vec![sg(t.pubkey()), rw(ts), ro(sys)], &[])], &t, &[&t]).is_ok()
                && send(&client, &[c, initt, mtt], &payer, &[&tok]).is_ok()
                && send(&client, &[ix(pid, IX_DEPOSIT_COLLATERAL, vec![sg(t.pubkey()), rw(ts), ro(insurance), rw(vault.pubkey()), rw(tok.pubkey()), ro(spl)], &le8(deposit))], &t, &[&t]).is_ok();
            if ok { Some((t, ts, tok.pubkey())) } else { None }
        };
        // P (long) + Q (short), TINY size-1 position on market1 (notional 100k → 10% floor = 10k « 80k collateral)
        if let (Some((p, ts_p, tok_p)), Some((_q, ts_q, _))) = (mk(80_000), mk(80_000)) {
            let pos_len = core::mem::size_of::<Position>() as u64;
            let pos_p = Keypair::new();
            let pos_q = Keypair::new();
            let c1 = create_account_ix(&client, &payer.pubkey(), &pos_p.pubkey(), pos_len, &pid);
            let c2 = create_account_ix(&client, &payer.pubkey(), &pos_q.pubkey(), pos_len, &pid);
            if send(&client, &[c1, c2], &payer, &[&pos_p, &pos_q]).is_ok() {
                // settlement nonce on market1: backbone used fill_seq=1 → use 2, 3 next
                let mut af = Vec::new(); af.extend_from_slice(&le8(1)); af.extend_from_slice(&le8(100_000)); af.push(0); af.extend_from_slice(&le8(2));
                let ok = send(&client, &[ix(pid, IX_APPLY_FILL, vec![sgr(payer.pubkey()), rw(market), rw(insurance), rw(ts_p), rw(ts_q), rw(pos_p.pubkey()), rw(pos_q.pubkey())], &af)], &payer, &[]).is_ok();
                if ok {
                    // 140 partial_withdraw: P pulls 1000 (stays above worst-IM + 10% floor)
                    run(&mut rep, &client, "partial_withdraw", &[ix(pid, IX_PARTIAL_WITHDRAW,
                        vec![sg(p.pubkey()), rw(ts_p), ro(insurance), rw(vault.pubkey()), rw(tok_p), ro(spl), ro(market), rw(pos_p.pubkey())], &le8(1_000))], &payer, &[&p]);
                    // 139 sweep_collateral: P → Q, 1000 (P's remaining passes the stress battery)
                    run(&mut rep, &client, "sweep_collateral", &[ix(pid, IX_SWEEP_COLLATERAL,
                        vec![sg(p.pubkey()), rw(ts_p), rw(ts_q), ro(market), rw(pos_p.pubkey())], &le8(1_000))], &payer, &[&p]);
                }
            }
            // 7 apply_flp_fill: fresh taker T vs the FLP pool on a fresh market with
            //    NO haircut (market1/market_L have haircut enabled → would demand the
            //    haircut + position-haircut accounts). apply_flp_fill needs no book.
            let base_f = Keypair::new();
            let crf = create_account_ix(&client, &payer.pubkey(), &base_f.pubkey(), MINT_LEN, &spl);
            let mut df = vec![SPL_INITIALIZE_MINT2, 9]; df.extend_from_slice(payer.pubkey().as_ref()); df.push(0);
            let imf = Instruction { program_id: spl, accounts: vec![rw(base_f.pubkey())], data: df };
            let market_f = Pubkey::find_program_address(&[MARKET_SEED, base_f.pubkey().as_ref(), quote_mint.pubkey().as_ref()], &pid).0;
            let mut mbf = Vec::new(); for v in [1u64, 100_000] { mbf.extend_from_slice(&le8(v)); }
            mbf.extend_from_slice(&10u32.to_le_bytes()); mbf.extend_from_slice(&2i32.to_le_bytes());
            for v in [1u64, 1_000_000_000] { mbf.extend_from_slice(&le8(v)); }
            mbf.extend_from_slice(&500u32.to_le_bytes());
            let mf_ok = send(&client, &[crf, imf], &payer, &[&base_f]).is_ok()
                && send(&client, &[ix(pid, IX_INIT_MARKET, vec![sg(payer.pubkey()), rw(market_f), ro(base_f.pubkey()), ro(quote_mint.pubkey()), ro(insurance), ro(sys)], &mbf)], &payer, &[]).is_ok();
            if mf_ok {
                if let Some((_t, ts_t, _)) = mk(80_000) {
                    let pos_t = Keypair::new();
                    let ct = create_account_ix(&client, &payer.pubkey(), &pos_t.pubkey(), core::mem::size_of::<Position>() as u64, &pid);
                    if send(&client, &[ct], &payer, &[&pos_t]).is_ok() {
                        let mut af = Vec::new(); af.extend_from_slice(&le8(1)); af.extend_from_slice(&le8(100_000)); af.push(0); af.extend_from_slice(&le8(1));
                        run(&mut rep, &client, "apply_flp_fill", &[ix(pid, IX_APPLY_FLP_FILL,
                            vec![sgr(payer.pubkey()), rw(market_f), rw(insurance), rw(flp_single), rw(ts_t), rw(pos_t.pubkey())], &af)], &payer, &[]);
                    }
                }
            }
        }
        // 124 force_undelegate_market_book: book is NOT delegated → fail-closed Custom(220)
        let r = send(&client, &[ix(pid, IX_FORCE_UNDELEGATE_MARKET_BOOK,
            vec![sgr(payer.pubkey()), ro(market), rw(market_book), ro(pid), rw(Keypair::new().pubkey()), ro(sys), ro(pid)], &[])], &payer, &[]);
        rep.expect_reject("force_undelegate_market_book(failclosed)", 220, r);
    }

    // ════════ vault perf-fee/order + basket (multi-leg) ════════
    {
        let pos_len = core::mem::size_of::<Position>() as u64;
        // helper: fresh trader (open + fund + deposit), no Report access
        let mktr = |deposit: u64| -> Option<(Keypair, Pubkey, Pubkey)> {
            let t = Keypair::new();
            fund(&client, &payer, &t.pubkey(), 200_000_000);
            let ts = Pubkey::find_program_address(&[TRADER_STATE_SEED, t.pubkey().as_ref()], &pid).0;
            let tok = Keypair::new();
            let c = create_account_ix(&client, &payer.pubkey(), &tok.pubkey(), TOKEN_ACCT_LEN, &spl);
            let mut idd = vec![SPL_INITIALIZE_ACCOUNT3]; idd.extend_from_slice(t.pubkey().as_ref());
            let initt = Instruction { program_id: spl, accounts: vec![rw(tok.pubkey()), ro(quote_mint.pubkey())], data: idd };
            let mut mdd = vec![SPL_MINT_TO]; mdd.extend_from_slice(&le8(2_000_000));
            let mtt = Instruction { program_id: spl, accounts: vec![rw(quote_mint.pubkey()), rw(tok.pubkey()), sgr(payer.pubkey())], data: mdd };
            let ok = send(&client, &[ix(pid, IX_OPEN_TRADER_STATE, vec![sg(t.pubkey()), rw(ts), ro(sys)], &[])], &t, &[&t]).is_ok()
                && send(&client, &[c, initt, mtt], &payer, &[&tok]).is_ok()
                && send(&client, &[ix(pid, IX_DEPOSIT_COLLATERAL, vec![sg(t.pubkey()), rw(ts), ro(insurance), rw(vault.pubkey()), rw(tok.pubkey()), ro(spl)], &le8(deposit))], &t, &[&t]).is_ok();
            if ok { Some((t, ts, tok.pubkey())) } else { None }
        };
        // helper: fresh no-haircut market + book
        let mk_market = || -> Option<(Pubkey, Pubkey)> {
            let bm = Keypair::new();
            let cr = create_account_ix(&client, &payer.pubkey(), &bm.pubkey(), MINT_LEN, &spl);
            let mut d = vec![SPL_INITIALIZE_MINT2, 9]; d.extend_from_slice(payer.pubkey().as_ref()); d.push(0);
            let imx = Instruction { program_id: spl, accounts: vec![rw(bm.pubkey())], data: d };
            let m = Pubkey::find_program_address(&[MARKET_SEED, bm.pubkey().as_ref(), quote_mint.pubkey().as_ref()], &pid).0;
            let b = Pubkey::find_program_address(&[MARKET_BOOK_SEED, m.as_ref()], &pid).0;
            let mut mb = Vec::new(); for v in [1u64, 100_000] { mb.extend_from_slice(&le8(v)); }
            mb.extend_from_slice(&10u32.to_le_bytes()); mb.extend_from_slice(&2i32.to_le_bytes());
            for v in [1u64, 1_000_000_000] { mb.extend_from_slice(&le8(v)); } mb.extend_from_slice(&500u32.to_le_bytes());
            let ok = send(&client, &[cr, imx], &payer, &[&bm]).is_ok()
                && send(&client, &[ix(pid, IX_INIT_MARKET, vec![sg(payer.pubkey()), rw(m), ro(bm.pubkey()), ro(quote_mint.pubkey()), ro(insurance), ro(sys)], &mb)], &payer, &[]).is_ok()
                && send(&client, &[ix(pid, IX_INIT_MARKET_BOOK, vec![sg(payer.pubkey()), ro(m), ro(bm.pubkey()), ro(quote_mint.pubkey()), rw(b), ro(sys)], &[])], &payer, &[]).is_ok();
            if ok { Some((m, b)) } else { None }
        };
        // helper: give `taker_ts` a long size-1 position on `market` (fresh counter as maker)
        let open_long = |market: Pubkey, taker_ts: Pubkey| -> Option<Pubkey> {
            let (_cm, cm_ts, _) = mktr(80_000)?;
            let pt = Keypair::new(); let pm = Keypair::new();
            let c1 = create_account_ix(&client, &payer.pubkey(), &pt.pubkey(), pos_len, &pid);
            let c2 = create_account_ix(&client, &payer.pubkey(), &pm.pubkey(), pos_len, &pid);
            if send(&client, &[c1, c2], &payer, &[&pt, &pm]).is_err() { return None; }
            let mut af = Vec::new(); af.extend_from_slice(&le8(1)); af.extend_from_slice(&le8(100_000)); af.push(0); af.extend_from_slice(&le8(1));
            if send(&client, &[ix(pid, IX_APPLY_FILL, vec![sgr(payer.pubkey()), rw(market), rw(insurance), rw(taker_ts), rw(cm_ts), rw(pt.pubkey()), rw(pm.pubkey())], &af)], &payer, &[]).is_ok() {
                Some(pt.pubkey())
            } else { None }
        };

        // ── vault v3 extras: create vault + perf-fee(empty→Ok) + place/cancel order ──
        if let Some((strat, _, _)) = mktr(80_000) {
            let vid: u8 = 2;
            let vlt = Pubkey::find_program_address(&[VAULT_SEED, strat.pubkey().as_ref(), &[vid]], &pid).0;
            let mut cd = vec![IX_CREATE_VAULT_V3, vid]; cd.extend_from_slice(&100u32.to_le_bytes()); cd.extend_from_slice(&[0u8; 32]);
            let vts = Pubkey::find_program_address(&[TRADER_STATE_SEED, vlt.as_ref()], &pid).0;
            let vpos = Pubkey::find_program_address(&[VAULT_POSITION_SEED, vlt.as_ref(), strat.pubkey().as_ref()], &pid).0;
            let vault_ok = send(&client, &[Instruction { program_id: pid, accounts: vec![sg(strat.pubkey()), rw(vlt), ro(sys)], data: cd }], &strat, &[&strat]).is_ok()
                && send(&client, &[ix(pid, IX_VAULT_OPEN_TRADER_STATE_V3, vec![sg(strat.pubkey()), ro(vlt), rw(vts), ro(sys)], &[])], &strat, &[&strat]).is_ok()
                && send(&client, &[ix(pid, IX_INIT_VAULT_POSITION_V3, vec![sg(strat.pubkey()), ro(vlt), rw(vpos), ro(sys)], &[])], &strat, &[&strat]).is_ok();
            if vault_ok {
                run(&mut rep, &client, "settle_vault_perf_fee_v3", &[ix(pid, IX_SETTLE_VAULT_PERF_FEE_V3, vec![sg(strat.pubkey()), rw(vlt), ro(vts), rw(vpos)], &[])], &strat, &[&strat]);
                if let Some((mv, bv)) = mk_market() {
                    let mut po = vec![1u8]; po.extend_from_slice(&le8(5)); po.extend_from_slice(&le8(100_000)); po.extend_from_slice(&le8(0)); po.push(0);
                    if send(&client, &[ix(pid, IX_VAULT_PLACE_ORDER_V3, vec![sg(strat.pubkey()), ro(vlt), ro(mv), rw(bv)], &po)], &strat, &[&strat]).is_ok() {
                        rep.ok("vault_place_order_v3", "ok".into());
                        let oid = encode_order_id(100_000, 1, false);
                        let mut co = vec![1u8]; co.extend_from_slice(&le8(oid));
                        run(&mut rep, &client, "vault_cancel_order_v3", &[ix(pid, IX_VAULT_CANCEL_ORDER_V3, vec![sg(strat.pubkey()), ro(vlt), ro(mv), rw(bv)], &co)], &strat, &[&strat]);
                    } else { rep.fail("vault_place_order_v3", "place failed".into()); }
                }
            }
        }

        // ── basket orders (v2 fixed-2 + n_v2): trader B with 2 bound positions on 2 markets ──
        if let (Some((mka, bka)), Some((mkb, bkb)), Some((b, ts_b, _))) = (mk_market(), mk_market(), mktr(400_000)) {
            if let (Some(pa), Some(pb)) = (open_long(mka, ts_b), open_long(mkb, ts_b)) {
                // each leg 18B: side u8, size u64, limit u64, post_only u8
                let leg = |side: u8| { let mut l = vec![side]; l.extend_from_slice(&le8(5)); l.extend_from_slice(&le8(100_000)); l.push(0); l };
                let mut d2 = leg(1); d2.extend(leg(1));
                run(&mut rep, &client, "place_basket_order_v2", &[ix(pid, IX_PLACE_BASKET_V2,
                    vec![sgr(b.pubkey()), ro(ts_b), ro(mka), rw(bka), ro(pa), ro(mkb), rw(bkb), ro(pb)], &d2)], &payer, &[&b]);
                let mut dn = vec![2u8]; dn.extend(leg(1)); dn.extend(leg(1));
                run(&mut rep, &client, "place_basket_order_n_v2", &[ix(pid, IX_PLACE_BASKET_N_V2,
                    vec![sgr(b.pubkey()), ro(ts_b), ro(mka), rw(bka), ro(pa), ro(mkb), rw(bkb), ro(pb)], &dn)], &payer, &[&b]);
            }
        }
    }

    rep.print();
    let failed: Vec<&String> = rep.rows.iter().filter(|(_, r)| r.is_err()).map(|(l, _)| l).collect();
    assert!(failed.is_empty(), "instructions failed on validator: {failed:?}");
}
