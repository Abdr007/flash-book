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
    FLP_EXPOSURE_SEED, HAIRCUT_SEED, INSURANCE_SEED, LEVERAGE_TIERS_SEED, LP_POSITION_SEED,
    MARKET_SEED, ORACLE_CONFIG_SEED, SIDE_ACCRUAL_SEED, TRADER_STATE_SEED, TRIGGER_ORDER_SEED,
    VAULT_POSITION_SEED, VAULT_SEED,
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
                }
            }
        }
    }

    rep.print();
    let failed: Vec<&String> = rep.rows.iter().filter(|(_, r)| r.is_err()).map(|(l, _)| l).collect();
    assert!(failed.is_empty(), "instructions failed on validator: {failed:?}");
}
