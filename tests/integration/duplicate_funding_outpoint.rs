//! A malicious taker can repeat one or more valid on-chain output and its matching
//! metadata. Without an explicit uniqueness check, each entry passes individual
//! verification and the maker counts the same value twice when funding the next
//! hop. The rejection must therefore happen before the maker creates or
//! broadcasts any outgoing funding transaction.

use std::{sync::atomic::Ordering::Relaxed, thread};

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

#[test]
fn makers_reject_duplicate_funding_outpoints() {
    let makers_config_map = vec![(8802, Some(21301)), (18802, Some(21302))];
    let taker_behaviors = vec![
        TakerBehavior::DuplicateFundingOutpoint,
        TakerBehavior::DuplicateFundingOutpoint,
    ];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behaviors, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    for taker in &mut takers {
        fund_taker(
            taker,
            bitcoind,
            3,
            Amount::from_btc(0.05).unwrap(),
            AddressType::P2TR,
        );
    }
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
            let maker = maker.clone();
            thread::spawn(move || start_server(maker).unwrap())
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(&makers, 120);
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }
    generate_blocks(bitcoind, 1);

    // The Legacy behavior repeats one complete FundingTxInfo entry in
    // ProofOfFunding, causing two entries to reference the same txid:vout.
    let legacy_params = SwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500_000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);
    let legacy_summary = takers[0]
        .prepare_coinswap(legacy_params)
        .expect("Legacy prepare_coinswap should succeed");
    assert!(
        takers[0].start_coinswap(&legacy_summary.swap_id).is_err(),
        "Legacy maker must reject a duplicated funding outpoint"
    );

    // The Taproot behavior repeats one contract transaction together with all
    // aligned per-contract vectors, so length and script checks still pass.
    let taproot_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500_000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);
    let taproot_summary = takers[1]
        .prepare_coinswap(taproot_params)
        .expect("Taproot prepare_coinswap should succeed");
    assert!(
        takers[1].start_coinswap(&taproot_summary.swap_id).is_err(),
        "Taproot maker must reject a duplicated contract outpoint"
    );

    // Assert both the taker side duplicate contract passing and the maker-side rejection.
    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log(
        "Test behavior: duplicating funding outpoint in ProofOfFunding",
        &log_path,
    );
    test_framework.assert_log("Duplicate funding outpoint", &log_path);
    test_framework.assert_log(
        "Test behavior: duplicating Taproot contract outpoint",
        &log_path,
    );
    test_framework.assert_log("Duplicate Taproot contract outpoint", &log_path);

    let log_contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        !log_contents.contains("SECURITY: Broadcasting"),
        "Legacy maker must reject before broadcasting outgoing funding"
    );
    assert!(
        !log_contents.contains("Broadcast Taproot contract tx"),
        "Taproot maker must reject before broadcasting outgoing funding"
    );

    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    drop(takers);
    test_framework.stop();
    block_generation_handle.join().unwrap();
}
