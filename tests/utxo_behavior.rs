#![cfg(feature = "integration-test")]
use bitcoin::{Amount, OutPoint};
use coinswap::{
    maker::{start_maker_server, Maker, MakerBehavior},
    taker::{SwapParams, Taker, TakerBehavior},
    utill::{ConnectionType, MIN_FEE_RATE},
    wallet::WalletError,
};
use log::info;
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    thread::{self},
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

    // 2 Makers with Normal behavior.
    let makers_config_map = [
        ((6102, Some(19051)), MakerBehavior::Normal),
        ((16102, Some(19052)), MakerBehavior::Normal),
    ];

    // First taker for 2 sequential Coinswaps and Second taker for discrete grouped utxo testing
    let taker_behavior = vec![TakerBehavior::Normal, TakerBehavior::Normal];
    let connection_type = ConnectionType::CLEARNET;

    // Initiate test framework, Makers and a Taker with default behavior.
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior, connection_type);

    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();

    fund_and_verify_maker(
        makers_ref.clone(),
        &test_framework.bitcoind,
        6,
        Amount::from_btc(0.1).unwrap(),
    ); // 60M sats total

    fund_and_verify_taker(
        &mut takers[0],
        &test_framework.bitcoind,
        6,
        Amount::from_btc(0.1).unwrap(),
    ); // 60M sats total

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Makers take time to fully setup.
    let _ = makers
        .iter()
        .map(|maker| {
            while !maker.is_setup_complete.load(Relaxed) {
                info!("⏳ Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }
        })
        .collect::<Vec<_>>();

    println!("\n🔧 Starting Address Grouping Test");
    test_address_grouping_behavior(&test_framework, &mut takers[1]);

    println!("\n🔧 Starting Separated UTXO Test");
    test_separated_utxo_coin_selection(&test_framework, &makers, &mut takers[0]);

    println!("\n🔧 Starting Manual UTXO Selection Test");
    test_manual_coinswap(&test_framework, &makers, &mut takers[0]);

    // Shutdown maker servers
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    println!("\n✅ All UTXO Tests Completed Successfully");

    // Clean shutdown of all resources
    info!("🧹 Cleaning up test resources");
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

fn test_address_grouping_behavior(test_framework: &TestFramework, taker: &mut Taker) {
    println!("=== Testing Smart Address Grouping Behavior ===");

    let bitcoind = &test_framework.bitcoind;

    println!("=== UTXO Setup ===");

    let mut addresses = std::collections::HashMap::new();

    // Create the UTXO structure
    for &(addr_id, amounts) in UTXO_SETUP {
        let address = addresses
            .entry(addr_id)
            .or_insert_with(|| taker.get_wallet_mut().get_next_external_address().unwrap());

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
        let wallet = taker.get_wallet_mut();
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

        let target_amount = Amount::from_btc(target_btc).unwrap();

        // Call the coin selection algorithm we're testing
        let selected_utxos = taker
            .get_wallet()
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
}

fn test_separated_utxo_coin_selection(
    test_framework: &TestFramework,
    makers: &[Arc<Maker>],
    taker: &mut Taker,
) {
    println!("=== Testing Separated UTXO Coin Selection ===");

    // The taker and makers are already set up and funded by the main test function
    // No need to re-initialize or setup again - use the passed parameters

    let bitcoind = &test_framework.bitcoind;

    // Perform coinswap to create swap coins
    info!("🔄 Performing coinswap to create swap coins");
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(35000000), // 35M sats (0.35 BTC)
        maker_count: 2,
        tx_count: 3,
        manually_selected_outpoints: None,
    };
    taker.do_coinswap(swap_params).unwrap();

    // Sync both maker wallets
    for maker in makers {
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

        let result_1 = wallet.coin_select(target_1, MIN_FEE_RATE, None);
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

        let result_2 = wallet.coin_select(target_2, MIN_FEE_RATE, None);
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

        let result_3 = wallet.coin_select(target_3, MIN_FEE_RATE, None);
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
        let result_3_retry = wallet.coin_select(target_3, MIN_FEE_RATE, None);
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
}

fn test_manual_coinswap(
    test_framework: &TestFramework,
    makers: &[Arc<Maker>],
    // maker_threads: &Vec<JoinHandle<()>>,
    taker: &mut Taker,
) {
    let bitcoind = &test_framework.bitcoind;

    // Deterministic amounts in the range of 0.005 to 0.01 BTC (5,000,000 to 10,000,000 sats)
    let amounts: Vec<u64> = vec![
        590_283, 550_813, 612_842, 685_372, 700_000, 750_000, 800_000, 850_000, 900_000, 1_000_000,
    ];

    for &amount in &amounts {
        let taker_address = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &taker_address, Amount::from_sat(amount));
        generate_blocks(bitcoind, 1);
    }

    taker.get_wallet_mut().sync_and_save().unwrap();

    // Get all UTXOs after funding
    let all_utxos = taker.get_wallet_mut().get_all_utxo().unwrap();

    // Select the first 4 amounts from our predefined amounts vector
    let target_amounts: Vec<u64> = amounts.iter().take(4).cloned().collect();

    // Find UTXOs corresponding to those specific amounts
    let manually_selected_utxos: Vec<OutPoint> = target_amounts
        .iter()
        .filter_map(|&target_amount| {
            // Find the first UTXO with matching amount
            all_utxos
                .iter()
                .find(|utxo| utxo.amount.to_sat() == target_amount)
                .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        })
        .collect();

    // Verify we found all 4 UTXOs
    assert_eq!(
        manually_selected_utxos.len(),
        4,
        "Expected to find 4 UTXOs with specific amounts, but found {}",
        manually_selected_utxos.len()
    );

    log::info!(
        "📋 Selected {} UTXOs manually:",
        manually_selected_utxos.len()
    );
    for outpoint in &manually_selected_utxos {
        info!("  - {outpoint}");
    }

    // Fund the Maker with 3 utxos of 0.35 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();

    makers_ref.iter().for_each(|&maker| {
        let mut wallet_write = maker.wallet.write().unwrap();

        for _ in 0..2 {
            let maker_addr = wallet_write.get_next_external_address().unwrap();
            send_to_address(bitcoind, &maker_addr, Amount::from_sat(3_500_000));
        }
    });

    // confirm balances
    generate_blocks(bitcoind, 1);

    // Initiate Coinswap
    info!("🔄 Initiating coinswap protocol");

    // Swap params for coinswap with manually selected UTXOs
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(4_500_000),
        maker_count: 2,
        tx_count: 1,
        manually_selected_outpoints: Some(manually_selected_utxos.clone()),
    };
    taker.do_coinswap(swap_params).unwrap();

    // Verify that manually selected UTXOs were actually used
    info!("🔍 Verifying manually selected UTXOs were used in the swap");
    taker.get_wallet_mut().sync_and_save().unwrap();

    // Check that the manually selected UTXOs are no longer in the wallet (they were spent)
    let remaining_utxos: Vec<OutPoint> = taker
        .get_wallet_mut()
        .get_all_utxo()
        .unwrap()
        .iter()
        .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        .collect();

    let manual_utxos_spent = manually_selected_utxos
        .iter()
        .all(|manual_utxo| !remaining_utxos.contains(manual_utxo));

    assert!(
        manual_utxos_spent,
        "Not all manually selected UTXOs were spent in the coinswap"
    );

    info!(
        "✅ Verified: All {} manually selected UTXOs were used in the coinswap",
        manually_selected_utxos.len()
    );
    for outpoint in &manually_selected_utxos {
        info!("  ✓ Used: {outpoint}");
    }

    info!("🎯 All coinswaps processed successfully. Transaction complete.");
}
