//! Integration tests for Taker timelock-only recovery.
//!
//! These tests verify recovery when the last maker skips broadcasting
//! its funding transaction, forcing timelock-only recovery (hashlock
//! recovery is impossible since the funding is never on-chain).

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{
    sync::atomic::Ordering::Relaxed,
    thread,
    time::{Duration, Instant},
};

/// Test: Timelock-only recovery when last maker skips funding broadcast.
///
/// Route: Taker → Maker1 → Maker2 → Taker
///
/// Scenario:
/// 1. Maker2 (last hop) receives contract sigs and saves swapcoins, but
///    skips broadcasting its outgoing funding transaction and closes the connection.
/// 2. Taker gets a connection error, calls `recover_active_swap()`.
/// 3. Hashlock recovery fails because Maker2's funding is not on-chain,
///    so the contract tx can't be broadcast.
/// 4. Everyone falls back to timelock recovery:
///    - Taker recovers outgoing (to Maker1) via timelock.
///    - Maker1 recovers outgoing (to Maker2) via timelock.
///    - Maker2 has nothing to recover (outgoing was never broadcast).
#[test]
fn test_legacy_timelock_only_recovery() {
    // ---- Setup ----
    warn!("Running Test: Legacy Timelock-Only Recovery");

    let makers_config_map = vec![(15102, Some(19151)), (25102, Some(19152))];
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::SkipFundingBroadcast];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();

    // Fund the taker with 3 UTXOs of 0.05 BTC each (P2WPKH for Legacy)
    let taker_original_balance = fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Fund the makers with 4 UTXOs of 0.05 BTC each
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Start the maker server threads
    log::info!("Starting Maker servers...");

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for makers to complete setup
    wait_for_makers_setup(&makers, 120);

    // Sync wallets after setup
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Use post-fidelity, pre-swap balances as the correct baseline
    let maker_spendable_balance = verify_maker_pre_swap_balances(&makers);
    log::info!("Starting Legacy timelock-only recovery test...");

    // Start periodic swap tracker logging (every 10s)
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("taker1"),
        Duration::from_secs(10),
    );

    // Swap params for coinswap (Legacy)
    let swap_params = SwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Prepare should succeed; execution should fail because Maker2 closes the connection
    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("Prepare should succeed");
    let swap_result = taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to Maker2 skipping funding broadcast"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    taker.log_tracker_state();

    // Wait for timelocks to mature. Maker timeout is 60s in tests;
    // block generation thread mines 10 blocks every 3s, so 150s ~ 500 blocks.
    info!("Waiting for makers to timeout and blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(150));

    // Shut down makers
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Verify maker balances after recovery
    for (i, maker) in makers.iter().enumerate() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let maker_balances = maker.wallet.read().unwrap().get_balances().unwrap();
        info!(
            "Maker {} balances after recovery: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
            i,
            maker_balances.regular,
            maker_balances.swap,
            maker_balances.contract,
            maker_balances.spendable,
        );
        assert_eq!(
            maker_balances.contract,
            Amount::ZERO,
            "Maker {} should have no contract balance after recovery",
            i
        );
    }

    info!("Makers shut down. Waiting for background recovery loop to complete...");

    // The background recovery loop (spawned by recover_active_swap) periodically
    // retries timelock recovery. Wait for it to finish.
    let recovery_timeout = Duration::from_secs(120);
    let recovery_start = Instant::now();
    while !taker.is_recovery_complete() {
        if recovery_start.elapsed() > recovery_timeout {
            panic!("Background recovery did not complete within timeout");
        }
        thread::sleep(Duration::from_secs(5));
    }
    info!("Background recovery loop completed.");

    // Mine a block to confirm recovery txs, then sync wallet
    generate_blocks(bitcoind, 1);
    taker.get_wallet().write().unwrap().sync_and_save().unwrap();

    // Verify taker balance
    let taker_balances = taker.get_wallet().read().unwrap().get_balances().unwrap();

    info!(
        "Taker balances after recovery: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
        taker_balances.regular,
        taker_balances.swap,
        taker_balances.contract,
        taker_balances.spendable,
    );

    // Contract balance should be 0
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "Taker should have no contract balance after recovery"
    );

    // Balance diff should be small (timelock recovery fees only)
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    assert!(
        balance_diff.to_sat() < 10000,
        "Taker should have recovered most funds. Lost {} sats (expected < 10000)",
        balance_diff.to_sat(),
    );

    // Verify maker balances are close to pre-swap (post-fidelity) balances
    for (i, maker) in makers.iter().enumerate() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let maker_balances = maker.wallet.read().unwrap().get_balances().unwrap();
        let original = maker_spendable_balance[i];

        let maker_diff = original
            .checked_sub(maker_balances.spendable)
            .unwrap_or(Amount::ZERO);

        info!(
            "Maker {} balance diff: {} sats (pre-swap: {}, current: {})",
            i,
            maker_diff.to_sat(),
            original,
            maker_balances.spendable,
        );

        // Makers should recover most of their funds (small loss from tx fees)
        assert!(
            maker_diff.to_sat() < 10000,
            "Maker {} should have recovered most funds. Lost {} sats (expected < 10000)",
            i,
            maker_diff.to_sat(),
        );
    }

    taker.log_tracker_state();
    info!("Legacy timelock-only recovery test completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

/// Test: Timelock-only recovery when last maker skips Taproot funding broadcast.
///
/// Route: Taker → Maker1 → Maker2 → Taker
///
/// Scenario:
/// 1. Maker2 (last hop) creates Taproot contract and saves swapcoins, but
///    skips broadcasting its outgoing funding transaction and closes the connection.
/// 2. Taker gets a connection error, calls `recover_active_swap()`.
/// 3. Hashlock recovery fails because Maker2's funding is not on-chain,
///    so the contract tx can't be broadcast.
/// 4. Everyone falls back to timelock recovery:
///    - Taker recovers outgoing (to Maker1) via timelock.
///    - Maker1 recovers outgoing (to Maker2) via timelock.
///    - Maker2 has nothing to recover (outgoing was never broadcast).
#[test]
fn test_taproot_timelock_only_recovery() {
    // ---- Setup ----
    warn!("Running Test: Taproot Timelock-Only Recovery");

    let makers_config_map = vec![(16102, Some(19161)), (26102, Some(19162))];
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::SkipFundingBroadcast];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();

    // Fund the taker with 3 UTXOs of 0.05 BTC each (P2TR for Taproot)
    let taker_original_balance = fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Fund the makers with 4 UTXOs of 0.05 BTC each
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Start the maker server threads
    log::info!("Starting Maker servers...");

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for makers to complete setup
    wait_for_makers_setup(&makers, 120);

    // Sync wallets after setup
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Use post-fidelity, pre-swap balances as the correct baseline
    let maker_spendable_balance = verify_maker_pre_swap_balances(&makers);
    log::info!("Starting Taproot timelock-only recovery test...");

    // Start periodic swap tracker logging (every 10s)
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("taker1"),
        Duration::from_secs(10),
    );

    // Swap params for coinswap (Taproot)
    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Prepare should succeed; execution should fail because Maker2 closes the connection
    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("Prepare should succeed");
    let swap_result = taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to Maker2 skipping funding broadcast"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    taker.log_tracker_state();

    // Wait for timelocks to mature. Maker timeout is 60s in tests;
    // block generation thread mines 10 blocks every 3s, so 150s ~ 500 blocks.
    info!("Waiting for makers to timeout and blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(150));

    // Shut down makers
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Verify maker balances after recovery
    for (i, maker) in makers.iter().enumerate() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let maker_balances = maker.wallet.read().unwrap().get_balances().unwrap();
        info!(
            "Maker {} balances after recovery: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
            i,
            maker_balances.regular,
            maker_balances.swap,
            maker_balances.contract,
            maker_balances.spendable,
        );
        assert_eq!(
            maker_balances.contract,
            Amount::ZERO,
            "Maker {} should have no contract balance after recovery",
            i
        );
    }

    info!("Makers shut down. Waiting for background recovery loop to complete...");

    // The background recovery loop (spawned by recover_active_swap) periodically
    // retries timelock recovery. The block generation thread mines ~10 blocks/3s,
    // so CSV timelocks (~20 blocks) will mature during the wait.
    let recovery_timeout = Duration::from_secs(120);
    let recovery_start = Instant::now();
    while !taker.is_recovery_complete() {
        if recovery_start.elapsed() > recovery_timeout {
            panic!("Background recovery did not complete within timeout");
        }
        thread::sleep(Duration::from_secs(5));
    }
    info!("Background recovery loop completed.");

    // Mine a block to confirm recovery txs, then sync wallet
    generate_blocks(bitcoind, 1);
    taker.get_wallet().write().unwrap().sync_and_save().unwrap();

    // Verify taker balance
    let taker_balances = taker.get_wallet().read().unwrap().get_balances().unwrap();

    info!(
        "Taker balances after recovery: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
        taker_balances.regular,
        taker_balances.swap,
        taker_balances.contract,
        taker_balances.spendable,
    );

    // Contract balance should be 0
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "Taker should have no contract balance after recovery"
    );

    // Balance diff should be small (timelock recovery fees only)
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    assert!(
        balance_diff.to_sat() < 10000,
        "Taker should have recovered most funds. Lost {} sats (expected < 10000)",
        balance_diff.to_sat(),
    );

    // Verify maker balances are close to pre-swap (post-fidelity) balances
    for (i, maker) in makers.iter().enumerate() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let maker_balances = maker.wallet.read().unwrap().get_balances().unwrap();
        let original = maker_spendable_balance[i];

        let maker_diff = original
            .checked_sub(maker_balances.spendable)
            .unwrap_or(Amount::ZERO);

        info!(
            "Maker {} balance diff: {} sats (pre-swap: {}, current: {})",
            i,
            maker_diff.to_sat(),
            original,
            maker_balances.spendable,
        );

        // Makers should recover most of their funds (small loss from tx fees)
        assert!(
            maker_diff.to_sat() < 10000,
            "Maker {} should have recovered most funds. Lost {} sats (expected < 10000)",
            i,
            maker_diff.to_sat(),
        );
    }

    taker.log_tracker_state();
    info!("Taproot timelock-only recovery test completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
