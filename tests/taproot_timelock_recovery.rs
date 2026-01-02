#![cfg(feature = "integration-test")]
//! Integration test for Taproot Timelock Recovery
//!
//! This test demonstrates end-to-end taproot timelock-based recovery when a maker
//! closes the connection after sweeping their incoming contract but before completing
//! the private key handover. The taker must wait for the timelock to mature, then
//! recover funds via script-path timelock spending.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMakerBehavior as MakerBehavior},
    taker::api2::{SwapParams, TakerBehavior},
};

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
    let naughty = 7104;
    let makers_config_map = vec![
        (7103, Some(19071), MakerBehavior::Normal),
        (
            naughty,
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
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_maker_pre_swap_balance_taproot(&taproot_makers);
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

    // Mine a block to confirm any broadcasted transactions
    // Note: do_coinswap may have already attempted recovery internally, but timelock
    // recovery requires waiting for blocks, so funds may still be in contract
    generate_blocks(bitcoind, 1);
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

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
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

    info!("📊 Taproot Taker balance after timelock recovery:");
    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
         "📊 Taproot Taker balance after timelock recovery: Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
    );
    // Wait for maker's automatic recovery to trigger
    // The idle-checker detects dropped connections after 60 seconds (IDLE_CONNECTION_TIMEOUT)
    info!("⏳ Waiting for maker's automatic recovery (65 seconds)...");
    thread::sleep(Duration::from_secs(65));
    // Mine blocks to confirm maker's recovery transactions
    generate_blocks(bitcoind, 10);

    info!("🚫 Verifying naughty maker gets banned");
    // Maker gets banned for being naughty.
    assert_eq!(
        format!("127.0.0.1:{naughty}"),
        taproot_taker.get_bad_makers()[0].address.to_string()
    );

    // Verify swap results
    let taker_wallet = taproot_taker.get_wallet();
    let taker_balances = taker_wallet.get_balances().unwrap();
    // Use spendable balance (regular + swap) since swept coins from V2 swaps
    // are tracked as SweptCoinV2 and appear in swap balance
    let taker_total_after = taker_balances.spendable;
    assert_in_range!(
        taker_total_after.to_sat(),
        [14999492, 14999496], // swap never happened, funds recovered via timelock (with slight fee variance)
        "Taproot Taker Balance should decrease a little."
    );

    // But the taker should still have a reasonable amount left (not all spent on fees)
    let balance_diff = taproot_taker_original_balance - taker_total_after;
    assert_in_range!(
        balance_diff.to_sat(),
        [504, 508], // here a little fund loss because of outgoing contract creation, and therefore a timelock recovery transaction for recovering it.
        "Taproot Taker should have paid a little fees."
    );
    info!(
        "Taproot Taker balance verification passed. Original spendable: {}, After spendable: {} (fees paid: {})",
        taproot_taker_original_balance,
        taker_total_after,
        balance_diff
    );

    // Verify makers earned fees
    for (i, (maker, original_spendable)) in taproot_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let wallet = maker.wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Taproot Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {},Swap:{}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,balances.swap,
        );

        // Use spendable (regular + swap) for comparison
        assert_in_range!(
            balances.spendable.to_sat(),
            [
                14999500, // The corresponding maker didn't had any outgoing contract,so no need of recovery, hence no fund loss.
                14999518, // Slight fee variance case (no recovery needed)
                14999010, // Slight fee variance case (recovery needed)
                14998996, // Here a little fund loss because of outgoing contract creation, and therefore a timelock recovery transaction for recovering it.
            ],
            "Taproot Maker after balance check."
        );

        let balance_diff = original_spendable.to_sat() - balances.spendable.to_sat();
        // Maker may haVe lost some sats here due to timelock recovery transaction
        assert_in_range!(
            balance_diff,
            [
                0,   // Here the maker was not having any outgoing contract (depends on the order of maker)
                490, // Slight fee variance
                504, // Corresponding maker recovered it's outgoing contract via timelock spending path
                508, // Slight fee variance
            ],
            "Taproot Maker should have loose some funds here due to timelock recovery transaction."
        );
        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable, balance_diff
        );
    }
    info!("✅ Timelock recovery test passed!");
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
