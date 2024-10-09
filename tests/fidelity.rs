#![cfg(feature = "integration-test")]
use bitcoin::{absolute::LockTime, Amount};
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    utill::ConnectionType,
};

mod test_framework;
use test_framework::*;

use std::{assert_eq, sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test Fidelity Bond Creation and Redemption
///
/// This test covers the full lifecycle of Fidelity Bonds, including creation, valuation, and redemption:
///
/// - The Maker starts with insufficient funds to create a fidelity bond (0.04 BTC),
///   triggering log messages requesting more funds.
/// - Once provided with sufficient funds (1 BTC), the Maker creates the first fidelity bond (0.05 BTC).
/// - A second fidelity bond (0.08 BTC) is created and its higher value is verified.
/// - The test simulates bond maturity by advancing the blockchain height and redeems them sequentially,
///   verifying correct balances and proper bond status updates after redemption.
#[test]
fn test_fidelity() {
    // ---- Setup ----
    let makers_config_map = [((6102, None), MakerBehavior::Normal)];

    let (test_framework, _, makers, directory_server_instance) = TestFramework::init(
        None,
        makers_config_map.into(),
        None,
        ConnectionType::CLEARNET,
    );

    let maker = makers.first().unwrap();

    // ----- Test -----

    // Provide insufficient funds to the maker and start the server.
    // This will continuously log about insufficient funds and request 0.01 BTC to create a fidelity bond.
    let maker_addrs = maker
        .get_wallet()
        .write()
        .unwrap()
        .get_next_external_address()
        .unwrap();
    test_framework.send_to_address(&maker_addrs, Amount::from_btc(0.04).unwrap());

    test_framework.generate_blocks(1);

    let maker_clone = maker.clone();

    let maker_thread = thread::spawn(move || start_maker_server(maker_clone));

    thread::sleep(Duration::from_secs(12));
    maker.shutdown().unwrap();
    let _ = maker_thread.join().unwrap();

    // TODO: Assert that fund request for fidelity is printed in the log.
    maker.shutdown.store(false, Relaxed);

    // Provide the maker with more funds.
    test_framework.send_to_address(&maker_addrs, Amount::ONE_BTC);
    test_framework.generate_blocks(1);

    let maker_clone = maker.clone();

    let maker_thread = thread::spawn(move || start_maker_server(maker_clone));

    thread::sleep(Duration::from_secs(1));
    maker.shutdown().unwrap();

    let _ = maker_thread.join().unwrap();

    // Verify that the fidelity bond is created correctly.
    let first_maturity_height = {
        let wallet_read = maker.get_wallet().read().unwrap();

        // Get the index of the bond with the highest value,
        // which should be 0 as there is only one fidelity bond.
        let highest_bond_index = wallet_read.get_highest_fidelity_index().unwrap().unwrap();
        assert_eq!(highest_bond_index, 0);

        let bond_value = wallet_read
            .calculate_bond_value(highest_bond_index)
            .unwrap();
        assert_eq!(bond_value, Amount::from_sat(550));

        let (bond, _, is_spent) = wallet_read
            .get_fidelity_bonds()
            .get(&highest_bond_index)
            .unwrap();

        assert_eq!(bond.amount, Amount::from_sat(5000000));
        assert!(!is_spent);

        bond.lock_time.to_consensus_u32()
    };

    // Create another fidelity bond of 0.08 BTC and validate it.
    let second_maturity_height = {
        let mut wallet_write = maker.get_wallet().write().unwrap();

        let index = wallet_write
            .create_fidelity(
                Amount::from_sat(8000000),
                LockTime::from_height((test_framework.get_block_count() as u32) + 150).unwrap(),
            )
            .unwrap();

        // Since this bond has a larger amount than the first, it should now be the highest value bond.
        let highest_bond_index = wallet_write.get_highest_fidelity_index().unwrap().unwrap();
        assert_eq!(highest_bond_index, index);

        let bond_value = wallet_write.calculate_bond_value(index).unwrap();
        assert_eq!(bond_value, Amount::from_sat(1801));

        let (bond, _, is_spent) = wallet_write.get_fidelity_bonds().get(&index).unwrap();
        assert_eq!(bond.amount, Amount::from_sat(8000000));
        assert!(!is_spent);

        bond.lock_time.to_consensus_u32()
    };

    // Verify balances
    {
        let mut wallet_write = maker.get_wallet().write().unwrap();

        // Sync the wallet to get accurate balances.
        wallet_write.sync().unwrap();

        let fidelity_balance = wallet_write.balance_fidelity_bonds(None).unwrap();
        let seed_balance = wallet_write.balance_descriptor_utxo(None).unwrap();

        assert_eq!(fidelity_balance.to_sat(), 13000000);
        assert_eq!(seed_balance.to_sat(), 90998000);
    }

    // Wait for the bonds to mature, redeem them, and validate the process.
    let mut required_height = first_maturity_height;

    loop {
        let current_height = test_framework.get_block_count() as u32;

        if current_height < required_height {
            log::info!(
                "Waiting for bond maturity. Current height: {}, required height: {}",
                current_height,
                required_height
            );

            thread::sleep(Duration::from_secs(10));
        } else {
            let mut wallet_write = maker.get_wallet().write().unwrap();

            if required_height == first_maturity_height {
                log::info!("First Fidelity Bond  is matured. Sending redemption transaction");

                let _ = wallet_write.redeem_fidelity(0).unwrap();

                log::info!("First Fidelity Bond is successfully redeemed.");

                // The second bond should now be the highest value bond.
                let highest_bond_index =
                    wallet_write.get_highest_fidelity_index().unwrap().unwrap();
                assert_eq!(highest_bond_index, 1);

                // Wait for the second bond to mature.
                required_height = second_maturity_height;
            } else {
                log::info!("Second Fidelity Bond  is matured. sending redemption transactions");

                let _ = wallet_write.redeem_fidelity(1).unwrap();

                log::info!("Second Fidelity Bond is successfully redeemed.");

                // There should now be no unspent bonds left.
                let index = wallet_write.get_highest_fidelity_index().unwrap();
                assert_eq!(index, None);
                break;
            }
        }
    }

    // Verify the balances again after all bonds are redeemed.
    {
        let wallet_read = maker.get_wallet().read().unwrap();
        let fidelity_balance = wallet_read.balance_fidelity_bonds(None).unwrap();
        let seed_balance = wallet_read.balance_descriptor_utxo(None).unwrap();

        assert_eq!(fidelity_balance.to_sat(), 0);
        assert_eq!(seed_balance.to_sat(), 103996000);
    }

    // Stop the directory server.
    let _ = directory_server_instance.shutdown();

    thread::sleep(Duration::from_secs(10));

    test_framework.stop();
}
