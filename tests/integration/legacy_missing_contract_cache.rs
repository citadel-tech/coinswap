//! Legacy proof-of-funding must fail closed when no sender-contract binding exists.
//!
//! Normally, `ReqContractSigsForSender` makes the first maker bind each funding
//! outpoint to its approved contract before funding is broadcast.
//!
//! This test keeps both makers unmodified and makes a malicious taker:
//! 1. Negotiate a normal two-maker Legacy swap.
//! 2. Skip `ReqContractSigsForSender`, leaving maker 0's cache empty.
//! 3. Confirm funding, then send valid transactions and SPV proofs to maker 0.
//!
//! The test asserts rejection, the operator-visible error, no maker outgoing
//! broadcast, and unchanged maker liquidity.
//! The taker intentionally gives up its recovery signature using disposable regtest funds.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use std::{sync::atomic::Ordering::Relaxed, thread};

#[test]
fn maker_rejects_proof_of_funding_with_missing_contract_cache() {
    let makers_config_map = vec![(6102, None), (16102, None)];
    let taker_behavior = vec![TakerBehavior::SkipSenderContractSigs];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

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
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_before = makers[0]
        .wallet
        .read()
        .unwrap()
        .get_balances()
        .unwrap()
        .spendable;

    let swap_params = SwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500_000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);
    generate_blocks(bitcoind, 1);

    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("prepare_coinswap should succeed");

    let result = taker.start_coinswap(&summary.swap_id);
    assert!(
        result.is_err(),
        "maker must reject ProofOfFunding without a cached contract binding"
    );

    // Assert both the adversarial action and the maker's fail-closed reason.
    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log(
        "Test behavior: skipping sender contract signature request before funding",
        &log_path,
    );
    test_framework.assert_log("No cached sender contract for funding prevout", &log_path);

    // Rejection must happen before the maker reaches the outgoing broadcast
    // boundary in process_resp_contract_sigs_for_recvr_and_sender.
    let log_contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        !log_contents.contains("SECURITY: Broadcasting"),
        "maker must reject before broadcasting outgoing funding transactions"
    );

    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    makers[0].wallet.write().unwrap().sync_and_save().unwrap();
    let maker_spendable_after = makers[0]
        .wallet
        .read()
        .unwrap()
        .get_balances()
        .unwrap()
        .spendable;
    assert_eq!(
        maker_spendable_after, maker_spendable_before,
        "rejected proof must not spend maker liquidity"
    );

    test_framework.stop();
    block_generation_handle.join().unwrap();
}
