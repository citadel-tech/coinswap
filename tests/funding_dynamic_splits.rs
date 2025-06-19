#![cfg(feature = "integration-test")]
mod test_framework;
use std::{env, vec};

use bitcoin::{Address, Amount};
use coinswap::{
    taker::{Taker, TakerBehavior},
    utill::{ConnectionType, DEFAULT_TX_FEE_RATE},
    wallet::RPCConfig,
};
use test_framework::*;

#[test]
fn test_create_funding_txn_with_varied_distributions() {
    let utxo_sets: Vec<Vec<u64>> = vec![
        vec![5_000_000],
        vec![
            1_000_000, 1_500_000, 2_000_000, 3_000_000, 2_500_000, 4_000_000, 500_000, 6_000_000,
            70_000, 800_000, 900_000, 100_000,
        ],
        vec![12_345_673, 9_876_542, 3_456_787, 8_765_432, 1_230_851],
    ];

    let coinswap_amount = Amount::from_sat(1_000_000); // Example: 0.01 BTC
    let fee_rate = Amount::from_sat(DEFAULT_TX_FEE_RATE as u64);

    let (test_framework, mut takers, _, _, _) = TestFramework::init(
        vec![],
        vec![TakerBehavior::Normal],
        ConnectionType::CLEARNET,
    );

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
            let _ = fund_and_verify_taker(taker, bitcoind, 1, Amount::from_sat(*individual_utxo));
        }

        // Generate 5 random addresses from the taker's wallet
        let mut destinations: Vec<Address> = Vec::with_capacity(5);
        for _ in 0..5 {
            let addr = taker.get_wallet_mut().get_next_external_address().unwrap();
            destinations.push(addr);
        }

        println!(
            "\nTest Case (utxo set {} {:?}, coinswap_amount={}, fee_rate={}):",
            i + 1,
            individual_utxo_set,
            Amount::to_sat(coinswap_amount),
            Amount::to_sat(fee_rate),
        );

        // Call create_funding_txes with the generated addresses
        match taker.get_wallet_mut().create_funding_txes_regular_swaps(
            false,
            coinswap_amount,
            destinations,
            fee_rate,
        ) {
            Ok(result) => {
                println!("Funding transactions created successfully!");
                for (idx, tx) in result.funding_txes.iter().enumerate() {
                    println!("Tx {}: txid={}", idx, tx.compute_txid());
                }
            }
            Err(e) => {
                println!("Failed to create funding transactions: {e:?}");
            }
        }
    }

    test_framework.stop();
}
