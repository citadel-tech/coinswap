#![cfg(feature = "integration-test")]

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server_taproot, TaprootMakerBehavior},
    taker::{
        api2::{SwapParams, TakerBehavior},
        error::TakerError,
    },
};
use std::convert::TryInto;

mod test_framework;
use test_framework::*;

use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

#[test]
fn taproot_manual_maker_selection_smoke_test() {
    std::env::set_var("COINSWAP_DISABLE_NOSTR", "1");

    // Spin up FIVE makers
    let maker_ports = [62000, 62001, 62002, 62003, 62004];
    let makers_config_map = maker_ports
        .iter()
        .map(|p| (*p, None, TaprootMakerBehavior::Normal))
        .collect::<Vec<_>>();

    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init_taproot(makers_config_map, taker_behavior);

    let taker = takers.get_mut(0).expect("taker exists");
    let bitcoind = &test_framework.bitcoind;

    // Fund wallets
    fund_taproot_taker(taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());
    fund_taproot_makers(&makers, bitcoind, 3, Amount::from_btc(0.05).unwrap());

    // Start maker servers
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker = maker.clone();
            thread::spawn(move || start_maker_server_taproot(maker).unwrap())
        })
        .collect::<Vec<_>>();

    // Wait for maker setup
    for maker in &makers {
        while !maker.is_setup_complete.load(Relaxed) {
            thread::sleep(Duration::from_secs(1));
        }
    }

    // Allow offer discovery
    for _ in 0..10 {
        if !taker.is_offerbook_syncing() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }

    // Manually select ONLY TWO makers
    let selected_makers = vec![
        ("127.0.0.1:62000").to_string().try_into().unwrap(),
        ("127.0.0.1:62001").to_string().try_into().unwrap(),
    ];

    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500_000),
        maker_count: 2,
        tx_count: 1,
        required_confirms: 1,
        manually_selected_outpoints: None,
        manually_selected_makers: Some(selected_makers.clone()),
    };

    // ✅ BEHAVIORAL ASSERTION
    let result = taker.do_coinswap(swap_params);
    let report = result
        .expect("manual maker selection swap should succeed using only selected makers")
        .expect("swap report should be present");

    let used_makers = report.maker_addresses;
    assert_eq!(used_makers.len(), 2, "Should have used exactly 2 makers");

    // Check that used makers are exactly the ones we selected
    let expected_makers: Vec<String> = selected_makers
        .iter()
        .map(|formatted| formatted.to_string())
        .collect();

    for maker in &used_makers {
        assert!(
            expected_makers.contains(maker),
            "Used maker {} which was not manually selected",
            maker
        );
    }
    // --- NEGATIVE ASSERTION ---
    // If the taker were silently pulling extra makers from the offerbook,
    // this would succeed. It must FAIL.

    let bad_params = SwapParams {
        send_amount: Amount::from_sat(500_000),
        maker_count: 3, // ❌ mismatch: only 2 manual makers provided
        tx_count: 1,
        required_confirms: 1,
        manually_selected_outpoints: None,
        manually_selected_makers: Some(vec![
            ("127.0.0.1:62000").to_string().try_into().unwrap(),
            ("127.0.0.1:62001").to_string().try_into().unwrap(),
        ]),
    };

    let err = taker.do_coinswap(bad_params).unwrap_err();

    match err {
        TakerError::General(msg) => {
            assert!(
                msg.contains("manually selected"),
                "unexpected error message: {msg}"
            );
        }
        other => panic!("expected General error, got {:?}", other),
    }

    // Cleanup
    makers.iter().for_each(|m| m.shutdown.store(true, Relaxed));
    maker_threads.into_iter().for_each(|t| t.join().unwrap());
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
