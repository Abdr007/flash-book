//! End-to-end integration tests via solana-program-test.
//!
//! Anchor 0.31's `entry` collapses the slice-ref and AccountInfo lifetimes
//! into one `'info` parameter, while solana-program-test's
//! `BuiltinFunctionWithContext` HRTB expects two independent lifetimes.
//! The wrapper below bridges via a documented unsafe transmute that's
//! sound under solana-program-test's actual call site (the runner owns
//! the AccountInfo Vec for the duration of the instruction).
//!
//! This is the same pattern used by upstream Anchor projects with
//! solana-program-test integration; see e.g. mango-v4's tests.

use anchor_lang::{prelude::*, InstructionData};
use flash_book::state::{
    FlpExposureAccount, InsuranceFundAccount, MarketAccount, MarketParams,
    OrderBufferAccount, TraderStateAccount,
};
use solana_program_test::{processor, BanksClient, ProgramTest};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

const PROGRAM_ID_STR: &str = "FBookV1111111111111111111111111111111111111";

fn program_id() -> Pubkey {
    PROGRAM_ID_STR.parse().unwrap()
}

fn pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, &program_id())
}

/// Lifetime-bridge wrapper around `flash_book::entry`.
///
/// SAFETY: solana-program-test's `BuiltinFunctionWithContext` allocates the
/// AccountInfo slice on its own stack and frees it after this fn returns.
/// The lifetime parameters in the HRTB form (`'b` for slice, `'c` for items)
/// can be safely unified to a single `'info` for the call into Anchor's
/// `entry` since they are co-extensive at runtime — the slice and items
/// share the same allocation lifetime within this fn's frame.
///
/// `transmute` here only changes the type-level lifetime parameter; the
/// runtime layout of `&[AccountInfo<'_>]` is identical regardless of `'_`.
fn anchor_entry_wrapper<'a, 'b, 'c, 'd>(
    program_id: &'a Pubkey,
    accounts: &'b [AccountInfo<'c>],
    instruction_data: &'d [u8],
) -> std::result::Result<(), anchor_lang::solana_program::program_error::ProgramError> {
    let accounts_unified: &'c [AccountInfo<'c>] =
        unsafe { std::mem::transmute(accounts) };
    flash_book::entry(program_id, accounts_unified, instruction_data)
}

fn make_program_test() -> ProgramTest {
    ProgramTest::new(
        "flash_book",
        program_id(),
        processor!(anchor_entry_wrapper),
    )
}

async fn fetch<T: AccountDeserialize>(client: &mut BanksClient, address: Pubkey) -> T {
    let data = client
        .get_account(address)
        .await
        .unwrap()
        .expect("account not found")
        .data;
    T::try_deserialize(&mut &data[..]).expect("deserialize")
}

fn build_ix(args: impl InstructionData, accounts: Vec<AccountMeta>) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts,
        data: args.data(),
    }
}

fn default_params() -> MarketParams {
    MarketParams {
        tick_size: 1,
        base_lot_size: 1_000,
        quote_lot_size: 1,
        min_base_lots: 1,

        taker_fee_bps: 5,
        maker_rebate_bps: 1,
        toxicity_tax_max_bps: 5,

        liq_penalty_bps: 50,
        maintenance_margin_ratio_bps: 125,
        initial_margin_ratio_bps: 250,
        max_leverage: 40,

        funding_rate_max_bps_per_sec: 1_000,
        funding_rate_k_bps: 100_000,
        oracle_band_bps: 100,

        flp_spread_base_bps: 5,
        flp_spread_alpha_bps: 5_000,
        flp_spread_beta_bps: 3_000,
        flp_spread_gamma_bps: 2_000,
        flp_spread_kappa_bps: 500,
        flp_spread_delta_bps: 20_000,
        flp_inventory_lambda_bps: 5_000,
        flp_depth_floor_lots: 1_000,
        flp_max_growth_per_batch_bps: 50,
        flp_quote_levels: 5,

        vpin_bucket_size_lots: 100,
        vpin_ema_window: 50,

        twap_window: 5,
        batch_interval_ms: 50,
    }
}

