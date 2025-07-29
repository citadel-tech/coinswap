#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::ConnectionType,
};
use std::sync::Arc;
mod test_framework;
use test_framework::*;

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
fn test_abort_case_2_recover_if_no_makers_found() {
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
    warn!(
        "Running test: Maker {naughty} Closes before sending sender's sigs. Taker recovers. Or Swap cancels"
    );

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

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
    log::info!("Initiating Maker...");

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
                log::info!("Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }

            // Check balance after setting up maker server.
            let wallet = maker.wallet.read().unwrap();

            let balances = wallet.get_balances().unwrap();

            assert_eq!(balances.regular, Amount::from_btc(0.14999).unwrap());
            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(balances.swap, Amount::ZERO);
            assert_eq!(balances.contract, Amount::ZERO);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap
    log::info!("Initiating coinswap protocol");

    // Swap params for coinswap.
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
    };

    if let Err(e) = taker.do_coinswap(swap_params) {
        assert_eq!(format!("{e:?}"), "NotEnoughMakersInOfferBook".to_string());
        info!("Coinswap failed because the first maker rejected for signature");
    }

    // After Swap is done, wait for maker threads to conclude.
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All coinswaps processed successfully. Transaction complete.");

    // Shutdown Directory Server
    directory_server_instance.shutdown.store(true, Relaxed);

    thread::sleep(Duration::from_secs(10));

    ///////////////////
    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync().unwrap();
    }
    ///////////////

    // -------- Fee Tracking and Workflow --------
    //
    // Case 1: Maker6102 is the second maker, and the Taker recovers from an initiated swap.
    // Workflow: Taker -> Maker16102 -> Maker6102 (CloseAtReqContractSigsForSender)
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 3,000                      | 3,000             |
    //
    // - Taker sends [ProofOfFunding] to Maker16102.
    // - Maker16102 responds with [ReqContractSigsAsRecvrAndSender] to the Taker.
    // - Taker forwards [ReqContractSigsForSender] to Maker6102, but Maker6102 does not respond, and the Taker recovers from the swap.
    //
    // Final Outcome for Taker (Recover from Swap):
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // |----------------|------------------------------------|---------------------|--------------------|----------------------------|
    // | **Taker**      | 3,000                              | 768                 | 3,000              | 6,768                      |
    //
    // - The Taker regains their initial funding amounts but incurs a total loss of **6,768 sats** due to mining fees.
    //
    // Case 2: Maker6102 is the first maker.
    // Workflow: Taker -> Maker6102 (CloseAtReqContractSigsForSender)
    //
    // - Taker creates unbroadcasted funding transactions and sends [ReqContractSigsForSender] to Maker6102.
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

    // Maker gets banned for being naughty.
    assert_eq!(
        format!("127.0.0.1:{naughty}"),
        taker.get_bad_makers()[0].address.to_string()
    );

    // After Swap checks:
    verify_swap_results(
        taker,
        &makers,
        org_taker_spend_balance,
        org_maker_spend_balances,
    );
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
