#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::ConnectionType,
};
mod test_framework;
use log::{info, warn};
use std::{assert_eq, sync::atomic::Ordering::Relaxed, thread, time::Duration};
use test_framework::*;

/// Abort 1: TAKER Drops After Full Setup.
/// This test demonstrates the situation where the Taker drops connection after broadcasting all the
/// funding transactions. The Makers identifies this and waits for a timeout (5mins in prod, 30 secs in test)
/// for the Taker to come back. If the Taker doesn't come back within timeout, the Makers broadcasts the contract
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

    // Initiate test framework, Makers.
    // Taker has a special behavior DropConnectionAfterFullSetup.
    let (test_framework, taker, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            TakerBehavior::DropConnectionAfterFullSetup,
            ConnectionType::CLEARNET,
        );

    warn!("Running Test: Taker Cheats on Everybody.");

    let bitcoind = &test_framework.bitcoind;

    info!("Initiating Takers...");
    // Fund the Taker and Makers with 3 utxos of 0.05 btc each.
    for _ in 0..3 {
        let taker_address = taker
            .write()
            .unwrap()
            .get_wallet_mut()
            .get_next_external_address()
            .unwrap();

        send_to_address(bitcoind, &taker_address, Amount::from_btc(0.05).unwrap());
        makers.iter().for_each(|maker| {
            let maker_addrs = maker
                .get_wallet()
                .write()
                .unwrap()
                .get_next_external_address()
                .unwrap();

            send_to_address(bitcoind, &maker_addrs, Amount::from_btc(0.05).unwrap());
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

        send_to_address(bitcoind, &maker_addrs, Amount::from_btc(0.05).unwrap());
    });

    // confirm balances
    generate_blocks(bitcoind, 1);

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
        while !maker.is_setup_complete.load(Relaxed) {
            log::info!("Waiting for maker setup completion");
            // Introduce a delay of 10 seconds to prevent write lock starvation.
            thread::sleep(Duration::from_secs(10));
            continue;
        }
    });

    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
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

    taker_thread.join().unwrap();

    // Wait for Maker threads to conclude.
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // ---- After Swap checks ----

    directory_server_instance.shutdown.store(true, Relaxed);

    thread::sleep(Duration::from_secs(10));

    // Wait for Taker swap thread to conclude.

    // Taker still has 6 swapcoins in its list
    assert_eq!(taker.read().unwrap().get_wallet().get_swapcoins_count(), 6);

    //Run Recovery script
    warn!("Starting Taker recovery process");
    taker.write().unwrap().recover_from_swap().unwrap();

    // All pending swapcoins are cleared now.
    assert_eq!(taker.read().unwrap().get_wallet().get_swapcoins_count(), 0);

    all_utxos = taker.read().unwrap().get_wallet().get_all_utxo().unwrap();

    //-------- Fee Tracking and Workflow:------------
    //
    // | Participant    | Amount Received (Sats) | Amount Forwarded (Sats) | Fee (Sats) | Funding Mining Fees (Sats) | Total Fees (Sats) |
    // |----------------|------------------------|-------------------------|------------|----------------------------|-------------------|
    // | **Taker**      | _                      | 500,000                 | _          | 3,000                      | 3,000             |
    // | **Maker16102** | 500,000                | 465,384                 | 31,616     | 3,000                      | 34,616            |
    // | **Maker6102**  | 465,384                | 442,325                 | 20,059     | 3,000                      | 23,059            |
    //
    // ## 3. Final Outcome for Taker (Successful Coinswap):
    //
    // | Participant   |  Coinswap Outcome (Sats)                                                |
    // |---------------|--------------------------------------------------------------------|
    // | **Taker**     | 442,325 = 500,000 - (Total Fees for Maker16102 + Total Fees for Maker6102) |
    //
    // ## Regaining Funds After a Failed Coinswap:
    //
    // | Participant    | Mining Fee for Contract txes (Sats) | Timelock Fee (Sats) | Total Recovery Fees (Sats) | Total Loss (Sats) |
    // |----------------|------------------------------------|---------------------|----------------------------|-------------------|
    // | **Taker**      | 3,000                              | 768                 | 3,768                      | 6,768             |
    // | **Maker16102** | 3,000                              | 768                 | 3,768                      | 6,768             |
    // | **Maker6102**  | 3,000                              | 768                 | 3,768                      | 6,768             |
    //
    // - Participants regain their initial funding amounts but incur a total loss of **6,768 sats** due to mining fees (recovery + initial transaction fees).

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
    //  This Maker is forwarding = 0.00465384 BTC to next Maker | Next maker's fees = 33500 | Miner fees covered by us = 1116
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

    block_generation_handle.join().unwrap();
}