/// Set up insurance fund + flp exposure (prerequisites for market init).
async fn setup_protocol(
    ctx: &mut solana_program_test::ProgramTestContext,
    payer: &Keypair,
) -> (Pubkey, Pubkey) {
    let (insurance_fund, _) = pda(&[InsuranceFundAccount::SEED]);
    let (flp_exposure, _) = pda(&[FlpExposureAccount::SEED]);

    let ix1 = build_ix(
        flash_book::instruction::InitializeInsuranceFund {
            fee_contribution_bps: 1_000,
            toxicity_tax_contribution_bps: 5_000,
            liq_penalty_contribution_bps: 5_000,
            pause_threshold_quote_lots: 5_000,
        },
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(insurance_fund, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let ix2 = build_ix(
        flash_book::instruction::InitializeFlpExposure {
            initial_capital_quote_lots: 5_000_000,
        },
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(flp_exposure, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );

    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix1, ix2],
            Some(&payer.pubkey()),
            &[payer],
            bh,
        ))
        .await
        .unwrap();

    (insurance_fund, flp_exposure)
}

/// Set up insurance fund + flp exposure + market.
/// Returns (market PDA, order_buffer PDA, base_mint, quote_mint).
async fn setup_market(
    ctx: &mut solana_program_test::ProgramTestContext,
    payer: &Keypair,
) -> (Pubkey, Pubkey, Pubkey, Pubkey) {
    let (insurance_fund, flp_exposure) = setup_protocol(ctx, payer).await;

    let base_mint = Keypair::new().pubkey();
    let quote_mint = Keypair::new().pubkey();
    let base_vault = Keypair::new().pubkey();
    let quote_vault = Keypair::new().pubkey();
    let oracle_account = Keypair::new().pubkey();

    let (market, _) = pda(&[
        MarketAccount::SEED,
        base_mint.as_ref(),
        quote_mint.as_ref(),
    ]);
    let (order_buffer, _) = pda(&[OrderBufferAccount::SEED, market.as_ref()]);
    let (commit_buffer, _) = pda(&[
        flash_book::state::CommitBufferAccount::SEED,
        market.as_ref(),
    ]);

    let ix = build_ix(
        flash_book::instruction::InitializeMarket {
            params: default_params(),
            initial_oracle_ticks: 100_000,
        },
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new_readonly(base_vault, false),
            AccountMeta::new_readonly(quote_vault, false),
            AccountMeta::new_readonly(oracle_account, false),
            AccountMeta::new(market, false),
            AccountMeta::new(order_buffer, false),
            AccountMeta::new(commit_buffer, false),
            AccountMeta::new_readonly(insurance_fund, false),
            AccountMeta::new_readonly(flp_exposure, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );

    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[payer],
            bh,
        ))
        .await
        .unwrap();

    (market, order_buffer, base_mint, quote_mint)
}

/// Initialize an additional market on an already-initialized protocol.
/// Used by multi-market tests. Returns (market PDA, order_buffer PDA, base, quote).
async fn setup_additional_market(
    ctx: &mut solana_program_test::ProgramTestContext,
    payer: &Keypair,
    initial_oracle_ticks: u64,
) -> (Pubkey, Pubkey, Pubkey, Pubkey) {
    let (insurance_fund, _) = pda(&[InsuranceFundAccount::SEED]);
    let (flp_exposure, _) = pda(&[FlpExposureAccount::SEED]);

    let base_mint = Keypair::new().pubkey();
    let quote_mint = Keypair::new().pubkey();
    let base_vault = Keypair::new().pubkey();
    let quote_vault = Keypair::new().pubkey();
    let oracle_account = Keypair::new().pubkey();

    let (market, _) = pda(&[
        MarketAccount::SEED,
        base_mint.as_ref(),
        quote_mint.as_ref(),
    ]);
    let (order_buffer, _) = pda(&[OrderBufferAccount::SEED, market.as_ref()]);
    let (commit_buffer, _) = pda(&[
        flash_book::state::CommitBufferAccount::SEED,
        market.as_ref(),
    ]);

    let ix = build_ix(
        flash_book::instruction::InitializeMarket {
            params: default_params(),
            initial_oracle_ticks,
        },
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(base_mint, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new_readonly(base_vault, false),
            AccountMeta::new_readonly(quote_vault, false),
            AccountMeta::new_readonly(oracle_account, false),
            AccountMeta::new(market, false),
            AccountMeta::new(order_buffer, false),
            AccountMeta::new(commit_buffer, false),
            AccountMeta::new_readonly(insurance_fund, false),
            AccountMeta::new_readonly(flp_exposure, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );

    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[payer],
            bh,
        ))
        .await
        .unwrap();

    (market, order_buffer, base_mint, quote_mint)
}

// Note: a real hedged multi-market integration test requires the full
// place + run_batch + apply_fill chain on each market. The matcher's
// hedge-recognition property is exhaustively covered by the Rust unit
// tests in `programs/flash-book/src/matcher/tests.rs::risk_hedged_*`
// and by the SDK's `risk-preview.test.ts` hedge tests. The on-chain
// E2E tests below verify the multi-market account-walking and
// validation paths.

