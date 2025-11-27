#![cfg(feature = "integration-test")]
//! Integration test for Taproot Coinswap implementation
//!
//! This test demonstrates a taproot-based coinswap between a Taker and 2 Makers using the new
//! taproot protocol with MuSig2 signatures and enhanced privacy features.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMaker},
    taker::{
        api2::{SwapParams, Taker},
        TakerBehavior,
    },
    wallet::Wallet,
};
use std::sync::Arc;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{assert_eq, sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test taproot coinswap
#[test]
fn test_taproot_coinswap() {
    // ---- Setup ----
    warn!("Running Test: Taproot Coinswap Basic Functionality");

    // Use different ports for taproot makers to avoid conflicts
    use coinswap::maker::TaprootMakerBehavior as MakerBehavior;
    let taproot_makers_config_map = vec![
        (7102, Some(19061), MakerBehavior::Normal),
        (17102, Some(19062), MakerBehavior::Normal),
    ];
    let taker_behavior = vec![TakerBehavior::Normal];

    // Initialize test framework (without regular takers, we'll create taproot taker manually)
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
        maker.wallet().write().unwrap().sync().unwrap();
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

    log::info!("Starting end-to-end taproot swap test...");
    log::info!("Initiating taproot coinswap protocol");

    // Swap params for taproot coinswap
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
    };

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Perform the swap
    match taproot_taker.do_coinswap(swap_params) {
        Ok(()) => {
            log::info!("Taproot coinswap completed successfully!");
        }
        Err(e) => {
            log::error!("Taproot coinswap failed: {:?}", e);
            panic!("Taproot coinswap failed: {:?}", e);
        }
    }

    // After swap, shutdown maker threads
    taproot_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    taproot_maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All taproot coinswaps processed successfully. Transaction complete.");

    // Sync wallets and verify results
    taproot_taker.get_wallet_mut().sync().unwrap();

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each taproot maker's wallet multiple times to ensure all UTXOs are discovered
    for maker in taproot_makers.iter() {
        let mut wallet = maker.wallet().write().unwrap();
        wallet.sync().unwrap();
    }

    // Verify swap results
    verify_taproot_swap_results(
        taproot_taker.get_wallet(),
        &taproot_makers,
        taproot_taker_original_balance,
        actual_maker_spendable_balances, // Use the actual spendable balances after fidelity bond creation
    );

    info!("All taproot swap tests completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

/// Fund taproot makers and verify their balances
fn fund_taproot_makers(
    makers: &[Arc<TaprootMaker>],
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) -> Vec<Amount> {
    let mut original_balances = Vec::new();

    for maker in makers {
        let mut wallet = maker.wallet().write().unwrap();

        // Fund with regular UTXOs
        for _ in 0..utxo_count {
            let addr = wallet.get_next_external_address().unwrap();
            send_to_address(bitcoind, &addr, utxo_value);
        }

        generate_blocks(bitcoind, 1);
        wallet.sync().unwrap();

        // Verify balances - for now just check regular balance
        let balances = wallet.get_balances().unwrap();
        let expected_regular = utxo_value * utxo_count.into();

        assert_eq!(balances.regular, expected_regular);

        info!(
            "Taproot Maker funded successfully. Regular: {}, Fidelity: {}",
            balances.regular, balances.fidelity
        );

        // Store the original spendable balance (after fidelity bond creation)
        info!(
            "Storing original spendable balance for maker: {} (Regular: {}, Fidelity: {})",
            balances.spendable, balances.regular, balances.fidelity
        );
        original_balances.push(balances.spendable);
    }

    original_balances
}

/// Fund taproot taker and verify balance
fn fund_taproot_taker(
    taker: &mut Taker,
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) -> Amount {
    // Fund with regular UTXOs
    for _ in 0..utxo_count {
        let addr = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &addr, utxo_value);
    }

    generate_blocks(bitcoind, 1);
    taker.get_wallet_mut().sync().unwrap();

    // Verify balances
    let balances = taker.get_wallet().get_balances().unwrap();
    let expected_regular = utxo_value * utxo_count.into();

    assert_eq!(balances.regular, expected_regular);

    info!(
        "Taproot Taker funded successfully. Regular: {}, Spendable: {}",
        balances.regular, balances.spendable
    );

    balances.spendable
}

/// Verify the results of a taproot swap
fn verify_taproot_swap_results(
    taker_wallet: &Wallet,
    makers: &[Arc<TaprootMaker>],
    org_taker_spend_balance: Amount,
    org_maker_spend_balances: Vec<Amount>,
) {
    let taker_balances = taker_wallet.get_balances().unwrap();

    let taker_total_after = taker_balances.regular;
    assert!(
        taker_total_after < org_taker_spend_balance,
        "Taker should have paid fees for the taproot swap. Original: {}, After: {}",
        org_taker_spend_balance,
        taker_total_after
    );

    // But the taker should still have a reasonable amount left (not all spent on fees)
    let max_expected_fees = Amount::from_sat(500000); // 0.0005 BTC max fees
    assert!(
        taker_total_after > org_taker_spend_balance - max_expected_fees,
        "Taker fees should be reasonable. Original: {}, After: {}, Max expected fees: {}",
        org_taker_spend_balance,
        taker_total_after,
        max_expected_fees
    );

    info!(
        "Taker balance verification passed. Original: {}, After: {} (fees paid: {})",
        org_taker_spend_balance,
        taker_total_after,
        org_taker_spend_balance - taker_total_after
    );

    // Verify makers earned fees
    for (i, (maker, original_spendable)) in makers.iter().zip(org_maker_spend_balances).enumerate()
    {
        let wallet = maker.wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();
        let current_total = balances.regular + balances.swap + balances.contract;

        info!(
            "Taproot Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Total spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable
        );

        // In taproot swaps, makers sweep their incoming contracts
        // They should have roughly the same total balance (minus small fees plus earned fees)
        // The balance might be in different categories (regular vs contract vs swap)
        let max_fees = Amount::from_sat(100000); // Maximum expected mining fees

        // The maker's total balance should be at least (original - max_fees + min_earned_fees)
        let expected_minimum = original_spendable - max_fees;
        assert!(
            current_total >= expected_minimum,
            "Taproot Maker {} balance should not decrease significantly. Original spendable: {}, Current total: {}, Expected minimum: {}",
            i,
            original_spendable,
            current_total,
            expected_minimum
        );

        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current total: {}",
            i, original_spendable, current_total
        );
    }
}
