//! Maker admission must fail closed after its watcher thread exits.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{error::TakerError, SwapParams, TakerBehavior},
    wallet::AddressType,
};
use std::{sync::atomic::Ordering::Relaxed, thread};

use super::test_framework::*;

#[test]
fn maker_server_does_not_start_when_watcher_exits() {
    let (test_framework, _takers, makers, block_generation_handle) =
        TestFramework::init::<BitcoindBackend>(
            vec![(7901, Some(20900))],
            Vec::new(),
            vec![MakerBehavior::StopWatcherOnStartup],
        );

    let maker = makers[0].clone();
    let error =
        start_server(maker.clone()).expect_err("maker server started without a live watcher");
    assert!(
        format!("{error:?}").contains("watchtower is down, refusing to start maker server"),
        "unexpected startup error: {:?}",
        error
    );
    assert!(!maker.watch_service.is_alive());
    assert!(!maker.is_setup_complete.load(Relaxed));

    maker.watch_service.shutdown();
    drop(maker);
    drop(makers);
    test_framework.stop();
    block_generation_handle.join().unwrap();
}

#[test]
fn maker_rejects_new_swaps_after_watcher_exit() {
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<BitcoindBackend>(
            vec![(7902, Some(20901))],
            vec![TakerBehavior::Normal],
            vec![MakerBehavior::Normal],
        );
    let bitcoind = &test_framework.bitcoind;

    let taker = &mut takers[0];
    fund_taker(
        taker,
        bitcoind,
        2,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );
    fund_makers(
        &makers,
        bitcoind,
        2,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker = maker.clone();
            thread::spawn(move || start_server(maker).unwrap())
        })
        .collect::<Vec<_>>();
    wait_for_makers_setup(&makers, 120);

    let maker = &makers[0];
    assert!(maker.watch_service.is_alive());
    maker.watch_service.stop_watcher_for_test();
    assert!(!maker.watch_service.is_alive());

    for protocol in [ProtocolVersion::Legacy, ProtocolVersion::Taproot] {
        let result = taker.prepare_coinswap(
            SwapParams::new(protocol, Amount::from_sat(500_000), 1)
                .with_tx_count(1)
                .with_required_confirms(1),
        );
        match result {
            Err(TakerError::General(message)) => assert!(
                message.contains("Maker 0 rejected swap"),
                "unexpected admission error: {}",
                message
            ),
            other => panic!("unexpected admission result: {:?}", other),
        }
    }

    drop(takers);
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|handle| handle.join().unwrap());
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
