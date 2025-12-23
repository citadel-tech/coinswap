#![cfg(feature = "integration-test")]
use bitcoin::{Amount, OutPoint};
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::MIN_FEE_RATE,
    wallet::WalletError,
};
use log::{info, warn};
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    thread,
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
    println!("\n🔧 Starting Manual Swapping in conjunction with Regular-Swapcoin clause Test");
    test_maunal_coinselection();

    println!("\n⏳ Waiting for cleanup...");
    std::thread::sleep(Duration::from_secs(10));

    println!("\n🔧 Starting Separated UTXO Test");
    test_separated_utxo_coin_selection();

    println!("\n⏳ Waiting for cleanup...");
    std::thread::sleep(Duration::from_secs(10));

    println!("\n🔧 Starting Address Grouping Test");
    test_address_grouping_behavior();

    println!("\n✅ All UTXO Tests Completed Successfully");

    println!("\n⏳ Waiting for cleanup...");
    std::thread::sleep(Duration::from_secs(10));
}

fn test_address_grouping_behavior() {
    // Initialize test environment with one maker
    let makers_config_map = [((6102, None), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

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
        wallet.sync_and_save().unwrap();
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

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!("🔧 Running Test: Separated UTXO Coin Selection");
    let bitcoind = &test_framework.bitcoind;

    // Fund the Taker and Makers
    let taker = &mut takers[0];
    fund_and_verify_taker(taker, bitcoind, 6, Amount::from_btc(0.1).unwrap()); // 60M sats total

    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(
        makers_ref.clone(),
        bitcoind,
        6,
        Amount::from_btc(0.1).unwrap(),
    ); // 60M sats total

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
        manually_selected_outpoints: None,
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
        wallet.sync_and_save().unwrap();
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

    // Fund the maker with enough to cover the target (30M SATS)
    fund_and_verify_maker(makers_ref, bitcoind, 1, Amount::from_sat(30000000));

    // Generate blocks to confirm
    generate_blocks(bitcoind, 3);

    // Sync wallet and retry
    {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync_and_save().unwrap();
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

    // Clean shutdown
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

fn test_maunal_coinselection() {
    let (test_framework, mut takers, maker, block_generation_handle) = TestFramework::init(
        vec![
            ((26102, Some(19053)), MakerBehavior::Normal),
            ((36102, Some(19054)), MakerBehavior::Normal),
        ],
        vec![TakerBehavior::Normal],
    );

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    let amounts: Vec<u64> = vec![
        90_283, 150_813, 212_842, 185_372, 478_324, 314_332, 136_414, 23_894, 10_000,
    ];

    for &amount in &amounts {
        fund_and_verify_taker(taker, bitcoind, 1, Amount::from_sat(amount));
    }

    // Fund the Maker with 3 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = maker.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    let maker_threads = maker
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    for maker in maker.iter() {
        while !maker.is_setup_complete.load(Relaxed) {
            info!("⏳ Waiting for maker setup completion");
            thread::sleep(Duration::from_secs(10));
        }
    }

    taker.get_wallet_mut().sync_and_save().unwrap();

    let all_utxos = taker.get_wallet_mut().list_all_utxo();

    let manually_selected_utxos: Vec<OutPoint> = amounts
        .iter()
        .take(4)
        .cloned()
        .collect::<Vec<u64>>()
        .iter()
        .filter_map(|&target_amount| {
            all_utxos
                .iter()
                .find(|utxo| utxo.amount.to_sat() == target_amount)
                .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        })
        .collect();

    println!(
        "📋Test 1 : Selected {} UTXOs manually:",
        manually_selected_utxos.len()
    );

    for outpoint in &manually_selected_utxos {
        println!(" - {outpoint}");
    }

    let _ = taker.do_coinswap(SwapParams {
        send_amount: Amount::from_btc(0.01).unwrap(),
        maker_count: 2,
        manually_selected_outpoints: Some(manually_selected_utxos.clone()),
    });

    // After Swap is done, wait for maker threads to conclude.
    maker
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    info!("🎯 All coinswaps processed successfully. Transaction complete.");

    thread::sleep(Duration::from_secs(10));

    // Sync taker wallet to get the latest UTXO state after swap
    taker.get_wallet_mut().sync_and_save().unwrap();

    for maker in maker.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // Check that the originally manually selected UTXOs were spent
    let remaining_utxos: Vec<OutPoint> = taker
        .get_wallet_mut()
        .list_all_utxo()
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

    println!(
        "\n ✅ Test 1 : All {} originally selected UTXOs were used in the coinswap",
        manually_selected_utxos.len()
    );

    for outpoint in &manually_selected_utxos {
        println!(" ✓ Used: {outpoint}");
    }

    println!("\n === Post-Swap UTXO Analysis ===");

    // Get regular UTXOs (SeedCoin)
    let regular_utxos = taker.get_wallet_mut().list_descriptor_utxo_spend_info();
    println!("\n 📊 Regular UTXOs: {}", regular_utxos.len());

    for (utxo, spend_info) in &regular_utxos {
        println!(
            " - Regular UTXO: {} sats ({})",
            utxo.amount.to_sat(),
            spend_info
        );
    }

    // Get swept incoming swap UTXOs
    let swept_utxos = taker.get_wallet_mut().list_swept_incoming_swap_utxos();

    println!("\n 🧹 Swept Swap UTXOs: {}", swept_utxos.len());

    for (utxo, spend_info) in &swept_utxos {
        println!(
            " - Swept UTXO: {} sats ({})",
            utxo.amount.to_sat(),
            spend_info
        );
    }

    // Summary of UTXO distribution
    let total_regular = regular_utxos
        .iter()
        .map(|(u, _)| u.amount.to_sat())
        .sum::<u64>();
    let total_swept = swept_utxos
        .iter()
        .map(|(u, _)| u.amount.to_sat())
        .sum::<u64>();

    println!("\n === UTXO Distribution Summary ===");

    println!(
        "\n 📊 Regular UTXOs: {} sats ({} UTXOs)",
        total_regular,
        regular_utxos.len()
    );

    println!(
        "\n 🧹 Swept UTXOs: {} sats ({} UTXOs)",
        total_swept,
        swept_utxos.len()
    );

    println!(
        "\n Taker Wallet Balances: {:?}",
        taker.get_wallet().get_balances()
    );

    println!(
        "\n 📋 Test 2: Regular and Swap Coin Selection Testing in conjunction with Manual Selection"
    );

    /*
    ===========================================================================================================================================
    📋 UTXO Selection Test Cases -> Swap(900k~) > Regular(600k~) for this test
    ===========================================================================================================================================

    Index | Test_Case        | Condition                               | Target   | Manual | Expected Behaviour
    -----------------------------------------------------------------------------------------------------------------------
    2a    | Enough Regular   | amount < regular total                  | 400,000  | R      | Ok
    2b    | Enough Swap      | regular total < amount < swap total     | 700,000  | S      | Ok
    2c    | Both Enough      | amount < regular total < swap total     | 500,000  | None   | Ok -> Chooses R by default
    2d    | Not Enough Swap  | amount > swap total                     | 1,000,000| S      | Error -> insufficient funds
    2e    | Not Enough Reg   | amount > regular total                  | 1,000,000  | R      | Error -> insufficient funds
    2f    | Mixed            | amount < (regular + swap)               | 1,200,000| R + S  | Error -> Cannot be mixed
    -----------------------------------------------------------------------------------------------------------------------

    Legend:
    R = Regular UTXOs
    S = Swap UTXOs

    ===========================================================================================================================================
    */

    let test_cases = vec![
        // (test_name, target_amount, manual_selection_type, expected_result)
        ("2a: Enough Regular", 400_000, "R", "Ok"),
        ("2b: Enough Swap", 700_000, "S", "Ok"),
        ("2c: Both Enough (Auto)", 500_000, "None", "Ok"),
        ("2d: Not Enough Swap", 1_000_000, "S", "Error"),
        ("2e: Not Enough Regular", 1_000_000, "R", "Error"),
        ("2f: Mixed Selection", 900_000, "R+S", "Error"),
    ];

    for (test_name, target_amount, selection_type, expected) in test_cases {
        println!("\n--- Testing {test_name} ---");

        println!("Amount: {target_amount} sats, Selection: {selection_type}, Expected: {expected}");

        let target_amount = Amount::from_sat(target_amount);
        let manual_outpoints = match selection_type {
            "R" => {
                let regular_outpoints: Vec<OutPoint> = regular_utxos
                    .iter()
                    .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                    .collect();
                Some(regular_outpoints)
            }
            "S" => {
                let swap_outpoints: Vec<OutPoint> = swept_utxos
                    .iter()
                    .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                    .collect();
                Some(swap_outpoints)
            }
            "R+S" => {
                let mixed_outpoints: Vec<OutPoint> = vec![
                    regular_utxos
                        .first()
                        .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                        .unwrap(),
                    swept_utxos
                        .first()
                        .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                        .unwrap(),
                ];
                Some(mixed_outpoints)
            }
            "None" => None,
            _ => None,
        };

        let result =
            taker
                .get_wallet_mut()
                .coin_select(target_amount, MIN_FEE_RATE, manual_outpoints);
        match (result, expected) {
            (Ok(selection), "Ok") => {
                println!(
                    "✅ {test_name}: SUCCESS - Selected {} UTXOs",
                    selection.len()
                );
                for (utxo, spend_info) in &selection {
                    println!(" - {} sats ({})", utxo.amount.to_sat(), spend_info);
                }
            }
            (Err(e), "Error") => {
                let error_msg = format!("{e:?}");
                if selection_type == "R+S"
                    && error_msg.contains("Cannot mix regular and swap UTXOs")
                {
                    println!("✅ {test_name}: SUCCESS - Correctly rejected mixed UTXO selection");
                } else if error_msg.contains("InsufficientFund") {
                    println!("✅ {test_name}: SUCCESS - Correctly failed with insufficient funds");
                } else {
                    println!("✅ {test_name}: SUCCESS - {error_msg}");
                }
            }
            (Ok(_), "Error") => {
                panic!(
                    "❌ {}: FAILED - Expected error but selection succeeded!",
                    test_name
                );
            }
            (Err(e), "Ok") => {
                panic!(
                    "❌ {}: FAILED - Expected success but got error: {:?}",
                    test_name, e
                );
            }
            (Err(_), _) | (Ok(_), _) => {
                panic!("❌ {}: FAILED - Unexpected Behaviour", test_name);
            }
        }
    }

    println!("✅ All test cases completed successfully");
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
