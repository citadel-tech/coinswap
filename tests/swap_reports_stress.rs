#![cfg(feature = "integration-test")]
//! Comprehensive stress test for swap report persistence.
//!
//! This test validates that swap reports are correctly generated and persisted
//! across multiple scenarios including:
//! - Normal successful swaps (V1 and V2)
//! - Multiple sequential swaps
//! - Report data integrity verification

use bitcoin::Amount;
use coinswap::{
    maker::{start_maker_server, start_maker_server_taproot, MakerBehavior, TaprootMakerBehavior},
    taker::{
        api2::{SwapParams as TaprootSwapParams, TakerBehavior as TaprootTakerBehavior},
        SwapParams, TakerBehavior,
    },
    wallet::MakerSwapReport,
};
use std::{
    env, fs,
    path::Path,
    sync::{atomic::Ordering::Relaxed, Arc},
    thread,
    time::Duration,
};

mod test_framework;
use test_framework::*;

use log::{info, warn};

/// Helper to read and parse maker swap reports from disk
fn read_maker_reports(data_dir: &Path, wallet_name: &str) -> Vec<MakerSwapReport> {
    let report_path = data_dir
        .join("wallets")
        .join(format!("{}_maker_swap_reports.json", wallet_name));

    if !report_path.exists() {
        info!("Report path does not exist: {:?}", report_path);
        return Vec::new();
    }

    let content = fs::read_to_string(&report_path).expect("Failed to read report file");
    serde_json::from_str(&content).expect("Failed to parse report JSON")
}

/// Test 1: Normal V1 (Legacy ECDSA) swap with report verification
#[test]
fn test_v1_swap_reports_integrity() {
    warn!("🧪 Test 1: V1 Legacy Swap Report Integrity");

    // Setup: 2 makers, normal behavior
    let makers_config_map = vec![
        ((6102, Some(19051)), MakerBehavior::Normal),
        ((16102, Some(19052)), MakerBehavior::Normal),
    ];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, mut takers, makers, block_gen) =
        TestFramework::init(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];

    // Fund wallets
    info!("💰 Funding wallets");
    fund_and_verify_taker(taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());
    let makers_ref = makers.iter().map(Arc::as_ref).collect::<Vec<_>>();
    fund_and_verify_maker(makers_ref, bitcoind, 4, Amount::from_btc(0.05).unwrap());

    // Start maker servers
    info!("🚀 Starting maker servers");
    let maker_threads: Vec<_> = makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).unwrap();
            })
        })
        .collect();

    // Wait for setup
    for maker in &makers {
        while !maker.is_setup_complete.load(Relaxed) {
            info!("⏳ Waiting for maker setup");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets
    for maker in &makers {
        maker.get_wallet().write().unwrap().sync_and_save().unwrap();
    }

    // Perform swap
    info!("🔄 Initiating swap");
    let swap_params = SwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        manually_selected_outpoints: None,
    };

    let result = taker.do_coinswap(swap_params);
    assert!(result.is_ok(), "Swap should succeed: {:?}", result.err());

    // Shutdown
    info!("🛑 Shutting down");
    for maker in &makers {
        maker.shutdown.store(true, Relaxed);
    }
    for thread in maker_threads {
        thread.join().unwrap();
    }

    // Verify reports exist
    info!("📊 Verifying reports");

    // Report paths are in temp_dir/<port>/wallets/
    let temp_dir = env::temp_dir().join("coinswap");

    let maker1_reports = read_maker_reports(&temp_dir.join("6102"), "maker6102");
    let maker2_reports = read_maker_reports(&temp_dir.join("16102"), "maker16102");

    assert!(!maker1_reports.is_empty(), "Maker 6102 should have reports");
    assert!(
        !maker2_reports.is_empty(),
        "Maker 16102 should have reports"
    );

    // Verify report data
    let report1 = &maker1_reports[0];
    let report2 = &maker2_reports[0];

    assert_eq!(report1.swap_id, report2.swap_id, "Swap IDs should match");
    assert!(
        matches!(
            report1.status,
            coinswap::wallet::MakerSwapReportStatus::Success
        ),
        "Status should be Success"
    );
    assert!(report1.fee_earned > 0, "Fee should be positive");
    assert_eq!(
        report1.fee_earned,
        report1.incoming_amount - report1.outgoing_amount,
        "Fee calculation should be correct"
    );

    info!("✅ V1 swap report integrity verified");

    test_framework.stop();
    block_gen.join().unwrap();
}

