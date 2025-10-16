#![cfg(feature = "integration-test")]
mod test_framework;
use bitcoin::{Address, Amount};
use coinswap::{taker::TakerBehavior, utill::MIN_FEE_RATE};
use test_framework::*;

const UTXO_SETS: &[&[u64]] = &[
    &[
        107_831, 91_379, 712_971, 432_441, 301_909, 38_012, 298_092, 9_091,
    ],
    &[109_831, 3_919],
    &[1_946_436],
    &[1_000_000, 1_992_436],
    &[70_000, 800_000, 900_000, 100_000],
    &[46_824, 53_245, 65_658, 35_892],
];

// Test data structure: (target amount, expected selected inputs, expected number of outputs)
#[rustfmt::skip]
const TEST_CASES: &[(u64, &[u64], u64)] = &[
    (54_082, &[53245, 46824, 9091], 4), // CASE A : Threshold -> 2 Targets, 2 Changes
    (102_980, &[35892, 70000, 65658, 38012], 4), // CASE B : Threshold -> 2 Targets, 2 Changes
    (708_742, &[107831, 301909, 38012, 35892, 9091, 65658, 53245, 100000, 712971], 4), // CASE C.1 : Threshold -> 2 Targets, 2 Changes
    (500_000, &[91379, 3919, 107831, 35892, 46824, 9091, 38012, 70000, 100000, 65658, 298092, 53245, 109831], 4), // CASE C.2 : Deterministic -> 2 Targets, 2 Changes
    (654_321, &[301909, 46824, 70000, 38012, 9091, 100000, 91379, 53245, 65658, 432441, 107831], 4), // Edge Case A for the UTXO set
    (90_000, &[53245, 35892, 3919, 91379], 4), // Edge Case B
    (10_000, &[9091, 3919], 4), // Gradual scaling targets
    // (100_000, &[38012, 65658, 46824, 53245], 4), // OR  &[38012, 65658, 100000] Edge Case C. Will investigate 
    (1_000_000, &[91379, 100000, 9091, 65658, 109831, 46824, 107831, 35892, 3919, 432441, 38012, 70000, 900000], 4),
    (7_777_777, &[46824, 1992436, 1000000, 712971, 9091, 91379, 38012, 900000, 800000, 1946436, 65658, 70000, 107831], 3), // Odd, lopsided amounts
    (888_888, &[3919, 9091, 35892, 46824, 53245, 65658, 70000, 91379, 107831, 109831, 298092, 100000, 800000], 4),
    (999_999, &[432441, 109831, 107831, 100000, 91379, 65658, 46824, 35892, 9091, 3919, 900000, 38012, 70000], 4), // Near-round numbers
    (123_456, &[38012, 53245, 35892, 46824, 9091, 3919, 65658], 4), // Non-round numbers
    (250_000, &[9091, 53245, 107831, 46824, 35892, 109831, 100000, 38012, 3919], 4), // Large, uneven splits
    (7_500_000, &[1946436, 1000000, 900000, 3919, 9091, 38012, 100000, 712971, 800000, 1992436], 3),
    (500, &[3919], 3), // Small, uneven splits
    (1_500, &[9091], 6),
    (2_500, &[9091], 7),
];

#[test]
fn test_create_funding_txn_with_varied_distributions() {
    println!(
        "Sum of the Entire UTXO set: {}\n",
        UTXO_SETS.iter().flat_map(|x| x.iter()).sum::<u64>()
    );

    // Initialize the test framework with a single taker with Normal behavior
    let (test_framework, mut takers, _, _) =
        TestFramework::init(vec![], vec![TakerBehavior::Normal]);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    // Fund the taker with the UTXO sets
    for individual_utxo in UTXO_SETS.iter().flat_map(|x| x.iter()) {
        fund_and_verify_taker(taker, bitcoind, 1, Amount::from_sat(*individual_utxo));
    }

    // Generate 5 random addresses from the taker's wallet
    let mut destinations: Vec<Address> = Vec::with_capacity(5);
    for _ in 0..5 {
        let addr = taker.get_wallet_mut().get_next_external_address().unwrap();
        destinations.push(addr);
    }

    for (i, (target_amount, expected_inputs, expected_outputs)) in TEST_CASES.iter().enumerate() {
        let target = Amount::from_sat(*target_amount);

        // Call `create_funding_txes` with Normie Flag turned off, i.e Destination::MultiDynamic
        let result = taker
            .get_wallet_mut()
            .create_funding_txes_regular_swaps(
                false,
                target,
                destinations.clone(),
                Amount::from_sat(MIN_FEE_RATE as u64),
                None,
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
        for &utxo in *expected_inputs {
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
            *expected_outputs as usize,
            "Expected {} outputs, got {}",
            expected_outputs,
            outputs.len()
        );

        // Assert Fee is less than 98% of the expected Fee Rate or equal to MIN_FEE_RATE.
        assert!(
            actual_feerate > MIN_FEE_RATE * 0.98 || actual_fee == MIN_FEE_RATE as u64,
            "Fee rate ({}) is not less than 98% of MIN_FEE_RATE ({}) or fee is not equal to MIN_FEE_RATE",
            actual_feerate,
            MIN_FEE_RATE
        );
    }
    test_framework.stop();
    println!("\nTest completed successfully.");
}
