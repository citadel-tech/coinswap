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

/// Malice 2: Maker Broadcasts contract transactions prematurely.
///
/// The Taker and other Makers identify the situation and get their money back via contract txs. This is
/// a potential DOS on other Makers. But the attacker Maker would lose money too in the process.
///
/// This case is hard to "blame". As the contract transactions are available to both the Makers, it's not identifiable
/// which Maker is the culprit. Taker does not ban in this case.
#[test]
fn malice_2() {
    // ---- Setup ----

    let makers_config_map = [
        ((6102, None), MakerBehavior::BroadcastContractAfterSetup),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!("🧪 Running Test: Malice 2 - Maker broadcasts contract transactions prematurely");

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
    thread::sleep(Duration::from_secs(10));

    ///////////////////
    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync_and_save().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }
    ///////////////

    // -------- Fee Tracking and Workflow --------
    //
    // Case 1: Maker6102 is the First Maker.
    // Workflow: Taker -> Maker6102 (BroadcastContractAfterSetup) -> Maker16102
    // Maker6102 => BroadcastContractAfterSetup
    //
    // Seeing those contract txes, the Taker recovers from the swap.
    // Taker,Maker6102 recover funds but lose ** 858 sats** each in fees.
    //
    // Final Outcome for Taker & Maker6102:
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats)| Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | -                                  | 858                 | -                  | 858                        |
    // | **Maker6102**  | -                                  | 858                 | -                  | 858                        |
    //
    // Final Outcome for Maker16102:
    // | Participant    | Coinswap Outcome (Sats) |
    // |----------------|--------------------------|
    // | **Maker16102** | 0                        |
    //
    // ------------------------------------------------------------------------------------------------------------------------
    //
    // Case 2: Maker6102 is the Last Maker.
    // Workflow: Taker -> Maker16102 -> Maker6102 (BroadcastContractAfterSetup)
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|-------------------|
    // | **Taker**      | 443,633                | 500,000                 | _          | 1,179(mining fees)|
    // | **Maker16102** | 500,000                | 478,007                 | 21,992     | 21,992            |
    // | **Maker6102**  | 478,007                | 443,633                 | 33,500     | 33,500            |
    // Maker6102 => BroadcastContractAfterSetup

    info!("📊 Verifying malicious scenario recovery results");
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
            [14499696],
            "Taker seed balance mismatch"
        );

        assert_in_range!(
            balances.swap.to_sat(),
            [
                443633 //swap balance
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
        assert!(
            balance_diff.to_sat() == 56671,
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
                    14555287, // 1st maker
                    14533002, // 2nd maker
                ],
                "Maker seed balance mismatch"
            );

            assert_in_range!(
                balances.swap.to_sat(),
                [
                    465918, // First maker
                    499724, // Second maker,
                ],
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
                    21705, // First maker fee gained
                    33226, // Second maker fee gained
                ],
                "Maker spendable balance change mismatch"
            );
        });
    log::info!("✅ Swap results verification complete");

    info!("🎉 All checks successful. Terminating integration test case");

    //Shutdown Makers.
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());
}
