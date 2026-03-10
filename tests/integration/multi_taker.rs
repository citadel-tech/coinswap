//! Integration test for multi-taker concurrent coinswap.
//!
//! Setup: 2 takers with Normal behavior, 2 makers with Normal behavior.
//! Protocol: Legacy (ECDSA), AddressType::P2WPKH.
//! Both takers run swaps sequentially through the same pair of makers.

use bitcoin::Amount;
use coinswap::{
    maker::start_unified_server,
    protocol::common_messages::ProtocolVersion,
    taker::{UnifiedSwapParams, UnifiedTakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread};

#[test]
fn test_multi_taker_coinswap() {
    // ---- Setup ----
    warn!("Running Test: Multi-Taker Coinswap with Legacy (ECDSA) Protocol");

    let makers_config_map = vec![(7602, Some(20601)), (17602, Some(20602))];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal, UnifiedTakerBehavior::Normal];

    // Initialize test framework with 2 takers and 2 makers
    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, vec![]);

    let bitcoind = &test_framework.bitcoind;

    // Fund both takers with 3 UTXOs of 0.05 BTC each (P2WPKH for Legacy)
    let taker1_original_balance = fund_unified_taker(
        &unified_takers[0],
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    let taker2_original_balance = fund_unified_taker(
        &unified_takers[1],
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

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);

    // ---- Swap 1: First taker ----
    log::info!("Starting swap for Taker 1 (Legacy protocol)...");

    let swap_params1 = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    let unified_taker1 = &mut unified_takers[0];
    let summary1 = unified_taker1
        .prepare_coinswap(swap_params1)
        .expect("Failed to prepare Legacy coinswap for Taker 1");
    log::info!("Taker 1 swap summary: {:?}", summary1);

    match unified_taker1.start_coinswap(&summary1.swap_id) {
        Ok(report) => {
            log::info!("Taker 1 coinswap (Legacy) completed successfully!");
            log::info!("Taker 1 swap report: {:?}", report);
        }
        Err(e) => {
            log::error!("Taker 1 coinswap (Legacy) failed: {:?}", e);
            panic!("Taker 1 coinswap (Legacy) failed: {:?}", e);
        }
    }

    // Mine blocks between swaps to confirm transactions
    generate_blocks(bitcoind, 5);

    // Sync maker wallets between swaps
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // ---- Swap 2: Second taker ----
    log::info!("Starting swap for Taker 2 (Legacy protocol)...");

    let swap_params2 = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    let unified_taker2 = &mut unified_takers[1];
    let summary2 = unified_taker2
        .prepare_coinswap(swap_params2)
        .expect("Failed to prepare Legacy coinswap for Taker 2");
    log::info!("Taker 2 swap summary: {:?}", summary2);

    match unified_taker2.start_coinswap(&summary2.swap_id) {
        Ok(report) => {
            log::info!("Taker 2 coinswap (Legacy) completed successfully!");
            log::info!("Taker 2 swap report: {:?}", report);
        }
        Err(e) => {
            log::error!("Taker 2 coinswap (Legacy) failed: {:?}", e);
            panic!("Taker 2 coinswap (Legacy) failed: {:?}", e);
        }
    }

    // ---- Shutdown and verify ----
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All unified coinswaps processed successfully. Transactions complete.");

    // Sync all wallets
    for taker in unified_takers.iter() {
        taker.get_wallet().write().unwrap().sync_and_save().unwrap();
    }

    generate_blocks(bitcoind, 1);

    for maker in unified_makers.iter() {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // ---- Verify Taker 1 ----
    let taker1_balances_after = unified_takers[0]
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();
    info!("Taker 1 balance after swap (Legacy):");

    let balance_diff1 = taker1_original_balance - taker1_balances_after.spendable;
    info!(
        "Taker 1 balance verification passed. Original: {}, After: {} (fees paid: {})",
        taker1_original_balance, taker1_balances_after.spendable, balance_diff1
    );

    assert_in_range!(
        taker1_balances_after.spendable.to_sat(),
        [14995274],
        "Taker 1 spendable balance mismatch"
    );
    assert_in_range!(
        taker1_balances_after.contract.to_sat(),
        [0],
        "Taker 1 contract balance mismatch"
    );
    assert_eq!(taker1_balances_after.fidelity, Amount::ZERO);
    assert_in_range!(balance_diff1.to_sat(), [4726], "Taker 1 fee paid mismatch");

    // ---- Verify Taker 2 ----
    let taker2_balances_after = unified_takers[1]
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();
    info!("Taker 2 balance after swap (Legacy):");

    let balance_diff2 = taker2_original_balance - taker2_balances_after.spendable;
    info!(
        "Taker 2 balance verification passed. Original: {}, After: {} (fees paid: {})",
        taker2_original_balance, taker2_balances_after.spendable, balance_diff2
    );

    assert_in_range!(
        taker2_balances_after.spendable.to_sat(),
        [14995274],
        "Taker 2 spendable balance mismatch"
    );
    assert_in_range!(
        taker2_balances_after.contract.to_sat(),
        [0],
        "Taker 2 contract balance mismatch"
    );
    assert_eq!(taker2_balances_after.fidelity, Amount::ZERO);
    assert_in_range!(balance_diff2.to_sat(), [4726], "Taker 2 fee paid mismatch");

    // ---- Verify Makers earned fees ----
    for (i, (maker, original_spendable)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Unified Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,
        );

        assert_in_range!(
            balances.regular.to_sat(),
            [10000000],
            "Maker regular balance mismatch"
        );
        assert_in_range!(
            balances.swap.to_sat(),
            [5002790, 5002034],
            "Maker swap balance mismatch"
        );
        assert_in_range!(
            balances.contract.to_sat(),
            [0],
            "Maker contract balance mismatch"
        );
        assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());

        let maker_fee = balances
            .spendable
            .checked_sub(original_spendable)
            .unwrap_or(Amount::ZERO);

        info!(
            "Unified Maker {} fee earned: {} sats",
            i,
            maker_fee.to_sat()
        );

        assert_in_range!(
            maker_fee.to_sat(),
            [3292, 2536],
            "Maker fee earned mismatch"
        );
    }

    info!("All multi-taker swap tests (Legacy) completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
