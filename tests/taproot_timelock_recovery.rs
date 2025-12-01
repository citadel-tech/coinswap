#![cfg(feature = "integration-test")]
//! Integration test for Taproot Timelock Recovery
//!
//! This test demonstrates end-to-end taproot timelock-based recovery when a maker
//! closes the connection after sweeping their incoming contract but before completing
//! the private key handover. The taker must wait for the timelock to mature, then
//! recover funds via script-path timelock spending.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMaker, TaprootMakerBehavior as MakerBehavior},
    taker::{
        api2::{SwapParams, Taker},
        TakerBehavior,
    },
};
use std::sync::Arc;

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test taproot timelock recovery - full end-to-end
///
/// Scenario:
/// 1. Taker initiates swap with Maker
/// 2. Taker sends outgoing contract to Maker
/// 3. Maker closes connection after receiving incoming contract (before creating outgoing)
/// 4. Taker has outgoing contract stuck, no incoming contract received
/// 5. Both parties wait for timelock to mature
/// 6. Both parties recover via timelock (no preimage available)
#[test]
fn test_taproot_timelock_recovery_end_to_end() {
    // ---- Setup ----
    warn!("🧪 Running Test: Taproot Timelock Recovery - End to End");

    // Create one maker that closes at contract exchange (forces timelock recovery)
    let makers_config_map = vec![
        (7103, Some(19071), MakerBehavior::Normal),
        (
            7104,
            Some(19072),
            MakerBehavior::CloseAtContractSigsExchange,
        ),
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

    info!("🔄 Initiating taproot coinswap (will fail mid-swap)...");

    // Swap params - small amount for faster testing
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Attempt the swap - it will fail when maker closes connection
    match taproot_taker.do_coinswap(swap_params) {
        Ok(_) => {
            panic!("Swap should have failed due to maker closing connection!");
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

    // Verify taker has funds stuck in outgoing contract
    assert!(
        taker_balances.contract > Amount::ZERO,
        "Taker should have contract balance stuck. Contract: {}",
        taker_balances.contract
    );

    // Mine blocks to mature timelock (20 blocks from swap params)
    info!("⏰ Mining blocks to mature timelock (20+ blocks)...");
    generate_blocks(bitcoind, 25);

    // Taker recovers via TIMELOCK (no preimage, no incoming contract)
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

    // Maker should have approximately same balance (maybe some RPC fees)
    // They received incoming contract but taker recovered it via timelock
    let max_maker_loss = Amount::from_sat(1000); // Small fees only
    assert!(
        maker_balance_after >= maker_balance_before - max_maker_loss,
        "Maker balance shouldn't change much. Before: {}, After: {}, Change: {}",
        maker_balance_before,
        maker_balance_after,
        maker_balance_after.to_sat() as i64 - maker_balance_before.to_sat() as i64
    );

    info!("✅ Timelock recovery test passed!");
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

/// Fund taproot makers and verify their balances
fn fund_taproot_makers(
    makers: &[Arc<TaprootMaker>],
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) {
    for maker in makers {
        let mut wallet = maker.wallet().write().unwrap();

        // Fund with regular UTXOs
        for _ in 0..utxo_count {
            let addr = wallet.get_next_external_address().unwrap();
            send_to_address(bitcoind, &addr, utxo_value);
        }

        generate_blocks(bitcoind, 1);
        wallet.sync().unwrap();

        // Verify balances
        let balances = wallet.get_balances().unwrap();
        let expected_regular = utxo_value * utxo_count.into();

        assert_eq!(balances.regular, expected_regular);

        info!(
            "Taproot Maker funded successfully. Regular: {}, Fidelity: {}",
            balances.regular, balances.fidelity
        );
    }
}

/// Fund taproot taker and verify balance
fn fund_taproot_taker(
    taker: &mut Taker,
    bitcoind: &bitcoind::BitcoinD,
    utxo_count: u32,
    utxo_value: Amount,
) -> Amount {
    // Fund with regular UTXOs
    for _ in 0..utxo_count {
        let addr = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &addr, utxo_value);
    }

    generate_blocks(bitcoind, 1);
    taker.get_wallet_mut().sync().unwrap();

    // Verify balances
    let balances = taker.get_wallet().get_balances().unwrap();
    let expected_regular = utxo_value * utxo_count.into();

    assert_eq!(balances.regular, expected_regular);

    info!(
        "Taproot Taker funded successfully. Regular: {}, Spendable: {}",
        balances.regular, balances.spendable
    );

    balances.spendable
}
