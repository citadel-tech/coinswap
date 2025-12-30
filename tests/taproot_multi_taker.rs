#![cfg(feature = "integration-test")]
//! This test demonstrates a taproot-based coinswap between 2 Taker and 2 Makers,and it's purpose is
//! to ensure that maker's works fine when performing a taproot based swap with multiple taker a time.

use bitcoin::Amount;
use coinswap::{
    maker::start_maker_server_taproot,
    taker::api2::{SwapParams, TakerBehavior},
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn test_taproot_multi_taker() {
    // ---- Setup ----
    warn!("Running Test: Multiple Taproot Takers with normal behaviour");

    use coinswap::maker::TaprootMakerBehavior as MakerBehavior;
    let taproot_makers_config_map = vec![
        (7102, Some(19061), MakerBehavior::Normal),
        (17102, Some(19062), MakerBehavior::Normal),
    ];
    // Create two taker with normal behavior
    let taker_behavior = vec![TakerBehavior::Normal, TakerBehavior::Normal];

    // Initialize test framework
    let (test_framework, mut taproot_takers, taproot_makers, block_generation_handle) =
        TestFramework::init_taproot(taproot_makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;

    // Fund the Taproot Takers with 3 UTXOs of 0.05 BTC each
    for taker in taproot_takers.iter_mut() {
        fund_taproot_taker(taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());
    }

    // Fund the Taproot Makers with 8 UTXOs of 0.05 BTC each
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        8,
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

    let mut taker_balance_before = Vec::new();
    for taker in &mut taproot_takers {
        taker.get_wallet_mut().sync_and_save().unwrap();
        info!("📊 Taker balance before attempting swap:");
        let taker_balances = taker.get_wallet().get_balances().unwrap();
        info!(
            "  Regular: {}, Contract: {}, Spendable: {}",
            taker_balances.regular, taker_balances.contract, taker_balances.spendable
        );
        taker_balance_before.push(taker_balances.spendable);
    }

    log::info!("Initiating taproot multi taker test...");

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Spawn threads for each taker to initiate coinswap concurrently
    thread::scope(|s| {
        for taker in &mut taproot_takers {
            // Perform the swap concurrently — consume the takers vector so each thread gets ownership.
            let swap_params = SwapParams {
                send_amount: Amount::from_sat(500000), // 0.005 BTC
                maker_count: 2,
                tx_count: 3,
                required_confirms: 1,
                manually_selected_outpoints: None,
            };
            s.spawn(move || match taker.do_coinswap(swap_params) {
                Ok(Some(_report)) => {
                    log::info!("Taproot coinswap completed successfully!");
                }
                Ok(None) => {
                    log::warn!(
                        "Taproot coinswap completed but no report generated (recovery occurred)"
                    );
                }
                Err(e) => {
                    log::error!("Taproot coinswap failed: {:?}", e);
                    panic!("Taproot coinswap failed: {:?}", e);
                }
            });
            std::thread::sleep(Duration::from_secs(10));
        }
    });

    // After swap, shutdown maker threads
    taproot_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    taproot_maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All taproot coinswaps processed successfully. Transaction complete.");

    // Sync wallets
    for taker in &mut taproot_takers {
        taker.get_wallet_mut().sync_and_save().unwrap();
    }

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each taproot maker's wallet multiple times to ensure all UTXOs are discovered
    for maker in taproot_makers.iter() {
        let mut wallet = maker.wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // Verify swap results
    for (i, taproot_taker) in taproot_takers.into_iter().enumerate() {
        let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
        info!(
         "📊 Taproot Taker balance after swap: Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
    );
        let taker_wallet = taproot_taker.get_wallet();
        let taker_balances = taker_wallet.get_balances().unwrap();
        // Use spendable balance (regular + swap) since swept coins from V2 swaps
        // are tracked as SweptCoinV2 and appear in swap balance
        let taker_total_after = taker_balances.spendable;
        assert!(
            taker_total_after.to_sat() == 14943203, // Spendable balance like normal case
            "Taproot Taker {} spendable balance check. Original: {}, After: {}",
            i,
            taker_balance_before[i],
            taker_total_after
        );

        // But the taker should still have a reasonable amount left (not all spent on fees)
        let balance_diff = taker_balance_before[i] - taker_total_after;
        assert!(
            // Here the fee consist of each maker fee i.e. is 13500 sats, and mining fees -> 29797 sats
            balance_diff.to_sat() == 56797, // fee paid in normal swap case.
            "Taproot Taker should have paid reasonable fees. Original: {}, After: {},fees paid: {}",
            taker_balance_before[i],
            taker_total_after,
            balance_diff
        );
        info!(
        "Taproot Taker balance verification passed. Original spendable: {}, After spendable: {} (fees paid: {})",
        taker_balance_before[i],
        taker_total_after,
        balance_diff
    );

        // Verify makers earned fees
        for (i, (maker, original_spendable)) in taproot_makers
            .iter()
            .zip(maker_spendable_balance.clone())
            .enumerate()
        {
            let wallet = maker.wallet().read().unwrap();
            let balances = wallet.get_balances().unwrap();

            info!(
            "Taproot Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {},Swap:{}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,balances.swap,
        );

            // Use spendable (regular + swap) for comparison
            assert_in_range!(
                balances.spendable.to_sat(),
                [35040880, 35063890], // 8 utxos were funded to makers in this case,that's why more spendable balance
                "Taproot Maker after balance check."
            );

            let balance_diff = balances.spendable.to_sat() - original_spendable.to_sat();
            // maker gained fee
            assert_in_range!(
                balance_diff,
                [41378, 64390], // Each maker performed 2 swap,hence almost 2x sats earned compared to normal swap.
                "Taproot Maker should have gained some fee here."
            );

            info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable, balance_diff
        );
        }
    }
    info!("✅ Multi Taker test passed!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
