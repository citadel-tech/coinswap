#![cfg(feature = "integration-test")]
use bitcoin::{absolute::LockTime, Amount};
use coinswap::{
    maker::{error::MakerError, start_maker_server, MakerBehavior},
    market::directory::{start_directory_server, DirectoryServer},
    wallet::{FidelityError, WalletError},
};

mod test_framework;
use test_framework::*;

use log::info;
use std::{sync::Arc, thread, time::Duration};

/// Test Fidelity Transactions
///
/// These tests covers
///  - Creation
///  - Redemption
///  - Valuations of Fidelity Bonds.
///
/// Fidelity Bonds can be created either via running the maker server or by calling the `create_fidelity()` API
/// on the wallet. Both of them are performed here. At the start of the maker server it will try to create a fidelity
/// bond with value and timelock provided in the configuration (default: value = 5_000_000 sats, locktime = 100 block).
///
/// Maker server will error if not enough balance is present to create fidelity bond.
/// A custom fidelity bond can be create using the `create_fidelity()` API.
#[tokio::test]
async fn test_fidelity() {
    // ---- Setup ----

    let makers_config_map = [((6102, 19051), MakerBehavior::Normal)];

    let (test_framework, _, makers) =
        TestFramework::init(None, makers_config_map.into(), None).await;

    info!("Initiating Directory Server .....");

    let directory_server_instance =
        Arc::new(DirectoryServer::init(Some(8080), Some(19060)).unwrap());
    let directory_server_instance_clone = directory_server_instance.clone();
    thread::spawn(move || {
        start_directory_server(directory_server_instance_clone);
    });

    let maker = makers.first().unwrap();

    // ----- Test -----

    // Give insufficient fund to maker and start the server.
    // This should return Error of Insufficient fund.
    let maker_addrs = maker
        .get_wallet()
        .write()
        .unwrap()
        .get_next_external_address()
        .unwrap();
    test_framework.send_to_address(&maker_addrs, Amount::from_btc(0.04).unwrap());
    test_framework.generate_1_block();

    let maker_clone = maker.clone();
    let maker_thread = thread::spawn(move || start_maker_server(maker_clone));

    thread::sleep(Duration::from_secs(5));
    maker.shutdown().unwrap();
    let expected_error = maker_thread.join().unwrap();

    matches!(
        expected_error.err().unwrap(),
        MakerError::Wallet(WalletError::Fidelity(FidelityError::InsufficientFund {
            available: 4000000,
            required: 5000000
        }))
    );

    // Give Maker more funds and check fidelity bond is created at the restart of server.
    test_framework.send_to_address(&maker_addrs, Amount::from_btc(0.04).unwrap());
    test_framework.generate_1_block();

    let maker_clone = maker.clone();
    let maker_thread = thread::spawn(move || start_maker_server(maker_clone));

    thread::sleep(Duration::from_secs(5));
    maker.shutdown().unwrap();

    let success = maker_thread.join().unwrap();

    assert!(success.is_ok());

    // Check fidelity bond created correctly
    let first_conf_height = {
        let wallet_read = maker.get_wallet().read().unwrap();
        let (index, bond, is_spent) = wallet_read
            .get_fidelity_bonds()
            .iter()
            .map(|(i, (b, _, is_spent))| (i, b, is_spent))
            .next()
            .unwrap();
        assert_eq!(*index, 0);
        assert_eq!(bond.amount, 5000000);
        assert!(!is_spent);
        bond.conf_height
    };

    // Create another fidelity bond of 1000000 sats
    let second_conf_height = {
        let mut wallet_write = maker.get_wallet().write().unwrap();
        let index = wallet_write
            .create_fidelity(
                Amount::from_sat(1000000),
                LockTime::from_height(test_framework.get_block_count() as u32 + 100).unwrap(),
            )
            .unwrap();
        assert_eq!(index, 1);
        let (bond, _, is_spent) = wallet_write
            .get_fidelity_bonds()
            .get(&index)
            .expect("bond expected");
        assert_eq!(bond.amount, 1000000);
        assert!(!is_spent);
        bond.conf_height
    };

    // Check the balances
    {
        let wallet = maker.get_wallet().read().unwrap();
        let normal_balance = wallet.balance(false, false).unwrap();
        assert_eq!(normal_balance.to_sat(), 1998000);
    }

    let (first_maturity_heigh, second_maturity_height) =
        (first_conf_height + 100, second_conf_height + 100);

    // Wait for maturity and then redeem the bonds
    loop {
        let current_height = test_framework.get_block_count() as u32;
        let required_height = first_maturity_heigh.max(second_maturity_height);
        if current_height < required_height {
            log::info!(
                "Waiting for maturity. Current height {}, required height: {}",
                current_height,
                required_height
            );
            thread::sleep(Duration::from_secs(10));
            continue;
        } else {
            log::info!("Fidelity is matured. sending redemption transactions");
            let mut wallet_write = maker.get_wallet().write().unwrap();
            let indexes = wallet_write
                .get_fidelity_bonds()
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            for i in indexes {
                wallet_write.redeem_fidelity(i).unwrap();
            }
            break;
        }
    }

    // Check the balances again
    {
        let wallet = maker.get_wallet().read().unwrap();
        let normal_balance = wallet.balance(false, false).unwrap();
        assert_eq!(normal_balance.to_sat(), 7996000);
    }

    // stop directory server
    let _ = directory_server_instance.shutdown();

    thread::sleep(Duration::from_secs(10));

    // Stop test and clean everything.
    // comment this line if you want the wallet directory and bitcoind to live. Can be useful for
    // after test debugging.
    test_framework.stop();
}
