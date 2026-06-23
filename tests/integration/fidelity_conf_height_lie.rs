//! Regression test for issue #906: taker must not trust maker-supplied conf_height.
//!
//! A malicious maker creates a short-timelock fidelity bond on-chain but forges
//! conf_height in its offer so the bond appears to meet MIN_FIDELITY_TIMELOCK.
//! The taker should derive conf_height from the blockchain and reject the offer.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{error::TakerError, SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{sync::atomic::Ordering::Relaxed, thread};

#[test]
fn fidelity_conf_height_lie() {
    warn!("Running Test: Fidelity conf_height lie rejection");

    let makers_config_map = vec![(8303, None)];
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![MakerBehavior::LieFidelityConfHeight];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();
    let maker = &makers[0];

    info!("Funding taker and maker");
    fund_taker(
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

    info!("Initiating Maker server with LieFidelityConfHeight behavior");
    let maker_thread = {
        let maker_clone = maker.clone();
        thread::spawn(move || start_server(maker_clone).unwrap())
    };

    wait_for_makers_setup(std::slice::from_ref(maker), 120);
    maker.wallet.write().unwrap().sync_and_save().unwrap();

    info!("Initiating coinswap (should fail: maker lied about conf_height)");
    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500000), 1)
        .with_tx_count(2)
        .with_required_confirms(1);

    let err = taker
        .prepare_coinswap(swap_params)
        .expect_err("Swap should have failed due to NotEnoughMakersInOfferBook");
    assert!(
        matches!(err, TakerError::NotEnoughMakersInOfferBook),
        "Expected NotEnoughMakersInOfferBook, got: {:?}",
        err
    );
    info!("Coinswap failed as expected: {err:?}");

    maker.shutdown.store(true, Relaxed);
    maker_thread.join().unwrap();

    info!("Fidelity conf_height lie test passed");

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
