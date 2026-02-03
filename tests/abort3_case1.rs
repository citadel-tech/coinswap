#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::MakerBehavior,
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

/// ABORT 3: Maker Drops After Setup
/// Case 1: CloseAtContractSigsForRecvrAndSender
///
/// Maker closes the connection after receiving a `RespContractSigsForRecvrAndSender` and doesn't broadcast its funding txs.
/// Taker wait until a timeout (10ses for test, 5mins for prod) and starts recovery after that.
// This is problematic. Taker will mark this maker as "bad" and will not swap this maker again.
#[test]
fn maker_abort3_case1() {
    // ---- Setup ----

    // 6102 is naughty. And theres not enough makers.
    let naughty = 6102;
    let makers_config_map = [
        (
            (naughty, None),
            MakerBehavior::CloseAtContractSigsForRecvrAndSender,
        ),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!(
        "🧪 Running Test: Maker closes connection after receiving a ContractSigsForRecvrAndSender"
    );

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
    thread::sleep(Duration::from_secs(30));

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
    // Case 1: Maker6102 is the First Maker, and the Taker recovers from an initiated swap.
    // Workflow: Taker -> Maker6102 (CloseAtContractSigsForRecvrAndSender)
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 1,179                      | 1,179             |
    //
    // - Taker forwards [`ProofOfFunding`] to Maker6102, receives [`ReqContractSigsAsRecvrAndSender`].
    // - Maker6102 reaches CloseAtContractSigsForRecvrAndSender and doesn't broadcast funding tx.
    // - Taker recovers from the swap.
    //
    // Final Outcome for Taker (Recover from Swap):
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats)| Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | -                                  | 858                 | -                  | 858                        |
    //
    // - Taker recovers funds but loses **6,768 sats** in mining fees.
    //
    // Final Outcome for Makers (Case 1):
    //
    // | Participant    | Coinswap Outcome (Sats)                 |
    // |----------------|------------------------------------------|
    // | **Maker6102**  | 0 (Marked as a bad maker by the Taker)   |
    // | **Maker16102** | 0                                        |
    //
    // Case 2: Maker6102 is the Second Maker.
    // Workflow: Taker -> Maker16102 -> Maker6102 (CloseAtContractSigsForRecvrAndSender)
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 1,179(mining fees)         |
    // | **Maker16102** | 500,000                | 478,007                 | 21,992     | 21,992                     |
    //
    // - Maker6102 receives [`ProofOfFunding`] of Maker16102, sends [`ReqContractSigsAsRecvrAndSender`].
    // - Maker6102 reaches CloseAtContractSigsForRecvrAndSender and doesn't broadcast funding tx.
    //
    // - After timeout, Taker and Maker16102 recover funds but lose ** 858 sats** each in fees.
    //
    // Final Outcome for Maker6102:
    //
    // | Participant    | Coinswap Outcome (Sats)                 |
    // |----------------|------------------------------------------|
    // | **Maker6102**  | 0 (Marked as a bad maker by the Taker)   |
    //
    // Final Outcome for Maker16102 and Taker:
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats)|
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | -                                  | 858                 | -                  | 858                        |
    // | **Maker16102** | -                                  | 858                 | -                  | 858                        |

    info!("🚫 Verifying naughty maker gets banned");
    // Maker6102 gets banned for being naughty.
    assert_eq!(
        format!("127.0.0.1:{naughty}"),
        taker.get_bad_makers()[0].address.to_string()
    );

    info!("📊 Verifying swap results after connection close");
    // After Swap checks:
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
                    14999500, //  No spend
                    14998642, // Maker after recovering via timelock,it has lost some sats here.
                ],
                "Maker seed balance mismatch"
            );

            assert_in_range!(
                balances.swap.to_sat(),
                [0],
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
                    0,   // No spend
                    858, // Maker has lost some sats here due to timelock recovery transaction fee
                ],
                "Maker spendable balance change mismatch"
            );
        });
    log::info!("✅ Swap results verification complete");

    info!("🎉 All checks successful. Terminating integration test case");
    test_framework.shutdown_maker_servers().unwrap();
}
