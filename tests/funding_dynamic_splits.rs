#![cfg(feature = "integration-test")]
mod test_framework;
use std::vec;

use bitcoin::{Address, Amount};
use coinswap::{
    taker::{Taker, TakerBehavior},
    utill::{ConnectionType, MIN_FEE_RATE},
};
use test_framework::*;

const UTXO_SETS: &[&[u64]] = &[
    // CASE A : Threshold -> 2 Targets, 2 Changes
    &[
        107_831, 91_379, 712_971, 432_441, 301_909, 38_012, 298_092, 9_091,
    ],
    // CASE B : Threshold -> 2 Targets, 2 Changes
    &[109_831, 3_919],
    // CASE C : Threshold -> 2 Targets, 2 Changes
    &[1_946_436],
    // CASE C.1 : Deterministic -> 2 Targets, 1 Change
    &[1_000_000, 1_992_436],
    // ...Previously problematic Edge cases
    &[70_000, 800_000, 900_000, 100_000],
    &[46_824, 53_245, 65_658, 35_892],
];

const TARGETS: &[u64] = &[
    640_082, // Granular testing target
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

#[test]
fn test_regular_swaps() {
    let fee_rate = Amount::from_sat(MIN_FEE_RATE as u64);

    let (test_framework, mut takers, _, _, _) = TestFramework::init(
        vec![],
        vec![TakerBehavior::Normal],
        ConnectionType::CLEARNET,
    );

    let bitcoind = &test_framework.bitcoind;

    let taker = &mut takers[0];

    // Fund the taker with the first UTXO set for granular testing
    for individual_utxo in UTXO_SETS[0].iter() {
        let taker_address = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &taker_address, Amount::from_sat(*individual_utxo));
        generate_blocks(bitcoind, 1);
    }

    // Generate 5 random addresses from the taker's wallet
    let mut destinations: Vec<Address> = Vec::with_capacity(5);
    for _ in 0..5 {
        let addr = taker.get_wallet_mut().get_next_external_address().unwrap();
        destinations.push(addr);
    }

    test_assert_outputs_inputs(taker, destinations.clone(), fee_rate, TARGETS[0]);

    // Fund the taker with the remaining UTXO sets for the rest of the cases
    for individual_utxo in UTXO_SETS[1..].iter().flat_map(|x| x.iter()) {
        let taker_address = taker.get_wallet_mut().get_next_external_address().unwrap();
        send_to_address(bitcoind, &taker_address, Amount::from_sat(*individual_utxo));
        generate_blocks(bitcoind, 1);
    }

    test_create_funding_txn_with_varied_distributions(
        taker,
        destinations,
        fee_rate,
        TARGETS.to_vec(),
    );

    test_framework.stop();
}

fn test_create_funding_txn_with_varied_distributions(
    taker: &mut Taker,
    destinations: Vec<Address>,
    fee_rate: Amount,
    targets: Vec<u64>,
) {
    println!("\n ----- test_create_funding_txn_with_varied_distributions ----- ");

    taker.get_wallet_mut().sync_no_fail();

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
                    let selected_inputs = tx
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
                    assert!(selected_inputs.iter().all(|&x| selected_inputs
                        .iter()
                        .filter(|&&y| y == x)
                        .count()
                        == 1),);

                    println!("Inputs : {selected_inputs:?}");
                    println!("Sum of Inputs: {:?}", {
                        selected_inputs.iter().map(|a| a.to_sat()).sum::<u64>()
                    });

                    let outputs = tx.output.iter().map(|o| o.value).collect::<Vec<_>>();

                    println!("Outputs: {outputs:?}");
                }
            }
            Err(e) => {
                println!("Failed to create funding transactions: {e:?}");
            }
        }
    }
}

