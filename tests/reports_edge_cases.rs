#![cfg(feature = "integration-test")]
//! Edge case and stress tests for swap report persistence.
//!
//! These tests try to break the reports module with:
//! - Corrupted files
//! - Special characters in names
//! - Concurrent access
//! - Large data
//! - Missing directories

use coinswap::wallet::{
    persist_maker_report, persist_taker_report, MakerFeeInfo, MakerSwapReport,
    MakerSwapReportStatus, TakerSwapReport,
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

fn make_taker_report(swap_id: &str) -> TakerSwapReport {
    TakerSwapReport {
        swap_id: swap_id.to_string(),
        swap_duration_seconds: 60.0,
        target_amount: 50000,
        total_input_amount: 55000,
        total_output_amount: 50000,
        makers_count: 1,
        maker_addresses: vec!["test.onion:6102".to_string()],
        total_funding_txs: 1,
        funding_txids_by_hop: vec![vec!["abc123".to_string()]],
        total_fee: 5000,
        total_maker_fees: 3000,
        mining_fee: 2000,
        fee_percentage: 10.0,
        maker_fee_info: vec![MakerFeeInfo {
            maker_index: 0,
            maker_address: "test.onion:6102".to_string(),
            base_fee: 100.0,
            amount_relative_fee: 50.0,
            time_relative_fee: 10.0,
            total_fee: 160.0,
        }],
        input_utxos: vec![55000],
        output_change_amounts: vec![5000],
        output_swap_amounts: vec![50000],
        output_change_utxos: vec![(5000, "bc1qtest".to_string())],
        output_swap_utxos: vec![(50000, "bc1qswap".to_string())],
    }
}

fn make_maker_report(swap_id: &str) -> MakerSwapReport {
    MakerSwapReport {
        swap_id: swap_id.to_string(),
        status: MakerSwapReportStatus::Success,
        swap_duration_seconds: 120.0,
        start_timestamp: 1700000000,
        end_timestamp: 1700000120,
        incoming_amount: 100000,
        outgoing_amount: 95000,
        fee_earned: 5000,
        incoming_contract_txid: "incoming_txid".to_string(),
        outgoing_contract_txid: "outgoing_txid".to_string(),
        sweep_txid: Some("sweep_txid".to_string()),
        recovery_txid: None,
        timelock: 144,
        network: "regtest".to_string(),
        error_message: None,
    }
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Test: Corrupted JSON file - should recover gracefully
#[test]
fn test_corrupted_json_recovery() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_corrupted");
    cleanup(&temp_dir);
    fs::create_dir_all(temp_dir.join("wallets")).unwrap();

    // Write garbage to the report file
    let report_path = temp_dir.join("wallets/test_wallet_swap_reports.json");
    fs::write(&report_path, "{ this is not valid json [[[ }}}").unwrap();

    // Should handle gracefully and overwrite
    let report = make_taker_report("after-corruption");
    persist_taker_report(&temp_dir, "test_wallet", &report).unwrap();

    // Verify it recovered
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<TakerSwapReport> = serde_json::from_str(&content).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].swap_id, "after-corruption");

    cleanup(&temp_dir);
}

/// Test: Empty JSON array - should append correctly
#[test]
fn test_empty_json_array() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_empty_array");
    cleanup(&temp_dir);
    fs::create_dir_all(temp_dir.join("wallets")).unwrap();

    // Write empty array
    let report_path = temp_dir.join("wallets/test_wallet_swap_reports.json");
    fs::write(&report_path, "[]").unwrap();

    // Should append to empty array
    let report = make_taker_report("first-report");
    persist_taker_report(&temp_dir, "test_wallet", &report).unwrap();

    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<TakerSwapReport> = serde_json::from_str(&content).unwrap();
    assert_eq!(reports.len(), 1);

    cleanup(&temp_dir);
}

/// Test: Special characters in wallet name
#[test]
fn test_special_chars_wallet_name() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_special_chars");
    cleanup(&temp_dir);

    // Wallet names with special chars (but safe ones)
    let wallet_names = vec![
        "wallet-with-dashes",
        "wallet_with_underscores",
        "wallet123",
        "UPPERCASE",
        "MixedCase123",
    ];

    for name in wallet_names {
        let report = make_taker_report(&format!("swap-{}", name));
        persist_taker_report(&temp_dir, name, &report).unwrap();

        let report_path = temp_dir
            .join("wallets")
            .join(format!("{}_swap_reports.json", name));
        assert!(report_path.exists(), "Report file should exist for {}", name);
    }

    cleanup(&temp_dir);
}

