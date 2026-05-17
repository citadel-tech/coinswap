//! Regression test for the fee-rate undershoot bug in `Wallet::spend_coins`.
//!
//! `estimate_witness_size` historically assumed a 72-byte low-R ECDSA signature.
//! Bitcoin Core's `sign_ecdsa_low_r` actually produces 71-73 byte signatures, so
//! when an input's real signature landed at 73 bytes the post-sign vbytes
//! exceeded the pre-sign estimate by 1 byte per input, and the broadcast feerate
//! slipped below the requested rate. At `MIN_FEE_RATE = 1 sat/vB` that lands
//! below the mempool relay floor and the tx is rejected.
//!
//! With the witness-size estimates bumped to reserve the worst-case 73-byte sig,
//! the estimate is now an upper bound and the actual feerate must always meet
//! or exceed the requested rate.

use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    taker::TakerBehavior,
    wallet::{AddressType, Destination},
};

use super::test_framework::*;

/// Fund the taker with enough small UTXOs that a sweep must combine many
/// inputs — every extra input is an extra chance for low-R signing to land at
/// the worst-case 73-byte signature and trip the undershoot.
const UTXO_SET: &[u64] = &[
    21_111, 22_222, 23_333, 24_444, 25_555, 26_666, 27_777, 28_888, 29_999, 31_111, 32_222, 33_333,
];

const TARGET_FEERATE_SAT_PER_VB: f64 = 1.0;

#[test]
fn test_spend_coins_meets_minimum_relay_feerate() {
    let (test_framework, mut takers, _makers, _block_generation_handle) =
        TestFramework::init(vec![], vec![TakerBehavior::Normal], vec![]);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    for amount in UTXO_SET {
        let addr = taker
            .get_wallet()
            .write()
            .unwrap()
            .get_next_external_address(AddressType::P2WPKH)
            .unwrap();
        send_to_address(bitcoind, &addr, Amount::from_sat(*amount));
        generate_blocks(bitcoind, 1);
    }

    taker.get_wallet().write().unwrap().sync_and_save().unwrap();

    let coins = taker
        .get_wallet()
        .read()
        .unwrap()
        .list_descriptor_utxo_spend_info();

    let sweep_addr = taker
        .get_wallet()
        .write()
        .unwrap()
        .get_next_external_address(AddressType::P2WPKH)
        .unwrap();

    let tx = taker
        .get_wallet()
        .write()
        .unwrap()
        .spend_from_wallet(
            TARGET_FEERATE_SAT_PER_VB,
            Destination::Sweep(sweep_addr),
            &coins,
        )
        .expect("spend_from_wallet should succeed at feerate=1 sat/vB");

    let total_input: u64 = coins.iter().map(|(u, _)| u.amount.to_sat()).sum();
    let total_output: u64 = tx.output.iter().map(|o| o.value.to_sat()).sum();
    let actual_fee = total_input - total_output;
    let actual_vsize = tx.weight().to_vbytes_ceil();
    let actual_feerate = actual_fee as f64 / actual_vsize as f64;

    println!(
        "inputs={} vsize={} fee={} feerate={:.6} target={:.1}",
        tx.input.len(),
        actual_vsize,
        actual_fee,
        actual_feerate,
        TARGET_FEERATE_SAT_PER_VB,
    );

    assert!(
        actual_feerate >= TARGET_FEERATE_SAT_PER_VB,
        "Actual feerate ({}) fell below target ({}) — witness-size estimate is undershooting",
        actual_feerate,
        TARGET_FEERATE_SAT_PER_VB,
    );

    // The transaction must also be accepted by the default mempool policy.
    bitcoind
        .client
        .send_raw_transaction(&tx)
        .expect("tx must clear the mempool relay floor");

    test_framework.stop();
}
