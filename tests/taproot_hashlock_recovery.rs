#![cfg(feature = "integration-test")]
//! Integration test for Taproot Hashlock Recovery
//!
//! This test demonstrates end-to-end hashlock-based recovery when contracts are broadcasted
//! and one party sweeps via hashlock path, revealing the preimage. The other party should
//! extract the preimage from the blockchain and recover their funds via hashlock.

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMaker, TaprootMakerBehavior as MakerBehavior},
    taker::{
        api2::{SwapParams, Taker},
        TakerBehavior,
    },
    wallet::AddressType,
};
use std::sync::Arc;

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

    info!("📊 Taker balance after recovery:");
    let taker_balances_after = taproot_taker.get_wallet().get_balances().unwrap();
    info!(
        "  Regular: {}, Contract: {}, Spendable: {}",
        taker_balances_after.regular, taker_balances_after.contract, taker_balances_after.spendable
    );

    // Verify taker recovered their funds via hashlock
    let max_taker_fees = Amount::from_sat(100000);
    assert!(
        taker_balances_after.spendable > taproot_taker_original_balance - max_taker_fees,
        "Taker should have recovered via hashlock. Original: {}, After: {}, Lost: {}",
        taproot_taker_original_balance,
        taker_balances_after.spendable,
        taproot_taker_original_balance - taker_balances_after.spendable
    );

    // Now wait for maker to extract preimage and recover via hashlock
    info!("⏳ Waiting for maker to extract preimage and recover via hashlock...");
    std::thread::sleep(std::time::Duration::from_secs(60));

    // Mine more blocks to give maker time to see the hashlock sweep
    generate_blocks(bitcoind, 2);

    // Wait a bit more for maker's recovery
    std::thread::sleep(std::time::Duration::from_secs(10));

    // Verify maker recovered their incoming contract via hashlock
    let maker_balance_after = {
        let mut wallet = taproot_makers[0].wallet().write().unwrap();
        wallet.sync_and_save().unwrap();
        let balances = wallet.get_balances().unwrap();
        info!(
            "📊 Maker balance after hashlock recovery: Regular: {}, Spendable: {}",
            balances.regular, balances.spendable
        );
        balances.spendable
    };

    // Maker should have recovered their outgoing contract via hashlock after extracting preimage
    // They swept incoming (~500k sats) and should have it confirmed
    let max_maker_fees = Amount::from_sat(100000); // 0.001 BTC max fees
    assert!(
        maker_balance_after >= maker_balance_before - max_maker_fees,
        "Maker should have recovered via hashlock. Before: {}, After: {}, Lost: {}",
        maker_balance_before,
        maker_balance_after,
        maker_balance_before - maker_balance_after
    );

    info!("✅ Hashlock recovery test passed!");
    info!(
        "   Taker original balance: {}, Recovered: {}, Fees paid: {}",
        taproot_taker_original_balance,
        taker_balances_after.spendable,
        taproot_taker_original_balance - taker_balances_after.spendable
    );
    info!(
        "   Maker balance before: {}, After: {} (change: {})",
        maker_balance_before,
        maker_balance_after,
        maker_balance_after.to_sat() as i64 - maker_balance_before.to_sat() as i64
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
            let addr = wallet
                .get_next_external_address(AddressType::P2WPKH)
                .unwrap();
            send_to_address(bitcoind, &addr, utxo_value);
        }

        generate_blocks(bitcoind, 1);
        wallet.sync_and_save().unwrap();

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
        let addr = taker
            .get_wallet_mut()
            .get_next_external_address(AddressType::P2WPKH)
            .unwrap();
        send_to_address(bitcoind, &addr, utxo_value);
    }

    generate_blocks(bitcoind, 1);
    taker.get_wallet_mut().sync_and_save().unwrap();

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
