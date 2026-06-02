//! Abort 1 (Electrum backend): TAKER Drops After Full Setup.
//!
//! Same scenario as `abort1.rs`, but every participant runs on the Electrum
//! backend, and the scenario runs over both protocols (Taproot and Legacy).
//! This is the most Electrum-critical abort case: the taker vanishes after
//! broadcasting the funding transactions, so the makers must detect the
//! failure autonomously and recover via the preimage/hashlock cascade or
//! timelock. On Electrum that detection path has no ZMQ and no mempool scan —
//! it depends entirely on script subscriptions (`subscribe_script`/`poll_event`),
//! `get_tx_out` confirmation gating, and header-by-hash resolution.
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

/// Exact post-recovery balances for one protocol run. Fees differ between the
/// Legacy and Taproot transaction shapes (and locktime values), so each
/// protocol pins its own values.
struct ExpectedBalances {
    taker_regular: u64,
    taker_swap: u64,
    taker_spendable_diff: u64,
    maker_regular: [u64; 2],
    maker_swap: [u64; 2],
    maker_spendable: [u64; 2],
}

const LEGACY_EXPECTED: ExpectedBalances = ExpectedBalances {
    taker_regular: 14499076,
    taker_swap: 493687,
    taker_spendable_diff: 7237,
    maker_regular: [14500865, 14503103],
    maker_swap: [498200, 495925],
    maker_spendable: [14999065, 14999028],
};

const TAPROOT_EXPECTED: ExpectedBalances = ExpectedBalances {
    taker_regular: 14499076,
    taker_swap: 494744,
    taker_spendable_diff: 6180,
    maker_regular: [14500753, 14502916],
    maker_swap: [499070, 496907],
    maker_spendable: [14999823, 14999823],
};

/// Run the abort1 scenario (taker drops after funds broadcast) on the Electrum
/// backend with the given protocol and assert the exact recovery balances.
fn run_electrum_abort1(protocol: ProtocolVersion, expected: &ExpectedBalances) {
    // ---- Setup ----
    warn!("Running Test: Taker Drops After Full Setup (Electrum backend, {protocol:?})");

    let makers_config_map = vec![(6102, None), (16102, None)];
    let taker_behavior = vec![TakerBehavior::DropAfterFundsBroadcast];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<ElectrumBackend>(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();

    // Fund the taker with 3 UTXOs of 0.05 BTC each
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
    log::info!("Initiating Maker servers");

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

    verify_maker_pre_swap_balances(&makers);

    // Initiate Coinswap
    info!("Initiating coinswap protocol");

    let swap_params = SwapParams::new(protocol, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Start periodic swap tracker logging
    let tracker_logger = spawn_tracker_logger(
        test_framework.temp_dir.join("taker1"),
        Duration::from_secs(10),
    );

    // Prepare should succeed; execution should fail with DropAfterFundsBroadcast
    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("Prepare should succeed");
    let swap_result = taker.start_coinswap(&summary.swap_id);
    assert!(
        swap_result.is_err(),
        "Swap should fail due to DropAfterFundsBroadcast behavior"
    );
    info!("Swap failed as expected: {:?}", swap_result.err().unwrap());
    taker.log_tracker_state();

    // Wait for makers to timeout and broadcast contracts, then blocks mature timelocks.
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

    // Wait for taker's background recovery loop to finish
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

    assert_eq!(
        taker_balances.regular.to_sat(),
        expected.taker_regular,
        "Taker regular balance mismatch"
    );
    assert_eq!(
        taker_balances.swap.to_sat(),
        expected.taker_swap,
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
        expected.taker_spendable_diff,
        "Taker spendable balance change mismatch"
    );

    // Verify maker balances - makers should have recovered via timelock
    for (i, maker) in makers.iter().enumerate() {
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

        assert_eq!(
            maker_balances.regular.to_sat(),
            expected.maker_regular[i],
            "Maker {} regular balance mismatch",
            i
        );
        assert_eq!(
            maker_balances.swap.to_sat(),
            expected.maker_swap[i],
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

        assert_eq!(
            maker_balances.spendable.to_sat(),
            expected.maker_spendable[i],
            "Maker {} spendable balance mismatch",
            i,
        );
    }

    taker.log_tracker_state();
    info!("Electrum abort1 test ({protocol:?}) completed successfully!");

    tracker_logger.stop();
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

#[test]
fn electrum_taker_abort1() {
    run_electrum_abort1(ProtocolVersion::Taproot, &TAPROOT_EXPECTED);
    run_electrum_abort1(ProtocolVersion::Legacy, &LEGACY_EXPECTED);
}
