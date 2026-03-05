#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
};
mod test_framework;
use log::{info, warn};
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    thread,
    time::Duration,
};
use test_framework::*;

/// Abort 1: TAKER Drops After Full Setup.
/// This test demonstrates the situation where the Taker drops the connection after broadcasting all the
/// funding transactions. The Makers identify this and wait for a timeout (5mins in prod, 30 secs in test)
/// for the Taker to come back. If the Taker doesn't come back within timeout, the Makers broadcast the contract
/// transactions and reclaims their funds via timelock.
///
/// The Taker after coming live again will see unfinished coinswaps in his wallet. He can reclaim his funds via
/// broadcasting his contract transactions and claiming via timelock.
#[test]
fn taker_abort1() {
    // ---- Setup ----

    // 2 Makers with Normal behavior.
    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::DropConnectionAfterFullSetup];

    // Initiate test framework, Makers.
    // Taker has a special behavior DropConnectionAfterFullSetup.
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!("🧪 Running Test: Taker cheats on everybody");
    let taker = &mut takers[0];

    info!("💰 Funding taker and makers");
    // Fund the Taker  with 3 utxos of 0.05 btc each and do basic checks on the balance
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

    // After Swap is done, wait for maker threads to conclude.
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    info!("🎯 All coinswaps processed successfully. Transaction complete.");

    ///////////////////
    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync_and_save().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }
    ///////////////

    //Run Recovery script
    warn!("🔧 Starting Taker recovery process");
    taker.recover_from_swap().unwrap();

    // ## Fee Tracking and Workflow:
    // **Taker** => DropConnectionAfterFullSetup
    //
    // Participants regain their initial funding amounts but incur a total loss of **858 sats**
    // due to mining fees (timelock recovery fee).
    //
    // ### Recovery Fees Breakdown:
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats)| Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | -                                  | 858                 | -                  | 858                        |
    // | **Maker6102**  | -                                  | 858                 | -                  | 858                        |
    // | **Maker6102**  | -                                  | 858                 | -                  | 858                        |
    //
    info!("📊 Verifying swap results after taker recovery");
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
                14999142,// Recover via timelock
            ],
            "Taker seed balance mismatch"
        );

        assert_in_range!(
            balances.swap.to_sat(),
            [
                0 // No swap happened,each party recovered their outgoing contract via timelock
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
                858  // Timlock recovery fee
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
                    14998642, // Makers after recovering via timelock,it has lost some sats here.
                ],
                "Maker seed balance mismatch"
            );

            assert!(
                balances.swap.to_sat() == 0,
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
                [
                   858,// Maker has lost some sats here due to timelock recovery transaction fee
                ],
                "Maker spendable balance change mismatch"
            );
        });

    log::info!("✅ Swap results verification complete");

    info!("🎉 All checks successful. Terminating integration test case");
}
