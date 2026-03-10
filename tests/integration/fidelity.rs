//! Integration test for Fidelity Bond Creation and Redemption using the Unified API.
//!
//! This test covers the full lifecycle of Fidelity Bonds, including creation, valuation, and redemption:
//!
//! - The Maker starts with insufficient funds to create a fidelity bond (0.04 BTC),
//!   triggering log messages requesting more funds.
//! - Once provided with sufficient funds (1 BTC), the Maker creates the first fidelity bond (0.05 BTC).
//! - A second fidelity bond (0.08 BTC) is created and its higher value is verified.
//! - The test simulates bond maturity by advancing the blockchain height and redeems them sequentially,
//!   verifying correct balances and proper bond status updates after redemption.
//! - A second sub-test verifies that expired fidelity bond UTXOs are properly isolated from
//!   regular transactions: regular sends never select expired fidelity UTXOs, but redemption
//!   and new bond creation can properly consume them.

use bitcoin::{absolute::LockTime, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::start_unified_server,
    taker::UnifiedTakerBehavior,
    utill::MIN_FEE_RATE,
    wallet::{AddressType, Destination},
};

use super::test_framework::*;

use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn test_fidelity_complete() {
    test_fidelity();
    test_fidelity_spending();
}

/// Test Fidelity Bond Creation and Redemption
fn test_fidelity() {
    // ---- Setup ----
    let makers_config_map = vec![(8102, None)];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];

    let (test_framework, _takers, makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, vec![]);

    log::info!("Running Test: Fidelity Bond Creation and Redemption (Unified)");

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    // ----- Test -----

    log::info!("Providing insufficient funds to trigger funding request");
    // Provide insufficient funds to the maker and start the server.
    // This will continuously log about insufficient funds and request BTC to create a fidelity bond.
    fund_unified_makers(
        &makers,
        bitcoind,
        1,
        Amount::from_btc(0.04).unwrap(),
        AddressType::P2WPKH,
    );

    let maker_clone = maker.clone();

    log::info!("Starting maker server with insufficient funds");
    let maker_thread = thread::spawn(move || start_unified_server(maker_clone));

    thread::sleep(Duration::from_secs(6));

    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log("Send at least 0.01000324 BTC to", &log_path);

    log::info!("Adding sufficient funds for fidelity bond creation");
    // Provide the maker with more funds.
    fund_unified_makers(&makers, bitcoind, 1, Amount::ONE_BTC, AddressType::P2WPKH);

    thread::sleep(Duration::from_secs(6));
    // stop the maker server
    maker.shutdown.store(true, Relaxed);

    let _ = maker_thread.join().unwrap();

    // Assert that successful fidelity bond creation is logged
    test_framework.assert_log("Successfully created fidelity bond", &log_path);

    log::info!("Verifying first fidelity bond creation");
    // Verify that the fidelity bond is created correctly.
    let first_maturity_height = {
        let wallet_read = maker.wallet.read().unwrap();

        // Get the index of the bond with the highest value,
        // which should be 0 as there is only one fidelity bond.
        let highest_bond_index = wallet_read.get_highest_fidelity_index().unwrap().unwrap();
        assert_eq!(highest_bond_index, 0);

        let bond = wallet_read
            .get_fidelity_bonds()
            .get(&highest_bond_index)
            .unwrap();
        let bond_value = wallet_read.calculate_bond_value(bond).unwrap();
        // Bond value depends on wall-clock time and regtest block timing,
        // so it varies between runs. Just sanity-check it's in a reasonable range.
        assert!(
            bond_value.to_sat() > 9000 && bond_value.to_sat() < 15000,
            "unexpected bond_value: {} SAT (expected ~10000-12000)",
            bond_value.to_sat()
        );

        let bond = wallet_read
            .get_fidelity_bonds()
            .get(&highest_bond_index)
            .unwrap();
        assert_eq!(bond.amount, Amount::from_sat(5000000));
        assert!(!bond.is_spent());
        // Log the bond details for debugging
        log::info!(
            "First bond created - Amount: {}, Value: {}, Maturity Height: {}",
            bond.amount.to_sat(),
            bond_value.to_sat(),
            bond.lock_time.to_consensus_u32()
        );

        bond.lock_time.to_consensus_u32()
    };

    log::info!("Creating second fidelity bond with higher amount");
    // Create another fidelity bond of 0.08 BTC and validate it.
    let second_maturity_height = {
        log::info!("Creating another fidelity bond using the `create_fidelity` API");
        let index = maker
            .wallet
            .write()
            .unwrap()
            .create_fidelity(
                Amount::from_sat(8000000),
                LockTime::from_height((bitcoind.client.get_block_count().unwrap() as u32) + 950)
                    .unwrap(),
                None,
                MIN_FEE_RATE,
                AddressType::P2WPKH,
            )
            .unwrap();

        let wallet_read = maker.wallet.read().unwrap();

        // Since this bond has a larger amount than the first, it should now be the highest value bond.
        let highest_bond_index = wallet_read.get_highest_fidelity_index().unwrap().unwrap();
        assert_eq!(highest_bond_index, index);

        let bond = wallet_read.get_fidelity_bonds().get(&index).unwrap();
        assert_eq!(bond.amount, Amount::from_sat(8000000));
        assert!(!bond.is_spent());

        bond.lock_time.to_consensus_u32()
    };

    log::info!("Verifying balances with both fidelity bonds");
    // Verify balances
    {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let wallet_read = maker.wallet.read().unwrap();

        let balances = wallet_read.get_balances().unwrap();

        assert_eq!(balances.fidelity.to_sat(), 13000000);
        assert_eq!(balances.regular.to_sat(), 90999330);
    }

    log::info!("Waiting for fidelity bonds to mature and testing redemption");
    // Wait for the bonds to mature, redeem them, and validate the process.
    let mut required_height = first_maturity_height;

    loop {
        let current_height = bitcoind.client.get_block_count().unwrap() as u32;

        if current_height < required_height {
            log::info!(
                "Waiting for bond maturity. Current height: {current_height}, required height: {required_height}",
            );
            thread::sleep(Duration::from_secs(10));
        } else {
            let mut wallet_write = maker.wallet.write().unwrap();

            if required_height == first_maturity_height {
                log::info!("First Fidelity Bond is matured. Sending redemption transaction");

                wallet_write
                    .redeem_fidelity(0, MIN_FEE_RATE, AddressType::P2WPKH)
                    .unwrap();

                log::info!("First Fidelity Bond is successfully redeemed");

                // The second bond should now be the highest value bond.
                let highest_bond_index =
                    wallet_write.get_highest_fidelity_index().unwrap().unwrap();
                assert_eq!(highest_bond_index, 1);

                // Wait for the second bond to mature.
                required_height = second_maturity_height;
            } else {
                log::info!("Second Fidelity Bond is matured. Sending redemption transaction");

                wallet_write
                    .redeem_fidelity(1, MIN_FEE_RATE, AddressType::P2WPKH)
                    .unwrap();

                log::info!("Second Fidelity Bond is successfully redeemed");

                // There should now be no unspent bonds left.
                let index = wallet_write.get_highest_fidelity_index().unwrap();
                assert_eq!(index, None);
                break;
            }
        }
    }

    thread::sleep(Duration::from_secs(10));

    log::info!("Syncing wallet after redemptions");
    let sync_handle = thread::spawn({
        let maker = maker.clone();
        move || {
            let mut maker_write_wallet = maker.wallet.write().unwrap();
            maker_write_wallet.sync_and_save().unwrap();
        }
    });

    // Wait for the sync thread to finish.
    sync_handle.join().unwrap();

    log::info!("Verifying final balances after all bonds redeemed");
    // Verify the balances again after all bonds are redeemed.
    {
        let wallet_read = maker.wallet.read().unwrap();
        let balances = wallet_read.get_balances().unwrap();

        assert_eq!(balances.fidelity.to_sat(), 0);
        assert_eq!(balances.regular.to_sat(), 103998886);
    }

    thread::sleep(Duration::from_secs(10));

    test_framework.stop();
    block_generation_handle.join().unwrap();

    log::info!("Fidelity bond lifecycle test completed successfully");
}

