//! A malicious taker can repeat one or more valid on-chain output and its matching
//! metadata. Without an explicit uniqueness check, each entry passes individual
//! verification and the maker counts the same value twice when funding the next
//! hop. The rejection must therefore happen before the maker creates or
//! broadcasts any outgoing funding transaction.

use std::{sync::atomic::Ordering::Relaxed, thread, time::Duration};

use bitcoin::{consensus::encode::serialize_hex, Amount};
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::{AddressType, Destination},
};

use super::test_framework::*;

#[test]
fn makers_reject_duplicate_funding_outpoints() {
    let makers_config_map = vec![(8802, Some(21301)), (18802, Some(21302))];
    let taker_behaviors = vec![TakerBehavior::DuplicateFundingOutpoint];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<BitcoindBackend>(makers_config_map, taker_behaviors, maker_behaviors);

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
        maker
            .wallet
            .write()
            .unwrap()
            .sync_and_save(&coinswap::utill::NO_SHUTDOWN)
            .unwrap();
    }
    generate_blocks(bitcoind, 1);

    // The Taproot behavior repeats one contract transaction together with all
    // aligned per-contract vectors, so length and script checks still pass.
    let taproot_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500_000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);
    let taproot_summary = takers[0]
        .prepare_coinswap(taproot_params)
        .expect("Taproot prepare_coinswap should succeed");
    assert!(
        takers[0].start_coinswap(&taproot_summary.swap_id).is_err(),
        "Taproot maker must reject a duplicated contract outpoint"
    );

    // Assert both the taker side duplicate contract passing and the maker-side rejection.
    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log(
        "Test behavior: duplicating Taproot contract outpoint",
        &log_path,
    );
    test_framework.assert_log("Duplicate Taproot contract outpoint", &log_path);

    let log_contents = std::fs::read_to_string(&log_path).unwrap();
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

/// The maker guards a Legacy funding proof in order — entry count, declared
/// sum, then outpoint duplication — so each malice keeps the earlier guards
/// satisfied to reach its own. One maker is enough: the rejection is the point.
fn run_legacy_proof_guard(port: u16, rpc: u16, behavior: TakerBehavior, expected: &str) {
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<BitcoindBackend>(
            vec![(port, Some(rpc))],
            vec![behavior],
            vec![MakerBehavior::Normal],
        );

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
            let maker = maker.clone();
            thread::spawn(move || start_server(maker).unwrap())
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(&makers, 120);
    for maker in &makers {
        maker
            .wallet
            .write()
            .unwrap()
            .sync_and_save(&coinswap::utill::NO_SHUTDOWN)
            .unwrap();
    }
    generate_blocks(bitcoind, 1);

    let params = SwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500_000), 1)
        .with_tx_count(3)
        .with_required_confirms(1);
    let summary = taker
        .prepare_coinswap(params)
        .expect("prepare_coinswap should succeed");
    assert!(
        taker.start_coinswap(&summary.swap_id).is_err(),
        "maker must reject the crafted ProofOfFunding"
    );

    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log(expected, &log_path);

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

#[test]
fn maker_rejects_overcounted_proof_of_funding() {
    run_legacy_proof_guard(
        8808,
        21310,
        TakerBehavior::ExtraFundingTxEntry,
        "does not match negotiated",
    );
}

#[test]
fn maker_rejects_overstated_proof_of_funding() {
    run_legacy_proof_guard(
        8810,
        21311,
        TakerBehavior::OverstatedFundingAmount,
        "exceeds negotiated swap amount",
    );
}

#[test]
fn maker_rejects_duplicated_funding_outpoint() {
    run_legacy_proof_guard(
        8812,
        21312,
        TakerBehavior::DuplicateFundingOutpoint,
        "Duplicate funding outpoint",
    );
}

