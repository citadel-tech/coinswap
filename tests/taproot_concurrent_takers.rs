#![cfg(feature = "integration-test")]
//! This test demonstrates a taproot-based coinswap between 2 Taker and 2 Makers,and it's purpose is
//! to ensure that makers function correctly when performing taproot-based swaps with multiple takers concurrently, when each maker has only liquidity for one swap

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
fn test_taproot_concurrent_takers() {
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
    let (test_framework, mut taproot_takers, taproot_makers) =
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
        2,
        Amount::from_btc(0.05009).unwrap(),
    );

    // Start the Taproot Maker Server threads
    log::info!("🚀Initiating Taproot Makers...");

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
        for (taker_index, taker) in taproot_takers.iter_mut().enumerate() {
            // Perform the swap concurrently — consume the takers vector so each thread gets ownership.
            let swap_params = SwapParams {
                send_amount: Amount::from_sat(5000000), // 0.05 BTC
                maker_count: 2,
                tx_count: 3,
                required_confirms: 1,
                manually_selected_outpoints: None,
            };
            s.spawn(move || {
                info!("🚀 Taproot Taker {} starting coinswap", taker_index);
                match taker.do_coinswap(swap_params) {
                    Ok(Some(_report)) => {
                        log::info!("✅ Taproot Taker {} coinswap completed successfully!", taker_index);
                    }
                    Ok(None) => {
                        log::warn!(
                            "⚠️ Taproot Taker {} coinswap completed but no report generated (recovery occurred)",
                            taker_index
                        );
                    }
                    Err(e) => {
                        log::warn!("❌ Taproot Taker {} coinswap failed: {:?}", taker_index, e);
                    }
                }
            });
            std::thread::sleep(Duration::from_secs(3));
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

    // Verify takers
    for (i, taproot_taker) in taproot_takers.iter().enumerate() {
        let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
        info!(
            "📊 Taproot Taker {i} balance after swap: Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
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
        assert_in_range!(
            taker_total_after.to_sat(),
            [14461248,
             15000000, // failed taker with full balance
             ],
            "Taproot Taker spendable balance check."
        );

        // But the taker should still have a reasonable amount left
        let balance_diff = taker_balance_before[i] - taker_total_after;
        assert_in_range!(
            balance_diff.to_sat(),
            [538752, 0], // fee paid in normal swap case and no swap
            "Taproot Taker should have paid reasonable fees."
        );
        info!(
            "Taproot Taker balance verification passed. Original spendable: {}, After spendable: {} (fees paid: {})",
            taker_balance_before[i],
            taker_total_after,
            balance_diff
        );
    }

    // Verify makers
    for (i, (maker, original_spendable)) in taproot_makers
        .iter()
        .zip(maker_spendable_balance.iter())
        .enumerate()
    {
        let wallet = maker.wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "📊 Taproot Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}, Swap:{}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable, balances.swap,
        );

        // Use spendable (regular + swap) for comparison
        assert_in_range!(
            balances.spendable.to_sat(),
            [5227654, 5342322],
            "Taproot Maker after balance check"
        );

        let balance_diff = balances.spendable.to_sat() - original_spendable.to_sat();
        // maker gained fee
        assert_in_range!(
            balance_diff,
            [210022, 324692],
            "Taproot Maker should have gained some fee here."
        );

        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable, balance_diff
        );
    }
    info!("✅ Multi Taker test passed!");
}
