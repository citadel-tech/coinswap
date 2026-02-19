#![cfg(feature = "integration-test")]
//! Integration tests for automatic fidelity bond renewal.
//!
//! These tests verify that both v1 (legacy/P2WSH) and v2 (taproot) protocols
//! automatically renew fidelity bonds when they expire while the server is running.

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::{start_maker_server, start_maker_server_taproot, MakerBehavior, TaprootMakerBehavior},
    taker::TakerBehavior,
};

mod test_framework;
use test_framework::*;

use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test automatic fidelity bond renewal for v1 (legacy/P2WSH) protocol
#[test]
fn test_fidelity_auto_renewal_legacy() {
    // ---- Setup ----
    let makers_config_map = [((6102, None), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers) = TestFramework::init(makers_config_map.into(), taker_behavior);

    log::info!("Running Test: Fidelity Bond Auto-Renewal (Legacy/P2WSH)");

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    // Fund the maker
    let makers_ref = makers
        .iter()
        .map(std::sync::Arc::as_ref)
        .collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 4, Amount::from_btc(0.20).unwrap());

    // Start the maker server
    let maker_clone = maker.clone();
    let maker_thread = thread::spawn(move || start_maker_server(maker_clone));

    // Wait for setup to complete
    while !maker.is_setup_complete.load(Relaxed) {
        log::info!("Waiting for maker setup completion...");
        thread::sleep(Duration::from_secs(5));
    }

    // Verify initial bond was created and get its locktime
    let (initial_bond_index, bond_locktime) = {
        maker.get_wallet().write().unwrap().sync_and_save().unwrap();
        let wallet_read = maker.get_wallet().read().unwrap();

        let highest_index = wallet_read.get_highest_fidelity_index().unwrap();
        assert!(
            highest_index.is_some(),
            "Initial fidelity bond should be created"
        );

        let idx = highest_index.unwrap();
        let bond = wallet_read.get_fidelity_bonds().get(&idx).unwrap();
        let locktime = bond.lock_time.to_consensus_u32();

        log::info!(
            "Initial bond created - Index: {}, Amount: {} sats, Locktime: {} blocks",
            idx,
            bond.amount.to_sat(),
            locktime
        );

        (idx, locktime)
    };

    // Calculate blocks to mine to expire the bond
    let current_height = bitcoind.client.get_block_count().unwrap() as u32;
    let blocks_to_mine = if bond_locktime > current_height {
        (bond_locktime - current_height) + 10
    } else {
        10
    };

    log::info!(
        "Mining blocks to expire bond. Current: {}, Bond locktime: {}, Blocks to mine: {}",
        current_height,
        bond_locktime,
        blocks_to_mine
    );

    // Mine blocks to expire the bond (in batches to avoid RPC timeout)
    let address = bitcoind
        .client
        .get_new_address(None, None)
        .unwrap()
        .require_network(bitcoin::Network::Regtest)
        .unwrap();

    let batch_size = 100u64;
    let mut remaining = blocks_to_mine as u64;
    while remaining > 0 {
        let to_mine = std::cmp::min(remaining, batch_size);
        bitcoind
            .client
            .generate_to_address(to_mine, &address)
            .unwrap();
        remaining -= to_mine;
        if remaining > 0 {
            log::info!("Mined {} blocks, {} remaining...", to_mine, remaining);
        }
    }

    let new_height = bitcoind.client.get_block_count().unwrap();
    log::info!(
        "Mined {} blocks. New height: {}",
        blocks_to_mine,
        new_height
    );

    log::info!("Waiting for automatic fidelity bond renewal (up to 90 seconds)...");

    let mut renewal_detected = false;
    for i in 0..18 {
        thread::sleep(Duration::from_secs(5));

        maker.get_wallet().write().unwrap().sync_and_save().unwrap();
        let wallet_read = maker.get_wallet().read().unwrap();

        // Check if original bond was redeemed (marked as spent)
        let original_bond = wallet_read
            .get_fidelity_bonds()
            .get(&initial_bond_index)
            .unwrap();
        let original_spent = original_bond.is_spent();

        // Check if there's a new highest bond with different index
        let highest_index = wallet_read.get_highest_fidelity_index().unwrap();
        let new_bond_created = highest_index
            .map(|idx| idx == initial_bond_index + 1)
            .unwrap_or(false);

        if original_spent && new_bond_created {
            log::info!(
                "Fidelity bond renewal detected at iteration {}! Original spent: {}, New bond index: {:?}",
                i,
                original_spent,
                highest_index
            );
            renewal_detected = true;
            break;
        }

        log::info!("Check {}/18 - Bond not yet renewed", i + 1);
    }

    assert!(
        renewal_detected,
        "Fidelity bond should have been automatically renewed"
    );

    let log_file = test_framework.temp_dir.join("taker/debug.log");
    let log_path = log_file.to_str().unwrap();
    test_framework.assert_log(
        "Fidelity Bond at index: 0 expired | Redeeming it.",
        log_path,
    );
    test_framework.assert_log("Successfully created fidelity bond", log_path);

    log::info!("Fidelity bond auto-renewal test (legacy) completed successfully");
}