fn test_assert_outputs_inputs(
    taker: &mut Taker,
    destinations: Vec<Address>,
    fee_rate: Amount,
    target: u64,
) {
    println!("\n ----- test_assert_outputs_inputs ----- \n");

    taker.get_wallet_mut().sync_no_fail();

    match taker.get_wallet().get_balances() {
        Ok(balance) => {
            println!(
                "Total Wallet Spendable Balance : {}",
                balance.spendable.to_sat()
            );
            let expected_total = UTXO_SETS[0].iter().sum::<u64>();
            assert_eq!(
                balance.spendable.to_sat(),
                expected_total,
                "Total wallet balance does not match expected total from UTXO set."
            );
        }
        Err(e) => println!("Failed to get wallet balance: {e:?}"),
    }
    println!("Expected Fee Rate : {MIN_FEE_RATE}");
    println!("Target = {target}");

    match taker.get_wallet_mut().create_funding_txes_regular_swaps(
        false,
        Amount::from_sat(target),
        destinations.clone(),
        fee_rate,
    ) {
        Ok(result) => {
            for tx in result.funding_txes.iter() {
                let selected_inputs = tx
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
                assert!(selected_inputs.iter().all(|&x| selected_inputs
                    .iter()
                    .filter(|&&y| y == x)
                    .count()
                    == 1),);

                let outputs = tx.output.iter().map(|o| o.value).collect::<Vec<_>>();

                // Assertions for Input
                println!("Inputs : {selected_inputs:?}");
                assert_eq!(
                    selected_inputs.len(),
                    5,
                    "Expected 5 input, got {}",
                    selected_inputs.len()
                );
                assert!(
                    selected_inputs.contains(&Amount::from_sat(38012)),
                    "Didn't find expected input amount 38012 SATS in inputs"
                );
                assert!(
                    selected_inputs.contains(&Amount::from_sat(298092)),
                    "Didn't find expected input amount 298092 SATS in inputs"
                );
                assert!(
                    selected_inputs.contains(&Amount::from_sat(301909)),
                    "Didn't find expected input amount 301909 SATS in inputs"
                );
                assert!(
                    selected_inputs.contains(&Amount::from_sat(9091)),
                    "Didn't find expected input amount 9091 SATS in inputs"
                );
                assert!(
                    selected_inputs.contains(&Amount::from_sat(712971)),
                    "Didn't find expected input amount 712971 SATS in inputs"
                );

                // Assertions for Outputs
                println!("Outputs: {outputs:?}");
                assert_eq!(
                    outputs.len(),
                    4,
                    "Expected 4 outputs, got {}",
                    outputs.len()
                );

                // Here, we expect the Change Chunks and Target Chunks to be similar to each other in the range Input / outputs.len()
                let input_sum = selected_inputs.iter().map(|a| a.to_sat()).sum::<u64>();
                let lower_bound = ((input_sum / 4) as f64 * 0.825) as u64;
                let upper_bound = ((input_sum / 4) as f64 * 1.175) as u64;
                assert!(
                    outputs
                        .iter()
                        .all(|x| (lower_bound..=upper_bound).contains(&x.to_sat())),
                    "All output chunks should be in range of Input/Outputs.len() ± 15 + 2.5%"
                );

                println!("Sum of all Inputs : {:?}", {
                    selected_inputs.iter().map(|a| a.to_sat()).sum::<u64>()
                });
                println!("Sum of all Outputs : {:?}", {
                    outputs.iter().map(|o| o.to_sat()).sum::<u64>()
                });

                // Assert that any two of the 4 elements of outputs will equate to the target
                let mut found_pair = false;

                for (i, &value1) in outputs.iter().enumerate() {
                    for &value2 in outputs.iter().skip(i + 1) {
                        if value1.to_sat() + value2.to_sat() == target {
                            found_pair = true;
                            println!(
                                "Found Target Chunks : {} + {} = {}",
                                value1.to_sat(),
                                value2.to_sat(),
                                target
                            );
                            break;
                        }
                    }
                    if found_pair {
                        break;
                    }
                }

                assert!(
                    found_pair,
                    "No pair of outputs equates to the target value."
                );

                // Asserting Fee is more than 2% range.
                let actual_fee = selected_inputs.iter().map(|a| a.to_sat()).sum::<u64>()
                    - outputs.iter().map(|o| o.to_sat()).sum::<u64>();
                let tx_size = tx.weight().to_vbytes_ceil();
                let actual_feerate = actual_fee as f64 / tx_size as f64;
                println!("Actual fee rate: {actual_feerate}");
                assert!(
                    actual_feerate >= MIN_FEE_RATE * 0.98,
                    "Fee rate ({}) is less than 98% of MIN_FEE_RATE ({})",
                    actual_feerate,
                    MIN_FEE_RATE
                );
            }
        }
        Err(e) => {
            println!("Failed to create funding transactions: {e:?}");
        }
    }
}
