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
    let naughty = 7104;
    let makers_config_map = vec![
        (7103, Some(19071), MakerBehavior::Normal),
        (
            naughty,
            Some(19072),
            MakerBehavior::CloseAtPrivateKeyHandover,
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

    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
         "📊 Taproot Taker balance after hashlock recovery: Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
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

    let balance_diff = taproot_taker_original_balance - taker_total_after;
    assert!(
        // In this swap case -: Each Maker fee is 13500 sats, mining fee (including hashlock recovery txn) is 28997 sats
        balance_diff.to_sat() == 55997, // Maker fee + hashlock_recovery_tx fee
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
        // Here in hashlock recovery the spendable balance is almost similar to key-path spend
        // as the parties are completing their swap by claiming their incoming contract
        // via script-path spend.
        assert_in_range!(
            balances.spendable.to_sat(),
            [
                14999500, // No fund loss,it occurs for a maker when it was not having any incoming contract (less likely to occur)
                15020989, // 1st Maker completed the swap via hashlock path spending and earned some sats.
                15032496, // 2nd Maker completed the swap via hashlock path spending and earned some sats.
            ],
            "Taproot Maker after hashlock recovery balance check."
        );

        let balance_diff = balances.spendable.to_sat() - original_spendable.to_sat();
        // maker gained fee arranged in the order of corresponding spendable balance in the above assertion
        assert_in_range!(
            balance_diff,
            [
                0, // No fund gain/lost for a maker,if it was not having any incoming contract(so no swap for this maker)
                21489, // 1st Maker gained fee after completing the swap via hashlock path spending.
                32996  // 2nd Maker gained fee after completing the swap via hashlock path spending.
            ],
            "Taproot Maker should have gained some fees here."
        );

        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable, balance_diff
        );
    }
    info!("✅ Taproot Maker abort 2 recovery test passed!");
    // Shutdown maker
    taproot_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    taproot_maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    info!("🚫 Verifying naughty maker gets banned");
    // Maker gets banned for being naughty.
    assert_eq!(
        format!("127.0.0.1:{naughty}"),
        taproot_taker.get_bad_makers()[0].address.to_string()
    );

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
