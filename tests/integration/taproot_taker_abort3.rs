//! Taproot taker abort test 3: Taker closes after receiving maker's contract response.
//!
//! Scenario:
//! 1. Taker initiates a Taproot coinswap with 2 makers.
//! 2. Taker drops after receiving the maker's contract data response
//!    (CloseAtSendersContractFromMaker behavior).
//! 3. Funding transactions have been broadcast at this point.
//! 4. Recovery is needed -- taker and makers recover via timelock.
//! 5. Verify: all parties recovered funds (contract == 0, small fee loss).

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

/// Test: Taker aborts after receiving maker's contract response (Taproot).
///
/// The taker drops the connection after receiving the maker's contract data
/// response. This is similar to abort2 but occurs one step later -- after
/// the maker has sent its contract data back. Funding transactions are
/// already on-chain, so timelock recovery is required.
#[test]
fn test_taproot_taker_abort3() {
    // ---- Setup ----
    warn!("Running Test: Taproot Taker Abort3 - Close at Sender's Contract From Maker");

    let makers_config_map = vec![(7002, Some(20001)), (17002, Some(20002))];
    let taker_behavior = vec![UnifiedTakerBehavior::CloseAtSendersContractFromMaker];
    let maker_behaviors = vec![UnifiedMakerBehavior::Normal, UnifiedMakerBehavior::Normal];

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
    wait_for_makers_setup(&unified_makers, 120);

    // Sync wallets after setup
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let _maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting taproot taker abort3 test...");

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

    // Prepare should succeed; execution should fail after receiving maker's contract data
    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Prepare should succeed");
    let swap_result = unified_taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to CloseAtSendersContractFromMaker behavior"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    unified_taker.log_tracker_state();

    // Wait for makers to timeout and blocks to mature timelocks.
    // Maker timeout is 60s in tests; block generation thread mines 10 blocks every 3s,
    // so 90s ~ 300 blocks -- more than enough for the 60-block CSV timelock.
    info!("Waiting for makers to timeout and blocks to mature timelocks...");
    thread::sleep(Duration::from_secs(90));

    // Shut down makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Verify maker balances -- makers should have recovered their outgoing funds via timelock
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
        let expected_regular = [14998936, 14999540];
        assert_eq!(
            maker_balances.regular.to_sat(),
            expected_regular[i],
            "Maker {} regular balance mismatch",
            i
        );
        assert_eq!(
            maker_balances.swap.to_sat(),
            0,
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
    }

    info!("Makers shut down. Waiting for background recovery loop to complete...");

    // The background recovery loop (spawned by recover_active_swap) periodically
    // retries hashlock sweeps and timelock recovery. Wait for it to finish.
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
        14999416,
        "Taker regular balance mismatch"
    );
    assert_eq!(
        taker_balances.swap.to_sat(),
        0,
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
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    assert_eq!(
        balance_diff.to_sat(),
        584,
        "Taker spendable balance change mismatch"
    );

    unified_taker.log_tracker_state();
    info!("Taproot taker abort3 test completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