/// Test: Very large report data
#[test]
fn test_large_report_data() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_large_data");
    cleanup(&temp_dir);

    // Create report with lots of data
    let mut report = make_taker_report("large-report");
    report.maker_addresses = (0..100).map(|i| format!("maker{}.onion:6102", i)).collect();
    report.funding_txids_by_hop = (0..50)
        .map(|i| {
            (0..10)
                .map(|j| format!("txid_{}_{}", i, j))
                .collect::<Vec<_>>()
        })
        .collect();
    report.input_utxos = (0..1000).map(|i| i * 1000).collect();

    persist_taker_report(&temp_dir, "large_wallet", &report).unwrap();

    // Read back and verify
    let report_path = temp_dir.join("wallets/large_wallet_swap_reports.json");
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<TakerSwapReport> = serde_json::from_str(&content).unwrap();
    assert_eq!(reports[0].maker_addresses.len(), 100);
    assert_eq!(reports[0].funding_txids_by_hop.len(), 50);
    assert_eq!(reports[0].input_utxos.len(), 1000);

    cleanup(&temp_dir);
}

/// Test: Many sequential reports (stress test)
#[test]
fn test_many_sequential_reports() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_many_reports");
    cleanup(&temp_dir);

    let count = 100;
    for i in 0..count {
        let report = make_taker_report(&format!("swap-{:04}", i));
        persist_taker_report(&temp_dir, "stress_wallet", &report).unwrap();
    }

    let report_path = temp_dir.join("wallets/stress_wallet_swap_reports.json");
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<TakerSwapReport> = serde_json::from_str(&content).unwrap();
    assert_eq!(reports.len(), count);

    // Verify order
    for (i, report) in reports.iter().enumerate() {
        assert_eq!(report.swap_id, format!("swap-{:04}", i));
    }

    cleanup(&temp_dir);
}

/// Test: Concurrent writes to DIFFERENT files (should be safe)
#[test]
fn test_concurrent_different_files() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_concurrent_diff");
    cleanup(&temp_dir);

    let num_threads = 10;
    let reports_per_thread = 20;
    let barrier = Arc::new(Barrier::new(num_threads));

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let temp_dir = temp_dir.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait(); // Start all threads at once
                for i in 0..reports_per_thread {
                    let wallet_name = format!("wallet_{}", thread_id);
                    let report = make_taker_report(&format!("swap-{}-{}", thread_id, i));
                    persist_taker_report(&temp_dir, &wallet_name, &report).unwrap();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // Verify each wallet has correct number of reports
    for thread_id in 0..num_threads {
        let report_path = temp_dir
            .join("wallets")
            .join(format!("wallet_{}_swap_reports.json", thread_id));
        let content = fs::read_to_string(&report_path).unwrap();
        let reports: Vec<TakerSwapReport> = serde_json::from_str(&content).unwrap();
        assert_eq!(
            reports.len(),
            reports_per_thread,
            "Thread {} should have {} reports",
            thread_id,
            reports_per_thread
        );
    }

    cleanup(&temp_dir);
}

/// Test: Unicode in report fields
#[test]
fn test_unicode_in_fields() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_unicode");
    cleanup(&temp_dir);

    let mut report = make_maker_report("unicode-test");
    report.error_message = Some("Error: 日本語 中文 한국어 emoji 🚀💰".to_string());
    report.network = "テスト".to_string();

    persist_maker_report(&temp_dir, "unicode_wallet", &report).unwrap();

    let report_path = temp_dir.join("wallets/unicode_wallet_maker_swap_reports.json");
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<MakerSwapReport> = serde_json::from_str(&content).unwrap();

    assert!(reports[0].error_message.as_ref().unwrap().contains("日本語"));
    assert!(reports[0].error_message.as_ref().unwrap().contains("🚀"));

    cleanup(&temp_dir);
}