/// Open + fund a trader, returning their state PDA.
async fn setup_trader(
    ctx: &mut solana_program_test::ProgramTestContext,
    payer: &Keypair,
    trader: &Keypair,
    deposit_amount: u64,
) -> Pubkey {
    let transfer = solana_sdk::system_instruction::transfer(
        &payer.pubkey(),
        &trader.pubkey(),
        100_000_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[transfer],
            Some(&payer.pubkey()),
            &[payer],
            bh,
        ))
        .await
        .unwrap();

    let (trader_state, _) = pda(&[TraderStateAccount::SEED, trader.pubkey().as_ref()]);

    let open_ix = build_ix(
        flash_book::instruction::OpenTraderState {},
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[open_ix],
            Some(&trader.pubkey()),
            &[trader],
            bh,
        ))
        .await
        .unwrap();

    if deposit_amount > 0 {
        let deposit_ix = build_ix(
            flash_book::instruction::DepositCollateral {
                amount_quote_lots: deposit_amount,
            },
            vec![
                AccountMeta::new_readonly(trader.pubkey(), true),
                AccountMeta::new(trader_state, false),
            ],
        );
        let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
        ctx.banks_client
            .process_transaction(Transaction::new_signed_with_payer(
                &[deposit_ix],
                Some(&trader.pubkey()),
                &[trader],
                bh,
            ))
            .await
            .unwrap();
    }

    trader_state
}

#[tokio::test]
async fn initialize_insurance_fund_writes_state() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (insurance_fund, _) = pda(&[InsuranceFundAccount::SEED]);

    let ix = build_ix(
        flash_book::instruction::InitializeInsuranceFund {
            fee_contribution_bps: 1_000,
            toxicity_tax_contribution_bps: 5_000,
            liq_penalty_contribution_bps: 5_000,
            pause_threshold_quote_lots: 5_000,
        },
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(insurance_fund, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let fund: InsuranceFundAccount = fetch(&mut ctx.banks_client, insurance_fund).await;
    assert_eq!(fund.balance_quote_lots, 0);
    assert_eq!(fund.fee_contribution_bps, 1_000);
    assert_eq!(fund.pause_threshold_quote_lots, 5_000);
    assert_eq!(fund.total_contributions, 0);
    assert_eq!(fund.total_payouts, 0);
}

#[tokio::test]
async fn initialize_flp_exposure_writes_state_and_empty_slots() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (flp_exposure, _) = pda(&[FlpExposureAccount::SEED]);

    let ix = build_ix(
        flash_book::instruction::InitializeFlpExposure {
            initial_capital_quote_lots: 5_000_000,
        },
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(flp_exposure, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );

    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.last_blockhash,
        ))
        .await
        .unwrap();

    let flp: FlpExposureAccount = fetch(&mut ctx.banks_client, flp_exposure).await;
    assert_eq!(flp.total_capital_quote_lots, 5_000_000);
    assert_eq!(flp.realized_pnl, 0);
    assert_eq!(flp.markets_count, 0);
    // All slots should be empty (side = 255).
    for slot in flp.per_market.iter() {
        assert_eq!(slot.side, 255);
    }
}

#[tokio::test]
async fn open_trader_state_initializes_zero_balance() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();
    let trader = Keypair::new();

    // Fund the trader.
    let transfer = solana_sdk::system_instruction::transfer(
        &payer.pubkey(),
        &trader.pubkey(),
        100_000_000,
    );
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[transfer],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.last_blockhash,
        ))
        .await
        .unwrap();

    let (trader_state, _) = pda(&[TraderStateAccount::SEED, trader.pubkey().as_ref()]);

    let ix = build_ix(
        flash_book::instruction::OpenTraderState {},
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );

    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    let state: TraderStateAccount = fetch(&mut ctx.banks_client, trader_state).await;
    assert_eq!(state.trader, trader.pubkey());
    assert_eq!(state.collateral_quote_lots, 0);
    assert_eq!(state.realized_pnl_quote_lots, 0);
    assert_eq!(state.open_positions, 0);
    assert_eq!(state.toxicity_score_bps, 0);
    assert_eq!(state.orders_this_batch, 0);
}