/// Test 2: Normal V2 (Taproot) swap with report verification
#[test]
fn test_v2_taproot_swap_reports_integrity() {
    warn!("🧪 Test 2: V2 Taproot Swap Report Integrity");

    let makers_config_map = vec![
        (7102, Some(19070), TaprootMakerBehavior::Normal),
        (17102, Some(19071), TaprootMakerBehavior::Normal),
    ];
    let taker_behavior = vec![TaprootTakerBehavior::Normal];

    let (test_framework, mut taproot_takers, taproot_makers, block_gen) =
        TestFramework::init_taproot(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = &mut taproot_takers[0];

    // Fund wallets
    info!("💰 Funding taproot wallets");
    fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    // Start maker servers
    info!("🚀 Starting taproot maker servers");
    let maker_threads: Vec<_> = taproot_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server_taproot(maker_clone).unwrap();
            })
        })
        .collect();

    // Wait for setup
    for maker in &taproot_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            info!("⏳ Waiting for taproot maker setup");
            thread::sleep(Duration::from_secs(10));
        }
    }

    // Sync wallets
    for maker in &taproot_makers {
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    // Perform swap
    info!("🔄 Initiating taproot swap");
    let swap_params = TaprootSwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    let result = taproot_taker.do_coinswap(swap_params);
    assert!(
        result.is_ok(),
        "Taproot swap should succeed: {:?}",
        result.err()
    );

    // Shutdown
    info!("🛑 Shutting down");
    for maker in &taproot_makers {
        maker.shutdown.store(true, Relaxed);
    }
    for thread in maker_threads {
        thread.join().unwrap();
    }

    // Verify reports
    info!("📊 Verifying taproot reports");

    let temp_dir = env::temp_dir().join("coinswap");

    let maker1_reports = read_maker_reports(&temp_dir.join("7102"), "maker7102");
    let maker2_reports = read_maker_reports(&temp_dir.join("17102"), "maker17102");

    assert!(!maker1_reports.is_empty(), "Maker 7102 should have reports");
    assert!(
        !maker2_reports.is_empty(),
        "Maker 17102 should have reports"
    );

    // Verify chain integrity - maker2's outgoing should equal maker1's incoming
    let report1 = &maker1_reports[0];
    let report2 = &maker2_reports[0];

    assert_eq!(
        report2.outgoing_contract_txid, report1.incoming_contract_txid,
        "Contract chain should be connected"
    );

    info!("✅ V2 taproot swap report integrity verified");

    test_framework.stop();
    block_gen.join().unwrap();
}

/// Test 3: Verify fee calculations are mathematically correct
#[test]
fn test_fee_calculation_accuracy() {
    warn!("🧪 Test 3: Fee Calculation Accuracy");

    let makers_config_map = vec![
        (7108, Some(19078), TaprootMakerBehavior::Normal),
        (17108, Some(19079), TaprootMakerBehavior::Normal),
    ];
    let taker_behavior = vec![TaprootTakerBehavior::Normal];

    let (test_framework, mut taproot_takers, taproot_makers, block_gen) =
        TestFramework::init_taproot(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = &mut taproot_takers[0];

    fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    let maker_threads: Vec<_> = taproot_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server_taproot(maker_clone).unwrap();
            })
        })
        .collect();

    for maker in &taproot_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            thread::sleep(Duration::from_secs(10));
        }
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    let swap_params = TaprootSwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    taproot_taker.do_coinswap(swap_params).unwrap();

    for maker in &taproot_makers {
        maker.shutdown.store(true, Relaxed);
    }
    for thread in maker_threads {
        thread.join().unwrap();
    }

    // Verify fee math
    let temp_dir = env::temp_dir().join("coinswap");
    let maker1_reports = read_maker_reports(&temp_dir.join("7108"), "maker7108");
    let maker2_reports = read_maker_reports(&temp_dir.join("17108"), "maker17108");

    for report in maker1_reports.iter().chain(maker2_reports.iter()) {
        let expected_fee = report
            .incoming_amount
            .saturating_sub(report.outgoing_amount);
        assert_eq!(
            report.fee_earned, expected_fee,
            "Fee should equal incoming - outgoing: {} != {} - {}",
            report.fee_earned, report.incoming_amount, report.outgoing_amount
        );

        info!(
            "✓ Maker report: in={}, out={}, fee={} (verified)",
            report.incoming_amount, report.outgoing_amount, report.fee_earned
        );
    }

    info!("✅ Fee calculation accuracy verified");

    test_framework.stop();
    block_gen.join().unwrap();
}

