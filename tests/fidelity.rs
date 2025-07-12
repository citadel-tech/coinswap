#![cfg(feature = "integration-test")]
use bitcoin::{absolute::LockTime, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::TakerBehavior,
    utill::{ConnectionType, MIN_FEE_RATE},
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
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    log::info!("🧪 Running Test: Fidelity Bond Creation and Redemption");

    let bitcoind = &test_framework.bitcoind;

    let maker = makers.first().unwrap();

    // ----- Test -----

    log::info!("💰 Providing insufficient funds to trigger funding request");
    // Provide insufficient funds to the maker and start the server.
    // This will continuously log about insufficient funds and request 0.01 BTC to create a fidelity bond.
    let maker_addrs = maker
        .get_wallet()
        .write()
        .unwrap()
        .get_next_external_address()
        .unwrap();
    send_to_address(bitcoind, &maker_addrs, Amount::from_btc(0.04).unwrap());
    generate_blocks(bitcoind, 1);

    // Add sync and verification before starting maker server
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
        let balances = wallet.get_balances().unwrap();
        log::info!(
            "📊 Initial wallet balance: {} sats",
            balances.regular.to_sat()
        );
    }

    let maker_clone = maker.clone();

    log::info!("🚀 Starting maker server with insufficient funds");
    let maker_thread = thread::spawn(move || start_maker_server(maker_clone));

    thread::sleep(Duration::from_secs(6));

    test_framework.assert_log(
        "Send at least 0.01000000 BTC to",
        "/tmp/coinswap/taker/debug.log",
    );

    log::info!("💰 Adding sufficient funds for fidelity bond creation");
    // Provide the maker with more funds.
    send_to_address(bitcoind, &maker_addrs, Amount::ONE_BTC);
    generate_blocks(bitcoind, 1);

    // Add sync and verification after adding more funds
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
        let balances = wallet.get_balances().unwrap();
        log::info!(
            "📊 Updated wallet balance: {} sats",
            balances.regular.to_sat()
        );
    }

    thread::sleep(Duration::from_secs(6));
    // stop the maker server
    maker.shutdown.store(true, Relaxed);

    let _ = maker_thread.join().unwrap();

    // Assert that successful fidelity bond creation is logged
    test_framework.assert_log(
        "Successfully created fidelity bond",
        "/tmp/coinswap/taker/debug.log",
    );

    log::info!("🔗 Verifying first fidelity bond creation");
    // Verify that the fidelity bond is created correctly.
    let first_maturity_height = {
        let wallet_read = maker.get_wallet().read().unwrap();

        // Get the index of the bond with the highest value,
        // which should be 0 as there is only one fidelity bond.
        let highest_bond_index = wallet_read.get_highest_fidelity_index().unwrap().unwrap();
        assert_eq!(highest_bond_index, 0);

        let bond = wallet_read
            .get_fidelity_bonds()
            .get(&highest_bond_index)
            .unwrap();
        let bond_value = wallet_read.calculate_bond_value(bond).unwrap();
        assert_eq!(bond_value, Amount::from_sat(10814));

        let bond = wallet_read
            .get_fidelity_bonds()
            .get(&highest_bond_index)
            .unwrap();
        assert_eq!(bond.amount, Amount::from_sat(5000000));
        assert!(!bond.is_spent());
        // Log the bond details for debugging
        log::info!(
            "📊 First bond created - Amount: {}, Value: {}, Maturity Height: {}",
            bond.amount.to_sat(),
            bond_value.to_sat(),
            bond.lock_time.to_consensus_u32()
        );

        bond.lock_time.to_consensus_u32()
    };

    log::info!("🔗 Creating second fidelity bond with higher amount");
    // Create another fidelity bond of 0.08 BTC and validate it.
    let second_maturity_height = {
        log::info!("Creating another fidelity bond using the `create_fidelity` API");
        let index = maker
            .get_wallet()
            .write()
            .unwrap()
            .create_fidelity(
                Amount::from_sat(8000000),
                LockTime::from_height((bitcoind.client.get_block_count().unwrap() as u32) + 950)
                    .unwrap(),
                None,
                MIN_FEE_RATE,
            )
            .unwrap();

        let wallet_read = maker.get_wallet().read().unwrap();

        // Since this bond has a larger amount than the first, it should now be the highest value bond.
        // Note: We test for highest bond rather than exact values to avoid timing-dependent failures.
        let highest_bond_index = wallet_read.get_highest_fidelity_index().unwrap().unwrap();
        assert_eq!(highest_bond_index, index);

        let bond = wallet_read.get_fidelity_bonds().get(&index).unwrap();
        assert_eq!(bond.amount, Amount::from_sat(8000000));
        (assert!(!bond.is_spent()));

        bond.lock_time.to_consensus_u32()
    };

    log::info!("📊 Verifying balances with both fidelity bonds");
    // Verify balances
    {
        let wallet_read = maker.get_wallet().read().unwrap();

        let balances = wallet_read.get_balances().unwrap();

        assert_eq!(balances.fidelity.to_sat(), 13000000);
        assert_eq!(balances.regular.to_sat(), 90999206);
    }

    log::info!("⏳ Waiting for fidelity bonds to mature and testing redemption");
    // Wait for the bonds to mature, redeem them, and validate the process.
    let mut required_height = first_maturity_height;

    loop {
        let current_height = bitcoind.client.get_block_count().unwrap() as u32;

        if current_height < required_height {
            log::info!(
                "⏳ Waiting for bond maturity. Current height: {current_height}, required height: {required_height}",
            );
            thread::sleep(Duration::from_secs(10));
        } else {
            let mut wallet_write = maker.get_wallet().write().unwrap();

            if required_height == first_maturity_height {
                log::info!("🔓 First Fidelity Bond is matured. Sending redemption transaction");

                wallet_write.redeem_fidelity(0, MIN_FEE_RATE).unwrap();

                log::info!("✅ First Fidelity Bond is successfully redeemed");

                // The second bond should now be the highest value bond.
                let highest_bond_index =
                    wallet_write.get_highest_fidelity_index().unwrap().unwrap();
                assert_eq!(highest_bond_index, 1);

                // Wait for the second bond to mature.
                required_height = second_maturity_height;
            } else {
                log::info!("🔓 Second Fidelity Bond is matured. Sending redemption transaction");

                wallet_write.redeem_fidelity(1, MIN_FEE_RATE).unwrap();

                log::info!("✅ Second Fidelity Bond is successfully redeemed");

                // There should now be no unspent bonds left.
                let index = wallet_write.get_highest_fidelity_index().unwrap();
                assert_eq!(index, None);
                break;
            }
        }
    }

    thread::sleep(Duration::from_secs(10));

    log::info!("🔄 Syncing wallet after redemptions");
    let sync_handle = thread::spawn({
        // Clone or move the necessary reference to the maker.
        let maker = maker.clone();
        move || {
            let mut maker_write_wallet = maker.get_wallet().write().unwrap();
            maker_write_wallet.sync_and_save().unwrap();
        }
    });

    // Wait for the sync thread to finish.
    sync_handle.join().unwrap();

    log::info!("📊 Verifying final balances after all bonds redeemed");
    // Verify the balances again after all bonds are redeemed.
    {
        let wallet_read = maker.get_wallet().read().unwrap();
        let balances = wallet_read.get_balances().unwrap();

        assert_eq!(balances.fidelity.to_sat(), 0);
        assert_eq!(balances.regular.to_sat(), 103998762);
    }

    // Stop the directory server.
    directory_server_instance.shutdown.store(true, Relaxed);

    thread::sleep(Duration::from_secs(10));

    test_framework.stop();
    block_generation_handle.join().unwrap();

    log::info!("🎉 Fidelity bond lifecycle test completed successfully");
}