/// Test automatic fidelity bond renewal for v2 (taproot) protocol
#[test]
fn test_fidelity_auto_renewal_taproot() {
    // ---- Setup ----
    log::info!("Running Test: Fidelity Bond Auto-Renewal (Taproot)");

    let taproot_makers_config_map = vec![(7102, Some(19061), TaprootMakerBehavior::Normal)];
    let taker_behavior = vec![coinswap::taker::api2::TakerBehavior::Normal];

    let (test_framework, _, taproot_makers) =
        TestFramework::init_taproot(taproot_makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let maker = taproot_makers.first().unwrap();

    // Fund the maker
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.20).unwrap(),
    );

    // Start the maker server
    let maker_clone = maker.clone();
    let maker_thread = thread::spawn(move || start_maker_server_taproot(maker_clone));

    // Wait for setup to complete
    while !maker.is_setup_complete.load(Relaxed) {
        log::info!("Waiting for taproot maker setup completion...");
        thread::sleep(Duration::from_secs(5));
    }

    // Verify initial bond was created and get its locktime
    let (initial_bond_index, bond_locktime) = {
        maker.wallet().write().unwrap().sync_and_save().unwrap();
        let wallet_read = maker.wallet().read().unwrap();

        let highest_index = wallet_read.get_highest_fidelity_index().unwrap();
        assert!(
            highest_index.is_some(),
            "Initial fidelity bond should be created"
        );

        let idx = highest_index.unwrap();
        let bond = wallet_read.get_fidelity_bonds().get(&idx).unwrap();
        let locktime = bond.lock_time.to_consensus_u32();

        log::info!(
            "Initial taproot bond created - Index: {}, Amount: {} sats, Locktime: {} blocks",
            idx,
            bond.amount.to_sat(),
            locktime
        );

        (idx, locktime)
    };

    // Calculate blocks to mine to expire the bond
    let current_height = bitcoind.client.get_block_count().unwrap() as u32;
    let blocks_to_mine = if bond_locktime > current_height {
        (bond_locktime - current_height) + 10
    } else {
        10
    };

    log::info!(
        "Mining blocks to expire bond. Current: {}, Bond locktime: {}, Blocks to mine: {}",
        current_height,
        bond_locktime,
        blocks_to_mine
    );

    // Mine blocks to expire the bond (in batches to avoid RPC timeout)
    let address = bitcoind
        .client
        .get_new_address(None, None)
        .unwrap()
        .require_network(bitcoin::Network::Regtest)
        .unwrap();

    let batch_size = 100u64;
    let mut remaining = blocks_to_mine as u64;
    while remaining > 0 {
        let to_mine = std::cmp::min(remaining, batch_size);
        bitcoind
            .client
            .generate_to_address(to_mine, &address)
            .unwrap();
        remaining -= to_mine;
        if remaining > 0 {
            log::info!("Mined {} blocks, {} remaining...", to_mine, remaining);
        }
    }

    let new_height = bitcoind.client.get_block_count().unwrap();
    log::info!(
        "Mined {} blocks. New height: {}",
        blocks_to_mine,
        new_height
    );

    log::info!("Waiting for automatic fidelity bond renewal (up to 90 seconds)...");

    let mut renewal_detected = false;
    for i in 0..18 {
        thread::sleep(Duration::from_secs(5));

        maker.wallet().write().unwrap().sync_and_save().unwrap();
        let wallet_read = maker.wallet().read().unwrap();

        // Check if original bond was redeemed (marked as spent)
        let original_bond = wallet_read
            .get_fidelity_bonds()
            .get(&initial_bond_index)
            .unwrap();
        let original_spent = original_bond.is_spent();

        // Check if there's a new highest bond with different index
        let highest_index = wallet_read.get_highest_fidelity_index().unwrap();
        let new_bond_created = highest_index
            .map(|idx| idx == initial_bond_index + 1)
            .unwrap_or(false);

        if original_spent && new_bond_created {
            log::info!(
                "Taproot fidelity bond renewal detected at iteration {}! Original spent: {}, New bond index: {:?}",
                i,
                original_spent,
                highest_index
            );
            renewal_detected = true;
            break;
        }

        log::info!("Check {}/18 - Taproot bond not yet renewed", i + 1);
    }

    assert!(
        renewal_detected,
        "Taproot fidelity bond should have been automatically renewed"
    );

    let log_file = test_framework.temp_dir.join("taker/debug.log");
    let log_path = log_file.to_str().unwrap();
    test_framework.assert_log(
        "Fidelity Bond at index: 0 expired | Redeeming it.",
        log_path,
    );
    test_framework.assert_log("Successfully created fidelity bond", log_path);

    // shutdown
    maker.shutdown.store(true, Relaxed);
    let _ = maker_thread.join();

    log::info!("Fidelity bond auto-renewal test (taproot) completed successfully");
}
