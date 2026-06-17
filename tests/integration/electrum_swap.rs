//! Electrum-only coinswap tests.
//!
//! - The watch-tower uses `ElectrumNotifier` + `electrum_chain_name`/`electrum_block_count` instead of ZMQ + Bitcoin Core REST.
//! - The offer-sync and Nostr discovery use `electrum_block_count`/`electrum_get_raw_tx`.
//!   Bitcoind is still spawned because it is the source of regtest funds and mines blocks, but the coinswap code itself talks only to electrs.

use super::test_framework::*;
use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};
use log::info;
use std::{sync::atomic::Ordering::Relaxed, thread};

/// Run an Electrum-only coinswap with the given protocol version and assert the
/// post-swap invariants on taker / maker balances.
fn run_electrum_swap(protocol: ProtocolVersion) {
    info!("Running Test: Electrum Coinswap Procedure ({protocol:?})");
    let makers_config_map = vec![(6102, Some(19051)), (16102, Some(19052))];
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<ElectrumBackend>(makers_config_map, taker_behavior, maker_behaviors);
    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();
    let taker_original_balance = fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );
    info!("Initiating Maker servers");
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();
    wait_for_makers_setup(&makers, 180);
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }
    verify_maker_pre_swap_balances(&makers);
    let swap_params = SwapParams::new(protocol, Amount::from_sat(500_000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);
    generate_blocks(bitcoind, 1);
    thread::sleep(std::time::Duration::from_secs(12));
    let summary = taker.prepare_coinswap(swap_params).unwrap();
    taker.start_coinswap(&summary.swap_id).unwrap();
    info!("Coinswap finished. Shutting down makers.");
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads.into_iter().for_each(|t| t.join().unwrap());
    taker.get_wallet().write().unwrap().sync_and_save().unwrap();
    generate_blocks(bitcoind, 1);
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }
    let taker_balances = taker.get_wallet().read().unwrap().get_balances().unwrap();
    assert!(
        taker_balances.swap > Amount::ZERO,
        "Taker should have a non-zero swap balance after a successful coinswap"
    );
    assert_eq!(
        taker_balances.contract,
        Amount::ZERO,
        "All contract outputs should be resolved post-swap"
    );
    assert_eq!(taker_balances.fidelity, Amount::ZERO);
    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .expect("Taker spendable balance should not exceed original");
    assert!(
        balance_diff.to_sat() > 0 && balance_diff.to_sat() < 50_000,
        "Taker fee {} sats out of reasonable range",
        balance_diff.to_sat()
    );
    for (i, maker) in makers.iter().enumerate() {
        let balances = maker.wallet.read().unwrap().get_balances().unwrap();
        assert_eq!(
            balances.contract,
            Amount::ZERO,
            "Maker {} contract balance should be zero",
            i
        );
        assert_eq!(
            balances.fidelity,
            Amount::from_btc(0.05).unwrap(),
            "Maker {} should still hold its fidelity bond",
            i
        );
    }
    info!("Electrum-only coinswap test ({protocol:?}) completed successfully!");
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

#[test]
fn test_electrum_coinswap() {
    run_electrum_swap(ProtocolVersion::Taproot);
    run_electrum_swap(ProtocolVersion::Legacy);
}
