#![cfg(feature = "integration-test")]
//! This test demonstrates the scenario when a maker closes connection after sending AckRespnse message to the taker that it accepts the swap details.
//! Nothing to recover for taker here as no outgoing/incoming contract are created.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMakerBehavior as MakerBehavior},
    taker::api2::{SwapParams, TakerBehavior},
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn test_taproot_maker_abort3() {
    // ---- Setup ----
    warn!("🧪 Running Test: Taproot Maker Abort 3");

    //Create a maker with normal behaviour and another maker with CloseAtAckResponse behaviour.
    let makers_config_map = vec![
        (7102, Some(19071), MakerBehavior::Normal),
        (17102, Some(19072), MakerBehavior::CloseAfterAckResponse),
    ];
    //Create a Taker
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

    info!("🔄 Initiating taproot abort 3 case (will fail due to one maker closing after accpeting swap details)");

    // Swap params - small amount for faster testing
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Attempt the swap
    match taproot_taker.do_coinswap(swap_params) {
        Ok(Some(_report)) => {
            panic!("Swap should have failed due to one maker closing connection, but succeeded with report!");
        }
        Ok(None) => {
            info!("✅ Taproot coinswap triggered recovery as expected (Ok(None))");
        }
        Err(e) => {
            info!("✅ Taproot coinswap failed as expected: {:?}", e);
        }
    }

    taproot_taker.get_wallet_mut().sync_and_save().unwrap();
    info!("📊 Taker balance after failed swap:");
    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}",
        taker_balances_after.regular, taker_balances_after.contract, taker_balances_after.spendable
    );

    assert!(
        taker_balances_after.spendable == taproot_taker_original_balance,
        "Taker should have no fund loss. Original: {}, After: {}, Lost: {}",
        taproot_taker_original_balance,
        taker_balances_after.spendable,
        taproot_taker_original_balance - taker_balances_after.spendable
    );

    // Verify maker's final balance
    let maker_balance_after = {
        let mut wallet = taproot_makers[0].wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
        let balances = wallet.get_balances().unwrap();
        info!(
            "📊 Maker balance after swap: Regular: {}, Spendable: {}",
            balances.regular, balances.spendable
        );
        balances.spendable
    };

    assert!(
        maker_balance_after == maker_balance_before,
        "Maker's balance -: Before: {}, After: {}, Change: {}",
        maker_balance_before,
        maker_balance_after,
        maker_balance_after.to_sat() as i64 - maker_balance_before.to_sat() as i64
    );

    info!("✅ Taproot maker abort 3 recovery test passed!");
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
