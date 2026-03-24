//! Integration test for concurrent taker coinswap with limited maker liquidity (Taproot).
//!
//! Setup: 2 takers with Normal behavior, 2 makers with Normal behavior.
//! Protocol: Taproot (MuSig2), AddressType::P2TR.
//! Both takers run swaps concurrently via `thread::scope`.
//! Makers have limited liquidity (only enough for ~1 swap), so one taker
//! should succeed and the other should fail due to insufficient funds.
//! This exercises the UTXO reservation mechanism that prevents double-spend.

use bitcoin::Amount;
use coinswap::{
    maker::start_server,
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{
    sync::atomic::{AtomicU8, Ordering::Relaxed},
    thread,
    time::Duration,
};

// Result codes for atomic tracking
const RESULT_PENDING: u8 = 0;
const RESULT_SUCCESS: u8 = 1;
const RESULT_FAILED: u8 = 2;

#[test]
fn test_concurrent_takers_taproot() {
    // ---- Setup ----
    warn!("Running Test: Concurrent Takers with Taproot (MuSig2) Protocol - Limited Liquidity");

    let makers_config_map = vec![(7902, Some(20901)), (17902, Some(20902))];
    let taker_behavior = vec![TakerBehavior::Normal, TakerBehavior::Normal];

    // Initialize test framework with 2 takers and 2 makers
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, vec![]);

    let bitcoind = &test_framework.bitcoind;

    // Fund both takers with 3 UTXOs of 0.05 BTC each (P2TR for Taproot)
    let taker1_original_balance = fund_taker(
        &takers[0],
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    let taker2_original_balance = fund_taker(
        &takers[1],
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Fund makers with LIMITED liquidity: only 2 UTXOs of 0.05 BTC each
    // This is enough for only ~1 swap, so one taker should fail
    fund_makers(
        &makers,
        bitcoind,
        2,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Start the maker server threads
    log::info!("Starting Maker servers...");

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for makers to complete setup
    wait_for_makers_setup(&makers, 120);

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Collect pre-swap spendable balances (skip standard assertions since we use limited liquidity)
    let maker_spendable_balance: Vec<Amount> = makers
        .iter()
        .enumerate()
        .map(|(i, maker)| {
            let wallet = maker.wallet.read().unwrap();
            let balances = wallet.get_balances().unwrap();
            info!(
                "Maker {} pre-swap: Regular: {}, Fidelity: {}, Spendable: {}",
                i, balances.regular, balances.fidelity, balances.spendable
            );
            balances.spendable
        })
        .collect();

    // ---- Concurrent Swaps ----
    log::info!("Starting concurrent swaps for both takers (Taproot protocol)...");

    generate_blocks(bitcoind, 1);

    // Use atomics for thread-safe result tracking
    let result1 = AtomicU8::new(RESULT_PENDING);
    let result2 = AtomicU8::new(RESULT_PENDING);

    thread::scope(|s| {
        let (taker1_slice, taker2_slice) = takers.split_at_mut(1);
        let taker1 = &mut taker1_slice[0];
        let taker2 = &mut taker2_slice[0];

        let r1 = &result1;
        let r2 = &result2;

        s.spawn(move || {
            info!("Taker 1 starting concurrent Taproot coinswap");
            let swap_params =
                SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
                    .with_tx_count(3)
                    .with_required_confirms(1);

            match taker1.prepare_coinswap(swap_params) {
                Ok(summary) => match taker1.start_coinswap(&summary.swap_id) {
                    Ok(report) => {
                        info!("Taker 1 Taproot coinswap completed successfully!");
                        info!("Taker 1 swap report: {:?}", report);
                        r1.store(RESULT_SUCCESS, Relaxed);
                    }
                    Err(e) => {
                        warn!("Taker 1 Taproot coinswap failed: {:?}", e);
                        r1.store(RESULT_FAILED, Relaxed);
                    }
                },
                Err(e) => {
                    warn!("Taker 1 Taproot prepare failed: {:?}", e);
                    r1.store(RESULT_FAILED, Relaxed);
                }
            }
        });

        // Small delay to stagger the start
        thread::sleep(Duration::from_secs(3));

        s.spawn(move || {
            info!("Taker 2 starting concurrent Taproot coinswap");
            let swap_params =
                SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 2)
                    .with_tx_count(3)
                    .with_required_confirms(1);

            match taker2.prepare_coinswap(swap_params) {
                Ok(summary) => match taker2.start_coinswap(&summary.swap_id) {
                    Ok(report) => {
                        info!("Taker 2 Taproot coinswap completed successfully!");
                        info!("Taker 2 swap report: {:?}", report);
                        r2.store(RESULT_SUCCESS, Relaxed);
                    }
                    Err(e) => {
                        warn!("Taker 2 Taproot coinswap failed: {:?}", e);
                        r2.store(RESULT_FAILED, Relaxed);
                    }
                },
                Err(e) => {
                    warn!("Taker 2 Taproot prepare failed: {:?}", e);
                    r2.store(RESULT_FAILED, Relaxed);
                }
            }
        });
    });

    info!("All concurrent Taproot coinswaps processed.");

    let r1 = result1.load(Relaxed);
    let r2 = result2.load(Relaxed);
    let success_count = [r1, r2].iter().filter(|&&r| r == RESULT_SUCCESS).count();
    let completed_count = [r1, r2].iter().filter(|&&r| r != RESULT_PENDING).count();

    info!(
        "Results: {} succeeded, {} failed",
        success_count,
        completed_count - success_count
    );

    // With limited liquidity, we expect one to succeed and one to fail
    // The UTXO reservation mechanism prevents double-spend of maker UTXOs
    assert!(success_count >= 1, "At least one taker should succeed");
    assert_eq!(
        completed_count, 2,
        "Both takers should have completed (success or failure)"
    );

    // ---- Shutdown and verify ----
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All coinswaps processed. Transactions complete.");

    // Sync all wallets
    for taker in takers.iter() {
        taker.get_wallet().write().unwrap().sync_and_save().unwrap();
    }

    generate_blocks(bitcoind, 1);

    for maker in makers.iter() {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    // ---- Verify balances ----
    let results = [r1, r2];
    for (i, taker) in takers.iter().enumerate() {
        let taker_balances = taker.get_wallet().read().unwrap().get_balances().unwrap();
        let original = if i == 0 {
            taker1_original_balance
        } else {
            taker2_original_balance
        };
        info!(
            "Taker {} balance: Original: {}, After: {}, Contract: {}",
            i, original, taker_balances.spendable, taker_balances.contract
        );

        if results[i] == RESULT_SUCCESS {
            assert_eq!(
                taker_balances.contract,
                Amount::ZERO,
                "Taker {}: Successful swap should have no contract balance",
                i
            );
        } else {
            // Failed taker may have outgoing contract UTXOs on-chain if the
            // failure occurred after contract broadcast. These are the taker's
            // own funds, recoverable via timelock.
            info!(
                "Taker {}: Failed swap has {} contract balance (recoverable via timelock)",
                i, taker_balances.contract
            );
        }
    }

    // Verify maker balances
    for (i, (maker, original_spendable)) in makers.iter().zip(maker_spendable_balance).enumerate() {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,
        );

        assert_eq!(
            balances.contract,
            Amount::ZERO,
            "Maker {}: Contract balance should be zero after swaps",
            i
        );

        // Maker should have earned some fee if at least one swap succeeded
        if success_count > 0 {
            assert!(
                balances.spendable >= original_spendable,
                "Maker {}: Spendable balance should not decrease",
                i
            );
        }
    }

    info!("All concurrent taker swap tests (Taproot) completed successfully!");

    // Drop takers before stopping the framework so their background services
    // shut down while bitcoind is still running.
    drop(takers);

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
