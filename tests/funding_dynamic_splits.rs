#![cfg(feature = "integration-test")]
mod test_framework;
use bitcoin::{Address, Amount};
use coinswap::{
    taker::TakerBehavior,
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

// This is the expected input set for each of the Target test case.
const SELECTED_INPUT: &[&[u64]] = &[
    &[
        298092, 100000, 70000, 53245, 46824, 38012, 35892, 432441, 9091, 107831, 91379,
    ],
    &[53245, 3919, 9091, 46824],
    &[65658, 35892, 3919, 107831],
    &[
        3919, 46824, 65658, 70000, 91379, 432441, 301909, 298092, 109831,
    ],
    &[46824, 100000, 53245, 301909, 298092, 91379, 3919, 107831],
    &[9091, 3919],
    &[35892, 53245, 3919, 9091, 100000],
    &[100000, 301909, 46824, 432441, 107831, 9091, 3919, 1000000],
    &[
        35892, 900000, 712971, 1992436, 800000, 1000000, 9091, 298092, 1946436, 38012, 46824,
    ],
    &[
        3919, 9091, 46824, 100000, 298092, 432441, 70000, 107831, 712971,
    ],
    &[3919, 46824, 9091, 65658, 53245, 38012, 35892],
    &[
        65658, 53245, 38012, 3919, 91379, 100000, 35892, 70000, 46824,
    ],
    &[
        1992436, 1946436, 1000000, 900000, 712971, 301909, 298092, 100000, 91379, 70000, 46824,
        38012, 3919,
    ],
    &[3919],
    &[3919],
    &[9091],
    &[432441, 301909, 107831, 100000, 46824, 9091, 3919, 1000000],
    &[
        298092, 109831, 100000, 65658, 46824, 35892, 91379, 9091, 53245, 432441, 70000,
    ],
    &[35892, 3919, 53245, 91379],
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
    654_321, // Edge Case A for the UTXO set
    90_000,  // Edge Case B
];

// This is the expected number of outputs for each of the Above Target test case.
const NUM_OUTPUT_CHUNKS: &[u64] = &[
    4, // Granular testing target
    4, // CASE A : Threshold -> 2 Targets, 2 Changes
    4, // CASE B : Threshold -> 2 Targets, 2 Changes
    4, // CASE C.1 : Threshold -> 2 Targets, 2 Changes
    4, // CASE C.2 : Deterministic -> 2 Targets, 2 Change
    4, 4, 4, // Gradual scaling targets
    2, 4, // Odd, lopsided amounts
    4, // Non-round numbers
    4, 2, // Large, uneven splits
    3, 8, 7, // Small, uneven splits
    4, // Near-round numbers
    4, // Edge Case A for the UTXO set
    4, // Edge Case B
];

#[test]
fn test_create_funding_txn_with_varied_distributions() {
    // Initialize the test framework with a single taker with Normal behavior
    let (test_framework, mut takers, _, _, _) = TestFramework::init(
        vec![],
        vec![TakerBehavior::Normal],
        ConnectionType::CLEARNET,
    );

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    // Fund the taker with the UTXO sets
    for individual_utxo in UTXO_SETS.iter().flat_map(|x| x.iter()) {
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

    taker.get_wallet_mut().sync_no_fail();

    for (i, target) in TARGETS.iter().enumerate() {
        let target = Amount::from_sat(*target);

        // Call `create_funding_txes` with Normie Flag turned off, i.e Destination::MultiDynamic
        let result = taker
            .get_wallet_mut()
            .create_funding_txes_regular_swaps(
                false,
                target,
                destinations.clone(),
                Amount::from_sat(MIN_FEE_RATE as u64),
            )
            .unwrap();

        let tx = &result.funding_txes[0];
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

        let outputs = tx.output.iter().map(|o| o.value).collect::<Vec<_>>();
        let sum_of_inputs = selected_inputs.iter().map(|a| a.to_sat()).sum::<u64>();
        let sum_of_outputs = tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
        let actual_fee = sum_of_inputs - sum_of_outputs;
        let tx_size = tx.weight().to_vbytes_ceil();
        let actual_feerate = actual_fee as f64 / tx_size as f64;

        println!("\nTarget = {}", Amount::to_sat(target));
        println!("Sum of Inputs: {sum_of_inputs:?}");
        println!("Inputs : {selected_inputs:?}");
        println!("Outputs: {outputs:?}");
        println!("Actual fee rate: {actual_feerate}");

        // Assert no duplicate inputs.
        assert!(selected_inputs.iter().all(|&x| selected_inputs
            .iter()
            .filter(|&&y| y == x)
            .count()
            == 1),);

        // Assert the Output UTXOs matches the expected outputs.
        for &utxo in SELECTED_INPUT[i] {
            assert!(
                selected_inputs.contains(&Amount::from_sat(utxo)),
                "Missing UTXO input: {} in test case {}",
                utxo,
                i
            );
        }

        // Assert the number of Outputs matches the expected number.
        assert_eq!(
            outputs.len(),
            NUM_OUTPUT_CHUNKS[i] as usize,
            "Expected {} outputs, got {}",
            NUM_OUTPUT_CHUNKS[i],
            outputs.len()
        );

        // Assert Fee is more than 98% of the expected Fee Rate.
        assert!(
            actual_feerate >= MIN_FEE_RATE * 0.98,
            "Fee rate ({}) is less than 98% of MIN_FEE_RATE ({})",
            actual_feerate,
            MIN_FEE_RATE
        );
    }
    test_framework.stop();
    println!("\nTest completed successfully.");
}
