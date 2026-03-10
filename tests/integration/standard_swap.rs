//! Standard coinswap test: normal swap between a Taker and 2 Makers.
//! Nothing goes wrong and the coinswap completes successfully.

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

/// This test demonstrates a standard coinswap round between a Taker and 2 Makers. Nothing goes wrong
/// and the coinswap completes successfully.
#[test]
fn test_standard_coinswap() {
    // ---- Setup ----
    warn!("Running Test: Standard Coinswap Procedure");

    let makers_config_map = vec![(6102, Some(19051)), (16102, Some(19052))];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];
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

    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Failed to prepare coinswap");
    unified_taker
        .start_coinswap(&summary.swap_id)
        .expect("Coinswap should complete successfully");

    info!("All coinswaps processed successfully. Transaction complete.");

    // After swap, shutdown maker threads
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Sync wallets
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

    // Verify taker balances
    info!("Verifying swap results");
    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

    info!(
        "Taker balances: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
        taker_balances.regular,
        taker_balances.swap,
        taker_balances.contract,
        taker_balances.spendable,
    );

    assert_in_range!(
        taker_balances.regular.to_sat(),
        [14499696],
        "Taker regular balance mismatch"
    );
    assert_in_range!(
        taker_balances.swap.to_sat(),
        [495578],
        "Taker swap balance mismatch"
    );
    assert_in_range!(
        taker_balances.contract.to_sat(),
        [0],
        "Taker contract balance mismatch"
    );
    assert_eq!(taker_balances.fidelity, Amount::ZERO);

    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap();

    info!("Taker fees paid: {} sats", balance_diff.to_sat());

    assert_in_range!(
        balance_diff.to_sat(),
        [4726],
        "Taker spendable balance change mismatch"
    );

    // Verify maker balances
    for (i, (maker, original)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let balances = maker.wallet.read().unwrap().get_balances().unwrap();

        info!(
            "Maker {} balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i,
            balances.regular,
            balances.swap,
            balances.contract,
            balances.fidelity,
            balances.spendable,
        );

        assert_in_range!(
            balances.regular.to_sat(),
            [14501444, 14503316],
            "Maker regular balance mismatch"
        );
        assert_in_range!(
            balances.swap.to_sat(),
            [499700, 497450],
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
            .checked_sub(original)
            .unwrap_or(Amount::ZERO);

        info!("Maker {} fee earned: {} sats", i, maker_fee.to_sat());

        assert_in_range!(
            maker_fee.to_sat(),
            [1646, 1268],
            "Maker fee earned mismatch"
        );
    }

    info!("Standard coinswap test completed successfully!");
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
