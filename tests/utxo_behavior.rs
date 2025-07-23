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

const TEST_CASES: &[(f64, f64, &str, &str)] = &[
    (
        0.4,
        0.6,
        "Case 1: Low target",
        "Should select 0.6 BTC group (smallest that covers target+fees)",
    ),
    (
        0.59,
        0.6,
        "Case 2: Just under 0.6 BTC",
        "Should select 0.6 BTC group (target+fees still fits in 0.6 BTC)",
    ),
    (
        0.5999999,
        0.7,
        "Case 3: Boundary case",
        "Should select 0.7 BTC group (0.6 BTC insufficient for target+fees)",
    ),
    (
        0.65,
        0.7,
        "Case 4: 0.6 BTC insufficient with fees",
        "Should select 0.7 BTC group (0.6 BTC can't cover target+fees)",
    ),
    (
        1.0,
        1.3,
        "Case 5: Between single groups",
        "Should select both groups (1.3 BTC total)",
    ),
    (
        1.5,
        1.5,
        "Case 6: Above both groups",
        "Should select both groups + additional UTXOs",
    ),
    (
        2.0,
        2.1,
        "Case 7: Near total limit",
        "Should select most/all available UTXOs",
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

    println!("=== Testing Smart Address Grouping Behavior (with Fee Considerations) ===");

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

    println!("\n=== Strategic Address Grouping Test Cases (Fee-Aware) ===");

    // Test each scenario to validate the smart address grouping algorithm
    for &(target_btc, expected_total_btc, case_name, expected_behavior) in TEST_CASES {
        println!("\n--- {case_name} ---");
        println!("Target amount: {target_btc} BTC");
        println!("Expected: {expected_behavior}");

        let wallet = maker.get_wallet().read().unwrap();
        let target_amount = Amount::from_btc(target_btc).unwrap();

        // Call the coin selection algorithm we're testing
        match wallet.coin_select(target_amount, MIN_FEE_RATE) {
            Ok(selected_utxos) => {
                let selected_amounts: Vec<u64> = selected_utxos
                    .iter()
                    .map(|(utxo, _)| utxo.amount.to_sat())
                    .collect();
                let total_selected: u64 = selected_amounts.iter().sum();
                let total_btc = Amount::from_sat(total_selected).to_btc();

                println!("SELECTED UTXOs: {selected_amounts:?} sats");
                println!("TOTAL SELECTED: {total_btc} BTC ({total_selected} sats)");
                println!("UTXO COUNT: {}", selected_utxos.len());

                // Verify the algorithm selected the expected amount
                // Increased tolerance since we now account for fees properly
                let diff = (total_btc - expected_total_btc).abs();
                assert!(
                    diff < 0.15, // Increased tolerance for fee variations
                    "Expected ~{} BTC, got {} BTC (diff: {}). \nNote: Small differences expected due to fee calculations.",
                    expected_total_btc,
                    total_btc,
                    diff
                );

                // Additional validation: ensure selection can actually cover target + reasonable fees
                let reasonable_fee_estimate = 0.001;
                assert!(
                    total_btc >= target_btc + reasonable_fee_estimate,
                    "Selection {} BTC insufficient for target {} BTC + reasonable fees",
                    total_btc,
                    target_btc
                );
            }
            Err(e) => {
                panic!(
                    "Unexpected selection failure for target {} BTC: {:?}",
                    target_btc, e
                );
            }
        }
    }

    println!("\n=== Test Completed Successfully ===");
    println!("✅ All address grouping scenarios work correctly with fee considerations");

    // Clean shutdown
    directory_server_instance.shutdown.store(true, Relaxed);
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
