#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::SwapParams,
    test_framework::*,
};
use log::{info, warn};
use std::{thread, time::Duration};

/// ABORT 2: Maker Drops Before Setup
/// This test demonstrates the situation where a Maker prematurely drops connections after doing
/// initial protocol handshake. This should not necessarily disrupt the round, the Taker will try to find
/// more makers in his address book and carry on as usual. The Taker will mark this Maker as "bad" and will
/// not swap this maker again.
///
/// CASE 2: Maker Drops Before Sending Sender's Signature, and Taker cannot find a new Maker, recovers from Swap.
#[tokio::test]
async fn test_abort_case_2_recover_if_no_makers_found() {
    // ---- Setup ----

    // 6102 is naughty. And theres not enough makers.
    let makers_config_map = [
        (6102, MakerBehavior::CloseAtReqContractSigsForSender),
        (16102, MakerBehavior::Normal),
    ];

    warn!("Running test: Maker 6102 Closes before sending sender's sigs. Taker recovers. Or Swap cancels");

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, taker, makers) =
        TestFramework::init(None, makers_config_map.into(), None).await;

    // Fund the Taker and Makers with 3 utxos of 0.05 btc each.
    for _ in 0..3 {
        let taker_address = taker
            .write()
            .unwrap()
            .get_wallet_mut()
            .get_next_external_address()
            .unwrap();
        test_framework.send_to_address(&taker_address, Amount::from_btc(0.05).unwrap());
        makers.iter().for_each(|maker| {
            let maker_addrs = maker
                .get_wallet()
                .write()
                .unwrap()
                .get_next_external_address()
                .unwrap();
            test_framework.send_to_address(&maker_addrs, Amount::from_btc(0.05).unwrap());
        })
    }

    // confirm balances
    test_framework.generate_1_block();

    // Get the original balances
    let org_taker_balance = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance(false, false)
        .unwrap();
    let org_maker_balances = makers
        .iter()
        .map(|maker| {
            maker
                .get_wallet()
                .read()
                .unwrap()
                .balance(false, false)
                .unwrap()
        })
        .collect::<Vec<_>>();

    // ---- Start Servers and attempt Swap ----

    // Start the Maker server threads
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Start swap
    thread::sleep(Duration::from_secs(20)); // Take a delay because Makers take time to fully setup.
    let swap_params = SwapParams {
        send_amount: 500000,
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        fee_rate: 1000,
    };

    // Spawn a Taker coinswap thread.
    let taker_clone = taker.clone();
    let taker_thread =
        thread::spawn(move || taker_clone.write().unwrap().send_coinswap(swap_params));

    // Wait for Taker swap thread to conclude.
    // The whole swap can fail if 6102 happens to be the first peer.
    // In that the swap isn't feasible, and user should modify SwapParams::maker_count.
    if let Err(e) = taker_thread.join().unwrap() {
        assert_eq!(format!("{:?}", e), "NotEnoughMakersInOfferBook".to_string());
        info!("Coinswap failed because the first maker rejected for signature");
        return;
    }

    // Wait for Maker threads to conclude.
    makers.iter().for_each(|maker| maker.shutdown().unwrap());
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // ---- After Swap checks ----

    // Maker gets banned for being naughty.
    assert_eq!(
        "localhost:6102",
        taker.read().unwrap().get_bad_makers()[0]
            .address
            .to_string()
    );

    // Assert that Taker burned the mining fees,
    // Makers are fine.
    let new_taker_balance = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance(false, false)
        .unwrap();
    assert_eq!(
        org_taker_balance - new_taker_balance,
        Amount::from_sat(4227)
    );
    makers
        .iter()
        .zip(org_maker_balances.iter())
        .for_each(|(maker, org_balance)| {
            let new_balance = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance(false, false)
                .unwrap();
            assert_eq!(*org_balance - new_balance, Amount::from_sat(0));
        });

    // Stop test and clean everything.
    // comment this line if you want the wallet directory and bitcoind to live. Can be useful for
    // after test debugging.
    test_framework.stop();
}
