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
    let utxo_sets: Vec<Vec<u64>> = vec![
        // CASE A : Threshold -> 2 Targets, 2 Changes
        vec![109_831, 3_919],
        // CASE B : Threshold -> 2 Targets, 2 Changes
        vec![107_831, 91_379, 712_971, 432_441],
        // CASE C : Threshold -> 2 Targets, 2 Changes
        vec![1_946_436],
        // CASE C.1 : Deterministic -> 2 Targets, 1 Change
        vec![1_000_000, 1_992_436],
        // ...Previously problematic Edge cases
        vec![70_000, 800_000, 900_000, 100_000],
        vec![46_824, 53_245, 65_658, 35_892],
    ];

    let targets = vec![
        54_082,  // CASE A : Threshold -> 2 Targets, 2 Changes
        102_980, // CASE B : Threshold -> 2 Targets, 2 Changes
        708_742, // CASE C : Threshold -> 2 Targets, 2 Changes
        500_000, // CASE C.1 : Deterministic -> 2 Targets, 1 Change
        10_000, 100_000, 1_000_000, // Gradual scaling targets
        7_777_777, 888_888, // Odd, lopsided amounts
        123_456, // Non-round numbers
        250_000, 7_500_000, // Large, uneven splits
        500, 1_500, 2_500,   // Small, uneven splits
        999_999, // Near-round numbers
        654_321, // Previous edge case for second last wallet
        90_000,  // Edge case for last wallet
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
        let taker_address = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &taker_address, Amount::from_sat(*individual_utxo));
        generate_blocks(bitcoind, 1);
    }

    taker.get_wallet_mut().sync_no_fail();

    // Generate 5 random addresses from the taker's wallet
    let mut destinations: Vec<Address> = Vec::with_capacity(5);
    for _ in 0..5 {
        let addr = taker.get_wallet_mut().get_next_external_address().unwrap();
        destinations.push(addr);
    }

    for target in targets {
        let target = Amount::from_sat(target);

        println!("\nTarget = {}", Amount::to_sat(target));

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

                    // Assert no duplicate inputs
                    assert!(input_amount.iter().all(|&x| input_amount
                        .iter()
                        .filter(|&&y| y == x)
                        .count()
                        == 1),);

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
