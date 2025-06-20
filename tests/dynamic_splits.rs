#![cfg(feature = "integration-test")]
mod test_framework;
use std::{env, vec};

// use super::*;
use bitcoin::{Amount, OutPoint};
// use std::sync::Arc;
use coinswap::{
    // maker::{start_maker_server, MakerBehavior},
    taker::{Taker, TakerBehavior},
    utill::{ConnectionType, DEFAULT_TX_FEE_RATE},
    wallet::RPCConfig,
};
use test_framework::*;
#[test]
fn test_create_dynamic_splits() {
    // Summation :- 5010000, 31100, 2500750, 13669000, 3678747, 100000, 1005500, 15080800
    let utxo_sets: Vec<Vec<u64>> = vec![
        // 1. "Whale" Set - One huge UTXO with dust
        vec![5000000], // One dominant UTXO (0.05 BTC)
        // 2. "Dust Storm" Set - Many tiny UTXOs only
        vec![1_000, 1_500, 2_000, 3_000, 2_500],
        // 3. "Binary" Set - Only very large UTXOs
        vec![
            1_000_000, 1_500_000, 2_000_000, 3_000_000, 2_500_000, 4_000_000, 500_000, 6_000_000,
            70_000, 800_000, 900_000, 100_000,
        ],
        // 4. "Power Law" Set - Natural but imbalanced distribution
        vec![
            100_000_000,
            10_000_000,
            12_000_000,
            80_000_000,
            10_000_000,
            10_000,
            20_000,
            30_000,
            50_000,
        ],
        // 5. "Odd Amounts" Set - Non-round numbers
        vec![12_345_673, 9_876_542, 3_456_787, 8_765_432, 1_230_851],
        // 6. "Just Barely Enough" Set - Minimal viable case
        vec![99_999],
        // 7. "High Precision" Set - Many similar-sized UTXOs
        vec![
            101_000, 102_000, 103_000, 104_000, 105_000, 106_000, 107_000, 108_000, 109_000,
        ],
    ];

    // Flatten all target sets into one big vector
    let target_sets: Vec<u64> = vec![
        // 50_000_000, 100_000_000,         // Very large amounts (50 BTC, 100 BTC)
        10_000, 100_000, 1_000_000, // Gradual scaling targets
        7_777_777, 8_888_888, // Odd, lopsided amounts
        123_456, 654_321, // Non-round numbers
        // 25_000_000, 75_000_000,          // Large, uneven splits
        500, 1_500, 2_500, // Small, uneven splits
        9_999_999, 999_999, // Near-round numbers
    ];

    // Initialize the test framework
    let (test_framework, mut takers, _, _, _) = TestFramework::init(
        vec![],                      // No makers needed for this test
        vec![TakerBehavior::Normal], // Default taker behavior
        ConnectionType::CLEARNET,
    );

    // Get the first taker
    let _taker = &mut takers[0];
    let bitcoind = &test_framework.bitcoind;

    for (i, individual_utxo_set) in utxo_sets.iter().enumerate() {
        if i > 0 {
            takers[0] = Taker::init(
                Some(
                    env::temp_dir()
                        .join("coinswap")
                        .join(format!("taker{}", i + 1)),
                ),
                None,
                Some(RPCConfig::from(&*test_framework)),
                TakerBehavior::Normal,
                None,
                None,
                Some(ConnectionType::CLEARNET),
            )
            .unwrap();
        }

        let taker = &mut takers[0];

        // Fund the taker with this UTXO set
        for individual_utxo in individual_utxo_set {
            let _ = fund_and_verify_taker(
                taker,
                bitcoind,
                1,
                // Amount::from_btc(*individual_utxo).unwrap(),
                Amount::from_sat(*individual_utxo),
            );
        }

        // Get all UTXOs from the wallet
        let _coins_to_spend = match taker.get_wallet_mut().list_all_utxo_spend_info() {
            Ok(coins) => coins,
            Err(e) => {
                println!("Failed to list UTXOs for set {}: {:?}", i + 1, e);
                continue;
            }
        };

        for target in target_sets.iter() {
            println!(
                "\nTest Case (utxo set {} {:?}, target={}):",
                i + 1,
                individual_utxo_set,
                target
            );

            let selected_utxos = match taker
                .get_wallet_mut()
                .coin_select(Amount::from_sat(*target), DEFAULT_TX_FEE_RATE)
            {
                Ok(utxos) => utxos,
                Err(e) => {
                    println!("Failed to select coins: {e:?}");
                    continue;
                }
            };

            // Try to create dynamic splits
            let (selected_inputs, target_splits, change_splits) = taker
                .get_wallet_mut()
                .create_dynamic_splits(selected_utxos.clone(), *target, DEFAULT_TX_FEE_RATE);

            // Prepare outpoints for locking
            let outpoints: Vec<_> = selected_inputs
                .iter()
                .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                .collect();

            // Handle the result (both success and error cases)
            println!("Successfully created splits:");
            println!(
                "Selected Inputs: {:?}",
                selected_inputs
                    .iter()
                    .map(|(utxo, _)| utxo.amount)
                    .collect::<Vec<_>>()
            );
            println!("Target Splits: {target_splits:?}");
            println!("Change Splits: {change_splits:?}");

            // Unlock UTXOs after use

            // THese doesn't actually unlock UTXOs in the test framework.
            // bitcoind.client.unlock_unspent_all();
            // bitcoind.client.unlock_unspent(&outpoints);

            // This does??? Even though it uses the same method as above
            taker.get_wallet_mut().lock_unspendable_utxos();
        }
    }

    // Stop the test framework
    test_framework.stop();
}
