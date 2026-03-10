//! Integration test for multi-maker coinswap with Taproot protocol.
//!
//! Setup: 1 taker with Normal behavior, 4 makers with Normal behavior.
//! Protocol: Taproot (MuSig2), AddressType::P2TR.
//! The taker routes the swap through all 4 makers.

use bitcoin::Amount;
use coinswap::{
    maker::start_unified_server,
    protocol::common_messages::ProtocolVersion,
    taker::{UnifiedSwapParams, UnifiedTakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread};

#[test]
fn test_taproot_multi_maker_coinswap() {
    // ---- Setup ----
    warn!("Running Test: Multi-Maker Coinswap with Taproot (MuSig2) Protocol - 4 Makers");

    let makers_config_map = vec![
        (7802, Some(20801)),
        (17802, Some(20802)),
        (27802, Some(20803)),
        (37802, Some(20804)),
    ];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];

    // Initialize test framework with 1 taker and 4 makers
    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, vec![]);

    let bitcoind = &test_framework.bitcoind;
    let unified_taker = unified_takers.get_mut(0).unwrap();

    // Fund the Unified Taker with 5 UTXOs of 0.05 BTC each (P2TR for Taproot)
    // Need more UTXOs for a 4-maker route
    let taker_original_balance = fund_unified_taker(
        unified_taker,
        bitcoind,
        5,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Fund all 4 Unified Makers with 4 UTXOs of 0.05 BTC each
    fund_unified_makers(
        &unified_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Start the Unified Maker Server threads
    log::info!("Initiating 4 Unified Makers with unified server...");

    let maker_threads = unified_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_unified_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for all makers to complete setup
    wait_for_makers_setup(&unified_makers, 120);

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);
    log::info!("Starting end-to-end unified swap test with Taproot protocol and 4 makers...");

    // Swap params for unified coinswap (Taproot) with 4 makers
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 4)
        .with_tx_count(5)
        .with_required_confirms(1);

    // Mine some blocks before the swap to ensure wallet is ready
    generate_blocks(bitcoind, 1);

    // Prepare the swap (negotiate with makers, get fee summary)
    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Failed to prepare Taproot coinswap with 4 makers");
    log::info!("Swap summary: {:?}", summary);

    // Execute the swap
    match unified_taker.start_coinswap(&summary.swap_id) {
        Ok(report) => {
            log::info!("Unified coinswap (Taproot, 4 makers) completed successfully!");
            log::info!("Swap report: {:?}", report);
        }
        Err(e) => {
            log::error!("Unified coinswap (Taproot, 4 makers) failed: {:?}", e);
            panic!("Unified coinswap (Taproot, 4 makers) failed: {:?}", e);
        }
    }

    // After swap, shutdown maker threads
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    log::info!("All unified coinswaps processed successfully. Transaction complete.");

    // Sync wallets and verify results
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    // Mine a block to confirm the sweep transactions
    generate_blocks(bitcoind, 1);

    // Synchronize each maker's wallet
    for maker in unified_makers.iter() {
        let mut wallet = maker.wallet.write().unwrap();
        wallet.sync_and_save().unwrap();
    }

    let taker_balances_after = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();
    info!("Unified Taker balance after completing swap (Taproot, 4 makers):");
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}, Swap: {}",
        taker_balances_after.regular,
        taker_balances_after.contract,
        taker_balances_after.spendable,
        taker_balances_after.swap,
    );

    // Verify swap results
    let balance_diff = taker_original_balance - taker_balances_after.spendable;
    info!(
        "Unified Taker (Taproot, 4 makers) balance verification passed. Original spendable: {}, After spendable: {} (fees paid: {})",
        taker_original_balance,
        taker_balances_after.spendable,
        balance_diff
    );

    // 4-maker routes have compounding DER signature variance (~100 sat range)
    let spendable = taker_balances_after.spendable.to_sat();
    assert!(
        (24989900..=24990200).contains(&spendable),
        "Taker spendable balance out of range: {}",
        spendable
    );
    assert_in_range!(
        taker_balances_after.contract.to_sat(),
        [0],
        "Taker contract balance mismatch"
    );
    assert_eq!(taker_balances_after.fidelity, Amount::ZERO);
    let diff = balance_diff.to_sat();
    assert!(
        (9800..=10100).contains(&diff),
        "Taker fee paid out of range: {}",
        diff
    );

    // Verify all 4 makers earned fees
    for (i, (maker, original_spendable)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let wallet = maker.wallet.read().unwrap();
        let balances = wallet.get_balances().unwrap();

        info!(
            "Unified Maker {} final balances - Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i, balances.regular, balances.swap, balances.contract, balances.fidelity, balances.spendable,
        );

        // Maker regular balances vary by position in the route (each hop deducts fees)
        let regular = balances.regular.to_sat();
        assert!(
            (14501900..=14508900).contains(&regular),
            "Maker {} regular balance out of range: {}",
            i,
            regular
        );
        // Swap amounts decrease along the route as fees are deducted
        let swap = balances.swap.to_sat();
        assert!(
            (491800..=500000).contains(&swap),
            "Maker {} swap balance out of range: {}",
            i,
            swap
        );
        assert_in_range!(
            balances.contract.to_sat(),
            [0],
            "Maker contract balance mismatch"
        );
        assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());

        let maker_fee = balances
            .spendable
            .checked_sub(original_spendable)
            .unwrap_or(Amount::ZERO);

        info!(
            "Unified Maker {} fee earned: {} sats",
            i,
            maker_fee.to_sat()
        );

        // Fee earned depends on position: first maker earns most, last earns least
        let fee = maker_fee.to_sat();
        assert!(
            (1000..=2500).contains(&fee),
            "Maker {} fee earned out of range: {}",
            i,
            fee
        );
    }

    info!("All multi-maker swap tests (Taproot, 4 makers) completed successfully!");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