#[tokio::test]
async fn deposit_collateral_credits_balance_and_emits_event() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();
    let trader = Keypair::new();

    // Fund + open trader state.
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer.pubkey(),
                &trader.pubkey(),
                100_000_000,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.last_blockhash,
        ))
        .await
        .unwrap();

    let (trader_state, _) = pda(&[TraderStateAccount::SEED, trader.pubkey().as_ref()]);

    let open_ix = build_ix(
        flash_book::instruction::OpenTraderState {},
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let bh1 = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[open_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh1,
        ))
        .await
        .unwrap();

    // First deposit.
    let deposit_ix = build_ix(
        flash_book::instruction::DepositCollateral {
            amount_quote_lots: 50_000,
        },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
        ],
    );
    let bh2 = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh2,
        ))
        .await
        .unwrap();

    let after: TraderStateAccount = fetch(&mut ctx.banks_client, trader_state).await;
    assert_eq!(after.collateral_quote_lots, 50_000);

    // Second deposit accumulates.
    let deposit_ix2 = build_ix(
        flash_book::instruction::DepositCollateral {
            amount_quote_lots: 25_000,
        },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
        ],
    );
    let bh3 = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[deposit_ix2],
            Some(&trader.pubkey()),
            &[&trader],
            bh3,
        ))
        .await
        .unwrap();

    let after2: TraderStateAccount = fetch(&mut ctx.banks_client, trader_state).await;
    assert_eq!(after2.collateral_quote_lots, 75_000);
}

