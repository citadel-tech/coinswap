//! Integration test: Taproot maker abort3 - Maker drops after AckSwapDetails.
//!
//! Setup: 3 makers (maker[0] Normal, maker[1] CloseAfterAckResponse, maker[2] Normal).
//! The taker only needs 2 makers for the route, so when maker[1] drops after sending
//! AckSwapDetails (before funding), it retries with the spare maker.
//! The swap should succeed.

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

/// Test: Maker drops after sending AckSwapDetails (Taproot). Taker finds spare maker.
///
/// Scenario:
/// 1. Taker initiates a Taproot coinswap requiring 2 makers, 3 are available.
/// 2. maker[1] drops after sending AckSwapDetails (before funding broadcast).
/// 3. Taker detects the failure and retries with the spare maker (maker[2]).
/// 4. Swap completes successfully with maker[0] and maker[2].
/// 5. Verify: taker lost fees, makers gained fees.
#[test]
fn test_taproot_maker_abort3() {
    // ---- Setup ----
    warn!("Running Test: Taproot Maker Abort3 - CloseAfterAckResponse, spare maker available");

    let makers_config_map = vec![(7302, None), (17302, None), (27302, None)];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];
    let maker_behaviors = vec![
        UnifiedMakerBehavior::Normal,
        UnifiedMakerBehavior::CloseAfterAckResponse,
        UnifiedMakerBehavior::Normal,
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
    wait_for_makers_setup(&unified_makers, 120);

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);

    // Swap params for unified coinswap (Taproot), requires 2 makers
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Prepare and execute the swap -- taker should retry with the spare maker
    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Failed to prepare Taproot coinswap");
    unified_taker
        .start_coinswap(&summary.swap_id)
        .expect("Swap should succeed with spare maker");

    // Shutdown makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Sync wallets and verify results
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    generate_blocks(bitcoind, 1);

    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Verify taker balance
    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

    info!(
        "Taker balance: original={}, after={}",
        taker_original_balance, taker_balances.spendable
    );

    assert_in_range!(
        taker_balances.spendable.to_sat(),
        [14997166],
        "Taker spendable balance mismatch"
    );
    assert_in_range!(
        taker_balances.contract.to_sat(),
        [0],
        "Taker contract balance mismatch"
    );
    assert_eq!(taker_balances.fidelity, Amount::ZERO);

    // Verify makers earned fees (only the two that participated)
    for (i, (maker, original)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let balances = maker.wallet.read().unwrap().get_balances().unwrap();
        info!(
            "Maker {} balances: original={}, after={}",
            i, original, balances.spendable
        );
        // Makers should not have lost funds after abort recovery
        assert!(
            balances.spendable.to_sat() >= 14999000,
            "Maker {} spendable balance too low: {}",
            i,
            balances.spendable.to_sat()
        );
        assert_in_range!(
            balances.contract.to_sat(),
            [0],
            "Maker contract balance mismatch"
        );
        assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
    }

    info!("Taproot maker abort3 test completed successfully!");
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
