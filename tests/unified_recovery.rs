#![cfg(feature = "integration-test")]
//! Integration tests for Unified Taker recovery after funds broadcast.
//!
//! These tests verify that the taker can recover funds when a crash occurs
//! after funding/contract transactions are on-chain but before finalization.

use bitcoin::Amount;
use coinswap::{
    maker::start_unified_server,
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
        TestFramework::init_unified(makers_config_map, taker_behavior);

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

    info!("Makers shut down. Retrying timelock recovery...");

    // Mine a block to confirm any pending transactions
    generate_blocks(bitcoind, 1);

    // Retry timelock recovery now that CSV has matured.
    // The initial recover_from_swap() correctly broadcast the contract tx but the
    // timelock recovery failed (non-BIP68-final) because CSV wasn't mature yet.
    // After ~300 blocks, the CSV is now satisfied and we can recover.
    {
        let mut wallet = unified_taker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
        match wallet.recover_unified_timelocked_swapcoins(2.0) {
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

    // Swap balance should be 0 (no unfinished swaps)
    assert_eq!(
        taker_balances.swap,
        Amount::ZERO,
        "Taker should have no swap balance after recovery"
    );

    // Contract balance should be 0
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "Taker should have no contract balance after recovery"
    );

    // Balance diff should be small (just timelock recovery fee ~858 sats)
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    // Allow up to ~5000 sats for mining fees (timelock recovery tx fees)
    assert!(
        balance_diff.to_sat() < 5000,
        "Taker should have recovered most funds. Lost {} sats (expected < 5000)",
        balance_diff.to_sat(),
    );

    info!("Legacy recovery test completed successfully!");

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
        TestFramework::init_unified(makers_config_map, taker_behavior);

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

    let _maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting Taproot recovery test...");

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

    // For Taproot, contracts are already on-chain (broadcast by taker).
    // Wait for blocks to mature timelocks. Block generation thread mines 10 blocks
    // every 3s, so 30s ≈ 100 blocks — enough for the 60-block timelock.
    info!("Waiting for blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(30));

    // Shut down makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    info!("Makers shut down. Retrying timelock recovery...");

    // Mine a block to confirm any pending transactions
    generate_blocks(bitcoind, 1);

    // Retry timelock recovery now that the timelock has matured.
    // The initial recover_from_swap() may have failed if the timelock wasn't
    // mature yet. After ~100 blocks, it should be satisfied.
    {
        let mut wallet = unified_taker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
        match wallet.recover_unified_timelocked_swapcoins(2.0) {
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

    // Swap balance should be 0 (no unfinished swaps)
    assert_eq!(
        taker_balances.swap,
        Amount::ZERO,
        "Taker should have no swap balance after recovery"
    );

    // Contract balance should be 0
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "Taker should have no contract balance after recovery"
    );

    // Balance diff should be small (just timelock recovery fee)
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    // Allow up to ~5000 sats for mining fees (timelock recovery tx fees)
    assert!(
        balance_diff.to_sat() < 5000,
        "Taker should have recovered most funds. Lost {} sats (expected < 5000)",
        balance_diff.to_sat(),
    );

    info!("Taproot recovery test completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
