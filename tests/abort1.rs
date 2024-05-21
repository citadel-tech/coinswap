#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::ConnectionType,
};

mod test_framework;
use log::{info, warn};
use std::{assert_eq, thread, time::Duration};
use test_framework::*;

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
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    // Initiate test framework, Makers.
    // Taker has a special behavior DropConnectionAfterFullSetup.
    let (test_framework, taker, makers, directory_server_instance) = TestFramework::init(
        None,
        makers_config_map.into(),
        Some(TakerBehavior::DropConnectionAfterFullSetup),
        ConnectionType::CLEARNET,
    )
    .await;

    warn!("Running Test: Taker Cheats on Everybody.");

    info!("Initiating Takers...");
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
        });
    }

    // Coins for fidelity creation
    makers.iter().for_each(|maker| {
        let maker_addrs = maker
            .get_wallet()
            .write()
            .unwrap()
            .get_next_external_address()
            .unwrap();
        test_framework.send_to_address(&maker_addrs, Amount::from_btc(0.05).unwrap());
    });

    // confirm balances
    test_framework.generate_blocks(1);

    let mut all_utxos = taker.read().unwrap().get_wallet().get_all_utxo().unwrap();

    // Get the original balances
    let org_taker_balance_fidelity = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_fidelity_bonds(Some(&all_utxos))
        .unwrap();
    let org_taker_balance_descriptor_utxo = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_descriptor_utxo(Some(&all_utxos))
        .unwrap();
    let org_taker_balance_swap_coins = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_swap_coins(Some(&all_utxos))
        .unwrap();
    let org_taker_balance_live_contract = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_live_contract(Some(&all_utxos))
        .unwrap();
    let org_taker_balance = org_taker_balance_descriptor_utxo + org_taker_balance_swap_coins;

    // ---- Start Servers and attempt Swap ----

    info!("Initiating Maker...");
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

    // Makers take time to fully setup.
    makers.iter().for_each(|maker| {
        while !*maker.is_setup_complete.read().unwrap() {
            log::info!("Waiting for maker setup completion");
            // Introduce a delay of 10 seconds to prevent write lock starvation.
            thread::sleep(Duration::from_secs(10));
            continue;
        }
    });

    let swap_params = SwapParams {
        send_amount: 500000,
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        fee_rate: 1000,
    };

    info!("Initiating coinswap protocol");

    // Calculate Original balance excluding fidelity bonds.
    // Bonds are created automatically after spawning the maker server.
    let org_maker_balances = makers
        .iter()
        .map(|maker| {
            all_utxos = maker.get_wallet().read().unwrap().get_all_utxo().unwrap();
            let maker_balance_fidelity = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_fidelity_bonds(Some(&all_utxos))
                .unwrap();
            let maker_balance_descriptor_utxo = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_descriptor_utxo(Some(&all_utxos))
                .unwrap();
            let maker_balance_swap_coins = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_swap_coins(Some(&all_utxos))
                .unwrap();
            let maker_balance_live_contract = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_live_contract(Some(&all_utxos))
                .unwrap();
            assert_eq!(maker_balance_fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(
                maker_balance_descriptor_utxo,
                Amount::from_btc(0.14999).unwrap()
            );
            assert_eq!(maker_balance_swap_coins, Amount::from_btc(0.0).unwrap());
            assert_eq!(maker_balance_live_contract, Amount::from_btc(0.0).unwrap());
            maker_balance_descriptor_utxo + maker_balance_swap_coins
        })
        .collect::<Vec<_>>();

    // Spawn a Taker coinswap thread.
    let taker_clone = taker.clone();
    let taker_thread = thread::spawn(move || {
        taker_clone
            .write()
            .unwrap()
            .do_coinswap(swap_params)
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

    let _ = directory_server_instance.shutdown();

    thread::sleep(Duration::from_secs(10));

    // Taker still has 6 swapcoins in its list
    assert_eq!(taker.read().unwrap().get_wallet().get_swapcoins_count(), 6);

    //Run Recovery script
    warn!("Starting Taker recovery process");
    taker.write().unwrap().recover_from_swap().unwrap();

    // All pending swapcoins are cleared now.
    assert_eq!(taker.read().unwrap().get_wallet().get_swapcoins_count(), 0);

    all_utxos = taker.read().unwrap().get_wallet().get_all_utxo().unwrap();

    // Check everybody looses mining fees of contract txs.
    let taker_balance_fidelity = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_fidelity_bonds(Some(&all_utxos))
        .unwrap();
    let taker_balance_descriptor_utxo = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_descriptor_utxo(Some(&all_utxos))
        .unwrap();
    let taker_balance_swap_coins = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_swap_coins(Some(&all_utxos))
        .unwrap();
    let taker_balance_live_contract = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_live_contract(Some(&all_utxos))
        .unwrap();
    let taker_balance = taker_balance_descriptor_utxo + taker_balance_swap_coins;

    assert_eq!(org_taker_balance - taker_balance, Amount::from_sat(6768));
    assert_eq!(org_taker_balance_fidelity, Amount::from_btc(0.0).unwrap());
    assert_eq!(
        org_taker_balance_descriptor_utxo,
        Amount::from_btc(0.15).unwrap()
    );
    assert_eq!(org_taker_balance_swap_coins, Amount::from_btc(0.0).unwrap());
    assert_eq!(
        org_taker_balance_live_contract,
        Amount::from_btc(0.0).unwrap()
    );
    assert_eq!(taker_balance_fidelity, Amount::from_btc(0.0).unwrap());
    assert_eq!(
        taker_balance_descriptor_utxo,
        Amount::from_btc(0.14993232).unwrap()
    );
    assert_eq!(taker_balance_swap_coins, Amount::from_btc(0.0).unwrap());
    assert_eq!(taker_balance_live_contract, Amount::from_btc(0.0).unwrap());

    makers
        .iter()
        .zip(org_maker_balances.iter())
        .for_each(|(maker, org_balance)| {
            all_utxos = maker.get_wallet().read().unwrap().get_all_utxo().unwrap();
            let maker_balance_fidelity = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_fidelity_bonds(Some(&all_utxos))
                .unwrap();
            let maker_balance_descriptor_utxo = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_descriptor_utxo(Some(&all_utxos))
                .unwrap();
            let maker_balance_swap_coins = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_swap_coins(Some(&all_utxos))
                .unwrap();
            let maker_balance_live_contract = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_live_contract(Some(&all_utxos))
                .unwrap();
            let new_balance = maker
                .get_wallet()
                .read()
                .unwrap()
                .balance_descriptor_utxo(Some(&all_utxos))
                .unwrap()
                + maker
                    .get_wallet()
                    .read()
                    .unwrap()
                    .balance_swap_coins(Some(&all_utxos))
                    .unwrap();

            assert_eq!(*org_balance - new_balance, Amount::from_sat(6768));

            assert_eq!(maker_balance_fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(
                maker_balance_descriptor_utxo,
                Amount::from_btc(0.14992232).unwrap()
            );
            assert_eq!(maker_balance_swap_coins, Amount::from_btc(0.0).unwrap());
            assert_eq!(maker_balance_live_contract, Amount::from_btc(0.0).unwrap());
        });

    info!("All checks successful. Terminating integration test case");

    test_framework.stop();
}
