//! Integration test for Unified Coinswap Protocol implementation

use bitcoin::Amount;
use coinswap::{
    maker::start_unified_server,
    protocol::common_messages::ProtocolVersion,
    taker::{UnifiedSwapParams, UnifiedTakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test unified coinswap with Legacy (ECDSA) protocol
#[test]
fn test_unified_coinswap_legacy() {
    // ---- Setup ----
    warn!("Running Test: Unified Coinswap with Legacy (ECDSA) Protocol");

    let makers_config_map = vec![(8102, Some(19081)), (18102, Some(19082))];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];

    // Initialize test framework
    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, vec![]);

    let bitcoind = &test_framework.bitcoind;
    let unified_taker = unified_takers.get_mut(0).unwrap();

    // Fund the Unified Taker with 3 UTXOs of 0.05 BTC each (P2WPKH for Legacy)
    let taker_original_balance = fund_unified_taker(
        unified_taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Fund the Unified Makers with 4 UTXOs of 0.05 BTC each
    fund_unified_makers(
        &unified_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Start the Unified Maker Server threads (uses unified message handlers)
    log::info!("Initiating Unified Makers with unified server...");

    let maker_threads = unified_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_unified_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for makers to complete setup
    for maker in &unified_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            log::info!("Waiting for unified maker setup completion");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting end-to-end unified swap test with Legacy protocol...");

    // Swap params for unified coinswap (Legacy)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Perform the swap
    match unified_taker.do_coinswap(swap_params) {
        Ok(report) => {
            log::info!("Unified coinswap (Legacy) completed successfully!");
            log::info!("Swap report: {:?}", report);
        }
        Err(e) => {
            log::error!("Unified coinswap (Legacy) failed: {:?}", e);
            panic!("Unified coinswap (Legacy) failed: {:?}", e);
        }
    }

    // After swap, shutdown maker threads
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All unified coinswaps processed successfully. Transaction complete.");

    // Sync wallets and verify results
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each maker's wallet
    for maker in unified_makers.iter() {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    let taker_balances_after = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();
    info!("Unified Taker balance after completing swap (Legacy):");
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
    );

    // Verify swap results - Taker should have spent some amount in fees
    let taker_total_after = taker_balances_after.spendable;
    assert!(
        taker_total_after < taker_original_balance,
        "Taker should have spent fees. Original: {}, After: {}",
        taker_original_balance,
        taker_total_after
    );

    let balance_diff = taker_original_balance - taker_total_after;
    info!(
        "Unified Taker (Legacy) balance verification passed. Original spendable: {}, After spendable: {} (fees paid: {})",
        taker_original_balance,
        taker_total_after,
        balance_diff
    );

    // Verify makers earned fees
    for (i, (maker, original_spendable)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Unified Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,
        );

        // Makers should have gained some balance from fees
        assert!(
            balances.spendable >= original_spendable,
            "Unified Maker {} should have gained fees. Original: {}, After: {}",
            i,
            original_spendable,
            balances.spendable
        );

        info!(
            "Unified Maker {} balance verification passed. Original spendable: {}, Current spendable: {}",
            i, original_spendable, balances.spendable
        );
    }

    info!("All unified swap tests (Legacy) completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

/// Test unified coinswap with Taproot (MuSig2) protocol
#[test]
fn test_unified_coinswap_taproot() {
    // ---- Setup ----
    warn!("Running Test: Unified Coinswap with Taproot (MuSig2) Protocol");

    let makers_config_map = vec![(9102, Some(19091)), (19102, Some(19092))];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];

    // Initialize test framework
    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, vec![]);

    let bitcoind = &test_framework.bitcoind;
    let unified_taker = unified_takers.get_mut(0).unwrap();

    // Fund the Unified Taker with 3 UTXOs of 0.05 BTC each (P2TR for Taproot)
    let taker_original_balance = fund_unified_taker(
        unified_taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Fund the Unified Makers with 4 UTXOs of 0.05 BTC each
    fund_unified_makers(
        &unified_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Start the Unified Maker Server threads (uses unified message handlers)
    log::info!("Initiating Unified Makers with unified server...");

    let maker_threads = unified_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_unified_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for makers to complete setup
    for maker in &unified_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            log::info!("Waiting for unified maker setup completion");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting end-to-end unified swap test with Taproot protocol...");

    // Swap params for unified coinswap (Taproot)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Perform the swap
    match unified_taker.do_coinswap(swap_params) {
        Ok(report) => {
            log::info!("Unified coinswap (Taproot) completed successfully!");
            log::info!("Swap report: {:?}", report);
        }
        Err(e) => {
            log::error!("Unified coinswap (Taproot) failed: {:?}", e);
            panic!("Unified coinswap (Taproot) failed: {:?}", e);
        }
    }

    // After swap, shutdown maker threads
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All unified coinswaps processed successfully. Transaction complete.");

    // Sync wallets and verify results
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each maker's wallet
    for maker in unified_makers.iter() {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    let taker_balances_after = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();
    info!("Unified Taker balance after completing swap (Taproot):");
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
    );

    // Verify swap results - Taker should have spent some amount in fees
    let taker_total_after = taker_balances_after.spendable;
    assert!(
        taker_total_after < taker_original_balance,
        "Taker should have spent fees. Original: {}, After: {}",
        taker_original_balance,
        taker_total_after
    );

    let balance_diff = taker_original_balance - taker_total_after;
    info!(
        "Unified Taker (Taproot) balance verification passed. Original spendable: {}, After spendable: {} (fees paid: {})",
        taker_original_balance,
        taker_total_after,
        balance_diff
    );

    // Verify makers earned fees
    for (i, (maker, original_spendable)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Unified Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,
        );

        // Makers should have gained some balance from fees
        assert!(
            balances.spendable >= original_spendable,
            "Unified Maker {} should have gained fees. Original: {}, After: {}",
            i,
            original_spendable,
            balances.spendable
        );

        info!(
            "Unified Maker {} balance verification passed. Original spendable: {}, Current spendable: {}",
            i, original_spendable, balances.spendable
        );
    }

    info!("All unified swap tests (Taproot) completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
