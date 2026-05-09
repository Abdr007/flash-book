//! Cross-language parity test.
//!
//! Reads `tests/parity/scenarios.json` from the repo root, runs each
//! scenario through the Rust matcher (`clear_batch`), and asserts the
//! computed clearing price/volume matches the documented expected
//! outputs.
//!
//! The TypeScript SDK has a *parallel* test (`sdk-ts/tests/parity.test.ts`)
//! that reads the SAME json file and runs each scenario through the TS
//! simulator (`simulateBatchClearing`). Both must agree.
//!
//! If the two implementations ever drift apart, both tests fail loudly,
//! and the canonical fixture file is the place to update or extend.

use anchor_lang::prelude::Pubkey;
use flash_book::matcher::{
    fba::clear_batch,
    lot::{BaseLots, Ticks},
    order::{Order, OrderType, Side},
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    orders: Vec<RawOrder>,
    prior_mark: u64,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct RawOrder {
    id: u64,
    trader_seed: u8,
    side: String,
    order_type: String,
    size: u64,
    limit: u64,
    seq: u64,
}

#[derive(Debug, Deserialize)]
struct Expected {
    #[serde(default)]
    clearing_price: Option<u64>,
    clearing_volume: u64,
    #[serde(default)]
    fill_count: Option<usize>,
    #[serde(default)]
    first_fill_taker_seed: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct File {
    scenarios: Vec<Scenario>,
}

fn parse_side(s: &str) -> Side {
    match s {
        "long" => Side::Long,
        "short" => Side::Short,
        other => panic!("bad side: {}", other),
    }
}

fn parse_order_type(s: &str) -> OrderType {
    match s {
        "limit" => OrderType::Limit,
        "taker" => OrderType::Taker,
        "flp_virtual" => OrderType::FlpVirtual,
        "liquidation" => OrderType::Liquidation,
        "adl" => OrderType::Adl,
        other => panic!("bad order_type: {}", other),
    }
}

fn raw_to_order(raw: &RawOrder) -> Order {
    Order {
        id: raw.id,
        trader: Pubkey::new_from_array([raw.trader_seed; 32]),
        side: parse_side(&raw.side),
        order_type: parse_order_type(&raw.order_type),
        size: BaseLots(raw.size),
        limit_price: Ticks(raw.limit),
        seq: raw.seq,
        post_only: false,
        stp_mode: flash_book::matcher::order::StpMode::CancelNewest,
    }
}

#[test]
fn cross_language_parity_against_shared_fixtures() {
    // Load fixture relative to the workspace root.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest)
        .join("..")
        .join("..")
        .join("tests")
        .join("parity")
        .join("scenarios.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {:?}: {}", path, e));
    let file: File = serde_json::from_str(&raw).expect("parse scenarios.json");

    for scenario in &file.scenarios {
        let orders: Vec<Order> = scenario.orders.iter().map(raw_to_order).collect();
        let result = clear_batch(&orders, Ticks(scenario.prior_mark))
            .unwrap_or_else(|e| panic!("[{}] clear_batch failed: {:?}", scenario.name, e));

        // Volume must match exactly.
        assert_eq!(
            result.clearing_volume.0, scenario.expected.clearing_volume,
            "[{}] clearing_volume mismatch", scenario.name,
        );

        // Clearing price (only checked when expected supplies it).
        if let Some(expected_price) = scenario.expected.clearing_price {
            assert_eq!(
                result.clearing_price.0, expected_price,
                "[{}] clearing_price mismatch", scenario.name,
            );
        }

        // Fill count.
        if let Some(expected_count) = scenario.expected.fill_count {
            assert_eq!(
                result.fills.len(), expected_count,
                "[{}] fill_count mismatch", scenario.name,
            );
        }

        // First-fill taker identity (by trader_seed).
        if let Some(seed) = scenario.expected.first_fill_taker_seed {
            let first = result
                .fills
                .first()
                .unwrap_or_else(|| panic!("[{}] expected at least one fill", scenario.name));
            assert_eq!(
                first.taker_trader, Pubkey::new_from_array([seed; 32]),
                "[{}] first_fill_taker_seed mismatch", scenario.name,
            );
        }
    }
}
