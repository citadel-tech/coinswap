#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    market::directory::{start_directory_server, DirectoryServer},
    taker::SwapParams,
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{fs::File, io::Read, path::PathBuf, sync::Arc, thread, time::Duration};

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
            (6102, 19051),
            MakerBehavior::CloseAtReqContractSigsForSender,
        ),
        ((16102, 19052), MakerBehavior::Normal),
        ((26102, 19053), MakerBehavior::Normal),
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
        Arc::new(DirectoryServer::init(Some(8080), Some(19060)).unwrap());
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
    test_framework.generate_1_block();

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
    thread::sleep(Duration::from_secs(360)); // Take a delay because Makers take time to fully setup.
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
        let onion_addr_path = PathBuf::from(format!("/tmp/tor-rust-maker{}/hs-dir/hostname", 6102));
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

    // Stop test and clean everything.
    // comment this line if you want the wallet directory and bitcoind to live. Can be useful for
    // after test debugging.
    test_framework.stop();
}
