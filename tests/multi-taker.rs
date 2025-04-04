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
    utill::ConnectionType,
};

use log::{info, warn};

mod test_framework;
use test_framework::*;

/// Multiple Takers with Different Behaviors
/// This test demonstrates a scenario where a single Maker is connected to two Takers
/// exhibiting different behaviors:
/// - Taker1: Normal
/// - Taker2: Drops connection after full setup
///
/// The test verifies that the Maker can properly manage multiple concurrent swaps with
/// different taker behaviors and recover appropriately in each case if required.
#[test]
fn mutli_taker_single_maker_swap() {
    let makers_config_map = [
        ((6102, None), MakerBehavior::Normal),
        ((16102, None), MakerBehavior::Normal),
    ];

    let taker_behavior = vec![
        TakerBehavior::Normal,
        TakerBehavior::DropConnectionAfterFullSetup,
    ];
    // Initiate test framework, Makers.
    // Taker has normal behavior.
    let (test_framework, mut takers, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    warn!("Running Test: Multiple Takers with Different Behaviors");

    // Fund the Takers with 3 utxos of 0.05 btc each and do basic checks on the balance
    let org_taker_spend_balances = takers
        .iter_mut()
        .map(|taker| {
            fund_and_verify_taker(
                taker,
                &test_framework.bitcoind,
                3,
                Amount::from_btc(0.05).unwrap(),
            )
        })
        .collect::<Vec<_>>();

    // Fund the Maker with 4 utxos of 0.05 btc each and do basic checks on the balance.
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(
        makers_ref,
        &test_framework.bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    // Start the Maker Server threads
    log::info!("Initiating Maker...");
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Makers take time to fully setup.
    let org_maker_spend_balances = makers
        .iter()
        .map(|maker| {
            while !maker.is_setup_complete.load(Relaxed) {
                log::info!("Waiting for maker setup completion");
                // Introduce a delay of 10 seconds to prevent write lock starvation.
                thread::sleep(Duration::from_secs(10));
                continue;
            }

            // Check balance after setting up maker server.
            let wallet = maker.wallet.read().unwrap();

            let balances = wallet.get_balances().unwrap();

            assert_eq!(balances.regular, Amount::from_btc(0.14999).unwrap());
            assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
            assert_eq!(balances.swap, Amount::ZERO);
            assert_eq!(balances.contract, Amount::ZERO);

            balances.spendable
        })
        .collect::<Vec<_>>();

    // Initiate Coinswap for both Takers concurrently
    log::info!("Initiating coinswap protocol for multiple takers");

    // Spawn threads for each taker to initiate coinswap concurrently
    thread::scope(|s| {
        for taker in &mut takers {
            let swap_params = SwapParams {
                send_amount: Amount::from_sat(500000),
                maker_count: 2,
                tx_count: 3,
                required_confirms: 1,
            };
            s.spawn(move || {
                taker.do_coinswap(swap_params).unwrap();
            });
            std::thread::sleep(Duration::from_secs(29));
        }
    });

    // After Swap is done, wait for maker threads to conclude
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All coinswaps processed. Transactions complete.");

    // Shutdown Directory Server
    directory_server_instance.shutdown.store(true, Relaxed);
    thread::sleep(Duration::from_secs(10));

    // For Taker2 (DropConnectionAfterFullSetup), run recovery
    warn!("Starting Taker recovery process");
    takers[1].recover_from_swap().unwrap();

    // Verify final state for all participants
    for (i, taker) in takers.iter().enumerate() {
        verify_swap_results(
            taker,
            &makers,
            org_taker_spend_balances[i],
            org_maker_spend_balances.clone(),
        );
    }

    info!("All checks successful. Terminating integration test case");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
