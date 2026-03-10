//! Tests that the funding transaction creation works correctly with varied UTXO distributions.
//! This test creates a taker wallet with specific UTXO sets and verifies that coin selection
//! and funding transaction creation produce the expected inputs and outputs.

use bitcoin::{Address, Amount};
use coinswap::{taker::UnifiedTakerBehavior, utill::MIN_FEE_RATE, wallet::AddressType};

use super::test_framework::*;

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
    (54_082, &[53245, 3919, 46824, 9091], 4),
    (102_980, &[70000, 35892, 100000, 3919], 4),
    (708_742, &[107831, 301909, 38012, 35892, 9091, 65658, 53245, 100000, 712971], 4),
    (500_000, &[91379, 3919, 107831, 35892, 46824, 9091, 38012, 70000, 100000, 65658, 298092, 53245, 109831], 4),
    (654_321, &[301909, 46824, 70000, 38012, 9091, 100000, 91379, 53245, 65658, 432441, 107831], 4),
    (90_000, &[53245, 35892, 3919, 91379], 4),
    (10_000, &[9091, 3919], 4),
    (1_000_000, &[91379, 100000, 9091, 65658, 109831, 46824, 107831, 35892, 3919, 432441, 38012, 70000, 900000], 4),
    (7_777_777, &[1992436, 1946436, 900000, 800000, 712971, 432441, 301909, 298092, 107831, 91379, 70000, 53245, 38012, 35892], 3),
    (888_888, &[3919, 9091, 35892, 46824, 53245, 65658, 70000, 91379, 107831, 109831, 298092, 100000, 800000], 4),
    (999_999, &[432441, 109831, 107831, 100000, 91379, 65658, 46824, 35892, 9091, 3919, 900000, 38012, 70000], 4),
    (123_456, &[38012, 53245, 35892, 46824, 9091, 3919, 65658], 4),
    (250_000, &[9091, 53245, 107831, 46824, 35892, 109831, 100000, 38012, 3919], 4),
    (7_500_000, &[1946436, 1000000, 900000, 3919, 9091, 38012, 100000, 712971, 800000, 1992436], 3),
    (500, &[3919], 3),
    (1_500, &[9091], 6),
    (2_500, &[9091], 7),
];

#[test]
fn test_create_funding_txn_with_varied_distributions() {
    println!(
        "Sum of the Entire UTXO set: {}\n",
        UTXO_SETS.iter().flat_map(|x| x.iter()).sum::<u64>()
    );

    // Initialize the test framework with a single taker with Normal behavior, no makers
    let (test_framework, mut unified_takers, _makers, _block_generation_handle) =
        TestFramework::init_unified(vec![], vec![UnifiedTakerBehavior::Normal], vec![]);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut unified_takers[0];

    // Fund the taker with the UTXO sets
    for individual_utxo in UTXO_SETS.iter().flat_map(|x| x.iter()) {
        let addr = taker
            .get_wallet()
            .write()
            .unwrap()
            .get_next_external_address(AddressType::P2WPKH)
            .unwrap();
        send_to_address(bitcoind, &addr, Amount::from_sat(*individual_utxo));
        generate_blocks(bitcoind, 1);
    }

    // Sync taker wallet
    taker.get_wallet().write().unwrap().sync_and_save().unwrap();

    // Generate 5 random addresses from the taker's wallet
    let mut destinations: Vec<Address> = Vec::with_capacity(5);
    for _ in 0..5 {
        let addr = taker
            .get_wallet()
            .write()
            .unwrap()
            .get_next_external_address(AddressType::P2WPKH)
            .unwrap();
        destinations.push(addr);
    }

    for (i, (target_amount, expected_inputs, expected_outputs)) in TEST_CASES.iter().enumerate() {
        let target = Amount::from_sat(*target_amount);

        // Call `create_funding_txes_regular_swaps` with Normie Flag turned off
        let result = taker
            .get_wallet()
            .write()
            .unwrap()
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
                    .read()
                    .unwrap()
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
