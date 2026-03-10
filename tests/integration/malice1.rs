//! Malice test 1: Taker broadcasts contract transactions maliciously after full setup.
//!
//! Scenario:
//! 1. Taker initiates a Legacy coinswap with 2 makers.
//! 2. Full setup completes (funding broadcast, contract sigs exchanged).
//! 3. Taker broadcasts outgoing contract txs maliciously (BroadcastContractAfterFullSetup).
//! 4. Swap fails. Makers detect the broadcast contracts and recover via timelock.
//! 5. After recovery, verify: makers recovered funds (contract == 0, small fee loss).

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

/// Test: Taker maliciously broadcasts contract txs after full setup.
///
/// The taker completes the full contract exchange, then broadcasts contract
/// transactions and closes. Makers detect the on-chain contracts and recover
/// their funds via timelock spending.
#[test]
fn test_malice1_taker_broadcast_contract() {
    // ---- Setup ----
    warn!("Running Test: Malice1 - Taker Broadcasts Contract After Full Setup");

    let makers_config_map = vec![(6602, Some(19601)), (16602, Some(19602))];
    let taker_behavior = vec![UnifiedTakerBehavior::BroadcastContractAfterFullSetup];
    let maker_behaviors = vec![UnifiedMakerBehavior::Normal, UnifiedMakerBehavior::Normal];

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
    wait_for_makers_setup(&unified_makers, 120);

    // Sync wallets after setup
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting malice1 test...");

    // Swap params for unified coinswap (Legacy)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Prepare should succeed; execution should fail with BroadcastContractAfterFullSetup
    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Prepare should succeed");
    let swap_result = unified_taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to BroadcastContractAfterFullSetup behavior"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    unified_taker.log_tracker_state();

    // Wait for makers to timeout and broadcast contracts, then blocks mature timelocks.
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
        assert_in_range!(
            maker_balances.regular.to_sat(),
            [14501444, 14503316],
            "Maker regular balance mismatch"
        );
        assert_in_range!(
            maker_balances.swap.to_sat(),
            [0],
            "Maker swap balance mismatch"
        );
        assert_in_range!(
            maker_balances.contract.to_sat(),
            [0],
            "Maker contract balance mismatch"
        );
        assert_eq!(maker_balances.fidelity, Amount::from_btc(0.05).unwrap());

        // Makers lost some funds due to taker maliciously broadcasting contracts
        let original = maker_spendable_balance[i];
        let maker_diff = original
            .checked_sub(maker_balances.spendable)
            .unwrap_or(Amount::ZERO);
        info!(
            "Maker {} lost {} sats (pre-swap: {}, current: {})",
            i,
            maker_diff.to_sat(),
            original,
            maker_balances.spendable,
        );
    }

    // Wait for taker's background recovery loop to finish
    info!("Waiting for background recovery loop to complete...");
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

    assert_in_range!(
        taker_balances.contract.to_sat(),
        [0],
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

    unified_taker.log_tracker_state();
    info!("Malice1 test completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
