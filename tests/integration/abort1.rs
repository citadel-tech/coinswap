//! Abort 1: TAKER Drops After Full Setup.
//!
//! The Taker drops the connection after broadcasting all the funding transactions.
//! The Makers identify this and wait for a timeout (60s in test) for the Taker to come back.
//! If the Taker doesn't return, the Makers broadcast the contract transactions and reclaim
//! their funds via timelock.
//!
//! The Taker after coming live again will see unfinished coinswaps in its wallet.
//! It can reclaim funds via broadcasting contract transactions and claiming via timelock.

use bitcoin::Amount;
use coinswap::{
    maker::{start_unified_server, UnifiedMakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{UnifiedSwapParams, UnifiedTakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{
    sync::atomic::Ordering::Relaxed,
    thread,
    time::{Duration, Instant},
};

#[test]
fn taker_abort1() {
    // ---- Setup ----
    warn!("Running Test: Taker Drops After Full Setup");

    let makers_config_map = vec![(6102, None), (16102, None)];
    let taker_behavior = vec![UnifiedTakerBehavior::DropAfterFundsBroadcast];
    let maker_behaviors = vec![UnifiedMakerBehavior::Normal, UnifiedMakerBehavior::Normal];

    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let unified_taker = unified_takers.get_mut(0).unwrap();

    // Fund the Taker with 3 UTXOs of 0.05 BTC each
    let taker_original_balance = fund_unified_taker(
        unified_taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Fund the Makers with 4 UTXOs of 0.05 BTC each
    fund_unified_makers(
        &unified_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Start the Maker Server threads
    log::info!("Initiating Maker servers");

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
    wait_for_makers_setup(&unified_makers, 120);

    // Sync wallets after setup
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);

    // Initiate Coinswap
    info!("Initiating coinswap protocol");

    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Start periodic swap tracker logging
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("unified_taker1"),
        Duration::from_secs(10),
    );

    // Prepare should succeed; execution should fail with DropAfterFundsBroadcast
    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Prepare should succeed");
    let swap_result = unified_taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to DropAfterFundsBroadcast behavior"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    unified_taker.log_tracker_state();

    // Wait for makers to timeout and broadcast contracts, then blocks mature timelocks.
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

    info!("Makers shut down. Waiting for background recovery loop to complete...");

    // Wait for taker's background recovery loop to finish
    let recovery_timeout = Duration::from_secs(120);
    let recovery_start = Instant::now();
    while !unified_taker.is_recovery_complete() {
        if recovery_start.elapsed() > recovery_timeout {
            panic!("Background recovery did not complete within timeout");
        }
        thread::sleep(Duration::from_secs(5));
    }
    info!("Background recovery loop completed.");

    // Mine a block to confirm recovery txs, then sync wallet
    generate_blocks(bitcoind, 1);
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

    assert_eq!(
        taker_balances.regular.to_sat(),
        14499696,
        "Taker regular balance mismatch"
    );
    assert_eq!(
        taker_balances.swap.to_sat(),
        495278,
        "Taker swap balance mismatch"
    );
    assert_eq!(
        taker_balances.contract.to_sat(),
        0,
        "Taker contract balance mismatch"
    );
    assert_eq!(taker_balances.fidelity, Amount::ZERO);

    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap();

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    assert_eq!(
        balance_diff.to_sat(),
        5026,
        "Taker spendable balance change mismatch"
    );

    // Verify maker balances - makers should have recovered via timelock
    for (i, maker) in unified_makers.iter().enumerate() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let maker_balances = maker.wallet.read().unwrap().get_balances().unwrap();

        info!(
            "Maker {} balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i,
            maker_balances.regular,
            maker_balances.swap,
            maker_balances.contract,
            maker_balances.fidelity,
            maker_balances.spendable,
        );

        let expected_regular = [14501444u64, 14503316][i];
        let expected_swap = [499400u64, 497150][i];
        assert_eq!(
            maker_balances.regular.to_sat(),
            expected_regular,
            "Maker {} regular balance mismatch",
            i
        );
        assert_eq!(
            maker_balances.swap.to_sat(),
            expected_swap,
            "Maker {} swap balance mismatch",
            i
        );
        assert_eq!(
            maker_balances.contract.to_sat(),
            0,
            "Maker {} contract balance mismatch",
            i
        );
        assert_eq!(maker_balances.fidelity, Amount::from_btc(0.05).unwrap());

        // Makers should not have lost funds (they gained slightly from fee income)
        let original = maker_spendable_balance[i];
        assert!(
            maker_balances.spendable >= original,
            "Maker {} should not have lost funds. Original: {}, After: {}",
            i,
            original,
            maker_balances.spendable
        );
    }

    unified_taker.log_tracker_state();
    info!("Abort1 test completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