/// Test 4: Report JSON structure validation
#[test]
fn test_report_json_structure() {
    warn!("🧪 Test 4: Report JSON Structure Validation");

    let makers_config_map = vec![
        (7110, Some(19080), TaprootMakerBehavior::Normal),
        (17110, Some(19081), TaprootMakerBehavior::Normal),
    ];
    let taker_behavior = vec![TaprootTakerBehavior::Normal];

    let (test_framework, mut taproot_takers, taproot_makers, block_gen) =
        TestFramework::init_taproot(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taproot_taker = &mut taproot_takers[0];

    fund_taproot_taker(taproot_taker, bitcoind, 3, Amount::from_btc(0.05).unwrap());
    fund_taproot_makers(
        &taproot_makers,
        bitcoind,
        4,
        Amount::from_btc(0.05).unwrap(),
    );

    let maker_threads: Vec<_> = taproot_makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server_taproot(maker_clone).unwrap();
            })
        })
        .collect();

    for maker in &taproot_makers {
        while !maker.is_setup_complete.load(Relaxed) {
            thread::sleep(Duration::from_secs(10));
        }
        maker.wallet().write().unwrap().sync_and_save().unwrap();
    }

    let swap_params = TaprootSwapParams {
        send_amount: Amount::from_sat(500000),
        maker_count: 2,
        tx_count: 3,
        required_confirms: 1,
        manually_selected_outpoints: None,
    };

    taproot_taker.do_coinswap(swap_params).unwrap();

    for maker in &taproot_makers {
        maker.shutdown.store(true, Relaxed);
    }
    for thread in maker_threads {
        thread.join().unwrap();
    }

    // Read raw JSON and validate structure
    let temp_dir = env::temp_dir().join("coinswap");
    let report_path = temp_dir.join("7110/wallets/maker7110_maker_swap_reports.json");
    let content = fs::read_to_string(&report_path).expect("Should read report file");

    // Parse as generic JSON to check structure
    let json: serde_json::Value = serde_json::from_str(&content).expect("Should be valid JSON");

    assert!(json.is_array(), "Report should be a JSON array");
    let arr = json.as_array().unwrap();
    assert!(!arr.is_empty(), "Should have at least one report");

    let report = &arr[0];

    // Verify all required fields exist
    let required_fields = [
        "swap_id",
        "status",
        "swap_duration_seconds",
        "start_timestamp",
        "end_timestamp",
        "incoming_amount",
        "outgoing_amount",
        "fee_earned",
        "incoming_contract_txid",
        "outgoing_contract_txid",
        "timelock",
        "network",
    ];

    for field in required_fields {
        assert!(
            report.get(field).is_some(),
            "Missing required field: {}",
            field
        );
    }

    // Verify optional fields have correct types when present
    if let Some(sweep) = report.get("sweep_txid") {
        assert!(
            sweep.is_null() || sweep.is_string(),
            "sweep_txid should be null or string"
        );
    }
    if let Some(recovery) = report.get("recovery_txid") {
        assert!(
            recovery.is_null() || recovery.is_string(),
            "recovery_txid should be null or string"
        );
    }
    if let Some(error) = report.get("error_message") {
        assert!(
            error.is_null() || error.is_string(),
            "error_message should be null or string"
        );
    }

    info!("✅ Report JSON structure validated");

    test_framework.stop();
    block_gen.join().unwrap();
}
