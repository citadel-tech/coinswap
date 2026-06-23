//! Taproot contract validation tests.
//!
//! Scenario:
//! 1. Taker initiates a Taproot coinswap with 1 maker.
//! 2. The maker creates a valid Taproot contract tx, but deliberately funds it
//!    below the amount claimed in its TaprootContractData response.
//! 3. The taker verifies the maker response before creating its incoming
//!    swapcoin.
//! 4. Verify: the taker rejects the maker contract data at amount binding.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{error::TakerError, SwapParams, TakerBehavior},
    wallet::AddressType,
};
use std::{sync::atomic::Ordering::Relaxed, thread};

use super::test_framework::*;

/// Test: taker rejects maker Taproot contract data that is internally
/// inconsistent.
///
/// Without this check, a malicious maker can fund a smaller contract output
/// than it advertises. The taker may then store an incoming swapcoin with the
/// inflated claimed amount and later fail to sweep it.
#[test]
fn test_taproot_rejects_underfunded_maker_contract() {
    // ---- Setup ----
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

    // Fund the taker and maker with P2TR coins so the swap runs through the
    // Taproot funding and contract-data exchange path.
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

    // Start the malicious maker server.
    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || start_server(maker_clone).unwrap())
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(&makers, 120);

    // Mine one block before preparing the swap so wallet state and offer data
    // are settled.
    generate_blocks(bitcoind, 1);

    // A 30k-sat swap keeps the maker's 10k-sat underfunded output valid enough
    // to broadcast while still making the amount mismatch obvious.
    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(30_000), 1)
        .with_tx_count(1)
        .with_required_confirms(1);
    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("failed to prepare Taproot coinswap");

    // The taker must reject during maker contract verification, before storing
    // an incoming swapcoin from the malicious contract data.
    let error = taker
        .start_coinswap(&summary.swap_id)
        .expect_err("taker must reject maker contract data that claims more than the tx output");
    match error {
        TakerError::General(message) => {
            assert!(
                message.contains("Taproot claimed amount")
                    && message.contains("does not match output value"),
                "unexpected taker error: {}",
                message
            );
        }
        other => panic!("unexpected taker error: {:?}", other),
    }

    // Assert the rejection came from the contract amount binding check.
    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log("Taproot claimed amount", &log_path);

    // ---- Cleanup ----
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
