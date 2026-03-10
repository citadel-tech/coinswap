//! First maker drops before sending sender's contract sigs. Taker continues with remaining makers.
//!
//! Setup: 3 makers (maker[0] CloseAtReqContractSigsForSender, maker[1] Normal, maker[2] Normal).
//! The taker only needs 2 makers for the route, so when maker[0] drops, it retries with the spare.
//! The swap should succeed.

use bitcoin::Amount;
use coinswap::{
    maker::{start_unified_server, UnifiedMakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{UnifiedSwapParams, UnifiedTakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread};

#[test]
fn maker_abort2_case2() {
    warn!("Running Test: First maker drops before sending sender's sigs. Taker continues with remaining makers.");

    let makers_config_map = vec![(6102, None), (16102, None), (26102, None)];
    let taker_behavior = vec![UnifiedTakerBehavior::Normal];
    let maker_behaviors = vec![
        UnifiedMakerBehavior::CloseAtReqContractSigsForSender,
        UnifiedMakerBehavior::Normal,
        UnifiedMakerBehavior::Normal,
    ];

    let (test_framework, mut unified_takers, unified_makers, block_generation_handle) =
        TestFramework::init_unified(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let unified_taker = unified_takers.get_mut(0).unwrap();

    // Fund the Unified Taker with 3 UTXOs of 0.05 BTC each (P2WPKH for Legacy)
    let taker_original_balance = fund_unified_taker(
        unified_taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Fund the Unified Makers with 4 UTXOs of 0.05 BTC each
    fund_unified_makers(
        &unified_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2WPKH,
    );

    // Start the Unified Maker Server threads
    log::info!("Initiating Unified Makers with unified server...");

    let maker_threads = unified_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_unified_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for makers to complete setup
    wait_for_makers_setup(&unified_makers, 120);

    // Sync wallets after setup to ensure fidelity bonds are accounted for
    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_unified_maker_pre_swap_balances(&unified_makers);

    // Swap params for unified coinswap (Legacy)
    let swap_params = UnifiedSwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    // Prepare and execute the swap — taker should retry with the spare maker
    let summary = unified_taker
        .prepare_coinswap(swap_params)
        .expect("Failed to prepare coinswap");
    unified_taker
        .start_coinswap(&summary.swap_id)
        .expect("Swap should succeed with spare maker");

    // Shutdown makers
    unified_makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Sync wallets and verify results
    unified_taker
        .get_wallet()
        .write()
        .unwrap()
        .sync_and_save()
        .unwrap();

    generate_blocks(bitcoind, 1);

    for maker in &unified_makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Verify taker balance
    let taker_balances = unified_taker
        .get_wallet()
        .read()
        .unwrap()
        .get_balances()
        .unwrap();

    info!(
        "Taker balance: original={}, after={}",
        taker_original_balance, taker_balances.spendable
    );

    assert_in_range!(
        taker_balances.spendable.to_sat(),
        [14995274],
        "Taker spendable balance mismatch"
    );
    assert_in_range!(
        taker_balances.contract.to_sat(),
        [0],
        "Taker contract balance mismatch"
    );
    assert_eq!(taker_balances.fidelity, Amount::ZERO);

    // Verify makers earned fees (only the two that participated)
    for (i, (maker, original)) in unified_makers
        .iter()
        .zip(maker_spendable_balance)
        .enumerate()
    {
        let balances = maker.wallet.read().unwrap().get_balances().unwrap();
        info!(
            "Maker {} balances: original={}, after={}",
            i, original, balances.spendable
        );
        assert_in_range!(
            balances.spendable.to_sat(),
            [14999498, 15000766, 15001144],
            "Maker spendable balance mismatch"
        );
        assert_in_range!(
            balances.contract.to_sat(),
            [0],
            "Maker contract balance mismatch"
        );
        assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());
    }

    info!("maker_abort2_case2 completed successfully!");
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
