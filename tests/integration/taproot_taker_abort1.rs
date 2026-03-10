//! Taproot taker abort test 1: Taker closes at AckSwapDetails response.
//!
//! Scenario:
//! 1. Taker initiates a Taproot coinswap with 2 makers.
//! 2. Taker closes the connection immediately after receiving AckSwapDetails
//!    from a maker (CloseAtAckResponse behavior).
//! 3. This is an early abort -- no funding transactions are broadcast.
//! 4. No recovery is needed since no funds are on-chain.
//! 5. Verify: taker balance is approximately unchanged (no fund loss).

use bitcoin::Amount;
use coinswap::{
    maker::{start_unified_server, UnifiedMakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{UnifiedSwapParams, UnifiedTakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread};

/// Test: Taker aborts at AckSwapDetails response (Taproot).
///
/// The taker closes the connection right after the maker acknowledges the
/// swap details. No funding transactions have been broadcast at this point,
/// so no recovery is needed. Balances should remain unchanged.
#[test]
fn test_taproot_taker_abort1() {
    // ---- Setup ----
    warn!("Running Test: Taproot Taker Abort1 - Close at AckResponse");

    let makers_config_map = vec![(6802, Some(19801)), (16802, Some(19802))];
    let taker_behavior = vec![UnifiedTakerBehavior::CloseAtAckResponse];
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
    log::info!("Starting taproot taker abort1 test...");

    // Swap params for unified coinswap (Taproot)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Prepare should fail at AckResponse — the taker closes the connection
    // right after receiving AckSwapDetails, before any funding is broadcast.
    let prepare_result = unified_taker.prepare_coinswap(swap_params);
    assert!(
        prepare_result.is_err(),
        "Prepare should fail due to CloseAtAckResponse behavior"
    );
    info!(
        "Prepare failed as expected: {:?}",
        prepare_result.err().unwrap()
    );
    unified_taker.log_tracker_state();

    // No recovery needed -- this is an early abort before any funding broadcast.
    // Shut down makers.
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Sync taker wallet and verify balance
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

    info!(
        "Taker balances after abort: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
        taker_balances.regular,
        taker_balances.swap,
        taker_balances.contract,
        taker_balances.spendable,
    );

    // Contract balance should be 0 (no contracts were created on-chain)
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "Taker should have no contract balance after early abort"
    );

    // Balance diff should be 0 or very small (no funds were spent on-chain)
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap_or(Amount::ZERO);

    info!(
        "Taker balance diff: {} sats (original: {}, current: {})",
        balance_diff.to_sat(),
        taker_original_balance,
        taker_balances.spendable,
    );

    // No funds should have been lost since no transactions were broadcast
    assert_eq!(
        balance_diff.to_sat(),
        0,
        "Taker should not have lost funds on early abort. Lost {} sats",
        balance_diff.to_sat(),
    );

    unified_taker.log_tracker_state();
    info!("Taproot taker abort1 test completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
