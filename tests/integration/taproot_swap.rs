//! Integration test for Taproot Coinswap implementation.
//!
//! This test demonstrates a taproot-based coinswap between a Taker and 2 Makers using
//! the Taproot protocol with MuSig2 signatures.

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

/// Test taproot coinswap
#[test]
fn test_taproot_coinswap() {
    // ---- Setup ----
    warn!("Running Test: Taproot Coinswap Basic Functionality");

    let makers_config_map = vec![(7102, Some(19061)), (17102, Some(19062))];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];
    let maker_behaviors = vec![UnifiedMakerBehavior::Normal, UnifiedMakerBehavior::Normal];

    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let unified_taker = unified_takers.get_mut(0).unwrap();

    // Fund the Taproot Taker with 3 UTXOs of 0.05 BTC each (P2TR)
    let taker_original_balance = fund_unified_taker(
        unified_taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Fund the Taproot Makers with 4 UTXOs of 0.05 BTC each
    fund_unified_makers(
        &unified_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Start the Maker Server threads
    log::info!("Initiating Taproot Makers...");

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
    log::info!("Starting end-to-end taproot swap test...");

    // Swap params for taproot coinswap
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Prepare and execute the swap
    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Failed to prepare Taproot coinswap");
    unified_taker
        .start_coinswap(&summary.swap_id)
        .expect("Taproot coinswap should complete successfully");
    log::info!("Taproot coinswap completed successfully!");

    // After swap, shutdown maker threads
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

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    for maker in unified_makers.iter() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

    info!(
        "Taproot Taker balance after swap: Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances.regular,
        taker_balances.contract,
        taker_balances.spendable,
        taker_balances.swap,
    );

    assert_eq!(
        taker_balances.regular.to_sat(),
        14499716,
        "Taker regular balance mismatch"
    );
    assert_eq!(
        taker_balances.swap.to_sat(),
        497450,
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

    info!("Taproot Taker fees paid: {} sats", balance_diff.to_sat());

    assert_eq!(
        balance_diff.to_sat(),
        2834,
        "Taker spendable balance change mismatch"
    );

    // Verify makers earned fees
    for (i, (maker, original_spendable)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let balances = maker.wallet.read().unwrap().get_balances().unwrap();

        info!(
            "Taproot Maker {} final balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,
        );

        let expected_regular = [14500361, 14501486];
        assert_eq!(
            balances.regular.to_sat(),
            expected_regular[i],
            "Maker {} regular balance mismatch",
            i
        );
        let expected_swap = [499700, 498575];
        assert_eq!(
            balances.swap.to_sat(),
            expected_swap[i],
            "Maker {} swap balance mismatch",
            i
        );
        assert_eq!(
            balances.contract.to_sat(),
            0,
            "Maker {} contract balance mismatch",
            i
        );
        assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());

        let maker_fee = balances
            .spendable
            .checked_sub(original_spendable)
            .unwrap_or(Amount::ZERO);

        info!(
            "Taproot Maker {} fee earned: {} sats",
            i,
            maker_fee.to_sat()
        );

        assert_eq!(maker_fee.to_sat(), 521, "Maker {} fee earned mismatch", i);
    }

    info!("All taproot swap tests completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
