#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::ConnectionType,
};
mod test_framework;
use log::{info, warn};
use std::{
    assert_eq,
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
fn test_stop_taker_after_setup() {
    // ---- Setup ----

    // 2 Makers with Normal behavior.
    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::DropConnectionAfterFullSetup];

    // Initiate test framework, Makers.
    // Taker has a special behavior DropConnectionAfterFullSetup.
    let (test_framework, mut takers, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    warn!("Running Test: Taker Cheats on Everybody.");
    let taker = &mut takers[0];
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
    taker.do_coinswap(swap_params).unwrap();

    // After Swap is done, wait for maker threads to conclude.
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    // After Swap is done, wait for maker threads to conclude.
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

    //Run Recovery script
    // TODO: do something about this?
    warn!("Starting Taker recovery process");
    taker.recover_from_swap().unwrap();

    // ## Fee Tracking and Workflow:
    //
    // ### Fee Breakdown:
    //
    // +------------------+-------------------------+--------------------------+------------+----------------------------+-------------------+
    // | Participant      | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // +------------------+-------------------------+--------------------------+------------+----------------------------+-------------------+
    // | Taker            | _                      | 500,000                  | _          | 3,000                      | 3,000             |
    // | Maker16102       | 500,000                | 463,500                  | 33,500     | 3,000                      | 36,500            |
    // | Maker6102        | 463,500                | 438,642                  | 21,858     | 3,000                      | 24,858            |
    // +------------------+-------------------------+--------------------------+------------+----------------------------+-------------------+
    //
    //
    // **Taker** => DropConnectionAfterFullSetup
    //
    // Participants regain their initial funding amounts but incur a total loss of **6,768 sats**
    // due to mining fees (recovery + initial transaction fees).
    //
    // ### Recovery Fees Breakdown:
    //
    // +------------------+------------------------------------+---------------------+--------------------+----------------------------+
    // | Participant      | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Funding Fee (Sats) | Total Recovery Fees (Sats) |
    // +------------------+------------------------------------+---------------------+--------------------+----------------------------+
    // | Taker            | 3,000                              | 768                 | 3,000             | 6,768                      |
    // | Maker16102       | 3,000                              | 768                 | 3,000             | 6,768                      |
    // | Maker6102        | 3,000                              | 768                 | 3,000             | 6,768                      |
    // +------------------+------------------------------------+---------------------+--------------------+----------------------------+
    //
    verify_swap_results(
        taker,
        &makers,
        org_taker_spend_balance,
        org_maker_spend_balances,
    );
    info!("All checks successful. Terminating integration test case");

    test_framework.stop();

    block_generation_handle.join().unwrap();
}
