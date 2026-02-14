use super::test_framework::*;
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
};
use std::sync::Arc;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// ABORT 2: Maker Drops Before Setup
/// This test demonstrates the situation where a Maker prematurely drops connections after doing
/// initial protocol handshake. This should not necessarily disrupt the round, the Taker will try to find
/// more makers in his address book and carry on as usual. The Taker will mark this Maker as "bad" and will
/// not swap this maker again.
///
/// CASE 2: Maker Drops Before Sending Sender's Signature, and Taker cannot find a new Maker, recovers from Swap.
#[test]
fn maker_abort2_case2() {
    // ---- Setup ----

    // 6102 is naughty. And theres not enough makers.
    let naughty = 6102;
    let makers_config_map = [
        (
            (naughty, None),
            MakerBehavior::CloseAtReqContractSigsForSender,
        ),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal];

    warn!(
        "Running test: Maker {naughty} Closes before sending sender's sigs. Taker recovers. Or Swap cancels"
    );

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

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
    // Case 1: Maker6102 is the second maker, and the Taker recovers from an initiated swap.
    // Workflow: Taker -> Maker16102 -> Maker6102 (CloseAtReqContractSigsForSender)
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) |
    // |----------------|------------------------|-------------------------|------------|
    // | **Taker**      | _                      | 500,000                 | _          |
    //
    // - Taker sends [`ProofOfFunding`] to Maker16102.
    // - Maker16102 responds with [`ReqContractSigsAsRecvrAndSender`] to the Taker.
    // - Taker forwards [`ReqContractSigsForSender`] to Maker6102, but Maker6102 does not respond, and the Taker recovers from the swap.
    //
    // Final Outcome for Taker (Recover from Swap):
    //
    // | Participant    | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // |----------------|------------------------------------------|----------------------------|
    // | **Taker**      | 858                 | -                  |  858                       |
    //
    // - The Taker regains their initial funding amounts but incurs a total loss of *858 sats** due to recovery fees.
    //
    // Case 2: Maker6102 is the first maker.
    // Workflow: Taker -> Maker6102 (CloseAtReqContractSigsForSender)
    //
    // - Taker creates unbroadcasted funding transactions and sends [`ReqContractSigsForSender`] to Maker6102.
    // - Maker6102 does not respond, and the swap fails.
    //
    // Final Outcome for Taker:
    //
    // | Participant    | Coinswap Outcome (Sats) |
    // |----------------|--------------------------|
    // | **Taker**      | 0                       |
    //
    // Final Outcome for Makers (In both cases):
    //
    // | Participant    | Coinswap Outcome (Sats)                 |
    // |----------------|------------------------------------------|
    // | **Maker6102**  | 0 (Marked as a bad maker by the Taker)   |
    // | **Maker16102** | 0                                        |

    info!("🚫 Verifying naughty maker gets banned");
    // Maker gets banned for being naughty.
    assert_eq!(
        format!("127.0.0.1:{naughty}"),
        taker.get_bad_makers()[0].address.to_string()
    );

    info!("📊 Verifying swap results after maker drops connection");
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
                0 // No swap happened
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
                    14999500, // No spend from maker in this case
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

            assert!(
                balance_diff.to_sat() == 0,
                "Maker spendable balance change mismatch {}",
                balance_diff,
            );
        });

    log::info!("✅ Swap results verification complete");

    info!("🎉 All checks successful. Terminating integration test case");

    // After all balance checks are complete, shut down maker threads
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
