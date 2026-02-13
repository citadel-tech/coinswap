#![cfg(feature = "integration-test")]
//! Integration tests for Unified Taker recovery after funds broadcast.
//!
//! These tests verify that the taker can recover funds when a crash occurs
//! after funding/contract transactions are on-chain but before finalization.

use bitcoin::Amount;
use coinswap::{
    maker::{start_unified_server, UnifiedMakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{UnifiedSwapParams, UnifiedTakerBehavior},
    wallet::AddressType,
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test: Taker drops after Legacy funding txs are broadcast.
///
/// Scenario:
/// 1. Taker initiates a Legacy coinswap with 2 makers.
/// 2. Funding transactions are broadcast and confirmed.
/// 3. Taker "crashes" (DropAfterFundsBroadcast behavior fires).
/// 4. `recover_from_swap()` runs inside `do_coinswap()` — persists swapcoins,
///    attempts timelock recovery (partial, since makers haven't broadcast contracts yet),
///    registers outpoints for monitoring.
/// 5. Makers detect timeout (~60s), broadcast their contract txs, recover via timelock.
/// 6. After blocks mature, verify: taker recovered funds (minus fees), no unfinished swap balance.
#[test]
fn test_unified_legacy_recovery_after_funding_broadcast() {
    // ---- Setup ----
    warn!("Running Test: Unified Legacy Recovery After Funding Broadcast");

    let makers_config_map = vec![(12102, Some(19121)), (22102, Some(19122))];
    let taker_behavior = vec![UnifiedTakerBehavior::DropAfterFundsBroadcast];

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

    // Start the Unified Maker Server threads
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

    // Sync wallets after setup
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let _maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting Legacy recovery test...");

    // Start periodic swap tracker logging (every 10s)
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("unified_taker1"),
        Duration::from_secs(10),
    );

    // Swap params for unified coinswap (Legacy)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Perform the swap — should fail with DropAfterFundsBroadcast
    let swap_result = unified_taker.do_coinswap(swap_params);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to DropAfterFundsBroadcast behavior"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    unified_taker.log_tracker_state();

    // Wait for makers to timeout and broadcast contracts, then blocks mature timelocks.
    // Maker timeout is 60s in tests; block generation thread mines 10 blocks every 3s,
    // so 90s ≈ 300 blocks — more than enough for the 60-block CSV timelock.
    info!("Waiting for makers to timeout and blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(90));

    // Shut down makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Verify maker balances — makers should have recovered their outgoing funds via timelock
    for (i, maker) in unified_makers.iter().enumerate() {
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

    info!("Makers shut down. Retrying timelock recovery...");

    // Mine a block to confirm any pending transactions
    generate_blocks(bitcoind, 1);

    // Retry hashlock recovery (incoming swapcoins) now that makers have
    // broadcast their contracts. The taker has the preimage and can spend
    // the incoming contract outputs via the hashlock path.
    {
        unified_taker
            .get_wallet()
            .write()
            .unwrap()
            .sync_and_save()
            .unwrap();
        match unified_taker.sweep_incoming_swapcoins(2.0) {
            Ok(swept) => info!("Swept {} incoming hashlock transactions", swept.len()),
            Err(e) => warn!("Hashlock sweep retry: {:?}", e),
        }
    }

    // Mine a block to confirm the hashlock sweep tx
    generate_blocks(bitcoind, 1);

    // Retry timelock recovery (outgoing swapcoins) now that CSV has matured.
    // The initial recover_from_swap() correctly broadcast the contract tx but the
    // timelock recovery failed (non-BIP68-final) because CSV wasn't mature yet.
    // After ~300 blocks, the CSV is now satisfied and we can recover.
    {
        unified_taker
            .get_wallet()
            .write()
            .unwrap()
            .sync_and_save()
            .unwrap();
        match unified_taker.recover_timelocked_swapcoins(2.0) {
            Ok(recovered) => info!("Recovered {} timelock transactions", recovered.len()),
            Err(e) => warn!("Timelock recovery retry: {:?}", e),
        }
    }

    // Mine a block to confirm the recovery tx
    generate_blocks(bitcoind, 1);

    // Sync taker wallet to pick up recovery tx confirmations
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    // Verify taker balance
    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

    info!(
        "Taker balances after recovery: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
        taker_balances.regular,
        taker_balances.swap,
        taker_balances.contract,
        taker_balances.spendable,
    );

    // Swap balance may be non-zero: the taker's recover_from_swap() sweeps
    // incoming swapcoins via hashlock immediately after the drop. These swept
    // UTXOs are tracked as "swap" balance. This is correct — the taker claimed
    // its incoming funds before the makers had a chance to recover them.

    // Contract balance should be 0
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "Taker should have no contract balance after recovery"
    );

    // Balance diff should be small (recovery tx fees)
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    // Allow up to ~10000 sats for mining fees (hashlock sweep + timelock recovery)
    assert!(
        balance_diff.to_sat() < 10000,
        "Taker should have recovered most funds. Lost {} sats (expected < 10000)",
        balance_diff.to_sat(),
    );

    unified_taker.log_tracker_state();
    info!("Legacy recovery test completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

/// Test: Taker drops after Taproot contract txs are broadcast.
///
/// Scenario:
/// 1. Taker initiates a Taproot coinswap with 2 makers.
/// 2. Contract transactions are broadcast and confirmed.
/// 3. Taker "crashes" (DropAfterFundsBroadcast behavior fires).
/// 4. `recover_from_swap()` runs — contracts are already on-chain,
///    timelock recovery may succeed if chain height is past locktime.
/// 5. Verify: taker recovered funds, no unfinished swap balance.
#[test]
fn test_unified_taproot_recovery_after_contract_broadcast() {
    // ---- Setup ----
    warn!("Running Test: Unified Taproot Recovery After Contract Broadcast");

    let makers_config_map = vec![(13102, Some(19131)), (23102, Some(19132))];
    let taker_behavior = vec![UnifiedTakerBehavior::DropAfterFundsBroadcast];

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

    // Start the Unified Maker Server threads
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

    // Sync wallets after setup
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting Taproot recovery test...");

    // Start periodic swap tracker logging (every 10s)
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("unified_taker1"),
        Duration::from_secs(10),
    );

    // Swap params for unified coinswap (Taproot)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Perform the swap — should fail with DropAfterFundsBroadcast
    let swap_result = unified_taker.do_coinswap(swap_params);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to DropAfterFundsBroadcast behavior"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    unified_taker.log_tracker_state();

    // Wait for makers to timeout and recover. The maker's IDLE_CONNECTION_TIMEOUT
    // is 60s in tests — after that the connection handler detects the dropped taker,
    // and check_for_idle_states() triggers recovery. Block generation thread mines
    // 10 blocks every 3s, so 90s ≈ 300 blocks — more than enough for timelock maturity.
    info!("Waiting for makers to timeout and blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(90));

    // Shut down makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Verify maker balances — makers should have recovered their outgoing funds
    for (i, maker) in unified_makers.iter().enumerate() {
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

    info!("Makers shut down. Retrying timelock recovery...");

    // Mine a block to confirm any pending transactions
    generate_blocks(bitcoind, 1);

    // Retry hashlock recovery (incoming swapcoins) — for Taproot, the contract
    // tx is already on-chain (broadcast by taker). The taker can spend via hashlock.
    {
        unified_taker
            .get_wallet()
            .write()
            .unwrap()
            .sync_and_save()
            .unwrap();
        match unified_taker.sweep_incoming_swapcoins(2.0) {
            Ok(swept) => info!("Swept {} incoming hashlock transactions", swept.len()),
            Err(e) => warn!("Hashlock sweep retry: {:?}", e),
        }
    }

    // Mine a block to confirm the hashlock sweep tx
    generate_blocks(bitcoind, 1);

    // Retry timelock recovery now that the timelock has matured.
    // The initial recover_from_swap() may have failed if the timelock wasn't
    // mature yet. After ~100 blocks, it should be satisfied.
    {
        unified_taker
            .get_wallet()
            .write()
            .unwrap()
            .sync_and_save()
            .unwrap();
        match unified_taker.recover_timelocked_swapcoins(2.0) {
            Ok(recovered) => info!("Recovered {} timelock transactions", recovered.len()),
            Err(e) => warn!("Timelock recovery retry: {:?}", e),
        }
    }

    // Mine a block to confirm the recovery tx
    generate_blocks(bitcoind, 1);

    // Sync taker wallet to pick up recovery tx confirmations
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    // Verify taker balance
    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

    info!(
        "Taker balances after recovery: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
        taker_balances.regular,
        taker_balances.swap,
        taker_balances.contract,
        taker_balances.spendable,
    );

    // Swap balance may be non-zero: the taker's recover_from_swap() sweeps
    // incoming swapcoins via hashlock immediately after the drop. These swept
    // UTXOs are tracked as "swap" balance. This is correct — the taker claimed
    // its incoming funds before the makers had a chance to recover them.

    // Contract balance should be 0
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "Taker should have no contract balance after recovery"
    );

    // Balance diff should be small (recovery tx fees)
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    // Allow up to ~10000 sats for mining fees (hashlock sweep + timelock recovery)
    assert!(
        balance_diff.to_sat() < 10000,
        "Taker should have recovered most funds. Lost {} sats (expected < 10000)",
        balance_diff.to_sat(),
    );

    // Verify maker balances are close to pre-swap (post-fidelity) balances
    for (i, maker) in unified_makers.iter().enumerate() {
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

    unified_taker.log_tracker_state();
    info!("Taproot recovery test completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

/// Test: Timelock-only recovery when last maker skips funding broadcast.
///
/// Route: Taker → Maker1 → Maker2 → Taker
///
/// Scenario:
/// 1. Maker2 (last hop) receives contract sigs and saves swapcoins, but
///    skips broadcasting its outgoing funding transaction and closes the connection.
/// 2. Taker gets a connection error, calls `recover_from_swap()`.
/// 3. Hashlock recovery fails because Maker2's funding is not on-chain,
///    so the contract tx can't be broadcast.
/// 4. Everyone falls back to timelock recovery:
///    - Taker recovers outgoing (to Maker1) via timelock.
///    - Maker1 recovers outgoing (to Maker2) via timelock.
///    - Maker2 has nothing to recover (outgoing was never broadcast).
#[test]
fn test_unified_legacy_timelock_only_recovery() {
    // ---- Setup ----
    warn!("Running Test: Unified Legacy Timelock-Only Recovery");

    let makers_config_map = vec![(15102, Some(19151)), (25102, Some(19152))];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];
    let maker_behaviors = vec![
        UnifiedMakerBehavior::Normal,
        UnifiedMakerBehavior::SkipFundingBroadcast,
    ];

    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, maker_behaviors);

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

    // Start the Unified Maker Server threads
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

    // Sync wallets after setup
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Use post-fidelity, pre-swap balances as the correct baseline
    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting Legacy timelock-only recovery test...");

    // Start periodic swap tracker logging (every 10s)
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("unified_taker1"),
        Duration::from_secs(10),
    );

    // Swap params for unified coinswap (Legacy)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Perform the swap — should fail because Maker2 closes the connection
    let swap_result = unified_taker.do_coinswap(swap_params);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to Maker2 skipping funding broadcast"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    unified_taker.log_tracker_state();

    // Wait for timelocks to mature. Maker timeout is 60s in tests;
    // block generation thread mines 10 blocks every 3s, so 90s ~ 300 blocks.
    info!("Waiting for makers to timeout and blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(90));

    // Shut down makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Verify maker balances after recovery
    for (i, maker) in unified_makers.iter().enumerate() {
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

    info!("Makers shut down. Running taker timelock recovery...");

    // Mine a block to confirm any pending transactions
    generate_blocks(bitcoind, 1);

    // Sync taker wallet
    {
        let mut wallet = unified_taker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // Timelock recovery for outgoing swapcoins
    match unified_taker.recover_timelocked_swapcoins(2.0) {
        Ok(recovered) => info!("Recovered {} timelock transactions", recovered.len()),
        Err(e) => warn!("Timelock recovery: {:?}", e),
    }

    // Mine a block to confirm the recovery tx
    generate_blocks(bitcoind, 1);

    // Sync taker wallet to pick up recovery tx confirmations
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    // Verify taker balance
    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

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
    for (i, maker) in unified_makers.iter().enumerate() {
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

    unified_taker.log_tracker_state();
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
/// 2. Taker gets a connection error, calls `recover_from_swap()`.
/// 3. Hashlock recovery fails because Maker2's funding is not on-chain,
///    so the contract tx can't be broadcast.
/// 4. Everyone falls back to timelock recovery:
///    - Taker recovers outgoing (to Maker1) via timelock.
///    - Maker1 recovers outgoing (to Maker2) via timelock.
///    - Maker2 has nothing to recover (outgoing was never broadcast).
#[test]
fn test_unified_taproot_timelock_only_recovery() {
    // ---- Setup ----
    warn!("Running Test: Unified Taproot Timelock-Only Recovery");

    let makers_config_map = vec![(16102, Some(19161)), (26102, Some(19162))];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];
    let maker_behaviors = vec![
        UnifiedMakerBehavior::Normal,
        UnifiedMakerBehavior::SkipFundingBroadcast,
    ];

    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, maker_behaviors);

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

    // Start the Unified Maker Server threads
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

    // Sync wallets after setup
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Use post-fidelity, pre-swap balances as the correct baseline
    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting Taproot timelock-only recovery test...");

    // Start periodic swap tracker logging (every 10s)
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("unified_taker1"),
        Duration::from_secs(10),
    );

    // Swap params for unified coinswap (Taproot)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Perform the swap — should fail because Maker2 closes the connection
    let swap_result = unified_taker.do_coinswap(swap_params);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to Maker2 skipping funding broadcast"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    unified_taker.log_tracker_state();

    // Wait for timelocks to mature. Maker timeout is 60s in tests;
    // block generation thread mines 10 blocks every 3s, so 90s ~ 300 blocks.
    info!("Waiting for makers to timeout and blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(90));

    // Shut down makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Verify maker balances after recovery
    for (i, maker) in unified_makers.iter().enumerate() {
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

    info!("Makers shut down. Running taker timelock recovery...");

    // Mine a block to confirm any pending transactions
    generate_blocks(bitcoind, 1);

    // Sync taker wallet
    {
        let mut wallet = unified_taker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // First recovery attempt: broadcasts contract/funding txs on-chain.
    // The CSV relative timelock may not be mature yet, so recovery txs
    // might fail with "non-BIP68-final". That's expected.
    match unified_taker.recover_timelocked_swapcoins(2.0) {
        Ok(recovered) => info!(
            "First attempt: recovered {} timelock transactions",
            recovered.len()
        ),
        Err(e) => warn!("First recovery attempt (expected): {:?}", e),
    }

    // Mine enough blocks to mature the CSV relative timelock.
    // The contract uses ~20 block CSV; mine 70 to be safe.
    info!("Mining 70 blocks to mature CSV timelocks...");
    generate_blocks(bitcoind, 70);

    // Retry recovery now that CSV is mature
    {
        unified_taker
            .get_wallet()
            .write()
            .unwrap()
            .sync_and_save()
            .unwrap();
        match unified_taker.recover_timelocked_swapcoins(2.0) {
            Ok(recovered) => info!("Retry: recovered {} timelock transactions", recovered.len()),
            Err(e) => warn!("Retry recovery: {:?}", e),
        }
    }

    // Mine a block to confirm the recovery tx
    generate_blocks(bitcoind, 1);

    // Sync taker wallet to pick up recovery tx confirmations
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    // Verify taker balance
    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

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
    for (i, maker) in unified_makers.iter().enumerate() {
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

    unified_taker.log_tracker_state();
    info!("Taproot timelock-only recovery test completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
