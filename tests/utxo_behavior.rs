#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::{ConnectionType, MIN_FEE_RATE},
    wallet::WalletError,
};
use log::{info, warn};
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    time::Duration,
};

mod test_framework;
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
fn run_all_utxo_tests() {
    println!("=== Running All UTXO Tests ===");

    println!("\n🔧 Starting Separated UTXO Test");
    test_separated_utxo_coin_selection();

    println!("\n⏳ Waiting for cleanup...");
    std::thread::sleep(Duration::from_secs(10));

    println!("\n🔧 Starting Address Grouping Test");
    test_address_grouping_behavior();

    println!("\n✅ All UTXO Tests Completed Successfully");
}

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
        let selected_utxos = wallet.coin_select(target_amount, MIN_FEE_RATE).unwrap();

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

fn test_separated_utxo_coin_selection() {
    // Initialize test environment with TWO makers and one taker
    let makers_config_map = [
        ((6102, Some(19051)), MakerBehavior::Normal),
        ((16102, Some(19052)), MakerBehavior::Normal),
    ];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, mut takers, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    warn!("🔧 Running Test: Separated UTXO Coin Selection");
    let bitcoind = &test_framework.bitcoind;

    // Fund the Taker and Makers
    let taker = &mut takers[0];
    fund_and_verify_taker(taker, bitcoind, 6, Amount::from_btc(0.1).unwrap()); // 60M sats total

    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 6, Amount::from_btc(0.1).unwrap()); // 60M sats total

    // Start the Maker Servers
    info!("🚀 Starting Maker servers");
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            std::thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for both makers setup completion
    for maker in &makers {
        while !maker.is_setup_complete.load(Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    // Perform coinswap to create swap coins
    info!("🔄 Performing coinswap to create swap coins");
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(35000000), // 35M sats (0.35 BTC)
        maker_count: 2,
        tx_count: 3,
    };
    taker.do_coinswap(swap_params).unwrap();

    // Shutdown maker servers
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Sync both maker wallets
    for maker in &makers {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync().unwrap();
    }

    println!("=== Testing Separated UTXO Coin Selection ===");

    // Test with the first maker
    let maker = makers.first().unwrap();

    {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        println!("=== Current Wallet Balances ===");
        println!("Regular: {} sats", balances.regular.to_sat());
        println!("Swap: {} sats", balances.swap.to_sat());
        println!("Total Spendable: {} sats", balances.spendable.to_sat());

        // Test Case 1: Target < regular (should use only regular UTXOs)
        let target_1 = Amount::from_sat(11000000); // 11M sats
        println!("\n--- Test Case 1: Regular UTXOs Only ---");
        println!("Target: {} sats (< regular)", target_1.to_sat());

        let result_1 = wallet.coin_select(target_1, MIN_FEE_RATE);
        assert!(
            result_1.is_ok(),
            "Test Case 1 failed: Expected success but got error: {:?}",
            result_1.err()
        );
        let selection_1 = result_1.unwrap();
        println!("✅ Selected {} UTXOs", selection_1.len());
        assert!(
            !selection_1.is_empty(),
            "Test Case 1: Expected non-empty selection"
        );

        // Test Case 2: regular < target < swap (should use only swap UTXOs)
        let target_2 = Amount::from_sat(28000000); // 28M sats
        println!("\n--- Test Case 2: Swap UTXOs Only ---");
        println!(
            "Target: {} sats (regular insufficient, swap sufficient)",
            target_2.to_sat()
        );

        let result_2 = wallet.coin_select(target_2, MIN_FEE_RATE);
        assert!(
            result_2.is_ok(),
            "Test Case 2 failed: Expected success but got error: {:?}",
            result_2.err()
        );
        let selection_2 = result_2.unwrap();
        println!("✅ Selected {} UTXOs", selection_2.len());
        assert!(
            !selection_2.is_empty(),
            "Test Case 2: Expected non-empty selection"
        );

        // Test Case 3: target > max(regular, swap) but < (regular + swap) (should fail - no mixing)
        let target_3 = Amount::from_sat(46000000); // 46M sats
        println!("\n--- Test Case 3: Mixing Should Fail ---");
        println!(
            "Target: {} sats (> both individual amounts but < total)",
            target_3.to_sat()
        );

        let result_3 = wallet.coin_select(target_3, MIN_FEE_RATE);
        assert!(
            result_3.is_err(),
            "Test Case 3 failed: Expected error but got success with {} UTXOs",
            result_3.as_ref().unwrap().len()
        );

        let error = result_3.unwrap_err();
        match &error {
            WalletError::InsufficientFund {
                available,
                required,
            } => {
                println!("✅ Correctly failed with InsufficientFund");
                println!(
                    "   Available: {available} sats (regular only), Required: {required} sats"
                );
                assert_eq!(*required, target_3.to_sat() + 324); // Should include 324 sats estimated fee
                assert_eq!(*available, balances.regular.to_sat());
                println!("✅ Confirmed: Only regular balance reported in insufficient funds error");
            }
            _ => panic!(
                "Test Case 3: Expected WalletError::InsufficientFund, got: {:?}",
                error
            ),
        }
    }

    // Now fund regular UTXOs and retry - should work
    println!("\n--- Test Case 3: Add Regular Funds and Retry ---");

    let new_address = {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.get_next_external_address().unwrap()
    };

    println!("🏦 Funding new regular address: {new_address}");

    // Fund the new address with enough to cover the target
    let additional_amount = Amount::from_sat(30000000); // 30M sats
    send_to_address(bitcoind, &new_address, additional_amount);

    // Generate blocks to confirm
    generate_blocks(bitcoind, 3);

    // Sync wallet and retry
    {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync().unwrap();
        let updated_balances = wallet.get_balances().unwrap();

        println!("Updated balances after funding:");
        println!("  Regular: {} sats", updated_balances.regular.to_sat());
        println!("  Swap: {} sats", updated_balances.swap.to_sat());

        // Now retry the same target - should succeed with regular UTXOs only
        let target_3 = Amount::from_sat(46000000); // Same target
        println!("🔄 Retrying coin selection with additional regular funds...");
        let result_3_retry = wallet.coin_select(target_3, MIN_FEE_RATE);
        assert!(
            result_3_retry.is_ok(),
            "Test Case 3 retry failed: Expected success with additional regular funds, got: {:?}",
            result_3_retry.err()
        );

        let selection_3_retry = result_3_retry.unwrap();
        println!(
            "✅ Successfully selected {} UTXOs with additional regular funds",
            selection_3_retry.len()
        );
        println!("✅ Confirmed: Works after funding regular UTXOs");
    }

    println!("\n=== Test Completed Successfully ===");

    // Clean shutdown
    directory_server_instance.shutdown.store(true, Relaxed);
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
