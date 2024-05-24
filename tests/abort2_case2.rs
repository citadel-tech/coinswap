#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::SwapParams,
    utill::ConnectionType,
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{fs::File, io::Read, path::PathBuf, thread, time::Duration};

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
        ((6102, None), MakerBehavior::CloseAtReqContractSigsForSender),
        ((16102, None), MakerBehavior::Normal),
    ];

    warn!(
        "Running test: Maker 6102 Closes before sending sender's sigs. Taker recovers. Or Swap cancels"
    );
    warn!(
        "Running test: Maker 6102 Closes before sending sender's sigs. Taker recovers. Or Swap cancels"
    );

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, taker, makers, directory_server_instance) = TestFramework::init(
        None,
        makers_config_map.into(),
        None,
        ConnectionType::CLEARNET,
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

    let org_taker_balance = org_taker_balance_descriptor_utxo + org_taker_balance_swap_coins;

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

            (
                maker_balance_fidelity,
                maker_balance_descriptor_utxo,
                maker_balance_swap_coins,
                maker_balance_live_contract,
                maker_balance_descriptor_utxo + maker_balance_swap_coins,
            )
        })
        .collect::<Vec<_>>();

    // Spawn a Taker coinswap thread.
    let taker_clone = taker.clone();
    let taker_thread = thread::spawn(move || taker_clone.write().unwrap().do_coinswap(swap_params));

    // Wait for Taker swap thread to conclude.
    // The whole swap can fail if 6102 happens to be the first peer.
    // In that the swap isn't feasible, and user should modify SwapParams::maker_count.
    if let Err(e) = taker_thread.join().unwrap() {
        assert_eq!(format!("{:?}", e), "NotEnoughMakersInOfferBook".to_string());
        info!("Coinswap failed because the first maker rejected for signature");
    }

    // Wait for Maker threads to conclude.
    makers.iter().for_each(|maker| maker.shutdown().unwrap());
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // ---- After Swap checks ----

    let _ = directory_server_instance.shutdown();

    thread::sleep(Duration::from_secs(10));

    // Maker gets banned for being naughty.
    match taker.read().unwrap().config.connection_type {
        ConnectionType::CLEARNET => {
            assert_eq!(
                format!("127.0.0.1:{}", 6102),
                taker.read().unwrap().get_bad_makers()[0]
                    .address
                    .to_string()
            );
        }
        ConnectionType::TOR => {
            let onion_addr_path =
                PathBuf::from(format!("/tmp/tor-rust-maker{}/hs-dir/hostname", 6102));
            let mut file = File::open(onion_addr_path).unwrap();
            let mut onion_addr: String = String::new();
            file.read_to_string(&mut onion_addr).unwrap();
            onion_addr.pop();
            assert_eq!(
                format!("{}:{}", onion_addr, 6102),
                taker.read().unwrap().get_bad_makers()[0]
                    .address
                    .to_string()
            );
        }
    }

    all_utxos = taker.read().unwrap().get_wallet().get_all_utxo().unwrap();

    // Assert that Taker burned the mining fees,
    // Makers are fine.

    let new_taker_balance_descriptor_utxo = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_descriptor_utxo(Some(&all_utxos))
        .unwrap();
    let new_taker_balance_swap_coins = taker
        .read()
        .unwrap()
        .get_wallet()
        .balance_swap_coins(Some(&all_utxos))
        .unwrap();

    let new_taker_balance = new_taker_balance_descriptor_utxo + new_taker_balance_swap_coins;

    // Balance will not differ if the first maker drops and swap doesn't take place.
    // The recovery will happen only if the 2nd maker drops, which has 50% probabiltiy.
    // Only do this assert if the balance differs, implying that the swap took place.
    if new_taker_balance != org_taker_balance {
        assert_eq!(
            org_taker_balance - new_taker_balance,
            Amount::from_sat(6768)
        );
    }
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

            let new_balance = maker_balance_descriptor_utxo + maker_balance_swap_coins;

            assert_eq!(org_balance.4 - new_balance, Amount::from_sat(0));

            assert_eq!(maker_balance_fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(
                maker_balance_descriptor_utxo,
                Amount::from_btc(0.14999).unwrap()
            );
            assert_eq!(maker_balance_swap_coins, Amount::from_btc(0.0).unwrap());
            assert_eq!(maker_balance_live_contract, Amount::from_btc(0.0).unwrap());
        });

    test_framework.stop();
}
