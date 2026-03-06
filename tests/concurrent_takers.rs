#![cfg(feature = "integration-test")]
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    thread,
    time::Duration,
};

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, MakerBehavior},
    taker::{SwapParams, TakerBehavior},
};

use log::{info, warn};

mod test_framework;
use test_framework::*;

/// Multiple Takers Concurrent Swap
/// This test demonstrates a scenario where a Maker is connected to two Takers concurrently.
/// Both takers initiate a swap at the same time.
///
/// The test verifies that the Maker can properly manage multiple concurrent swaps
/// and prevent race conditions, ensuring takers do not broadcast funding transactions when the maker does not have enough liquidity.
#[test]
fn multi_taker_concurrent_swap() {
    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![TakerBehavior::Normal, TakerBehavior::Normal];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    warn!("🧪 Running Test: Multiple Takers with Different Behaviors");

    info!("💰 Funding multiple takers with UTXOs");
    // Fund the Takers with 3 utxos of 0.05 btc each and do basic checks on the balance
    for taker in takers.iter_mut() {
        fund_and_verify_taker(
            taker,
            &test_framework.bitcoind,
            3,
            Amount::from_btc(0.05).unwrap(),
        );
    }

    // Fund the Maker with 6 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(
        makers_ref,
        &test_framework.bitcoind,
        2,
        Amount::from_btc(0.05002).unwrap(),
    );

    // Start the Maker Server threads
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

    test_framework.register_maker_threads(maker_threads);

    // Makers take time to fully setup.
    let org_maker_spend_balances = makers
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

            verify_maker_pre_swap_balances(&balances, 5003500);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Capture original taker spendable balances before swap for verification later
    let org_taker_spend_balances: Vec<Amount> = takers
        .iter()
        .map(|taker| {
            let wallet = taker.get_wallet();
            wallet.get_balances().unwrap().spendable
        })
        .collect();

    // Initiate Coinswap for both Takers concurrently
    info!("🔄 Initiating coinswap protocol for multiple takers");

    // Spawn threads for each taker to initiate coinswap concurrently
    thread::scope(|s| {
        for (taker_index, taker) in takers.iter_mut().enumerate() {
            let swap_params = SwapParams {
                send_amount: Amount::from_sat(5000000),
                maker_count: 2,
                manually_selected_outpoints: None,
            };
            s.spawn(move || {
                info!("🚀 Taker {} starting coinswap", taker_index);
                match taker.do_coinswap(swap_params) {
                    Ok(_) => {
                        info!("✅ Taker {} coinswap completed successfully", taker_index);
                    }
                    Err(e) => {
                        warn!("❌ Taker {} coinswap failed: {:?}", taker_index, e);
                    }
                }
            });
            std::thread::sleep(Duration::from_secs(3));
        }
    });

    info!("🎯 All coinswaps processed. Transactions complete.");

    info!("📊 Verifying Taker balances");
    for (taker_index, taker) in takers.iter().enumerate() {
        let wallet = taker.get_wallet();
        let balances = wallet.get_balances().unwrap();

        // Debug logging
        info!(
            "🔍Taker {} - Regular: {} Swap: {} Contract: {} Spendable: {}",
            taker_index, balances.regular, balances.swap, balances.contract, balances.spendable
        );

        assert_in_range!(
            balances.regular.to_sat(),
            [9999560, 15000000],
            "Taker regular balance mismatch"
        );
        assert_in_range!(
            balances.swap.to_sat(),
            [4461667, 0],
            "Taker swap balance mismatch"
        );
        assert_eq!(balances.contract, Amount::ZERO, "Taker contract mismatch");
        assert_eq!(balances.fidelity, Amount::ZERO, "Taker fidelity mismatch");

        // Check balance_diff (fees paid) is in expected range
        let balance_diff = org_taker_spend_balances[taker_index]
            .checked_sub(balances.spendable)
            .unwrap();
        assert_in_range!(
            balance_diff.to_sat(),
            [
                538773,0  // Fee spent on successful coinswap
            ],
            "Taker spendable balance change mismatch"
        );
    }

    info!("📊 Verifying Maker balances");
    // Verify spendable balances for makers.
    makers
        .iter()
        .zip(org_maker_spend_balances.iter())
        .enumerate()
        .for_each(|(maker_idx, (maker, org_spend_balance))| {
            let wallet = maker.get_wallet().read().unwrap();
            let balances = wallet.get_balances().unwrap();
            log::info!(
                "🔍 Maker {} - Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
                maker_idx,
                balances.regular.to_btc(),
                balances.swap.to_btc(),
                balances.contract.to_btc(),
                balances.spendable.to_btc()
            );
            assert_eq!(
                balances.contract,
                Amount::ZERO,
                "Maker {}: Contract balance should be zero after successful swaps",
                maker_idx
            );

            assert_eq!(
                balances.fidelity,
                Amount::from_btc(0.05).unwrap(),
                "Maker {}: Fidelity bond should remain at 0.05 BTC",
                maker_idx
            );
            assert!(
                balances.spendable == balances.regular + balances.swap,
                "Maker balances mismatch"
            );
            let balance_diff = balances
                .spendable
                .to_sat()
                .saturating_sub(org_spend_balance.to_sat());
            log::info!("🔍 DEBUG: Multi-taker balance diff: {balance_diff} sats");
            assert_in_range!(
                balance_diff,
                [211037, 325860],
                "Expected balance diff between 40000-70000 sats"
            );
        });

    info!("🎉 All checks successful. Terminating integration test case");
}
