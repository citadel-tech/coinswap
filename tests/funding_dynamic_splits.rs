#![cfg(feature = "integration-test")]
mod test_framework;
use std::vec;

use bitcoin::{Address, Amount};
use coinswap::{
    taker::TakerBehavior,
    utill::{ConnectionType, DEFAULT_TX_FEE_RATE},
};
use test_framework::*;

#[test]
fn test_create_funding_txn_with_varied_distributions() {
    let utxo_sets: Vec<Vec<u64>> = vec![vec![
        //  1_500_000, 2_000_000, 3_000_000, 2_500_000, 4_000_000, 500_000, 6_000_000,
        70_000, 800_000, 900_000, 100_000, 1_000_000,
    ]];

    let targets = vec![
        // 50_000_000, 100_000_000,         // Very large amounts (50 BTC, 100 BTC)
        10_000, 100_000, 1_000_000, // Gradual scaling targets
        7_777_777, 8_888_888, // Odd, lopsided amounts
        123_456, 654_321, // Non-round numbers
        // 25_000_000, 75_000_000,          // Large, uneven splits
        500, 1_500, 2_500, // Small, uneven splits
        9_999_999, 999_999, // Near-round numbers
    ];
    //let coinswap_amount = Amount::from_sat(1_000_000); // Example: 0.01 BTC
    let fee_rate = Amount::from_sat(DEFAULT_TX_FEE_RATE as u64);

    let (test_framework, mut takers, _, _, _) = TestFramework::init(
        vec![],
        vec![TakerBehavior::Normal],
        ConnectionType::CLEARNET,
    );

    let bitcoind = &test_framework.bitcoind;

    let taker = &mut takers[0];

    // Fund the taker with this UTXO set
    for individual_utxo in utxo_sets.iter().flatten() {
        let _ = fund_and_verify_taker(taker, bitcoind, 1, Amount::from_sat(*individual_utxo));
    }

    // Generate 5 random addresses from the taker's wallet
    let mut destinations: Vec<Address> = Vec::with_capacity(5);
    for _ in 0..5 {
        let addr = taker.get_wallet_mut().get_next_external_address().unwrap();
        destinations.push(addr);
    }

    for target in targets {
        let target = Amount::from_sat(target);

        println!("\nCoinswap_amount={}", Amount::to_sat(target));

        // Call create_funding_txes with the generated addresses
        match taker.get_wallet_mut().create_funding_txes_regular_swaps(
            false,
            target,
            destinations.clone(),
            fee_rate,
        ) {
            Ok(result) => {
                for tx in result.funding_txes.iter() {
                    let input_amount = tx
                        .input
                        .iter()
                        .map(|txin| {
                            taker
                                .get_wallet()
                                .list_all_utxo_spend_info()
                                .unwrap()
                                .iter()
                                .find(|(utxo, _)| {
                                    txin.previous_output.txid == utxo.txid
                                        && txin.previous_output.vout == utxo.vout
                                })
                                .map(|(u, _)| u.amount)
                                .expect("should find utxo")
                        })
                        .collect::<Vec<_>>();

                    println!("inputs : {input_amount:?}");

                    let out_values = tx.output.iter().map(|o| o.value).collect::<Vec<_>>();

                    println!("outputs: {out_values:?}");
                }
            }
            Err(e) => {
                println!("Failed to create funding transactions: {e:?}");
            }
        }
    }
    test_framework.stop();
}
