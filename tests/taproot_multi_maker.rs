#![cfg(feature = "integration-test")]
//! This test demonstrates a taproot coinswap between a Taker and multiple Makers

use bitcoin::Amount;
use coinswap::taker::api2::{SwapParams, TakerBehavior};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

/// Test taproot multi maker coinswap
#[test]
fn test_taproot_multi_maker() {
    // ---- Setup ----
    warn!("Running Test: Taproot Multi Maker Coinswap");

    // Create 4 makers to perform a taproot swap with 1 taker and 4 makers.
    use coinswap::maker::TaprootMakerBehavior as MakerBehavior;
    let taproot_makers_config_map = vec![
        (7102, Some(19061), MakerBehavior::Normal),
        (17102, Some(19062), MakerBehavior::Normal),
        (27102, Some(19063), MakerBehavior::Normal),
        (15102, Some(19064), MakerBehavior::Normal),
    ];
    // Create a taker with normal behavior
    let taker_behavior = vec![TakerBehavior::Normal];

    // Initialize test framework
    let (test_framework, mut taproot_taker, taproot_makers) =
        TestFramework::init_taproot(taproot_makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = taproot_taker.get_mut(0).unwrap();

    // Fund the Taproot Taker with 3 UTXOs of 0.05 BTC each
    let taproot_taker_original_balance =
        fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Fund the Taproot Makers with 4 UTXOs of 0.05 BTC each
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    // Start the Taproot Maker Server threads
    log::info!("Initiating Taproot Makers...");
    test_framework.start_maker_servers();

    // Wait for taproot makers to complete setup
    for maker in &taproot_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            log::info!("Waiting for taproot maker setup completion");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &taproot_makers {
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_maker_pre_swap_balance_taproot(&taproot_makers);
    log::info!("Starting multi maker taproot coinswap...");
    // Swap params for taproot coinswap
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000), // 0.005 BTC
        maker_count: 4,                        // 4 maker count
        tx_count: 5,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Perform the swap
    match taproot_taker.do_coinswap(swap_params) {
        Ok(Some(_report)) => {
            log::info!("Taproot multi maker coinswap completed successfully!");
        }
        Ok(None) => {
            log::warn!(
                "Taproot multi maker coinswap completed but no report generated (recovery occurred)"
            );
        }
        Err(e) => {
            log::error!("Taproot multi maker coinswap failed: {:?}", e);
        }
    }

    // After swap, shutdown maker threads
    test_framework.shutdown_maker_servers().unwrap();

    // Sync wallets and verify results
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each taproot maker's wallet multiple times to ensure all UTXOs are discovered
    for maker in taproot_makers.iter() {
        let mut wallet = maker.wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
    }
    let taker_wallet = taproot_taker.get_wallet();
    let taker_balances = taker_wallet.get_balances().unwrap();
    info!(
        "📊 Taproot Taker balance after swap: Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances.regular,
        taker_balances.contract,
        taker_balances.spendable,
        taker_balances.swap,
    );

    // Use spendable balance (regular + swap) since swept coins from V2 swaps
    // are tracked as SweptCoinV2 and appear in swap balance
    let taker_total_after = taker_balances.spendable;
    assert_in_range!(
        taker_total_after.to_sat(),
        [14860645, 14860649], // Multi maker case (less spendable balance due to higher no. of makers,more fee paid) with variance
        "Taproot Taker spendable balance check."
    );

    let balance_diff = taproot_taker_original_balance - taker_total_after;
    assert_in_range!(
        // This balance diff(fee paid) consist of Maker fee's paid + mining fees.
        balance_diff.to_sat(),
        [139351, 139355], // amount spended as fee (multi maker case,more no. of maker so more fee) with variance
        "Taproot taker spent on fees check."
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

        assert_in_range!(
            balances.spendable.to_sat(),
            [
                // Here it's arranged in the increasing order of maker's spendable balance, as maker's order is random during swap.
                15016299, 15016313, 15025685, 15025699, 15037147, 15037161, 15051694, 15051708,
            ],
            "Taproot Maker spendable balance check for multi maker test case"
        );

        let balance_diff = balances.spendable.to_sat() - original_spendable.to_sat();
        assert_in_range!(
            balance_diff,
            // These maker gained fee are arranged as per the corresponding spendable balance in the above assertion list.
            [
                16795, 16799, 16813, 26181, 26183, 26187, 37643, 37645, 37649, 37663, 52190, 52192,
                52196, 52210
            ],
            "Taproot Maker fee gained check"
        );
        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable,balance_diff
        );
    }

    log::info!("✅ Taproot mutli maker test passed successfully.");
    // TestFramework drop handles shutdown of all background processes.
}