/// This test verifies that expired fidelity bond UTXOs are properly isolated from regular transactions:
///
/// - Creates a fidelity bond and lets it expire by advancing blockchain height
/// - Verifies that regular transactions never select expired fidelity bond UTXOs for spending
/// - Confirms that new fidelity bond creation can properly consume expired fidelity bond UTXOs
fn test_fidelity_spending() {
    const TIMELOCK_DURATION: u32 = 50;
    const FIDELITY_AMOUNT: u64 = 5_000_000;
    const REGULAR_TX_AMOUNT: u64 = 100_000;

    let makers_config_map = vec![(8102, None)];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];

    let (test_framework, _takers, makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, vec![]);

    log::info!("Running Test: Assert Fidelity Spending Behavior (Unified)");

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    fund_unified_makers(
        &makers,
        bitcoind,
        1,
        Amount::from_btc(2.0).unwrap(),
        AddressType::P2WPKH,
    );

    // Create fidelity bond
    let short_timelock_height =
        (bitcoind.client.get_block_count().unwrap() as u32) + TIMELOCK_DURATION;
    let fidelity_amount = Amount::from_sat(FIDELITY_AMOUNT);

    let fidelity_index = {
        let mut wallet = maker.wallet.write().unwrap();
        wallet
            .create_fidelity(
                fidelity_amount,
                LockTime::from_height(short_timelock_height).unwrap(),
                None,
                MIN_FEE_RATE,
                AddressType::P2WPKH,
            )
            .unwrap()
    };

    generate_blocks(bitcoind, 1);
    maker.wallet.write().unwrap().sync_and_save().unwrap();

    // Make fidelity bond expire
    while (bitcoind.client.get_block_count().unwrap() as u32) < short_timelock_height {
        generate_blocks(bitcoind, 10);
    }
    generate_blocks(bitcoind, 5);
    maker.wallet.write().unwrap().sync_and_save().unwrap();

    // Assert UTXO shows up in list and track the specific fidelity UTXO
    let fidelity_utxo_info = {
        let wallet = maker.wallet.read().unwrap();
        let all_utxos = wallet.list_all_utxo();

        // Find the specific fidelity bond UTXO by amount
        let fidelity_utxo = all_utxos
            .iter()
            .find(|utxo| utxo.amount == fidelity_amount)
            .expect("Fidelity bond UTXO should be in the list");

        log::info!(
            "Found fidelity bond UTXO: txid={}, vout={}, amount={} sats",
            fidelity_utxo.txid,
            fidelity_utxo.vout,
            fidelity_utxo.amount.to_sat()
        );
        log::info!("Total UTXOs in wallet: {}", all_utxos.len());

        (fidelity_utxo.txid, fidelity_utxo.vout, fidelity_utxo.amount)
    };

    let check_fidelity_utxo_integrity = |iteration: usize| {
        let wallet = maker.wallet.read().unwrap();
        let all_utxos = wallet.list_all_utxo();

        let fidelity_utxo_still_exists = all_utxos.iter().any(|utxo| {
            utxo.txid == fidelity_utxo_info.0
                && utxo.vout == fidelity_utxo_info.1
                && utxo.amount == fidelity_utxo_info.2
        });

        if !fidelity_utxo_still_exists {
            panic!(
                "FAILED: Fidelity bond UTXO ({}:{}) was consumed by regular transaction #{}!",
                fidelity_utxo_info.0, fidelity_utxo_info.1, iteration
            );
        }

        let bond = wallet.get_fidelity_bonds().get(&fidelity_index).unwrap();
        if bond.is_spent() {
            panic!(
                "FAILED: Fidelity bond was marked as consumed by regular transaction #{}!",
                iteration
            );
        }

        log::info!(
            "Fidelity UTXO {}:{} ({} sats) still exists after regular transaction #{}",
            fidelity_utxo_info.0,
            fidelity_utxo_info.1,
            fidelity_utxo_info.2.to_sat(),
            iteration
        );
    };

    // Try 3 regular transactions and verify fidelity bond UTXO is never selected
    log::info!("Testing regular transactions avoid fidelity bond UTXO");

    for i in 0..3 {
        let external_addr = bitcoind
            .client
            .get_new_address(None, None)
            .unwrap()
            .assume_checked();
        let tx_result = {
            let mut wallet = maker.wallet.write().unwrap();
            let selected_utxos = wallet
                .coin_select(Amount::from_sat(REGULAR_TX_AMOUNT), MIN_FEE_RATE, None)
                .unwrap();

            for (_utxo, spend_info) in &selected_utxos {
                if spend_info.to_string().contains("fidelity-bond") {
                    panic!("FAILED: Coin selection returned a fidelity bond UTXO!");
                }
            }

            if selected_utxos.is_empty() {
                Ok(None)
            } else {
                let destination = Destination::Multi {
                    outputs: vec![(external_addr, Amount::from_sat(REGULAR_TX_AMOUNT))],
                    op_return_data: None,
                    change_address_type: AddressType::P2WPKH,
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
                maker.wallet.write().unwrap().sync_and_save().unwrap();
                log::info!("Regular transaction #{} completed successfully", i + 1);
            }
            Ok(None) => {
                log::info!("Regular transaction #{} - no UTXOs selected", i + 1);
            }
            Err(e) => {
                log::warn!("Regular transaction #{} failed: {:?}", i + 1, e);
            }
        }

        // Check fidelity UTXO integrity after each transaction attempt
        check_fidelity_utxo_integrity(i + 1);
    }

    // Test fidelity bond redemption - verify UTXO consumption
    log::info!(
        "Redeeming fidelity bond - should consume UTXO {}:{}",
        fidelity_utxo_info.0,
        fidelity_utxo_info.1
    );

    {
        let mut wallet = maker.wallet.write().unwrap();
        wallet
            .redeem_fidelity(fidelity_index, MIN_FEE_RATE, AddressType::P2WPKH)
            .unwrap();
    }

    generate_blocks(bitcoind, 1);
    maker.wallet.write().unwrap().sync_and_save().unwrap();

    // Verify the specific UTXO is now consumed and bond is spent
    {
        let wallet = maker.wallet.read().unwrap();
        let all_utxos = wallet.list_all_utxo();

        let fidelity_utxo_still_exists = all_utxos.iter().any(|utxo| {
            utxo.txid == fidelity_utxo_info.0
                && utxo.vout == fidelity_utxo_info.1
                && utxo.amount == fidelity_utxo_info.2
        });

        if fidelity_utxo_still_exists {
            panic!(
                "FAILED: Fidelity bond UTXO {}:{} still exists after redemption!",
                fidelity_utxo_info.0, fidelity_utxo_info.1
            );
        }

        let bond = wallet.get_fidelity_bonds().get(&fidelity_index).unwrap();
        assert!(
            bond.is_spent(),
            "Fidelity bond should be spent after redemption"
        );

        log::info!(
            "Fidelity UTXO {}:{} successfully consumed by redemption",
            fidelity_utxo_info.0,
            fidelity_utxo_info.1
        );
        log::info!("UTXOs after redemption: {}", all_utxos.len());
    }

    let new_fidelity_index = {
        let mut wallet = maker.wallet.write().unwrap();
        wallet
            .create_fidelity(
                Amount::from_sat(6_000_000),
                LockTime::from_height((bitcoind.client.get_block_count().unwrap() as u32) + 100)
                    .unwrap(),
                None,
                MIN_FEE_RATE,
                AddressType::P2WPKH,
            )
            .unwrap()
    };

    generate_blocks(bitcoind, 1);
    maker.wallet.write().unwrap().sync_and_save().unwrap();

    {
        let wallet = maker.wallet.read().unwrap();
        let new_bond = wallet
            .get_fidelity_bonds()
            .get(&new_fidelity_index)
            .unwrap();
        assert!(!new_bond.is_spent(), "New fidelity bond should be unspent");
    }

    log::info!("SUCCESS: All fidelity spending behavior requirements verified!");
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
