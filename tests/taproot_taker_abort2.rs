#![cfg(feature = "integration-test")]

//! This test demonstrates when the taker closes connection after sending contract details to maker.
//! The taker has an outgoing contract created,so it recovers via timelock later.

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
/// 2. Taker sends outgoing contract to Maker and closes connection.
/// 3. Maker receiving incoming contract,but unable to connect with taker as it has closed the connection.
/// 4. Taker has it's outgoing contract stuck.
/// 5. Taker will wait for timelock to mature, and recover via it.
#[test]
fn test_taproot_taker_abort2() {
    // ---- Setup ----
    warn!("🧪 Running Test: Taproot Taker Abort 2");

    // Create both normal makers.
    let makers_config_map = vec![
        (7103, Some(19071), MakerBehavior::Normal),
        (7104, Some(19072), MakerBehavior::Normal),
    ];
    // Create a taker that closes connection at SendersContract step.
    let taker_behavior = vec![TakerBehavior::CloseAtSendersContract];

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
        maker.wallet().write().unwrap().sync().unwrap();
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

    info!("🔄 Initiating taproot taker abort 2");

    // Swap params - small amount for faster testing
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Attempt the swap - it will fail when taker closes connection
    // After recovery, do_coinswap returns Ok(None) to indicate recovery was triggered
    match taproot_taker.do_coinswap(swap_params) {
        Ok(Some(_report)) => {
            panic!("Swap should have failed due to taker closing connection, but succeeded with report!");
        }
        Ok(None) => {
            info!("✅ Taproot coinswap triggered recovery as expected (Ok(None))");
        }
        Err(e) => {
            info!("✅ Taproot coinswap failed as expected: {:?}", e);
        }
    }

    // Mine a block to confirm any broadcasted transactions
    generate_blocks(bitcoind, 1);
    taproot_taker.get_wallet_mut().sync().unwrap();

    info!("📊 Taker balance after failed swap:");
    let taker_balances = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}",
        taker_balances.regular, taker_balances.contract, taker_balances.spendable
    );

    // Mine blocks to mature timelock (20 blocks from swap params)
    info!("⏰ Mining blocks to mature timelock (20+ blocks)...");
    generate_blocks(bitcoind, 25);

    // Taker recovers via TIMELOCK (may be no-op if already recovered)
    info!("🔧 Taker recovering via timelock...");
    match taproot_taker.recover_from_swap() {
        Ok(()) => {
            info!("✅ Taker recovery completed!");
        }
        Err(e) => {
            panic!("Taker timelock recovery failed: {:?}", e);
        }
    }

    // Mine blocks to confirm taker's recovery
    generate_blocks(bitcoind, 2);
    taproot_taker.get_wallet_mut().sync().unwrap();

    info!("📊 Taker balance after timelock recovery:");
    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}",
        taker_balances_after.regular, taker_balances_after.contract, taker_balances_after.spendable
    );

    // Verify taker recovered their funds via timelock
    let max_taker_fees = Amount::from_sat(10000); // Small fee for timelock tx
    assert!(
        taker_balances_after.spendable >= taproot_taker_original_balance - max_taker_fees,
        "Taker should have recovered via timelock. Original: {}, After: {}, Lost: {}",
        taproot_taker_original_balance,
        taker_balances_after.spendable,
        taproot_taker_original_balance - taker_balances_after.spendable
    );

    // Wait for maker's automatic recovery to trigger
    // The idle-checker detects dropped connections after 60 seconds (IDLE_CONNECTION_TIMEOUT)
    info!("⏳ Waiting for maker's automatic recovery (65 seconds)...");
    thread::sleep(Duration::from_secs(65));
    // Mine blocks to confirm maker's recovery transactions
    generate_blocks(bitcoind, 10);

    // Verify maker's final balance (they never created outgoing contract, no funds gained/lost)
    let maker_balance_after = {
        let mut wallet = taproot_makers[0].wallet().write().unwrap();
        wallet.sync().unwrap();
        let balances = wallet.get_balances().unwrap();
        info!(
            "📊 Maker balance after swap: Regular: {}, Spendable: {}",
            balances.regular, balances.spendable
        );
        balances.spendable
    };

    let max_maker_loss = Amount::from_sat(1000); // Small fees only
    assert!(
        maker_balance_after >= maker_balance_before - max_maker_loss,
        "Maker balance shouldn't change much. Before: {}, After: {}, Change: {}",
        maker_balance_before,
        maker_balance_after,
        maker_balance_after.to_sat() as i64 - maker_balance_before.to_sat() as i64
    );

    info!("✅ Taker abort 2 recovery test passed!");
    info!(
        "   Taker: Original {}, After timelock recovery: {}, Fees paid: {}",
        taproot_taker_original_balance,
        taker_balances_after.spendable,
        taproot_taker_original_balance - taker_balances_after.spendable
    );
    info!(
        "   Maker: Before {}, After: {}, No change (swap never completed)",
        maker_balance_before, maker_balance_after
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
