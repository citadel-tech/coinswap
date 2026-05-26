//! Taproot contract validation tests.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};
use std::{sync::atomic::Ordering::Relaxed, thread};

use super::test_framework::*;

#[test]
fn test_taproot_rejects_underfunded_maker_contract() {
    let makers_config_map = vec![(7102, Some(19061))];
    let taker_behavior = vec![TakerBehavior::Normal];
    // The maker funds a 10k-sat Taproot output but advertises the normal
    // post-fee amount in TaprootContractData. This models a maker trying to
    // make the taker accept an incoming swapcoin for more than the tx pays.
    let maker_behaviors = vec![MakerBehavior::UnderfundTaprootContract];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();

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

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || start_server(maker_clone).unwrap())
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(&makers, 120);
    generate_blocks(bitcoind, 1);

    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(20_000), 1)
        .with_tx_count(1)
        .with_required_confirms(1);
    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("failed to prepare Taproot coinswap");

    // The taker must reject during maker contract verification, before storing
    // an incoming swapcoin from the malicious contract data.
    assert!(
        taker.start_coinswap(&summary.swap_id).is_err(),
        "taker must reject maker contract data that claims more than the tx output"
    );

    // Assert the rejection came from the contract amount binding check.
    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log("Taproot claimed amount", &log_path);

    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
