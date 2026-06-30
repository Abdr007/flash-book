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

use flash_book_pin::book::{MARKET_BOOK_SEED, MARKET_BOOK_TOTAL_BYTES};
use flash_book_pin::fill_commitment::FILL_COMMIT_SEED;
use flash_book_pin::seeds::{
    ENVELOPE_CONFIG_SEED, ER_MARGIN_SEED, FEE_TIERS_SEED, FLP_EXPOSURE_SEED, FLP_PER_MARKET_SEED,
    FLP_POSITION_V3_SEED, HAIRCUT_SEED, ICEBERG_ORDER_SEED, INSURANCE_SEED, JIT_LIQ_OFFER_SEED,
    LEVERAGE_TIERS_SEED, LP_POSITION_SEED, MARKET_SEED, ORACLE_CONFIG_SEED, SESSION_SEED,
    SIDE_ACCRUAL_SEED, TRADER_STATE_SEED, TRIGGER_ORDER_SEED, TWAP_ORDER_SEED, VAULT_POSITION_SEED,
    VAULT_SEED,
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
const IX_EXPAND_MARKET_BOOK: u8 = 87;
const IX_REAP_EXPIRED_ORDERS: u8 = 88;
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

// SPL Token (classic) program id + instruction tags.
const SPL_TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const SPL_INITIALIZE_MINT2: u8 = 20;
const SPL_INITIALIZE_ACCOUNT3: u8 = 18;
const SPL_MINT_TO: u8 = 7;
const MINT_LEN: u64 = 82;
const TOKEN_ACCT_LEN: u64 = 165;

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
    fn print(&self) {
        let pass = self.rows.iter().filter(|(_, r)| r.is_ok()).count();
        eprintln!("\n================ LOCAL EXERCISE REPORT ================");
        for (label, r) in &self.rows {
            match r {
                Ok(sig) => eprintln!("  PASS  {label}   {}", &sig[..sig.len().min(16)]),
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

fn airdrop(client: &RpcClient, who: &Pubkey, lamports: u64) {
    let sig = client.request_airdrop(who, lamports).expect("airdrop");
    let bh = client.get_latest_blockhash().unwrap();
    client
        .confirm_transaction_with_spinner(&sig, &bh, CommitmentConfig::confirmed())
        .expect("confirm airdrop");
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
    let sys = solana_sdk_ids::system_program::id();
    let mut rep = Report::new();

    // ── payer = protocol authority + mint authority + sequencer ─────────────
    let payer = Keypair::new();
    airdrop(&client, &payer.pubkey(), 200_000_000_000);
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

    // ── two traders: open_trader_state + token acct + deposit_collateral ────
    let maker = Keypair::new();
    let taker = Keypair::new();
    let mut trader_state = std::collections::HashMap::new();
    for (label, trader) in [("maker", &maker), ("taker", &taker)] {
        airdrop(&client, &trader.pubkey(), 50_000_000_000);
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
        airdrop(&client, &flatty.pubkey(), 50_000_000_000);
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
        airdrop(&client, &lp.pubkey(), 50_000_000_000);
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
        let strategist = Keypair::new();
        airdrop(&client, &strategist.pubkey(), 50_000_000_000);
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
            airdrop(&client, &depositor.pubkey(), 50_000_000_000);
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
        let ix = Instruction {
            program_id: pid,
            accounts: vec![
                AccountMeta::new_readonly(payer.pubkey(), true),
                AccountMeta::new(market, false),
            ],
            data: d,
        };
        match send(&client, &[ix], &payer, &[]) {
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
        let ix = config_ix(pid, &payer.pubkey(), market, envelope, sys, IX_SET_ENVELOPE_CONFIG, &body, false);
        match send(&client, &[ix], &payer, &[]) {
            Ok(s) => rep.ok("set_envelope_config", s),
            Err(e) => rep.fail("set_envelope_config", e),
        }

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
        airdrop(&client, &subby.pubkey(), 50_000_000_000);
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
    }

    // ── FLP v3 (per-market exposure + LP token in/out) ──────────────────────
    {
        let exposure = Pubkey::find_program_address(&[FLP_PER_MARKET_SEED, market.as_ref()], &pid).0;
        run(&mut rep, &client, "init_flp_per_market",
            &[ix(pid, IX_INIT_FLP_PER_MARKET, vec![sg(payer.pubkey()), ro(market), rw(exposure), ro(sys)], &[])], &payer, &[]);
        let lp3 = Keypair::new();
        airdrop(&client, &lp3.pubkey(), 50_000_000_000);
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

    rep.print();
    let failed: Vec<&String> = rep.rows.iter().filter(|(_, r)| r.is_err()).map(|(l, _)| l).collect();
    assert!(failed.is_empty(), "instructions failed on validator: {failed:?}");
}
