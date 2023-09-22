#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    test_framework::*,
};
use std::{thread, time::Duration};

/// Abort 1: TAKER Drops After Full Setup.
/// This test demonstrates the situation where the Taker drops connection after broadcasting all the
/// funding transactions. The Makers identifies this and waits for a timeout (5mins in prod, 30 secs in test)
/// for the Taker to come back. If the Taker doesn't come back within timeout, the Makers broadcasts the contract
/// transactions and reclaims their funds via timelock.
///
/// The Taker after coming live again will see unfinished coinswaps in his wallet. He can reclaim his funds via
/// broadcasting his contract transactions and claiming via timelock.
#[tokio::test]
async fn test_stop_taker_after_setup() {
    // ---- Setup ----

    // 2 Makers with Normal behavior.
    let makers_config_map = [
        (6102, MakerBehavior::Normal),
        (16102, MakerBehavior::Normal),
    ];

    // Initiate test framework, Makers.
    // Taker has a special behavior DropConnectionAfterFullSetup.
    let (test_framework, taker, makers) = TestFramework::init(
        None,
        makers_config_map.into(),
        Some(TakerBehavior::DropConnectionAfterFullSetup),
    )
    .await;

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
            let thread = thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            });
            thread
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
    let taker_thread = thread::spawn(move || {
        taker_clone
            .write()
            .unwrap()
            .send_coinswap(swap_params)
            .unwrap();
    });

    // Wait for Taker swap thread to conclude.
    taker_thread.join().unwrap();

    // Wait for Maker threads to conclude.
    //makers.iter().for_each(|maker| maker.shutdown().unwrap());
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // ---- After Swap checks ----

    // Taker still has 6 swapcoins in its list
    assert_eq!(taker.read().unwrap().get_wallet().get_swapcoins_count(), 6);

    //Run Recovery script
    log::info!("Starting Taker recovery process");
    taker.write().unwrap().recover_from_swap().unwrap();

    // All pending swapcoins are cleared now.
    assert_eq!(taker.read().unwrap().get_wallet().get_swapcoins_count(), 0);

    // Check everybody looses mining fees of contract txs.
    let taker_balance = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance(false, false)
        .unwrap();
    assert_eq!(org_taker_balance - taker_balance, Amount::from_sat(4227));

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
            assert_eq!(*org_balance - new_balance, Amount::from_sat(4227));
        });

    // Stop test and clean everything.
    // comment this line if you want the wallet directory and bitcoind to live. Can be useful for
    // after-test debugging.
    test_framework.stop();
}
