#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    market::directory::{start_directory_server, DirectoryServer},
    taker::SwapParams,
    utill::ConnectionType,
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::Arc, thread, time::Duration};

/// ABORT 2: Maker Drops Before Setup
/// This test demonstrates the situation where a Maker prematurely drops connections after doing
/// initial protocol handshake. This should not necessarily disrupt the round, the Taker will try to find
/// more makers in his address book and carry on as usual. The Taker will mark this Maker as "bad" and will
/// not swap this maker again.
///
/// CASE 1: Maker Drops Before Sending Sender's Signature, and Taker carries on with a new Maker.
#[tokio::test]
async fn test_abort_case_2_move_on_with_other_makers() {
    // ---- Setup ----

    // 6102 is naughty. But theres enough good ones.
    let makers_config_map = [
        (
            (6102, 19051, ConnectionType::CLEARNET),
            MakerBehavior::CloseAtReqContractSigsForSender,
        ),
        (
            (16102, 19052, ConnectionType::CLEARNET),
            MakerBehavior::Normal,
        ),
        (
            (26102, 19053, ConnectionType::CLEARNET),
            MakerBehavior::Normal,
        ),
    ];

    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, taker, makers) =
        TestFramework::init(None, makers_config_map.into(), None).await;

    warn!(
        "Running Test: Maker 6102 closes before sending sender's sigs. Taker moves on with other Makers."
    );

    info!("Initiating Directory Server .....");

    let directory_server_instance =
        Arc::new(DirectoryServer::new(None, Some(ConnectionType::CLEARNET)).unwrap());
    let directory_server_instance_clone = directory_server_instance.clone();
    thread::spawn(move || {
        start_directory_server(directory_server_instance_clone);
    });

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
    makers.iter().for_each(|maker| maker.shutdown().unwrap());
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // ---- After Swap checks ----

    let _ = directory_server_instance.shutdown();

    thread::sleep(Duration::from_secs(10));

    // TODO: Do balance assertions.

    // Maker might not get banned as Taker may not try 6102 for swap. If it does then check its 6102.
    if !taker.read().unwrap().get_bad_makers().is_empty() {
        assert_eq!(
            format!("127.0.0.1:{}", 6102),
            taker.read().unwrap().get_bad_makers()[0]
                .address
                .to_string()
        );
    }

    test_framework.stop();
}
