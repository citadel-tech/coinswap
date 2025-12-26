#![cfg(feature = "integration-test")]
//! Integration test for Taproot Hashlock Recovery
//!
//! This test demonstrates end-to-end hashlock-based recovery when contracts are broadcasted
//! and one party sweeps via hashlock path, revealing the preimage. The other party should
//! extract the preimage from the blockchain and recover their funds via hashlock.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMakerBehavior as MakerBehavior},
    taker::api2::{SwapParams, TakerBehavior},
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test taproot hashlock recovery - full end-to-end
///
/// Scenario:
/// 1. Taker initiates swap with Maker
/// 2. Contracts are created and confirmed
/// 3. Maker sweeps their incoming contract but closes connection before completing handover
/// 4. Taker's incoming contract (maker's outgoing) is on-chain, not swept by maker
/// 5. Taker sweeps their incoming contract via hashlock (revealing preimage)
#[test]
fn test_taproot_hashlock_recovery_end_to_end() {
    // ---- Setup ----
    warn!("🧪 Running Test: Taproot Hashlock Recovery - End to End");

    // Create one maker that will close connection after sweeping incoming contract
    let makers_config_map = vec![
        (7104, Some(19072), MakerBehavior::CloseAfterSweep),
        (7105, Some(19073), MakerBehavior::CloseAfterSweep),
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

    let actual_maker_spendable_balances = verify_maker_pre_swap_balance_taproot(&taproot_makers);
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

    // Mine blocks to confirm any broadcasted transactions
    // Note: When do_coinswap returns Ok(None), recovery has already been attempted internally,
    // so contract balance may already be 0 if recovery succeeded
    info!("⛏️ Mining blocks to confirm contracts...");
    generate_blocks(bitcoind, 2);
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

    info!("📊 Taker balance after failed swap (recovery already attempted):");
    let taker_balances = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}",
        taker_balances.regular, taker_balances.contract, taker_balances.spendable
    );

    // Call recover_from_swap again - should be idempotent if already recovered
    info!("🔧 Calling recover_from_swap (may be no-op if already recovered)...");
    match taproot_taker.recover_from_swap() {
        Ok(()) => {
            info!("✅ Recovery call completed!");
        }
        Err(e) => {
            panic!("⚠️ Taker recovery failed: {:?}", e);
        }
    }

    // Mine blocks to confirm any recovery transactions
    info!("⛏️ Mining blocks to confirm recovery transactions...");
    generate_blocks(bitcoind, 2);
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

    info!("📊 Taproot Taker balance after recovery:");
    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}",
        taker_balances_after.regular, taker_balances_after.contract, taker_balances_after.spendable
    );

    // Now wait for maker to extract preimage and recover via hashlock
    info!("⏳ Waiting for maker to extract preimage and recover via hashlock...");
    std::thread::sleep(std::time::Duration::from_secs(60));
    // Mine more blocks to give maker time to see the hashlock sweep
    generate_blocks(bitcoind, 2);

    // Verify swap results
    let taker_wallet = taproot_taker.get_wallet();
    let taker_balances = taker_wallet.get_balances().unwrap();

    // Use spendable balance (regular + swap) since swept coins from V2 swaps
    // are tracked as SweptCoinV2 and appear in swap balance
    // Here in hashlock recovery the spendable balance is almost similar to key-path spend
    // as the parties are completing their swap by claiming their incoming contract
    // via script-path spend.
    let taker_total_after = taker_balances.spendable;
    assert!(
        taker_total_after.to_sat() == 14944003,
        "Taproot Taker Balance check after hashlock recovery. Original: {}, After: {}",
        taproot_taker_original_balance,
        taker_total_after
    );

    // But the taker should still have a reasonable amount left (not all spent on fees)
    let balance_diff = taproot_taker_original_balance - taker_total_after;
    assert!(
        balance_diff.to_sat() == 55997, // Hashlock recovery fee
        "Taproot Taker should have paid some fees. Original: {}, After: {},fees paid: {}",
        taproot_taker_original_balance,
        taker_total_after,
        balance_diff
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
        .zip(actual_maker_spendable_balances)
        .enumerate()
    {
        let wallet = maker.wallet().read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Taproot Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable
        );

        // Use spendable (regular + swap) for comparison
        // Here in hashlock recovery the spendable balance is almost similar to key-path spend
        // as the parties are completing their swap by claiming their incoming contract
        // via script-path spend.
        assert_in_range!(
            balances.spendable.to_sat(),
            [14999510, 15020999, 15032506],
            "Taproot Maker after hashlock recovery balance check."
        );

        let balance_diff = balances
            .spendable
            .to_sat()
            .saturating_sub(original_spendable.to_sat());
        // maker gained fee
        assert_in_range!(
            balance_diff,
            [0, 21489, 32996],
            "Taproot Maker fee gained by recovering via hashlock"
        );

        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable, balance_diff
        );
    }
    info!("✅ Hashlock recovery test passed!");
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
