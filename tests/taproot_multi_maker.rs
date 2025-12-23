#![cfg(feature = "integration-test")]
//! This test demonstrates a taproot coinswap between a Taker and multiple Makers

use bitcoin::Amount;
use coinswap::{
    maker::start_maker_server_taproot,
    taker::api2::{SwapParams, TakerBehavior},
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test taproot multi maker coinswap
#[test]
fn test_taproot_multi_maker() {
    // ---- Setup ----
    warn!("Running Test: Taproot Multi Maker Coinswap");

    // Create 4 makers to perform a taproot swap with 1 taker and 4 makers.
    use coinswap::maker::TaprootMakerBehavior as MakerBehavior;
    let taproot_makers_config_map = vec![
        (7102, Some(19061), MakerBehavior::Normal),
        (17102, Some(19062), MakerBehavior::Normal),
        (27102, Some(19063), MakerBehavior::Normal),
        (15102, Some(19064), MakerBehavior::Normal),
    ];
    // Create a taker with normal behavior
    let taker_behavior = vec![TakerBehavior::Normal];

    // Initialize test framework
    let (test_framework, mut taproot_taker, taproot_makers, block_generation_handle) =
        TestFramework::init_taproot(taproot_makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = taproot_taker.get_mut(0).unwrap();

    // Fund the Taproot Taker with 3 UTXOs of 0.05 BTC each
    let taproot_taker_original_balance =
        fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Fund the Taproot Makers with 4 UTXOs of 0.05 BTC each
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    // Start the Taproot Maker Server threads
    log::info!("Initiating Taproot Makers...");

    let taproot_maker_threads = taproot_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server_taproot(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for taproot makers to complete setup
    for maker in &taproot_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            log::info!("Waiting for taproot maker setup completion");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &taproot_makers {
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    // Get the actual spendable balances AFTER fidelity bond creation
    let mut actual_maker_spendable_balances = Vec::new();

    // Test taproot maker balance verification
    log::info!("Testing taproot maker balance verification");
    for (i, maker) in taproot_makers.iter().enumerate() {
        let wallet = maker.wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Taproot Maker {} balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity
        );

        // With real fidelity bonds, regular balance should be 0.20 BTC minus 0.05 BTC fidelity bond minus small fee
        assert!(
            balances.regular >= Amount::from_btc(0.14).unwrap(),
            "Regular balance should be around 0.14999 BTC after fidelity bond creation"
        );
        assert!(
            balances.regular <= Amount::from_btc(0.15).unwrap(),
            "Regular balance should not exceed 0.15 BTC"
        );
        assert_eq!(balances.swap, Amount::ZERO);
        assert_eq!(balances.contract, Amount::ZERO);
        assert_eq!(
            balances.fidelity,
            Amount::from_btc(0.05).unwrap(),
            "Fidelity bond should be 0.05 BTC"
        );
        assert!(
            balances.spendable > Amount::ZERO,
            "Maker should have spendable balance"
        );

        // Store the actual spendable balance AFTER fidelity bond creation
        actual_maker_spendable_balances.push(balances.spendable);
    }

    log::info!("Starting multi maker taproot coinswap...");
    // Swap params for taproot coinswap
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 4,                        // 4 maker count
        tx_count: 5,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Perform the swap
    match taproot_taker.do_coinswap(swap_params) {
        Ok(Some(_report)) => {
            log::info!("Taproot multi maker coinswap completed successfully!");
        }
        Ok(None) => {
            log::warn!(
                "Taproot multi maker coinswap completed but no report generated (recovery occurred)"
            );
        }
        Err(e) => {
            log::error!("Taproot multi maker coinswap failed: {:?}", e);
        }
    }

    // After swap, shutdown maker threads
    taproot_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    taproot_maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("✅ Taproot mutli maker test passed successfully.");

    // Sync wallets and verify results
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each taproot maker's wallet multiple times to ensure all UTXOs are discovered
    for maker in taproot_makers.iter() {
        let mut wallet = maker.wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // Verify swap results
    verify_taproot_swap_results(
        taproot_taker.get_wallet(),
        &taproot_makers,
        taproot_taker_original_balance,
        actual_maker_spendable_balances, // Use the actual spendable balances after fidelity bond creation
    );

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
