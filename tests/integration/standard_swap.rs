//! Standard coinswap test: normal swap between a Taker and 2 Makers.
//! Nothing goes wrong and the coinswap completes successfully.

use bitcoin::Amount;
use coinswap::{
    maker::{start_server, MakerBehavior},
    protocol::common_messages::ProtocolVersion,
    taker::{SwapParams, TakerBehavior},
    wallet::AddressType,
};

use super::test_framework::*;

use log::{info, warn};
use std::{fs, sync::atomic::Ordering::Relaxed, thread};

/// This test demonstrates a standard coinswap round between a Taker and 2 Makers. Nothing goes wrong
/// and the coinswap completes successfully.
#[test]
fn test_standard_coinswap() {
    // ---- Setup ----
    warn!("Running Test: Standard Coinswap Procedure");

    let makers_config_map = vec![(6102, Some(19051)), (16102, Some(19052))];
    let taker_behavior = vec![TakerBehavior::Normal];
    let maker_behaviors = vec![MakerBehavior::Normal, MakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map, taker_behavior, maker_behaviors);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).unwrap();

    // Fund the taker with 3 UTXOs of 0.05 BTC each
    let taker_original_balance = fund_taker(
        taker,
        bitcoind,
        3,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Fund the makers with 4 UTXOs of 0.05 BTC each
    fund_makers(
        &makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
        AddressType::P2TR,
    );

    // Start the maker server threads
    log::info!("Initiating Maker servers");

    let maker_threads = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_server(maker_clone).unwrap();
            })
        })
        .collect::<Vec<_>>();

    // Wait for makers to complete setup
    wait_for_makers_setup(&makers, 120);

    // Sync wallets after setup
    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    let maker_spendable_balance = verify_maker_pre_swap_balances(&makers);

    // Initiate Coinswap
    info!("Initiating coinswap protocol");

    let swap_params = SwapParams::new(ProtocolVersion::Legacy, Amount::from_sat(500000), 2)
        .with_tx_count(3)
        .with_required_confirms(1);

    generate_blocks(bitcoind, 1);

    let summary = taker
        .prepare_coinswap(swap_params)
        .expect("Failed to prepare coinswap");
    taker
        .start_coinswap(&summary.swap_id)
        .expect("Coinswap should complete successfully");

    info!("All coinswaps processed successfully. Transaction complete.");

    // After swap, shutdown maker threads
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));

    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    // Sync wallets
    taker.get_wallet().write().unwrap().sync_and_save().unwrap();

    generate_blocks(bitcoind, 1);

    for maker in &makers {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
    }

    // Verify taker balances
    info!("Verifying swap results");
    let taker_balances = taker.get_wallet().read().unwrap().get_balances().unwrap();

    info!(
        "Taker balances: Regular: {}, Swap: {}, Contract: {}, Spendable: {}",
        taker_balances.regular,
        taker_balances.swap,
        taker_balances.contract,
        taker_balances.spendable,
    );

    assert_eq!(
        taker_balances.regular.to_sat(),
        14499692,
        "Taker regular balance mismatch"
    );
    assert_eq!(
        taker_balances.swap.to_sat(),
        498487,
        "Taker swap balance mismatch"
    );
    assert_eq!(
        taker_balances.contract.to_sat(),
        0,
        "Taker contract balance mismatch"
    );
    assert_eq!(taker_balances.fidelity, Amount::ZERO);

    let balance_diff = taker_original_balance
        .checked_sub(taker_balances.spendable)
        .unwrap();

    info!("Taker fees paid: {} sats", balance_diff.to_sat());

    assert_eq!(
        balance_diff.to_sat(),
        1821,
        "Taker spendable balance change mismatch"
    );

    // Verify maker balances
    for (i, (maker, original)) in makers.iter().zip(maker_spendable_balance).enumerate() {
        maker.wallet.write().unwrap().sync_and_save().unwrap();
        let balances = maker.wallet.read().unwrap().get_balances().unwrap();

        info!(
            "Maker {} balances: Regular: {}, Swap: {}, Contract: {}, Fidelity: {}, Spendable: {}",
            i,
            balances.regular,
            balances.swap,
            balances.contract,
            balances.fidelity,
            balances.spendable,
        );

        let expected_regular = [14499833u64, 14500421][i];
        let expected_swap = [499700u64, 499075][i];
        assert_eq!(
            balances.regular.to_sat(),
            expected_regular,
            "Maker {} regular balance mismatch",
            i
        );
        assert_eq!(
            balances.swap.to_sat(),
            expected_swap,
            "Maker {} swap balance mismatch",
            i
        );
        assert_eq!(
            balances.contract.to_sat(),
            0,
            "Maker {} contract balance mismatch",
            i
        );
        assert_eq!(balances.fidelity, Amount::from_btc(0.05).unwrap());

        let maker_fee = balances
            .spendable
            .checked_sub(original)
            .unwrap_or(Amount::ZERO);

        info!("Maker {} fee earned: {} sats", i, maker_fee.to_sat());

        let expected_fee = [17u64, 0][i];
        assert_eq!(
            maker_fee.to_sat(),
            expected_fee,
            "Maker {} fee earned mismatch",
            i
        );
    }

    info!("Standard coinswap test completed successfully!");

    let taker_proofs = taker
        .get_wallet()
        .read()
        .unwrap()
        .list_deniability_proofs(Some(&summary.swap_id));
    assert!(
        !taker_proofs.is_empty(),
        "Taker should have generated a deniability proof for swap {}",
        summary.swap_id
    );

    let temp_dir = makers[0]
        .data_dir
        .parent()
        .expect("maker data dir should live under test temp dir");
    let taker_report_path = temp_dir
        .join("taker1")
        .join("wallets")
        .join("taker1_swap_report.json");
    assert_report_has_deniability_proofs(&taker_report_path, "taker");

    for (i, maker) in makers.iter().enumerate() {
        let maker_proofs = maker
            .wallet
            .read()
            .unwrap()
            .list_deniability_proofs(Some(&summary.swap_id));
        assert!(
            !maker_proofs.is_empty(),
            "Maker {} should have generated a deniability proof for swap {}",
            i,
            summary.swap_id
        );

        let maker_report_path = maker
            .data_dir
            .join("wallets")
            .join(format!("{}_swap_report.json", maker.config.wallet_name));
        assert_report_has_deniability_proofs(&maker_report_path, &format!("maker {i}"));
    }

    test_framework.stop();
    block_generation_handle.join().unwrap();
}

fn assert_report_has_deniability_proofs(report_path: &std::path::Path, label: &str) {
    let report = fs::read_to_string(report_path).unwrap_or_else(|e| {
        panic!(
            "Failed to read {label} report {}: {e}",
            report_path.display()
        )
    });
    let json: serde_json::Value = serde_json::from_str(&report).unwrap_or_else(|e| {
        panic!(
            "Failed to parse {label} report {}: {e}",
            report_path.display()
        )
    });
    let proofs = json
        .get("deniability_proofs")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!(
                "{label} report is missing deniability_proofs at {}",
                report_path.display()
            )
        });
    let proof_count = proofs.len();
    assert!(
        proof_count > 0,
        "{label} report should contain deniability proofs at {}",
        report_path.display()
    );
    assert!(
        proofs
            .iter()
            .all(|proof| proof.get("outgoing_swapcoin").is_some_and(|v| !v.is_null())),
        "{label} report proofs should link to outgoing swapcoins at {}",
        report_path.display()
    );
    info!(
        "{} report contains {} deniability proof(s): {}",
        label,
        proof_count,
        report_path.display()
    );
    println!(
        "\n{label} deniability proofs:\n{}\n",
        serde_json::to_string_pretty(proofs).expect("proofs should serialize")
    );
}
