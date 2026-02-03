#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::MakerBehavior,
    taker::{SwapParams, TakerBehavior},
};
use std::sync::Arc;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// ABORT 3: Maker Drops After Setup
/// Case 3: CloseAtHashPreimage
///
/// Maker closes the connection at hash preimage handling. Funding txs are already broadcasted.
/// The Maker will lose contract txs fees in that case, so it's not malice.
/// Taker waits for the response until timeout. Aborts if the Maker doesn't show up.
#[test]
fn maker_abort3_case3() {
    // ---- Setup ----

    // 6102 is naughty. And theres not enough makers.
    let makers_config_map = [
        ((6102, None), MakerBehavior::CloseAtHashPreimage),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!("🧪 Running Test: Maker closes connection at hash preimage handling");

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
    test_framework.start_maker_servers();

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

    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync_and_save().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }
    ///////////////

    //-------- Fee Tracking and Workflow:--------------------------------------------------------------------------
    //
    // This fee scenario would occur in both cases whether Maker6102 is the first or last maker.

    // Case 1: Maker6102 is the first maker
    // Workflow: Taker -> Maker6102(CloseAtHashPreimage) -> Maker16102
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Balance diff after recovering via hashlock (Sats |
    // |----------------|------------------------|-------------------------|--------------------------------------------------|
    // | **Taker**      | 443,339                | 500,000                 | 56,965                                           |
    // | **Maker16102** | 500,000                | 478,589                 | 21,411                                           |
    // | **Maker6102**  | 478,589                | 443,339                 | 35,250 (33,224 + 2026 (mining fee))              |
    //
    // Case 2: Maker16102 is the last maker.
    // Workflow: Taker -> Maker16102 -> Maker16102(CloseAtHashPreimage)
    //
    // Same as Case 1.
    //-----------------------------------------------------------------------------------------------------------------------------------------------

    info!("📊 Verifying swap results after connection close");
    // Check Taker balances
    {
        let wallet = taker.get_wallet();
        let balances = wallet.get_balances().unwrap();

        // Debug logging for taker
        log::info!(
            "🔍 DEBUG Taker - Regular: {}, Swap: {}, Spendable: {}",
            balances.regular.to_btc(),
            balances.swap.to_btc(),
            balances.spendable.to_btc()
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
                56965  // fee consisting of maker fees + mining fees + hashlock recovery
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
                    14533002, // first maker regular balance after claiming it's incoming contract via hashlock recovery
                    14555287, // second maker regular balance after claiming it's incoming contract via hashlock recovery
                ],
                "Maker seed balance mismatch"
            );

            assert_in_range!(
                balances.swap.to_sat(),
                [465624, 499430, 499724], //swap balance of each maker after recovering via hashlock. 2 possible combination based on the order of makers.
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

    test_framework.shutdown_maker_servers().unwrap();
}
