#![cfg(feature = "integration-test")]
//! Integration test for Taproot Coinswap implementation
//!
//! This test demonstrates a taproot-based coinswap between a Taker and 2 Makers using the new
//! taproot protocol with MuSig2 signatures and enhanced privacy features.

use bitcoin::Amount;
use coinswap::{
    maker::start_maker_server_taproot,
    taker::api2::{SwapParams, TakerBehavior},
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test taproot coinswap
#[test]
fn test_taproot_coinswap() {
    // ---- Setup ----
    warn!("Running Test: Taproot Coinswap Basic Functionality");

    // Use different ports for taproot makers to avoid conflicts
    use coinswap::maker::TaprootMakerBehavior as MakerBehavior;
    let taproot_makers_config_map = vec![
        (7102, Some(19061), MakerBehavior::Normal),
        (17102, Some(19062), MakerBehavior::Normal),
    ];
    let taker_behavior = vec![TakerBehavior::Normal];

    // Initialize test framework (without regular takers, we'll create taproot taker manually)
    let (test_framework, mut taproot_taker, taproot_makers, block_generation_handle) =
        TestFramework::init_taproot(taproot_makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = taproot_taker.get_mut(0).unwrap();

    // Fund the Taproot Taker with 3 UTXOs of 0.05 BTC each
    let taproot_taker_original_balance =
        fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Fund the Taproot Makers with 4 UTXOs of 0.05 BTC each
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    // Start the Taproot Maker Server threads
    log::info!("Initiating Taproot Makers...");

    let taproot_maker_threads = taproot_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server_taproot(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for taproot makers to complete setup
    for maker in &taproot_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            log::info!("Waiting for taproot maker setup completion");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &taproot_makers {
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_maker_pre_swap_balance_taproot(&taproot_makers);
    log::info!("Starting end-to-end taproot swap test...");
    log::info!("Initiating taproot coinswap protocol");

    // Swap params for taproot coinswap
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
        manually_selected_makers: None,
    };

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Perform the swap
    match taproot_taker.do_coinswap(swap_params) {
        Ok(Some(_report)) => {
            log::info!("Taproot coinswap completed successfully!");
        }
        Ok(None) => {
            log::warn!("Taproot coinswap completed but no report generated (recovery occurred)");
        }
        Err(e) => {
            log::error!("Taproot coinswap failed: {:?}", e);
            panic!("Taproot coinswap failed: {:?}", e);
        }
    }

    // After swap, shutdown maker threads
    taproot_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    taproot_maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All taproot coinswaps processed successfully. Transaction complete.");

    // Sync wallets and verify results
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each taproot maker's wallet multiple times to ensure all UTXOs are discovered
    for maker in taproot_makers.iter() {
        let mut wallet = maker.wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!("📊 Taproot Taker balance after completing swap:");
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
    );

    // verify swap results
    let taker_total_after = taker_balances_after.spendable;
    // Use spendable balance (regular + swap) since swept coins from V2 swaps
    // are tracked as SweptCoinV2 and appear in swap balance
    assert_in_range!(
        taker_total_after.to_sat(),
        [14943199, 14943203], // Normal Taproot swap case (with slight fee variance)
        "Taproot Taker spendable balance check."
    );

    // In a normal swap case -: Each Maker fee is 13500 sats, mining fee is 29797 sats
    let balance_diff = taproot_taker_original_balance - taker_total_after;
    assert_in_range!(
        balance_diff.to_sat(),
        [56797, 56801], // sats paid as fees in a normal swap case (with slight variance)
        "Taproot Taker should have paid reasonable fees."
    );
    info!(
        "Taproot Taker balance verification passed. Original spendable: {}, After spendable: {} (fees paid: {})",
        taproot_taker_original_balance,
        taker_total_after,
        balance_diff
    );

    // Verify makers earned fees
    for (i, (maker, original_spendable)) in taproot_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let wallet = maker.wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Taproot Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {},Swap:{}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,balances.swap,
        );

        // The spendable contains maker's regular balance + maker's swap balance,therefore used spendable balance for comparision.
        assert_in_range!(
            balances.spendable.to_sat(),
            [15020189, 15020203, 15031694, 15031708], // Normal swap spendable balance for makers (with slight variance)
            "Taproot Maker after balance check."
        );

        let balance_diff = balances.spendable.to_sat() - original_spendable.to_sat();
        // maker gained fee arranged in the order of corresponding spendable balance in the above assertion.
        assert_in_range!(
            balance_diff,
            [20685, 20689, 20703, 32190, 32194, 32208], // (with slight variance)
            "Taproot Maker should have gained some fee"
        );

        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable, balance_diff
        );
    }

    info!("All taproot swap tests completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
