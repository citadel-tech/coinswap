#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
};
use std::sync::Arc;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Malice 1: Taker Broadcasts contract transactions prematurely.
///
/// The Makers identify the situation and get their money back via contract txs. This is
/// a potential DOS on Makers. But Taker would lose money too for doing this.
#[test]
fn malice_1() {
    // ---- Setup ----

    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::BroadcastContractAfterFullSetup];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!("🧪 Running Test: Malice 1 - Taker broadcasts contract transaction prematurely");

    info!("💰 Funding taker and makers");
    // Fund the Taker  with 3 utxos of 0.05 btc each and do basic checks on the balance
    let taker = &mut takers[0];
    let org_taker_spend_balance = fund_and_verify_taker(
        taker,
        &test_framework.bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
    );

    // Fund the Maker with 4 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(
        makers_ref,
        &test_framework.bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    //  Start the Maker Server threads
    info!("🚀 Initiating Maker servers");

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Makers take time to fully setup.
    let org_maker_spend_balances = makers
        .iter()
        .map(|maker| {
            while !maker.is_setup_complete.load(Relaxed) {
                info!("⏳ Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }

            // Check balance after setting up maker server.
            let wallet = maker.wallet.read().unwrap();

            let balances = wallet.get_balances().unwrap();

            verify_maker_pre_swap_balances(&balances, 14999500);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap
    info!("🔄 Initiating coinswap protocol");

    // Swap params for coinswap.
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        manually_selected_outpoints: None,
    };
    taker.do_coinswap(swap_params).unwrap();

    info!("🎯 All coinswaps processed successfully. Transaction complete.");

    //wait for maker's to complete recovery
    info!("Waiting for maker to complete recovery");
    thread::sleep(Duration::from_secs(60));

    // shutdown makers thread
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    //join makers thread
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    ///////////////////
    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync_and_save().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }
    ///////////////

    //-------- Fee Tracking and Workflow:------------
    //
    // **Taker** => BroadcastContractAfterFullSetup
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Balance diff after recovering via hashlock (Sats |
    // |----------------|------------------------|-------------------------|--------------------------------------------------|
    // | **Taker**      | 443,339                | 500,000                 | 56,965                                           |
    // | **Maker16102** | 500,000                | 478,589                 | 21,411                                           |
    // | **Maker6102**  | 478,589                | 443,339                 | 35,250 (32932 + 2318(mining fee) )               |
    //

    info!("📊 Verifying malicious taker scenario results");
    // Check Taker balances
    {
        let wallet = taker.get_wallet();
        let balances = wallet.get_balances().unwrap();

        // Debug logging for taker
        log::info!(
            "🔍 DEBUG Taker - Regular: {}, Swap: {}, Spendable: {},Contract: {}",
            balances.regular.to_btc(),
            balances.swap.to_btc(),
            balances.spendable.to_btc(),
            balances.contract.to_btc()
        );
        assert_in_range!(
            balances.regular.to_sat(),
            [
                14499696,// Recover via hashlock
            ],
            "Taker seed balance mismatch"
        );

        assert_in_range!(
            balances.swap.to_sat(),
            [
                443339 // Taker claimed it's incoming contract via hashlock recovery.
            ],
            "Taker swapcoin balance mismatch"
        );

        assert_in_range!(balances.contract.to_sat(), [0], "Contract balance mismatch");
        assert_eq!(balances.fidelity, Amount::ZERO);

        // Check balance difference
        let balance_diff = org_taker_spend_balance
            .checked_sub(balances.spendable)
            .unwrap();

        log::info!(
            "🔍 DEBUG Taker balance diff: {} sats",
            balance_diff.to_sat()
        );
        assert_in_range!(
            balance_diff.to_sat(),
            [
                56965  // fee consisting of Maker fees + mining fees + hashlock recovery
            ],
            "Taker spendable balance change mismatch"
        );
    }

    // Check Maker balances
    makers
        .iter()
        .zip(org_maker_spend_balances.iter())
        .enumerate()
        .for_each(|(maker_index, (maker, org_spend_balance))| {
            let mut wallet = maker.get_wallet().write().unwrap();
            wallet.sync_and_save().unwrap();
            let balances = wallet.get_balances().unwrap();

            // Debug logging for makers
            log::info!(
                "🔍 DEBUG Maker {} - Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
                maker_index,
                balances.regular.to_btc(),
                balances.swap.to_btc(),
                balances.contract.to_btc(),
                balances.spendable.to_btc()
            );

            assert_in_range!(
                balances.regular.to_sat(),
                [
                    14533002, // first maker on claiming it's incoming contract via hashlock recovery
                    14555287, // second maker on claiming it's incoming contract via hashlock recovery
                ],
                "Maker seed balance mismatch"
            );

            // Here the swap balance is 0 because after taker went offline, makers were unable to complete the swap at their side.
            assert_in_range!(
                balances.swap.to_sat(),
                [465624, 499430, 499724], //swap balance of each maker after recovering via hashlock. 2possible combination based on the order of makers
                "Maker swapcoin balance mismatch"
            );
            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
            // Check spendable balance difference.
            let balance_diff = match org_spend_balance.checked_sub(balances.spendable) {
                None => balances.spendable.checked_sub(*org_spend_balance).unwrap(), // Successful swap as Makers balance increase by Coinswap fee.
                Some(diff) => diff, // No spending or unsuccessful swap , Maker may have lost some funds here, generally due to timelock recovery transaction
            };

            log::info!(
                "🔍 DEBUG Maker {} balance diff: {} sats",
                maker_index,
                balance_diff.to_sat()
            );
            assert_in_range!(
                balance_diff.to_sat(),
                // Here 2 possible combination based on the order of makers
                [
                    21411, // 1st maker total fee gained after recovering via hashlock spend
                    32932, // 2nd maker total fee gained after recovering via hashlock spend
                    33224, // 2nd maker total fee gained after recovering via hashlock spend
                ],
                "Maker spendable balance change mismatch"
            );
        });
    log::info!("✅ Swap results verification complete");

    info!("🎉 All checks successful. Terminating integration test case");
}
