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
use flash_book_pin::seeds::{INSURANCE_SEED, MARKET_SEED, TRADER_STATE_SEED};
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
const IX_INIT_MARKET: u8 = 11;
const IX_UPDATE_ORACLE: u8 = 15;
const IX_INIT_MARKET_BOOK: u8 = 81;

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

    rep.print();
    let failed: Vec<&String> = rep.rows.iter().filter(|(_, r)| r.is_err()).map(|(l, _)| l).collect();
    assert!(failed.is_empty(), "instructions failed on validator: {failed:?}");
}
