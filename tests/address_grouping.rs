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

    // Get multiple addresses from the wallet
    let (address_a, address_b, address_c, address_d) = {
        let mut wallet = maker.get_wallet().write().unwrap();
        let addr_a = wallet.get_next_external_address().unwrap();
        let addr_b = wallet.get_next_external_address().unwrap();
        let addr_c = wallet.get_next_external_address().unwrap();
        let addr_d = wallet.get_next_external_address().unwrap();
        (addr_a, addr_b, addr_c, addr_d)
    };

    // Multi-UTXO test setup: Multiple addresses with different UTXO counts
    // Address A: 4 UTXOs (larger amounts)
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.8).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.9).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.7).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.6).unwrap());
    generate_blocks(bitcoind, 1);

    // Address B: 5 UTXOs (medium amounts)
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.3).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.25).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.35).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.2).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.4).unwrap());
    generate_blocks(bitcoind, 1);

    // Address C: 6 UTXOs (smaller amounts)
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.15).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.18).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.12).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.22).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.16).unwrap());
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.14).unwrap());
    generate_blocks(bitcoind, 1);

    // Address D: 1 UTXO (single large amount)
    send_to_address(bitcoind, &address_d, Amount::from_btc(2.2).unwrap());
    generate_blocks(bitcoind, 1);

    // Sync wallet and verify UTXOs
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    let (utxo_count, address_a_utxos, address_b_utxos, address_c_utxos, address_d_utxos) = {
        let wallet = maker.get_wallet().read().unwrap();
        let all_utxos = wallet.get_all_utxo().unwrap();

        // Count UTXOs by address (using specific amount identification)
        let addr_a_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| {
                [80_000_000, 90_000_000, 70_000_000, 60_000_000].contains(&utxo.amount.to_sat())
            })
            .collect();

        let addr_b_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| {
                [30_000_000, 25_000_000, 35_000_000, 20_000_000, 40_000_000]
                    .contains(&utxo.amount.to_sat())
            })
            .collect();

        let addr_c_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| {
                [
                    15_000_000, 18_000_000, 12_000_000, 22_000_000, 16_000_000, 14_000_000,
                ]
                .contains(&utxo.amount.to_sat())
            })
            .collect();

        let addr_d_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| utxo.amount == Amount::from_btc(2.2).unwrap())
            .collect();

        (
            all_utxos.len(),
            addr_a_utxos.len(),
            addr_b_utxos.len(),
            addr_c_utxos.len(),
            addr_d_utxos.len(),
        )
    };

    assert_eq!(utxo_count, 16, "Should have exactly 16 UTXOs total");
    assert_eq!(
        address_a_utxos, 4,
        "Should have exactly 4 UTXOs at address A"
    );
    assert_eq!(
        address_b_utxos, 5,
        "Should have exactly 5 UTXOs at address B"
    );
    assert_eq!(
        address_c_utxos, 6,
        "Should have exactly 6 UTXOs at address C"
    );
    assert_eq!(
        address_d_utxos, 1,
        "Should have exactly 1 UTXO at address D"
    );

    // Test coin selection with amount that ALL addresses can satisfy
    let external_addr = bitcoind
        .client
        .get_new_address(None, None)
        .unwrap()
        .assume_checked();
    let test_amount = Amount::from_sat(50_000_000); // 50M sats

    log::info!("Testing coin selection for {} sats", test_amount.to_sat());

    let (selected_utxo_count, selected_total_amount) = {
        let mut wallet = maker.get_wallet().write().unwrap();
        let selected_utxos = wallet.coin_select(test_amount, MIN_FEE_RATE).unwrap();

        log::info!("Coin selection returned {} UTXOs", selected_utxos.len());

        let total_selected: Amount = selected_utxos.iter().map(|(utxo, _)| utxo.amount).sum();
        log::info!("Total amount selected: {} sats", total_selected.to_sat());

        if !selected_utxos.is_empty() {
            let destination = Destination::Multi {
                outputs: vec![(external_addr, test_amount)],
                op_return_data: None,
            };

            match wallet.spend_from_wallet(MIN_FEE_RATE, destination, &selected_utxos) {
                Ok(tx) => {
                    bitcoind.client.send_raw_transaction(&tx).unwrap();
                    generate_blocks(bitcoind, 1);
                    log::info!("Transaction broadcast successfully");
                }
                Err(e) => {
                    log::error!("Transaction failed: {e:?}");
                    panic!("Transaction should not fail with selected UTXOs");
                }
            }
        }

        (selected_utxos.len(), total_selected)
    };

    // Sync after spending
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    // Verify address grouping behavior
    if selected_utxo_count >= 6 && selected_total_amount >= Amount::from_sat(97_000_000) {
        log::info!(
            "✅ Address C selected - {} UTXOs grouped together",
            selected_utxo_count
        );
    } else if selected_utxo_count >= 5 && selected_total_amount >= Amount::from_sat(150_000_000) {
        log::info!(
            "✅ Address B selected - {} UTXOs grouped together",
            selected_utxo_count
        );
    } else if selected_utxo_count >= 4 && selected_total_amount >= Amount::from_sat(300_000_000) {
        log::info!(
            "✅ Address A selected - {} UTXOs grouped together",
            selected_utxo_count
        );
    } else {
        log::info!("Single UTXO or unexpected selection pattern");
    }

    // Assert the core requirement: if multiple UTXOs exist at same address, they should be grouped
    assert!(
        selected_utxo_count >= 4,
        "Address grouping failed: selected {} UTXOs, but should group multiple UTXOs from same address",
        selected_utxo_count
    );

    log::info!("✅ Address grouping behavior confirmed!");

    // Cleanup
    directory_server_instance.shutdown.store(true, Relaxed);
    test_framework.stop();
    block_generation_handle.join().unwrap();

    log::info!("🎉 Address grouping test completed!");
}
