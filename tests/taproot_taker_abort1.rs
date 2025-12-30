#![cfg(feature = "integration-test")]
//! This test demonstrates when a taker drops after receiving AckResponse message from maker,
//! In this case the taker doesn't have anything to recover.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMakerBehavior as MakerBehavior},
    taker::{
        api2::{SwapParams, TakerBehavior},
        error::TakerError,
    },
};

mod test_framework;
use test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn test_taproot_taker_abort1() {
    // ---- Setup ----
    warn!("🧪 Running Test: Taproot Taker Drop Abort 1");

    //Create Two makers with normal behaviour
    let makers_config_map = vec![
        (7103, Some(19071), MakerBehavior::Normal),
        (7104, Some(19072), MakerBehavior::Normal),
    ];
    //Create a Taker that drops after receiving AckResponse msg from Maker.
    let taker_behavior = vec![TakerBehavior::CloseAtAckResponse];

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
    info!("🔄 Initiating taproot taker abort 1");

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
            assert!(
                matches!(e, TakerError::General(_)),
                "Expected TakerError::General, got: {:?}",
                e
            );
        }
    }

    // Mine a block to confirm any broadcasted transactions
    generate_blocks(bitcoind, 1);
    taproot_taker.get_wallet_mut().sync_and_save().unwrap();
    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
         "📊 Taproot Taker balance after failed swap: Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
    );

    // Verify swap results
    let taker_wallet = taproot_taker.get_wallet();
    let taker_balances = taker_wallet.get_balances().unwrap();
    // Use spendable balance (regular + swap) since swept coins from V2 swaps
    // are tracked as SweptCoinV2 and appear in swap balance
    let taker_total_after = taker_balances.spendable;
    assert!(
        taker_total_after.to_sat() == 15000000, // swap never happened so no fund spent
        "Taproot Taker Balance should remain unchanged. Original: {}, After: {}",
        taproot_taker_original_balance,
        taker_total_after
    );

    let balance_diff = taproot_taker_original_balance - taker_total_after;
    assert!(
        balance_diff.to_sat() == 0, // here no fund loss because swap never happened
        "Taproot Taker shouldn't have paid any fees. Original: {}, After: {},fees paid: {}",
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
        assert_in_range!(
            balances.spendable.to_sat(),
            [14999500], // here no fund loss because swap never happen
            "Taproot Maker after balance check."
        );

        let balance_diff = balances.spendable.to_sat() - original_spendable.to_sat();
        // maker haven't gained anything here, as swap didn't happen
        assert!(balance_diff == 0, "Taproot Maker shouldn't gain any fee");

        info!(
            "Taproot Maker {} balance verification passed. Original spendable: {}, Current spendable: {}, fee gained: {}",
            i, original_spendable, balances.spendable, balance_diff
        );
    }
    info!("✅ Taker abort 1 recovery test passed!");
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
