#![cfg(feature = "integration-test")]
//! Integration test for Taproot Maker abort scenario
//!
//! This test demonstrates when a maker closes the connection at private key handover step,before sweeping.
//! The taker then recover funds via hashlock spend.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMakerBehavior as MakerBehavior},
    taker::api2::{SwapParams, TakerBehavior},
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Scenario:
/// 1. Taker initiates swap with Maker
/// 2. Taker sends private key to Maker
/// 3. Maker closes connection after receiving private key, before sweeping their incoming contract.
/// 4. Taker then sweeps it's incoming contract via hashlock.
#[test]
fn test_taproot_maker_abort2() {
    // ---- Setup ----
    warn!("🧪 Running Test: Taproot Maker Abort 2");

    // Create one normal maker, and a maker that closes connection at PrivateKeyHandover step
    let makers_config_map = vec![
        (7103, Some(19071), MakerBehavior::Normal),
        (7104, Some(19072), MakerBehavior::CloseAtPrivateKeyHandover),
    ];
    let taker_behavior = vec![TakerBehavior::Normal];

    // Initialize test framework
    let (test_framework, mut taproot_taker, taproot_makers, block_generation_handle) =
        TestFramework::init_taproot(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = taproot_taker.get_mut(0).unwrap();

    info!("💰 Funding taker and maker");

    // Fund the Taproot Taker with 3 UTXOs of 0.05 BTC each
    let taproot_taker_original_balance =
        fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Fund the Taproot Maker with 4 UTXOs of 0.05 BTC each
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    // Start the Taproot Maker Server thread
    info!("🚀 Initiating Maker server...");

    let taproot_maker_threads = taproot_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server_taproot(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for taproot maker to complete setup
    for maker in &taproot_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            info!("⏳ Waiting for taproot maker setup completion");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets after setup
    for maker in &taproot_makers {
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    // Get balances before swap
    let maker_balance_before = {
        let wallet = taproot_makers[0].wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();
        info!(
            "Maker balance before swap: Regular: {}, Spendable: {}",
            balances.regular, balances.spendable
        );
        balances.spendable
    };

    info!("🔄 Initiating taproot maker abort 2 case. (Will fail mid-swap due to one maker closing connection at handling private key step)");

    // Swap params - small amount for faster testing
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Attempt the swap - it will fail when maker closes connection
    // After recovery, do_coinswap returns Ok(None) to indicate recovery was triggered
    match taproot_taker.do_coinswap(swap_params) {
        Ok(Some(_report)) => {
            panic!("Swap should have failed due to maker closing connection, but succeeded with report!");
        }
        Ok(None) => {
            info!("✅ Taproot coinswap triggered recovery as expected (Ok(None))");
        }
        Err(e) => {
            info!("✅ Taproot coinswap failed as expected: {:?}", e);
        }
    }

    // Mine blocks to confirm any recovery transactions
    info!("⛏️ Mining blocks to confirm recovery transactions...");
    generate_blocks(bitcoind, 2);
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

    info!("📊 Taker balance after hashlock recovery:");
    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}",
        taker_balances_after.regular, taker_balances_after.contract, taker_balances_after.spendable
    );
    // Verify taker recovered their funds via hashlock
    let max_taker_fees = Amount::from_sat(100000);
    assert!(
        taker_balances_after.spendable > taproot_taker_original_balance - max_taker_fees,
        "Taker should have recovered via hashlock. Original: {}, After: {}, Lost: {}",
        taproot_taker_original_balance,
        taker_balances_after.spendable,
        taproot_taker_original_balance - taker_balances_after.spendable
    );

    // Now wait for maker to extract preimage and recover via hashlock
    info!("⏳ Waiting for maker to extract preimage and recover via hashlock...");
    std::thread::sleep(std::time::Duration::from_secs(60));

    // Mine more blocks to give maker time to see the hashlock sweep
    generate_blocks(bitcoind, 2);

    // Wait a bit more for maker's recovery
    std::thread::sleep(std::time::Duration::from_secs(10));

    // Verify maker recovered their incoming contract via hashlock
    let maker_balance_after = {
        let mut wallet = taproot_makers[0].wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
        let balances = wallet.get_balances().unwrap();
        info!(
            "📊 Maker balance after hashlock recovery: Regular: {}, Spendable: {}",
            balances.regular, balances.spendable
        );
        balances.spendable
    };

    // Maker should have recovered their outgoing contract via hashlock after extracting preimage
    // They swept incoming (~500k sats) and should have it confirmed
    let max_maker_fees = Amount::from_sat(100000); // 0.001 BTC max fees
    assert!(
        maker_balance_after >= maker_balance_before - max_maker_fees,
        "Maker should have recovered via hashlock. Before: {}, After: {}, Lost: {}",
        maker_balance_before,
        maker_balance_after,
        maker_balance_before - maker_balance_after
    );

    info!("✅ Taproot Maker abort 2 recovery test passed!");
    info!(
        "   Taker original balance: {}, Recovered: {}, Fees paid: {}",
        taproot_taker_original_balance,
        taker_balances_after.spendable,
        taproot_taker_original_balance - taker_balances_after.spendable
    );
    info!(
        "   Maker balance before: {}, After: {} (change: {})",
        maker_balance_before,
        maker_balance_after,
        maker_balance_after.to_sat() as i64 - maker_balance_before.to_sat() as i64
    );

    // Shutdown maker
    taproot_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    taproot_maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
