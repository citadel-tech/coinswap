#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    test_framework::*,
};
use std::{collections::BTreeSet, thread, time::Duration};

/// Malice 2: Maker Broadcasts contract transactions prematurely.
///
/// The Taker and other Makers identify the situation and gets their money back via contract txs. This is
/// a potential DOS on other Makers. But the attacker Maker would loose money too in the process.
///
/// This case is hard to "blame". As the contract transactions is available to both the Makers, its not identifiable
/// which Maker is the culrpit. This requires more protocol level considerations.
#[tokio::test]
async fn malice2_maker_broadcast_contract_prematurely() {
    // ---- Setup ----

    let makers_config_map = [
        (6102, MakerBehavior::Normal),
        (16102, MakerBehavior::BroadcastContractAfterSetup),
    ];

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, taker, makers) =
        TestFramework::init(None, makers_config_map.into(), Some(TakerBehavior::Normal)).await;

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
        .collect::<BTreeSet<_>>();

    let org_take_balance = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance(false, false)
        .unwrap();

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
    makers.iter().for_each(|maker| maker.shutdown().unwrap());
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // ---- After Swap checks ----
    let maker_balances = makers
        .iter()
        .map(|maker| {
            maker
                .get_wallet()
                .read()
                .unwrap()
                .balance(false, false)
                .unwrap()
        })
        .collect::<BTreeSet<_>>();

    let taker_balance = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance(false, false)
        .unwrap();

    assert_eq!(maker_balances.first().unwrap(), &Amount::from_sat(14995773));

    // Everybody looses 4227 sats for contract transactions.
    assert_eq!(
        org_maker_balances
            .first()
            .unwrap()
            .checked_sub(*maker_balances.first().unwrap())
            .unwrap(),
        Amount::from_sat(4227)
    );
    assert_eq!(
        org_take_balance.checked_sub(taker_balance).unwrap(),
        Amount::from_sat(4227)
    );

    // Stop test and clean everything.
    // comment this line if you want the wallet directory and bitcoind to live. Can be useful for
    // after test debugging.
    test_framework.stop();
}
