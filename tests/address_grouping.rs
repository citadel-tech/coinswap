#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::MakerBehavior,
    taker::TakerBehavior,
    utill::{ConnectionType, MIN_FEE_RATE},
    wallet::Destination,
};
mod test_framework;
use test_framework::*;

use std::sync::atomic::Ordering::Relaxed;

/// Test for Address Grouping Behavior in Coin Selection
///
/// Verifies that when multiple UTXOs exist at the same address,
/// coin selection groups them together and spends them as a unit.
#[test]
fn test_address_grouping_behavior() {
    // ---- Setup ----
    let makers_config_map = [((6102, None), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    log::info!("🧪 Testing Address Grouping Behavior");

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    // Setup wallet and get address for transactions
    let same_address = {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.get_next_external_address().unwrap()
    };

    log::info!("📍 Using address: {same_address}");

    // Send multiple transactions to same address
    send_to_address(bitcoind, &same_address, Amount::from_btc(1.0).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &same_address, Amount::from_btc(0.5).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &same_address, Amount::from_btc(0.3).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &same_address, Amount::from_btc(0.2).unwrap());
    generate_blocks(bitcoind, 1);

    // Sync wallet and verify UTXOs
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
        let balances = wallet.get_balances().unwrap();
        log::info!("📊 Wallet balance: {} sats", balances.regular.to_sat());
    }

    let (utxo_count, utxo_details) = {
        let wallet = maker.get_wallet().read().unwrap();
        let all_utxos = wallet.get_all_utxo().unwrap();

        // Collect UTXO details for verification
        let details: Vec<_> = all_utxos
            .iter()
            .map(|utxo| (utxo.txid, utxo.vout, utxo.amount))
            .collect();

        log::info!("📊 Total UTXOs: {}", all_utxos.len());
        (all_utxos.len(), details)
    };

    assert_eq!(
        utxo_count, 4,
        "Should have exactly 4 UTXOs from our transactions"
    );

    // Test coin selection with small amount
    let external_addr = bitcoind
        .client
        .get_new_address(None, None)
        .unwrap()
        .assume_checked();
    let small_amount = Amount::from_sat(100_000);

    log::info!(
        "🧪 Testing coin selection for {} sats",
        small_amount.to_sat()
    );

    let selected_utxo_count = {
        let mut wallet = maker.get_wallet().write().unwrap();
        let selected_utxos = wallet.coin_select(small_amount, MIN_FEE_RATE).unwrap();

        log::info!("🔍 Coin selection returned {} UTXOs", selected_utxos.len());

        // Verify the UTXOs selected are from our original set
        let selected_details: Vec<_> = selected_utxos
            .iter()
            .map(|(utxo, _)| (utxo.txid, utxo.vout, utxo.amount))
            .collect();

        // Check that selected UTXOs are from our original set
        for selected in &selected_details {
            assert!(
                utxo_details.contains(selected),
                "Selected UTXO {:?} should be from our original UTXO set",
                selected
            );
        }

        if !selected_utxos.is_empty() {
            let destination = Destination::Multi {
                outputs: vec![(external_addr, small_amount)],
                op_return_data: None,
            };

            match wallet.spend_from_wallet(MIN_FEE_RATE, destination, &selected_utxos) {
                Ok(tx) => {
                    bitcoind.client.send_raw_transaction(&tx).unwrap();
                    generate_blocks(bitcoind, 1);
                    log::info!("✅ Transaction broadcast successfully");
                }
                Err(e) => {
                    log::error!("❌ Transaction failed: {e:?}");
                    panic!("Transaction should not fail with selected UTXOs");
                }
            }
        }

        selected_utxos.len()
    };

    // Sync and verify address grouping behavior
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    // Verify the critical address grouping behavior
    assert!(
        selected_utxo_count >= 4,
        "Address grouping should select all UTXOs (selected: {})",
        selected_utxo_count
    );

    if selected_utxo_count >= 4 {
        log::info!(
            "✅ SUCCESS: Address grouping working - {selected_utxo_count} UTXOs selected together",
        );
    } else {
        log::info!("ℹ️ Only {selected_utxo_count} UTXOs selected");
    }

    // Test with larger amount
    let large_amount = Amount::from_sat(50_000_000);
    let selected_utxo_count_large = {
        let wallet = maker.get_wallet().read().unwrap();

        match wallet.coin_select(large_amount, MIN_FEE_RATE) {
            Ok(selected_utxos) => {
                log::info!("🔍 Large amount selection: {} UTXOs", selected_utxos.len());
                selected_utxos.len()
            }
            Err(_) => {
                log::info!("ℹ️ Large amount selection failed (insufficient funds)");
                0
            }
        }
    };

    // Verify address grouping behavior
    assert!(
        selected_utxo_count >= 2 || selected_utxo_count_large >= 2,
        "Address grouping should work in at least one scenario"
    );

    if selected_utxo_count >= 2 || selected_utxo_count_large >= 2 {
        log::info!("✅ Address grouping behavior confirmed");
    } else {
        panic!("Address grouping not working - this test validates the feature exists");
    }

    // Test address generation behavior
    let (addr1, addr2) = {
        let mut wallet = maker.get_wallet().write().unwrap();
        let a1 = wallet.get_next_external_address().unwrap();
        let a2 = wallet.get_next_external_address().unwrap();
        (a1, a2)
    };

    if addr1 == addr2 {
        log::info!("✅ Address reuse detected");
    } else {
        log::info!("ℹ️ HD wallet generates unique addresses");
    }

    // Test multiple transactions to same address
    send_to_address(bitcoind, &addr1, Amount::from_sat(1_000_000));
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &addr1, Amount::from_sat(2_000_000));
    generate_blocks(bitcoind, 1);

    // Verify we now have more UTXOs after additional funding
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
        let all_utxos = wallet.get_all_utxo().unwrap();
        log::info!("📊 Final UTXO count: {}", all_utxos.len());

        // Should have at least 3 UTXOs now (1 change + 2 new)
        assert!(
            all_utxos.len() >= 3,
            "Should have at least 3 UTXOs after additional funding"
        );
    }

    // Cleanup
    directory_server_instance.shutdown.store(true, Relaxed);
    test_framework.stop();
    block_generation_handle.join().unwrap();

    log::info!("🎉 Address grouping test completed!");
}
