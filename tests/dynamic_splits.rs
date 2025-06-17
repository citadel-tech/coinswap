#![cfg(feature = "integration-test")]
mod test_framework;
use std::vec;

// use super::*;
use bitcoin::Amount;
// use std::sync::Arc;
use coinswap::{
    // maker::{start_maker_server, MakerBehavior},
    taker::TakerBehavior,
    utill::{ConnectionType, DEFAULT_TX_FEE_RATE},
};
use test_framework::*;

#[test]
fn test_create_dynamic_splits() {
    // Summation :- 5010000, 31100, 2500750, 13669000, 3678747, 100000, 1005500, 15080800
    let utxo_sets: Vec<Vec<f64>> = vec![
        // // 1. "Whale" Set - One huge UTXO with dust
        vec![
            0.05, // One dominant UTXO (0.05 BTC)
                 //     0.000015, 0.00001, 0.00002, 0.00003, 0.000025, // Dust UTXOs (0.00001 BTC range)
        ],
        // // 2. "Dust Storm" Set - Many tiny UTXOs only
        // vec![
        //     0.00001, 0.000015, 0.00002, 0.00003, 0.000025, // All < 0.0001 BTC
        //     0.000012, 0.000018, 0.000035, 0.00004, 0.000008, 0.000011, 0.0000095, 0.000032, 0.000028, 0.0000175,
        // ],
        // // 3. "Binary" Set - Only very large and very small UTXOs
        // vec![
        //     0.01, 0.015, // Large UTXOs (0.01-0.015 BTC)
        //     0.0000005, 0.000001, 0.0000015, 0.000002, 0.0000025, // Dust (tiny amounts)
        // ],
        // // 4. "Power Law" Set - Natural but imbalanced distribution
        // vec![
        //     1.0, // 1 whale (10 BTC)
        //     0.1, 0.12, // Few large
        //     0.8,   // Large
        //     0.1, 0.15, // Medium (0.1-0.15 BTC)
        //     0.2, 0.075,  // Medium
        //     0.125, // Medium
        //     0.00001, 0.00002, 0.00003, // Small (0.00001 BTC)
        //     0.00005, 0.00008, // Small
        // ],
        // // 5. "Odd Amounts" Set - Non-round numbers
        // vec![
        //     0.1234567, 0.0987654, // Large odd amounts
        //     0.0345678, 0.0876543, // Large odd amounts
        //     0.012345, 0.006789, // Medium odd amounts
        //     0.0045678, 0.0089012, // Medium odd amounts
        //     0.0001234, 0.0005678, // Small odd amounts
        //     0.0009012, 0.0003456, // Small odd amounts
        // ],
        // // 6. "Just Barely Enough" Set - Minimal viable case
        // vec![
        //     0.00099999, // Just under 0.001 BTC
        //     // 0.00000001,  // Single satoshi
        // ],
        // // 7. "High Precision" Set - Many similar-sized UTXOs
        // vec![
        //     0.001001, 0.001002, // Clustered around 0.001 BTC
        //     0.001003, 0.001004, // with small increments
        //     0.001005, 0.001006, 0.001007, 0.001008, 0.001009, 0.00101,
        // ],
        // // 8. "Gap" Set - Missing medium denominations
        // vec![
        //     0.1, 0.05, // Very large (0.05 - 0.1 BTC)
        //     0.0001, 0.0002, // Small (0.0001-0.0002 BTC)
        //     0.0005, // Small
        //     0.000001, 0.000002, 0.000005, // Dust
        // ],
    ];

    // Corresponding target sets (each smaller than the UTXO set's total)
    let target_sets: Vec<Vec<u64>> = vec![
        // Targets for Whale Set (5,001,0000 total)
        vec![1_000_000, 2_500_000, 4_999_999], // Large targets
        // Targets for Dust Storm (29,000 total)
        vec![5_000, 10_000, 15_000, 20_000], // Small targets
        // Targets for Binary Set (2,500,750 total)
        vec![500_000, 1_250_000, 2_000_000], // Mixed targets
        // Targets for Power Law (14,029,000 total)
        vec![1_000_000, 5_000_000, 10_000_000], // Progressive targets
        // Targets for Odd Amounts (3,707,687 total)
        vec![500_000, 1_234_567, 3_000_000], // Odd-numbered targets
        // Targets for Just Barely Enough (100,000 total)
        vec![50_000, 99_999], // Edge case targets
        // Targets for High Precision (1,005,500 total)
        vec![100_100, 500_500, 1_000_000], // Precision targets
        // Targets for Gap Set (15,001,800 total)
        vec![5_000_000, 10_000_000, 15_000_000], // Gap-testing targets
    ];

    // Initialize the test framework
    let (test_framework, mut takers, _, _, _) = TestFramework::init(
        vec![],                      // No makers needed for this test
        vec![TakerBehavior::Normal], // Default taker behavior
        ConnectionType::CLEARNET,
    );

    // Get the first taker
    let taker = &mut takers[0];
    let bitcoind = &test_framework.bitcoind;

    let mut utxo_set_count = 0;
    for (i, individual_utxo_set) in utxo_sets.iter().enumerate() {
        for individual_utxo in individual_utxo_set {
            fund_and_verify_taker(
                taker,
                bitcoind,
                1,
                Amount::from_btc(*individual_utxo).unwrap(),
            );
        }
        let coins_to_spend = taker.get_wallet_mut().list_all_utxo_spend_info().unwrap();

        for target in target_sets[i].iter() {
            println!("Test Case (utxo set {}, target={}):", i + 1, target);
            println!(
                "Selected Coins: {:?}",
                coins_to_spend
                    .iter()
                    .skip(utxo_set_count)
                    .collect::<Vec<_>>()
            );
            let (selected_inputs, target_splits, change_splits) =
                taker.get_wallet_mut().create_dynamic_splits(
                    coins_to_spend
                        .iter()
                        .skip(utxo_set_count)
                        .cloned()
                        .collect::<Vec<_>>(),
                    *target,
                    DEFAULT_TX_FEE_RATE,
                );

            println!("Selected Inputs: {selected_inputs:?}");
            println!("Target Splits: {target_splits:?}");
            println!("Change Splits: {change_splits:?}");
        }

        utxo_set_count += individual_utxo_set.len();
    }
    // Stop the test framework
    test_framework.stop();
}
