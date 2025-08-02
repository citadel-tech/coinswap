#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::MakerBehavior,
    taker::TakerBehavior,
    utill::{ConnectionType, MIN_FEE_RATE},
};
mod test_framework;
use std::sync::atomic::Ordering::Relaxed;
use test_framework::*;

// Test data: Create addresses with different UTXO patterns
const UTXO_SETUP: &[(u8, &[f64])] = &[
    (1, &[0.2, 0.2, 0.2]),
    (2, &[0.5, 0.2]),
    (3, &[0.1]),
    (4, &[0.3]),
    (5, &[0.5]),
];

// Test scenarios: (target, expected_utxos, name, description)
const TEST_CASES: &[(f64, &[f64], &str, &str)] = &[
    (
        0.4,
        &[0.2, 0.2, 0.2],
        "Case 1: Low target",
        "Should select 0.6 BTC group (smallest that covers target+fees)",
    ),
    (
        0.65,
        &[0.2, 0.2, 0.2, 0.5, 0.2],
        "Case 2: Above 0.6 BTC",
        "Should select 0.7 BTC group (0.6 BTC insufficient for target+fees)",
    ),
    (
        1.0,
        &[0.2, 0.2, 0.2, 0.5, 0.2],
        "Case 3: Between single groups",
        "Should select both groups (1.3 BTC total)",
    ),
    (
        1.5,
        &[0.2, 0.2, 0.2, 0.5, 0.2, 0.3],
        "Case 4: Above both groups",
        "Should select both groups + additional UTXOs",
    ),
    (
        2.1,
        &[0.2, 0.2, 0.2, 0.5, 0.2, 0.1, 0.3, 0.5],
        "Case 5: Select all UTXOs",
        "Should select all available UTXOs (2.1 BTC total)",
    ),
];

#[test]
fn test_address_grouping_behavior() {
    // Initialize test environment with one maker
    let makers_config_map = [((6102, None), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    println!("=== Testing Smart Address Grouping Behavior ===");

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    println!("=== UTXO Setup ===");

    let mut addresses = std::collections::HashMap::new();

    // Create the UTXO structure
    for &(addr_id, amounts) in UTXO_SETUP {
        let address = addresses.entry(addr_id).or_insert_with(|| {
            maker
                .get_wallet()
                .write()
                .unwrap()
                .get_next_external_address()
                .unwrap()
        });

        if amounts.len() > 1 {
            println!(
                "Creating grouped address {addr_id} with {} UTXOs:",
                amounts.len()
            );
        } else {
            println!("Creating independent address {addr_id} with 1 UTXO:");
        }

        // Send BTC to each address according to the setup
        for &amount in amounts {
            send_to_address(bitcoind, address, Amount::from_btc(amount).unwrap());
            generate_blocks(bitcoind, 1);
            println!("  Added {amount} BTC to address {addr_id}");
        }
    }

    // Sync wallet to see all the UTXOs we just created
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    println!("\n=== Available UTXO Summary ===");
    let total_available: f64 = UTXO_SETUP
        .iter()
        .flat_map(|(_, amounts)| amounts.iter())
        .sum();
    println!("TOTAL AVAILABLE: {total_available} BTC");

    println!("\n=== Strategic Address Grouping Test Cases ===");

    // Test each scenario to validate the smart address grouping algorithm
    for &(target_btc, expected_utxos, case_name, expected_behavior) in TEST_CASES {
        println!("\n--- {case_name} ---");
        println!("Target amount: {target_btc} BTC");
        println!("Expected: {expected_behavior}");

        let wallet = maker.get_wallet().read().unwrap();
        let target_amount = Amount::from_btc(target_btc).unwrap();

        // Call the coin selection algorithm we're testing
        let selected_utxos = wallet
            .coin_select(target_amount, MIN_FEE_RATE, None)
            .unwrap();

        let selected_amounts: Vec<f64> = selected_utxos
            .iter()
            .map(|(utxo, _)| utxo.amount.to_btc())
            .collect();
        let total_selected: f64 = selected_amounts.iter().sum();

        println!("SELECTED UTXOs: {selected_amounts:?} BTC");
        println!("TOTAL SELECTED: {total_selected} BTC");
        println!("UTXO COUNT: {}", selected_utxos.len());

        // Convert expected UTXOs to sorted vec for comparison
        let mut expected_sorted = expected_utxos.to_vec();
        expected_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mut selected_sorted = selected_amounts;
        selected_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // Assert exact UTXO match (as mojo requested)
        assert_eq!(
            selected_sorted, expected_sorted,
            "UTXO mismatch! Expected {expected_sorted:?}, got {selected_sorted:?}"
        );

        // Additional validation: ensure selection can actually cover target + reasonable fees
        let reasonable_fee_estimate = 0.001;
        assert!(
            total_selected >= target_btc + reasonable_fee_estimate,
            "Selection {} BTC insufficient for target {} BTC + reasonable fees",
            total_selected,
            target_btc
        );
        println!("✅ Test passed - correct UTXOs selected");
    }

    println!("\n=== Test Completed Successfully ===");
    println!("✅ All address grouping scenarios work correctly");

    // Clean shutdown
    directory_server_instance.shutdown.store(true, Relaxed);
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