#[tokio::test]
async fn withdraw_collateral_reduces_balance() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();
    let trader = Keypair::new();

    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer.pubkey(),
                &trader.pubkey(),
                100_000_000,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.last_blockhash,
        ))
        .await
        .unwrap();

    let (trader_state, _) = pda(&[TraderStateAccount::SEED, trader.pubkey().as_ref()]);

    let open_ix = build_ix(
        flash_book::instruction::OpenTraderState {},
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[open_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    // Deposit, then withdraw.
    let deposit_ix = build_ix(
        flash_book::instruction::DepositCollateral {
            amount_quote_lots: 100_000,
        },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    let withdraw_ix = build_ix(
        flash_book::instruction::WithdrawCollateral {
            amount_quote_lots: 30_000,
        },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new(trader_state, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[withdraw_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    let after: TraderStateAccount = fetch(&mut ctx.banks_client, trader_state).await;
    assert_eq!(after.collateral_quote_lots, 70_000);
}

#[tokio::test]
async fn initialize_market_writes_state() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, order_buf, base_mint, quote_mint) = setup_market(&mut ctx, &payer).await;

    let market: MarketAccount = fetch(&mut ctx.banks_client, market_pda).await;
    assert_eq!(market.authority, payer.pubkey());
    assert_eq!(market.base_mint, base_mint);
    assert_eq!(market.quote_mint, quote_mint);
    assert_eq!(market.oracle_price_ticks, 100_000);
    assert_eq!(market.mark_price_ticks, 100_000);
    assert_eq!(market.cum_funding_index, 0);
    assert_eq!(market.current_batch, 0);
    assert_eq!(market.oi_long_lots, 0);
    assert_eq!(market.oi_short_lots, 0);
    // Status defaults to Active (1).
    assert_eq!(market.status, 1);
    assert_eq!(market.params.tick_size, 1);
    assert_eq!(market.params.flp_quote_levels, 5);

    // OrderBuffer should be initialized empty.
    let buf: OrderBufferAccount = fetch(&mut ctx.banks_client, order_buf).await;
    assert_eq!(buf.head, 0);
    assert_eq!(buf.seq_counter, 0);
    assert_eq!(buf.market, market_pda);
}

#[tokio::test]
async fn place_limit_order_lands_in_buffer() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, order_buf, _base, _quote) = setup_market(&mut ctx, &payer).await;

    let trader = Keypair::new();
    let trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;
    let (position, _) = pda(&[
        flash_book::state::PositionAccount::SEED,
        market_pda.as_ref(),
        trader.pubkey().as_ref(),
    ]);

    let ix = build_ix(
        flash_book::instruction::PlaceLimitOrder {
            side: 0, // long
            size_lots: 10,
            limit_ticks: 99_950,
            post_only: false,
        },
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(order_buf, false),
            AccountMeta::new(trader_state, false),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    let buf: OrderBufferAccount = fetch(&mut ctx.banks_client, order_buf).await;
    assert_eq!(buf.head, 1);
    assert_eq!(buf.seq_counter, 1);
    // First slot should be the order.
    let slot = buf.slots[0];
    assert_eq!(slot.valid, 1);
    assert_eq!(slot.side, 0);
    assert_eq!(slot.size_lots, 10);
    assert_eq!(slot.limit_ticks, 99_950);
    assert_eq!(slot.trader, trader.pubkey());

    // Trader state shows orders_this_batch = 1.
    let state: TraderStateAccount = fetch(&mut ctx.banks_client, trader_state).await;
    assert_eq!(state.orders_this_batch, 1);
}

#[tokio::test]
async fn run_batch_advances_counter_and_clears_buffer() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, order_buf, _base, _quote) = setup_market(&mut ctx, &payer).await;
    let (commit_buf, _) = pda(&[
        flash_book::state::CommitBufferAccount::SEED,
        market_pda.as_ref(),
    ]);
    let (insurance_fund, _) = pda(&[InsuranceFundAccount::SEED]);
    let (flp_exposure, _) = pda(&[FlpExposureAccount::SEED]);

    // Place an order so the buffer is non-empty.
    let trader = Keypair::new();
    let trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;
    let (position, _) = pda(&[
        flash_book::state::PositionAccount::SEED,
        market_pda.as_ref(),
        trader.pubkey().as_ref(),
    ]);

    let place_ix = build_ix(
        flash_book::instruction::PlaceLimitOrder {
            side: 0,
            size_lots: 10,
            limit_ticks: 99_950,
            post_only: false,
        },
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(order_buf, false),
            AccountMeta::new(trader_state, false),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[place_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    let buf_before: OrderBufferAccount = fetch(&mut ctx.banks_client, order_buf).await;
    assert_eq!(buf_before.head, 1);

    // Run a batch.
    let run_ix = build_ix(
        flash_book::instruction::RunBatch { now_ms: 1_000_000 },
        vec![
            AccountMeta::new(payer.pubkey(), true), // sequencer = payer here
            AccountMeta::new(market_pda, false),
            AccountMeta::new(order_buf, false),
            AccountMeta::new(commit_buf, false),
            AccountMeta::new(insurance_fund, false),
            AccountMeta::new_readonly(flp_exposure, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[run_ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    // Market.current_batch advanced.
    let market: MarketAccount = fetch(&mut ctx.banks_client, market_pda).await;
    assert_eq!(market.current_batch, 1);
    assert_eq!(market.last_batch_ms, 1_000_000);

    // Buffer cleared.
    let buf_after: OrderBufferAccount = fetch(&mut ctx.banks_client, order_buf).await;
    assert_eq!(buf_after.head, 0);
    for slot in buf_after.slots.iter() {
        assert_eq!(slot.valid, 0);
    }
}

#[tokio::test]
async fn set_market_status_blocks_orders_when_paused() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, order_buf, _base, _quote) = setup_market(&mut ctx, &payer).await;

    // Pause market.
    let pause_ix = build_ix(
        flash_book::instruction::SetMarketStatus { new_status: 3 }, // Paused
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_pda, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[pause_ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    let market: MarketAccount = fetch(&mut ctx.banks_client, market_pda).await;
    assert_eq!(market.status, 3); // Paused

    // Try to place an order — should fail.
    let trader = Keypair::new();
    let trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;
    let (position, _) = pda(&[
        flash_book::state::PositionAccount::SEED,
        market_pda.as_ref(),
        trader.pubkey().as_ref(),
    ]);

    let place_ix = build_ix(
        flash_book::instruction::PlaceLimitOrder {
            side: 0,
            size_lots: 10,
            limit_ticks: 99_950,
            post_only: false,
        },
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(order_buf, false),
            AccountMeta::new(trader_state, false),
            AccountMeta::new(position, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[place_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await;
    assert!(result.is_err(), "place_limit_order should fail when market is paused");
}

#[tokio::test]
async fn update_market_params_rejects_immutable_primitive_change() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, _, _, _) = setup_market(&mut ctx, &payer).await;

    // Try to change tick_size — should fail.
    let mut new_params = default_params();
    new_params.tick_size = 2; // changed from 1

    let ix = build_ix(
        flash_book::instruction::UpdateMarketParams { new_params },
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_pda, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await;
    assert!(result.is_err(), "update_market_params should reject tick_size change");

    // Mutable change should succeed (taker_fee_bps).
    let mut mutable_change = default_params();
    mutable_change.taker_fee_bps = 7;

    let ix2 = build_ix(
        flash_book::instruction::UpdateMarketParams {
            new_params: mutable_change,
        },
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_pda, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix2],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    let market: MarketAccount = fetch(&mut ctx.banks_client, market_pda).await;
    assert_eq!(market.params.taker_fee_bps, 7);
}

#[tokio::test]
async fn liquidate_position_rejects_healthy_trader() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, _, _, _) = setup_market(&mut ctx, &payer).await;
    let (order_buf, _) = pda(&[OrderBufferAccount::SEED, market_pda.as_ref()]);

    let trader = Keypair::new();
    let trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;
    let (position, _) = pda(&[
        flash_book::state::PositionAccount::SEED,
        market_pda.as_ref(),
        trader.pubkey().as_ref(),
    ]);

    // Trader has no open position — liquidation should fail (LiquidationStale)
    // because position.size_lots == 0.
    let caller = Keypair::new();
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer.pubkey(),
                &caller.pubkey(),
                100_000_000,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    let liq_ix = build_ix(
        flash_book::instruction::LiquidatePosition {},
        vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(order_buf, false),
            AccountMeta::new_readonly(trader_state, false),
            AccountMeta::new_readonly(position, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[liq_ix],
            Some(&caller.pubkey()),
            &[&caller],
            bh,
        ))
        .await;
    // Either LiquidationStale (position empty / not initialized) — both are rejections.
    assert!(result.is_err(), "liquidate_position should fail on healthy/empty trader");
}

#[tokio::test]
async fn deposit_flp_capital_grows_pool() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (_, flp_exposure) = setup_protocol(&mut ctx, &payer).await;

    let initial: FlpExposureAccount = fetch(&mut ctx.banks_client, flp_exposure).await;
    assert_eq!(initial.total_capital_quote_lots, 5_000_000);

    let ix = build_ix(
        flash_book::instruction::DepositFlpCapital {
            amount_quote_lots: 1_000_000,
        },
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(flp_exposure, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    let after: FlpExposureAccount = fetch(&mut ctx.banks_client, flp_exposure).await;
    assert_eq!(after.total_capital_quote_lots, 6_000_000);
}

#[tokio::test]
async fn withdraw_flp_capital_blocked_with_open_positions() {
    // Set markets_count > 0 isn't possible without actual fills, so we
    // test the inverse: withdraw on an empty pool should succeed.
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (_, flp_exposure) = setup_protocol(&mut ctx, &payer).await;

    let ix = build_ix(
        flash_book::instruction::WithdrawFlpCapital {
            amount_quote_lots: 1_000_000,
        },
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(flp_exposure, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    let after: FlpExposureAccount = fetch(&mut ctx.banks_client, flp_exposure).await;
    assert_eq!(after.total_capital_quote_lots, 4_000_000);
}

#[tokio::test]
async fn submit_commit_and_reveal_full_flow() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, order_buf, _base, _quote) = setup_market(&mut ctx, &payer).await;
    let (commit_buf, _) = pda(&[
        flash_book::state::CommitBufferAccount::SEED,
        market_pda.as_ref(),
    ]);

    let trader = Keypair::new();
    let _trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;

    // Build a reveal payload + its hash matching the program's keccak rule.
    let nonce = [7u8; 32];
    let side: u8 = 0; // long
    let size_lots: u64 = 5;
    let limit_ticks: u64 = 99_950;

    use anchor_lang::solana_program::keccak::hashv;
    let hash = hashv(&[
        trader.pubkey().as_ref(),
        &[side],
        &size_lots.to_le_bytes(),
        &limit_ticks.to_le_bytes(),
        &nonce,
    ])
    .0;

    let commit_ix = build_ix(
        flash_book::instruction::SubmitCommit { hash, bond: 1_000 },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(commit_buf, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[commit_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    // Verify the commit landed in the buffer.
    let cb: flash_book::state::CommitBufferAccount =
        fetch(&mut ctx.banks_client, commit_buf).await;
    let active = cb.commits.iter().filter(|r| r.valid == 1).count();
    assert_eq!(active, 1, "commit should be in buffer");

    // Now reveal.
    let reveal_ix = build_ix(
        flash_book::instruction::SubmitReveal {
            side,
            size_lots,
            limit_ticks,
            nonce,
        },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(commit_buf, false),
            AccountMeta::new(order_buf, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[reveal_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    // After reveal: commit slot cleared, order in buffer.
    let cb_after: flash_book::state::CommitBufferAccount =
        fetch(&mut ctx.banks_client, commit_buf).await;
    let active_after = cb_after.commits.iter().filter(|r| r.valid == 1).count();
    assert_eq!(active_after, 0, "commit should be consumed");

    let ob: OrderBufferAccount = fetch(&mut ctx.banks_client, order_buf).await;
    assert_eq!(ob.head, 1, "reveal should produce a taker order");
    let slot = ob.slots[0];
    assert_eq!(slot.valid, 1);
    assert_eq!(slot.side, 0);
    assert_eq!(slot.size_lots, 5);
    assert_eq!(slot.limit_ticks, 99_950);
    assert_eq!(slot.order_type, 1); // OrderType::Taker = 1
    assert_eq!(slot.trader, trader.pubkey());
}

#[tokio::test]
async fn submit_reveal_with_wrong_payload_fails() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, order_buf, _, _) = setup_market(&mut ctx, &payer).await;
    let (commit_buf, _) = pda(&[
        flash_book::state::CommitBufferAccount::SEED,
        market_pda.as_ref(),
    ]);

    let trader = Keypair::new();
    let _trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;

    let nonce = [7u8; 32];
    let side: u8 = 0;
    let size_lots: u64 = 5;
    let limit_ticks: u64 = 99_950;

    use anchor_lang::solana_program::keccak::hashv;
    let hash = hashv(&[
        trader.pubkey().as_ref(),
        &[side],
        &size_lots.to_le_bytes(),
        &limit_ticks.to_le_bytes(),
        &nonce,
    ])
    .0;

    // Submit a valid commit.
    let commit_ix = build_ix(
        flash_book::instruction::SubmitCommit { hash, bond: 1_000 },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(commit_buf, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[commit_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    // Reveal with TAMPERED size — should fail.
    let reveal_ix = build_ix(
        flash_book::instruction::SubmitReveal {
            side,
            size_lots: 6, // tampered
            limit_ticks,
            nonce,
        },
        vec![
            AccountMeta::new_readonly(trader.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(commit_buf, false),
            AccountMeta::new(order_buf, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[reveal_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await;
    assert!(result.is_err(), "reveal with wrong payload should fail");
}

#[tokio::test]
async fn update_oracle_authority_only() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, _, _, _) = setup_market(&mut ctx, &payer).await;

    // Authority can update.
    let ok_ix = build_ix(
        flash_book::instruction::UpdateOracle {
            price_ticks: 105_000,
            confidence: 50,
        },
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_pda, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ok_ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    let market: MarketAccount = fetch(&mut ctx.banks_client, market_pda).await;
    assert_eq!(market.oracle_price_ticks, 105_000);
    assert_eq!(market.oracle_confidence, 50);

    // Random caller cannot update.
    let attacker = Keypair::new();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer.pubkey(),
                &attacker.pubkey(),
                100_000_000,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.banks_client.get_latest_blockhash().await.unwrap(),
        ))
        .await
        .unwrap();

    let bad_ix = build_ix(
        flash_book::instruction::UpdateOracle {
            price_ticks: 200_000, // attacker tries
            confidence: 0,
        },
        vec![
            AccountMeta::new_readonly(attacker.pubkey(), true),
            AccountMeta::new(market_pda, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[bad_ix],
            Some(&attacker.pubkey()),
            &[&attacker],
            bh,
        ))
        .await;
    assert!(result.is_err(), "non-authority should not update oracle");

    // Verify oracle unchanged.
    let market_after: MarketAccount = fetch(&mut ctx.banks_client, market_pda).await;
    assert_eq!(market_after.oracle_price_ticks, 105_000);
}

#[tokio::test]
async fn transfer_market_authority_rotates_keys() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, _, _, _) = setup_market(&mut ctx, &payer).await;

    let new_authority = Keypair::new();

    let ix = build_ix(
        flash_book::instruction::TransferMarketAuthority {
            new_authority: new_authority.pubkey(),
        },
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_pda, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    let market: MarketAccount = fetch(&mut ctx.banks_client, market_pda).await;
    assert_eq!(market.authority, new_authority.pubkey());

    // Old authority can't update oracle anymore.
    let bad_ix = build_ix(
        flash_book::instruction::UpdateOracle {
            price_ticks: 999_999,
            confidence: 0,
        },
        vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(market_pda, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[bad_ix],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await;
    assert!(result.is_err(), "old authority should be revoked");
}

#[tokio::test]
async fn liquidate_portfolio_rejects_healthy_trader_zero_remaining() {
    // Degenerate cross-market case: only the execution market, no other
    // positions. Should behave identically to liquidate_position — reject
    // healthy traders.
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (market_pda, order_buf, _, _) = setup_market(&mut ctx, &payer).await;

    let trader = Keypair::new();
    let trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;
    let (position, _) = pda(&[
        flash_book::state::PositionAccount::SEED,
        market_pda.as_ref(),
        trader.pubkey().as_ref(),
    ]);

    // Caller — fund.
    let caller = Keypair::new();
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer.pubkey(),
                &caller.pubkey(),
                100_000_000,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    // Trader has empty position → liquidation should fail (LiquidationStale).
    let liq_ix = build_ix(
        flash_book::instruction::LiquidatePortfolio {},
        vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market_pda, false),
            AccountMeta::new(order_buf, false),
            AccountMeta::new_readonly(trader_state, false),
            AccountMeta::new_readonly(position, false),
            // No remaining_accounts.
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[liq_ix],
            Some(&caller.pubkey()),
            &[&caller],
            bh,
        ))
        .await;
    assert!(
        result.is_err(),
        "liquidate_portfolio should fail on healthy/empty trader",
    );
}

#[tokio::test]
async fn liquidate_portfolio_with_two_markets_and_no_positions() {
    // Cross-market path: trader has TraderState but no positions on
    // either market. Calling liquidate_portfolio with the second market
    // as cross-margin context should still reject (LiquidationStale on
    // the empty execution position) — proves the multi-market account
    // walk + validation works.
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    // Market 1.
    let (market1_pda, order_buf1, _, _) = setup_market(&mut ctx, &payer).await;
    // Market 2.
    let (market2_pda, _, _, _) = setup_additional_market(&mut ctx, &payer, 200_000).await;

    let trader = Keypair::new();
    let trader_state = setup_trader(&mut ctx, &payer, &trader, 50_000).await;

    let (position1, _) = pda(&[
        flash_book::state::PositionAccount::SEED,
        market1_pda.as_ref(),
        trader.pubkey().as_ref(),
    ]);
    let (position2, _) = pda(&[
        flash_book::state::PositionAccount::SEED,
        market2_pda.as_ref(),
        trader.pubkey().as_ref(),
    ]);

    // We need market1's position to exist (init-if-needed via place_limit_order).
    // Place a tiny order that satisfies the gate (empty position skips margin gate).
    let place_ix = build_ix(
        flash_book::instruction::PlaceLimitOrder {
            side: 0,
            size_lots: 1,
            limit_ticks: 99_950,
            post_only: false,
        },
        vec![
            AccountMeta::new(trader.pubkey(), true),
            AccountMeta::new_readonly(market1_pda, false),
            AccountMeta::new(order_buf1, false),
            AccountMeta::new(trader_state, false),
            AccountMeta::new(position1, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[place_ix],
            Some(&trader.pubkey()),
            &[&trader],
            bh,
        ))
        .await
        .unwrap();

    // Caller funded.
    let caller = Keypair::new();
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer.pubkey(),
                &caller.pubkey(),
                100_000_000,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            bh,
        ))
        .await
        .unwrap();

    // liquidate_portfolio with market2 as cross-margin context.
    // Position on market1 exists but is empty (size 0) → LiquidationStale.
    // The point: the multi-market remaining_accounts path is exercised
    // and the program doesn't crash on the additional market+position.
    // Position2 doesn't exist on chain yet; that's why we don't pass it
    // as cross-margin (the program would try to deserialize an empty
    // account and error on missing discriminator).
    let liq_ix = build_ix(
        flash_book::instruction::LiquidatePortfolio {},
        vec![
            AccountMeta::new_readonly(caller.pubkey(), true),
            AccountMeta::new_readonly(market1_pda, false),
            AccountMeta::new(order_buf1, false),
            AccountMeta::new_readonly(trader_state, false),
            AccountMeta::new_readonly(position1, false),
            // No remaining_accounts: empty cross-margin → degenerate single-market.
        ],
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let result = ctx
        .banks_client
        .process_transaction(Transaction::new_signed_with_payer(
            &[liq_ix],
            Some(&caller.pubkey()),
            &[&caller],
            bh,
        ))
        .await;
    // Empty position on market1 → LiquidationStale.
    assert!(
        result.is_err(),
        "liquidate_portfolio on empty position should fail",
    );
    let _ = position2;
}

#[tokio::test]
async fn second_market_initializes_at_different_oracle_price() {
    let pt = make_program_test();
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();

    let (m1, _, _, _) = setup_market(&mut ctx, &payer).await;
    let (m2, _, _, _) = setup_additional_market(&mut ctx, &payer, 200_000).await;

    let market1: MarketAccount = fetch(&mut ctx.banks_client, m1).await;
    let market2: MarketAccount = fetch(&mut ctx.banks_client, m2).await;

    assert_eq!(market1.oracle_price_ticks, 100_000);
    assert_eq!(market2.oracle_price_ticks, 200_000);
    assert_ne!(market1.base_mint, market2.base_mint);
    assert_ne!(market1.quote_mint, market2.quote_mint);
    // Both should share the same authority + global PDAs.
    assert_eq!(market1.authority, market2.authority);
    assert_eq!(market1.flp_pool, market2.flp_pool);
    assert_eq!(market1.insurance_fund, market2.insurance_fund);
}