/// Test: Zero and edge case amounts
#[test]
fn test_edge_case_amounts() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_edge_amounts");
    cleanup(&temp_dir);

    let mut report = make_maker_report("edge-amounts");
    report.incoming_amount = 0;
    report.outgoing_amount = 0;
    report.fee_earned = 0;
    report.swap_duration_seconds = 0.0;
    report.timelock = 0;

    persist_maker_report(&temp_dir, "edge_wallet", &report).unwrap();

    let report_path = temp_dir.join("wallets/edge_wallet_maker_swap_reports.json");
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<MakerSwapReport> = serde_json::from_str(&content).unwrap();

    assert_eq!(reports[0].incoming_amount, 0);
    assert_eq!(reports[0].fee_earned, 0);

    cleanup(&temp_dir);
}

/// Test: Maximum u64 values
#[test]
fn test_max_u64_values() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_max_u64");
    cleanup(&temp_dir);

    let mut report = make_maker_report("max-values");
    report.incoming_amount = u64::MAX;
    report.outgoing_amount = u64::MAX;
    report.start_timestamp = u64::MAX;
    report.end_timestamp = u64::MAX;

    persist_maker_report(&temp_dir, "max_wallet", &report).unwrap();

    let report_path = temp_dir.join("wallets/max_wallet_maker_swap_reports.json");
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<MakerSwapReport> = serde_json::from_str(&content).unwrap();

    assert_eq!(reports[0].incoming_amount, u64::MAX);

    cleanup(&temp_dir);
}

/// Test: Empty string fields
#[test]
fn test_empty_strings() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_empty_strings");
    cleanup(&temp_dir);

    let mut report = make_maker_report("");
    report.swap_id = "".to_string();
    report.incoming_contract_txid = "".to_string();
    report.outgoing_contract_txid = "".to_string();
    report.network = "".to_string();

    persist_maker_report(&temp_dir, "empty_strings_wallet", &report).unwrap();

    let report_path = temp_dir.join("wallets/empty_strings_wallet_maker_swap_reports.json");
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<MakerSwapReport> = serde_json::from_str(&content).unwrap();

    assert_eq!(reports[0].swap_id, "");

    cleanup(&temp_dir);
}

/// Test: All MakerSwapReportStatus variants
#[test]
fn test_all_status_variants() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_status_variants");
    cleanup(&temp_dir);

    let statuses = [
        MakerSwapReportStatus::Success,
        MakerSwapReportStatus::Failed,
        MakerSwapReportStatus::RecoveryHashlock,
        MakerSwapReportStatus::RecoveryTimelock,
    ];

    for (i, status) in statuses.iter().enumerate() {
        let mut report = make_maker_report(&format!("status-{}", i));
        report.status = status.clone();
        persist_maker_report(&temp_dir, "status_wallet", &report).unwrap();
    }

    let report_path = temp_dir.join("wallets/status_wallet_maker_swap_reports.json");
    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<MakerSwapReport> = serde_json::from_str(&content).unwrap();

    assert_eq!(reports.len(), 4);

    cleanup(&temp_dir);
}

/// Test: Directory doesn't exist - should be created
#[test]
fn test_creates_missing_directories() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_missing_dir");
    cleanup(&temp_dir);
    // Don't create any directories

    let report = make_taker_report("test");
    persist_taker_report(&temp_dir, "new_wallet", &report).unwrap();

    assert!(temp_dir.join("wallets").exists());
    assert!(temp_dir
        .join("wallets/new_wallet_swap_reports.json")
        .exists());

    cleanup(&temp_dir);
}

/// Test: File with wrong type (object instead of array)
#[test]
fn test_wrong_json_type() {
    let temp_dir = std::env::temp_dir().join("coinswap_test_wrong_type");
    cleanup(&temp_dir);
    fs::create_dir_all(temp_dir.join("wallets")).unwrap();

    // Write a JSON object instead of array
    let report_path = temp_dir.join("wallets/test_wallet_swap_reports.json");
    fs::write(&report_path, r#"{"key": "value"}"#).unwrap();

    // Should handle gracefully
    let report = make_taker_report("after-wrong-type");
    persist_taker_report(&temp_dir, "test_wallet", &report).unwrap();

    let content = fs::read_to_string(&report_path).unwrap();
    let reports: Vec<TakerSwapReport> = serde_json::from_str(&content).unwrap();
    assert_eq!(reports.len(), 1);

    cleanup(&temp_dir);
}
