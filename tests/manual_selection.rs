#![cfg(feature = "integration-test")]
use bitcoin::{Amount, OutPoint};
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
    utill::ConnectionType,
};
use std::sync::Arc;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{assert_eq, sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn test_manual_coinswap() {
    // ---- Setup ----

    // 2 Makers with Normal behavior.
    let makers_config_map = [
        ((6102, Some(19051)), MakerBehavior::Normal),
        ((16102, Some(19052)), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal];
    let connection_type = ConnectionType::CLEARNET;

    // Initiate test framework, Makers and a Taker with default behavior.
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior, connection_type);

    warn!("🧪 Running Test: Manual UTXO Selection Coinswap Procedure");
    let bitcoind = &test_framework.bitcoind;

    info!("💰 Funding taker and makers");
    // Get the taker
    let taker = &mut takers[0];

    // Fund the Taker with standard method first to establish base funds
    let _ = fund_and_verify_taker(taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Deterministic amounts in the range of 0.005 to 0.01 BTC (5,000,000 to 10,000,000 sats)
    let amounts: Vec<u64> = vec![
        590_283, 550_813, 612_842, 685_372, 700_000, 750_000, 800_000, 850_000, 900_000, 1_000_000,
    ];

    // Add specific amounts for manual selection
    for &amount in &amounts {
        let taker_address = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &taker_address, Amount::from_sat(amount));
        generate_blocks(bitcoind, 1);
    }

    taker.get_wallet_mut().sync_and_save().unwrap();

    // Get all UTXOs after funding
    let all_utxos = taker.get_wallet_mut().get_all_utxo().unwrap();

    // Select the first 4 amounts from our predefined amounts vector
    let target_amounts: Vec<u64> = amounts.iter().take(4).cloned().collect();

    // Find UTXOs corresponding to those specific amounts
    let manually_selected_utxos: Vec<OutPoint> = target_amounts
        .iter()
        .filter_map(|&target_amount| {
            // Find the first UTXO with matching amount
            all_utxos
                .iter()
                .find(|utxo| utxo.amount.to_sat() == target_amount)
                .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        })
        .collect();

    // Verify we found all 4 UTXOs
    assert_eq!(
        manually_selected_utxos.len(),
        4,
        "Expected to find 4 UTXOs with specific amounts, but found {}",
        manually_selected_utxos.len()
    );

    log::info!(
        "📋 Selected {} UTXOs manually:",
        manually_selected_utxos.len()
    );
    for outpoint in &manually_selected_utxos {
        info!("  - {outpoint}");
    }

    // Fund the Maker with 4 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 4, Amount::from_btc(0.05).unwrap());

    //  Start the Maker Server threads
    info!("🚀 Initiating Maker servers");

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Makers take time to fully setup.
    let _ = makers
        .iter()
        .map(|maker| {
            while !maker.is_setup_complete.load(Relaxed) {
                info!("⏳ Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }

            // Check balance after setting up maker server.
            let wallet = maker.wallet.read().unwrap();

            let balances = wallet.get_balances().unwrap();

            verify_maker_pre_swap_balances(&balances, 14999508);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap
    info!("🔄 Initiating coinswap protocol");

    // Swap params for coinswap with manually selected UTXOs
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        manually_selected_outpoints: Some(manually_selected_utxos.clone()),
    };
    taker.do_coinswap(swap_params).unwrap();

    // After Swap is done, wait for maker threads to conclude.
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    info!("🎯 All coinswaps processed successfully. Transaction complete.");

    thread::sleep(Duration::from_secs(10));

    let taker_wallet = taker.get_wallet_mut();
    taker_wallet.sync_and_save().unwrap();

    // Synchronize each maker's wallet.
    for maker in makers.iter() {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // Verify that manually selected UTXOs were actually used
    info!("🔍 Verifying manually selected UTXOs were used in the swap");

    // Check that the manually selected UTXOs are no longer in the wallet (they were spent)
    let remaining_utxos: Vec<OutPoint> = taker
        .get_wallet_mut()
        .get_all_utxo()
        .unwrap()
        .iter()
        .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        .collect();

    let manual_utxos_spent = manually_selected_utxos
        .iter()
        .all(|manual_utxo| !remaining_utxos.contains(manual_utxo));

    assert!(
        manual_utxos_spent,
        "Not all manually selected UTXOs were spent in the coinswap"
    );

    info!(
        "✅ Verified: All {} manually selected UTXOs were used in the coinswap",
        manually_selected_utxos.len()
    );
    for outpoint in &manually_selected_utxos {
        info!("  ✓ Used: {outpoint}");
    }

    info!(" All checks successful. Terminating integration test case");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
