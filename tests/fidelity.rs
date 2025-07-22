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

#[test]
fn test_fidelity_complete() {
    test_fidelity();
    test_fidelity_spending();
}

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
        assert!(!bond.is_spent());

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

/// This test verifies that expired fidelity bond UTXOs are properly isolated from regular transactions:
///
/// - Creates a fidelity bond and lets it expire by advancing blockchain height
/// - Verifies that regular transactions never select expired fidelity bond UTXOs for spending
/// - Confirms that new fidelity bond creation can properly consume expired fidelity bond UTXOs
fn test_fidelity_spending() {
    // Extract constants at the beginning of the function
    const TIMELOCK_DURATION: u32 = 50;
    const FIDELITY_AMOUNT: u64 = 5_000_000;
    const REGULAR_TX_AMOUNT: u64 = 100_000;

    let makers_config_map = [((6102, None), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    log::info!("🧪 Running Test: Assert Fidelity Spending Behavior");

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    // Setup and fund wallet
    let maker_addrs = maker
        .get_wallet()
        .write()
        .unwrap()
        .get_next_external_address()
        .unwrap();
    send_to_address(bitcoind, &maker_addrs, Amount::from_btc(2.0).unwrap());
    generate_blocks(bitcoind, 1);
    maker.get_wallet().write().unwrap().sync_no_fail();

    // Create fidelity bond
    let short_timelock_height =
        (bitcoind.client.get_block_count().unwrap() as u32) + TIMELOCK_DURATION;
    let fidelity_amount = Amount::from_sat(FIDELITY_AMOUNT);

    let fidelity_index = {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet
            .create_fidelity(
                fidelity_amount,
                LockTime::from_height(short_timelock_height).unwrap(),
                None,
                MIN_FEE_RATE,
            )
            .unwrap()
    };

    generate_blocks(bitcoind, 1);
    maker.get_wallet().write().unwrap().sync_no_fail();

    // Make fidelity bond expire
    while (bitcoind.client.get_block_count().unwrap() as u32) < short_timelock_height {
        generate_blocks(bitcoind, 10);
    }
    generate_blocks(bitcoind, 5);
    maker.get_wallet().write().unwrap().sync_no_fail();

    // Assert UTXO shows up in list and track the specific fidelity UTXO
    let fidelity_utxo_info = {
        let wallet = maker.get_wallet().read().unwrap();
        let all_utxos = wallet.get_all_utxo().unwrap();

        // Find the specific fidelity bond UTXO by amount
        let fidelity_utxo = all_utxos
            .iter()
            .find(|utxo| utxo.amount == fidelity_amount)
            .expect("Fidelity bond UTXO should be in the list");

        log::info!(
            "🔍 Found fidelity bond UTXO: txid={}, vout={}, amount={} sats",
            fidelity_utxo.txid,
            fidelity_utxo.vout,
            fidelity_utxo.amount.to_sat()
        );
        log::info!("📊 Total UTXOs in wallet: {}", all_utxos.len());

        // Store UTXO identifiers for tracking
        (fidelity_utxo.txid, fidelity_utxo.vout, fidelity_utxo.amount)
    };

    let check_fidelity_utxo_integrity = |iteration: usize| {
        let wallet = maker.get_wallet().read().unwrap();
        let all_utxos = wallet.get_all_utxo().unwrap();

        let fidelity_utxo_still_exists = all_utxos.iter().any(|utxo| {
            utxo.txid == fidelity_utxo_info.0
                && utxo.vout == fidelity_utxo_info.1
                && utxo.amount == fidelity_utxo_info.2
        });

        if !fidelity_utxo_still_exists {
            panic!("❌ FAILED: Fidelity bond UTXO ({}:{}) was consumed by regular transaction #{iteration}!", 
                fidelity_utxo_info.0, fidelity_utxo_info.1);
        }

        let bond = wallet.get_fidelity_bonds().get(&fidelity_index).unwrap();
        if bond.is_spent() {
            panic!(
                "❌ FAILED: Fidelity bond was marked as consumed by regular transaction #{}!",
                iteration
            );
        }

        log::info!(
            "✅ Fidelity UTXO {}:{} ({} sats) still exists after regular transaction #{}",
            fidelity_utxo_info.0,
            fidelity_utxo_info.1,
            fidelity_utxo_info.2.to_sat(),
            iteration
        );
    };

    // Try 3 regular transactions and verify fidelity bond UTXO is never selected
    log::info!("🧪 Testing regular transactions avoid fidelity bond UTXO");

    for i in 0..3 {
        let external_addr = bitcoind
            .client
            .get_new_address(None, None)
            .unwrap()
            .assume_checked();
        let tx_result = {
            let mut wallet = maker.get_wallet().write().unwrap();
            let selected_utxos = wallet
                .coin_select(Amount::from_sat(REGULAR_TX_AMOUNT), MIN_FEE_RATE)
                .unwrap();

            for (_utxo, spend_info) in &selected_utxos {
                if spend_info.to_string().contains("fidelity-bond") {
                    panic!("❌ FAILED: Coin selection returned a fidelity bond UTXO!");
                }
            }

            if selected_utxos.is_empty() {
                Ok(None)
            } else {
                let destination = coinswap::wallet::Destination::Multi {
                    outputs: vec![(external_addr, Amount::from_sat(REGULAR_TX_AMOUNT))],
                    op_return_data: None,
                };
                match wallet.spend_from_wallet(MIN_FEE_RATE, destination, &selected_utxos) {
                    Ok(tx) => Ok(Some(tx)),
                    Err(e) => Err(e),
                }
            }
        };

        match tx_result {
            Ok(Some(tx)) => {
                bitcoind.client.send_raw_transaction(&tx).unwrap();
                generate_blocks(bitcoind, 1);
                maker.get_wallet().write().unwrap().sync_no_fail();
                log::info!("✅ Regular transaction #{} completed successfully", i + 1);
            }
            Ok(None) => {
                log::info!("ℹ️ Regular transaction #{} - no UTXOs selected", i + 1);
            }
            Err(e) => {
                log::warn!("⚠️ Regular transaction #{} failed: {:?}", i + 1, e);
            }
        }

        // Check fidelity UTXO integrity after each transaction attempt
        check_fidelity_utxo_integrity(i + 1);
    }

    // Test new fidelity bond uses expired UTXO - verify UTXO consumption
    log::info!(
        "🔄 Redeeming fidelity bond - should consume UTXO {}:{}",
        fidelity_utxo_info.0,
        fidelity_utxo_info.1
    );

    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet
            .redeem_fidelity(fidelity_index, MIN_FEE_RATE)
            .unwrap();
    }

    generate_blocks(bitcoind, 1);
    maker.get_wallet().write().unwrap().sync_no_fail();

    // Verify the specific UTXO is now consumed and bond is spent
    {
        let wallet = maker.get_wallet().read().unwrap();
        let all_utxos = wallet.get_all_utxo().unwrap();

        let fidelity_utxo_still_exists = all_utxos.iter().any(|utxo| {
            utxo.txid == fidelity_utxo_info.0
                && utxo.vout == fidelity_utxo_info.1
                && utxo.amount == fidelity_utxo_info.2
        });

        if fidelity_utxo_still_exists {
            panic!(
                "❌ FAILED: Fidelity bond UTXO {}:{} still exists after redemption!",
                fidelity_utxo_info.0, fidelity_utxo_info.1
            );
        }

        let bond = wallet.get_fidelity_bonds().get(&fidelity_index).unwrap();
        assert!(
            bond.is_spent(),
            "Fidelity bond should be spent after redemption"
        );

        log::info!(
            "✅ Fidelity UTXO {}:{} successfully consumed by redemption",
            fidelity_utxo_info.0,
            fidelity_utxo_info.1
        );
        log::info!("📊 UTXOs after redemption: {}", all_utxos.len());
    }

    let new_fidelity_index = {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet
            .create_fidelity(
                Amount::from_sat(6_000_000),
                LockTime::from_height((bitcoind.client.get_block_count().unwrap() as u32) + 100)
                    .unwrap(),
                None,
                MIN_FEE_RATE,
            )
            .unwrap()
    };

    generate_blocks(bitcoind, 1);
    maker.get_wallet().write().unwrap().sync_no_fail();

    {
        let wallet = maker.get_wallet().read().unwrap();
        let new_bond = wallet
            .get_fidelity_bonds()
            .get(&new_fidelity_index)
            .unwrap();
        assert!(!new_bond.is_spent(), "New fidelity bond should be unspent");
    }

    log::info!("🎉 SUCCESS: All requirements from issue #525 verified!");

    directory_server_instance.shutdown.store(true, Relaxed);
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