/// A confirmed funding txid proves nothing about its outputs. Here the taker claims
/// its own funding output through the contract path first, then still names that
/// outpoint in ProofOfFunding. The maker must refuse before funding the next hop.
fn run_rejects_spent_funding_outpoint<B: TestBackend>(behavior: TakerBehavior) {
    let makers_config_map = vec![(8804, Some(21303)), (18804, Some(21304))];
    let taker_behaviors = vec![behavior];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<B>(makers_config_map, taker_behaviors, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    fund_taker(
        &takers[0],
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
            let maker = maker.clone();
            thread::spawn(move || start_server(maker).unwrap())
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(&makers, 120);
    for maker in &makers {
        maker
            .wallet
            .write()
            .unwrap()
            .sync_and_save(&coinswap::utill::NO_SHUTDOWN)
            .unwrap();
    }
    generate_blocks(bitcoind, 1);

    let params = SwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500_000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);
    let summary = takers[0]
        .prepare_coinswap(params)
        .expect("Legacy prepare_coinswap should succeed");
    assert!(
        takers[0].start_coinswap(&summary.swap_id).is_err(),
        "Legacy maker must reject an already spent funding outpoint"
    );

    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());
    test_framework.assert_log(
        "Test behavior: spending the funding outpoint before ProofOfFunding",
        &log_path,
    );
    test_framework.assert_log("Funding output already spent", &log_path);

    let log_contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        !log_contents.contains("SECURITY: Broadcasting"),
        "Maker must reject before broadcasting outgoing funding"
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

#[test]
fn maker_rejects_spent_funding_outpoint() {
    run_rejects_spent_funding_outpoint::<BitcoindBackend>(
        TakerBehavior::ReplaySpentFundingOutpoint,
    );
}

#[test]
fn maker_rejects_spent_funding_outpoint_mempool() {
    run_rejects_spent_funding_outpoint::<ElectrumBackend>(
        TakerBehavior::ReplaySpentFundingOutpointMempool,
    );
}

/// A funding tx the maker has already seen can still vanish from the mempool
/// (evicted or replaced). The maker must error out of its confirmation wait
/// once the re-armed broadcast window expires, not wait forever.
#[test]
fn maker_errors_when_seen_funding_tx_is_evicted() {
    let makers_config_map = vec![(8806, Some(21305)), (18806, Some(21306))];
    let taker_behaviors = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init::<BitcoindBackend>(makers_config_map, taker_behaviors, maker_behaviors);

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
            let maker = maker.clone();
            thread::spawn(move || start_server(maker).unwrap())
        })
        .collect::<Vec<_>>();

    wait_for_makers_setup(&makers, 120);
    for maker in &makers {
        maker
            .wallet
            .write()
            .unwrap()
            .sync_and_save(&coinswap::utill::NO_SHUTDOWN)
            .unwrap();
    }
    generate_blocks(bitcoind, 1);

    // Sign a double-spend of every taker UTXO up front, so it can replace the
    // funding tx (which signals RBF) the moment the maker reports seeing it.
    let conflict_tx = {
        let mut wallet = taker.get_wallet().write().unwrap();
        wallet.sync_and_save(&coinswap::utill::NO_SHUTDOWN).unwrap();
        let coins = wallet.list_all_utxo_spend_info();
        let destination = wallet
            .get_next_internal_addresses(1, AddressType::P2TR)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        wallet
            .spend_coins(&coins, Destination::Sweep(destination), 200.0)
            .unwrap()
    };

    // Zero required confirms sends the contract data while the funding tx is
    // still in the mempool, which is what puts the maker into its wait.
    let swap_params = SwapParams::new(ProtocolVersion::Taproot, Amount::from_sat(500_000), 2)
        .with_tx_count(1)
        .with_required_confirms(0);
    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("prepare_coinswap should succeed");

    let log_path = format!("{}/taker/debug.log", test_framework.temp_dir.display());

    // Mining is paused so the funding tx can never confirm; the maker stays in
    // its confirmation wait until the conflict evicts the tx from the mempool.
    test_framework.set_block_gen_paused(true);
    let swap_result = thread::scope(|s| {
        let swap_handle = s.spawn(|| taker.start_coinswap(&summary.swap_id));

        wait_for_new_log(&log_path, "seen in mempool", Duration::from_secs(120));
        // Sent through bitcoind, not the taker's wallet, whose lock the swap
        // thread holds for long stretches.
        bitcoind
            .client
            .send_raw_transaction(serialize_hex(&conflict_tx))
            .expect("conflict tx should replace the funding tx in the mempool");

        // One re-armed broadcast window must pass before the maker errors.
        wait_for_log(&log_path, "did not reappear", Duration::from_secs(240));
        test_framework.set_block_gen_paused(false);
        swap_handle.join().expect("taker thread panicked")
    });
    assert!(
        swap_result.is_err(),
        "The swap must fail once the maker's funding wait errors out"
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
