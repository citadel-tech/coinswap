//! Swap report persistence for takers and makers.
//!
//! This module provides unified swap report types and persistence for both
//! taker and maker participants in coinswap transactions.
//!
//! # Report Types
//!
//! - [`TakerSwapReport`]: Comprehensive report for taker-initiated swaps
//! - [`MakerSwapReport`]: Report for maker participation in swaps
//! - [`MakerFeeInfo`]: Detailed fee breakdown for individual makers
//!
//! # Persistence
//!
//! Reports are persisted as JSON arrays, allowing historical tracking:
//! - Taker reports: `{data_dir}/wallets/{wallet_name}_swap_reports.json`
//! - Maker reports: `{data_dir}/wallets/{wallet_name}_maker_swap_reports.json`

use crate::wallet::WalletError;
use serde::{Deserialize, Serialize};
use std::{io::BufWriter, path::Path};

/// Information about individual maker fees in a swap
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerFeeInfo {
    /// Index of maker in the swap route
    pub maker_index: usize,
    /// Maker Addresses (Onion:Port)
    pub maker_address: String,
    /// The fixed Base Fee for each maker
    pub base_fee: f64,
    /// Dynamic Amount Fee for each maker
    pub amount_relative_fee: f64,
    /// Dynamic Time Fee(Decreases for subsequent makers) for each maker
    pub time_relative_fee: f64,
    /// All inclusive fee for each maker
    pub total_fee: f64,
}

/// Complete swap report for taker-initiated swaps.
///
/// Contains comprehensive information about a completed coinswap including
/// fees paid, UTXOs involved, and transaction details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakerSwapReport {
    /// Unique swap ID
    pub swap_id: String,
    /// Duration of the swap in seconds
    pub swap_duration_seconds: f64,
    /// Target amount for the swap
    pub target_amount: u64,
    /// Total input amount
    pub total_input_amount: u64,
    /// Total output amount
    pub total_output_amount: u64,
    /// Number of makers involved
    pub makers_count: usize,
    /// List of maker addresses used
    pub maker_addresses: Vec<String>,
    /// Total number of funding transactions
    pub total_funding_txs: usize,
    /// Funding transaction IDs organized by hops
    pub funding_txids_by_hop: Vec<Vec<String>>,
    /// Total fees paid
    pub total_fee: u64,
    /// Total maker fees
    pub total_maker_fees: u64,
    /// Mining fees
    pub mining_fee: u64,
    /// Fee percentage relative to target amount
    pub fee_percentage: f64,
    /// Individual maker fee information
    pub maker_fee_info: Vec<MakerFeeInfo>,
    /// Input UTXOs amounts
    pub input_utxos: Vec<u64>,
    /// Output change UTXOs amounts
    pub output_change_amounts: Vec<u64>,
    /// Output swap coin UTXOs amounts
    pub output_swap_amounts: Vec<u64>,
    /// Output change coin UTXOs with amounts and addresses (amount, address)
    pub output_change_utxos: Vec<(u64, String)>,
    /// Output swap coin UTXOs with amounts and addresses (amount, address)
    pub output_swap_utxos: Vec<(u64, String)>,
}

/// Persists a taker swap report to the data directory.
///
/// Reports are stored as a JSON array in `{data_dir}/wallets/{wallet_name}_swap_reports.json`,
/// appending to existing reports to maintain history.
pub fn persist_taker_report(
    data_dir: &Path,
    wallet_name: &str,
    report: &TakerSwapReport,
) -> Result<(), WalletError> {
    let reports_path = data_dir
        .join("wallets")
        .join(format!("{}_swap_reports.json", wallet_name));

    persist_report_to_array(&reports_path, report)
}

/// Status of a maker swap
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MakerSwapReportStatus {
    /// Swap completed successfully
    Success,
    /// Swap recovered via hashlock path
    RecoveryHashlock,
    /// Swap recovered via timelock path
    RecoveryTimelock,
    /// Swap failed
    Failed,
}

impl std::fmt::Display for MakerSwapReportStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MakerSwapReportStatus::Failed => write!(f, "failed"),
            MakerSwapReportStatus::Success => write!(f, "success"),
            MakerSwapReportStatus::RecoveryHashlock => write!(f, "recovery_hashlock"),
            MakerSwapReportStatus::RecoveryTimelock => write!(f, "recovery_timelock"),
        }
    }
}

/// Swap report for maker participation.
///
/// Contains information about a swap from the maker's perspective,
/// including amounts, fees earned, and recovery information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerSwapReport {
    /// Unique identifier for the swap
    pub swap_id: String,
    /// Status of the swap
    pub status: MakerSwapReportStatus,
    /// Duration of the swap in seconds
    pub swap_duration_seconds: f64,
    /// Timestamp when the swap started (Unix epoch)
    pub start_timestamp: u64,
    /// Timestamp when the swap ended (Unix epoch)
    pub end_timestamp: u64,
    /// Amount received in the incoming contract (sats)
    pub incoming_amount: u64,
    /// Amount sent in the outgoing contract (sats)
    pub outgoing_amount: u64,
    /// Fee earned by the maker (sats)
    pub fee_earned: u64,
    /// Incoming contract transaction ID
    pub incoming_contract_txid: String,
    /// Outgoing contract transaction ID
    pub outgoing_contract_txid: String,
    /// Sweep transaction ID (if successful)
    pub sweep_txid: Option<String>,
    /// Recovery transaction ID (if recovered via hashlock/timelock)
    pub recovery_txid: Option<String>,
    /// Timelock value used in the swap
    pub timelock: u16,
    /// Network (mainnet, testnet, regtest)
    pub network: String,
    /// Error message (if failed)
    pub error_message: Option<String>,
}

#[allow(clippy::too_many_arguments)]
impl MakerSwapReport {
    /// Create a new successful swap report
    pub fn success(
        swap_id: String,
        start_time: std::time::Instant,
        incoming_amount: u64,
        outgoing_amount: u64,
        incoming_contract_txid: String,
        outgoing_contract_txid: String,
        sweep_txid: String,
        timelock: u16,
        network: String,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let end_timestamp = now.as_secs();
        let swap_duration = start_time.elapsed();

        Self {
            swap_id,
            status: MakerSwapReportStatus::Success,
            swap_duration_seconds: swap_duration.as_secs_f64(),
            start_timestamp: end_timestamp.saturating_sub(swap_duration.as_secs()),
            end_timestamp,
            incoming_amount,
            outgoing_amount,
            fee_earned: incoming_amount.saturating_sub(outgoing_amount),
            incoming_contract_txid,
            outgoing_contract_txid,
            sweep_txid: Some(sweep_txid),
            recovery_txid: None,
            timelock,
            network,
            error_message: None,
        }
    }

    /// Create a recovery swap report (hashlock or timelock)
    pub fn recovery(
        swap_id: String,
        recovery_type: &str, // "hashlock" or "timelock"
        incoming_amount: u64,
        outgoing_amount: u64,
        incoming_contract_txid: String,
        outgoing_contract_txid: String,
        recovery_txid: String,
        timelock: u16,
        network: String,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let end_timestamp = now.as_secs();

        let status = match recovery_type {
            "hashlock" => MakerSwapReportStatus::RecoveryHashlock,
            "timelock" => MakerSwapReportStatus::RecoveryTimelock,
            _ => MakerSwapReportStatus::Failed,
        };

        Self {
            swap_id,
            status,
            swap_duration_seconds: 0.0,
            start_timestamp: 0,
            end_timestamp,
            incoming_amount,
            outgoing_amount,
            fee_earned: 0,
            incoming_contract_txid,
            outgoing_contract_txid,
            sweep_txid: None,
            recovery_txid: Some(recovery_txid),
            timelock,
            network,
            error_message: None,
        }
    }

    /// Create a failed swap report
    pub fn failed(
        swap_id: String,
        incoming_amount: u64,
        outgoing_amount: u64,
        incoming_contract_txid: String,
        outgoing_contract_txid: String,
        timelock: u16,
        network: String,
        error_message: String,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let end_timestamp = now.as_secs();

        Self {
            swap_id,
            status: MakerSwapReportStatus::Failed,
            swap_duration_seconds: 0.0,
            start_timestamp: 0,
            end_timestamp,
            incoming_amount,
            outgoing_amount,
            fee_earned: 0,
            incoming_contract_txid,
            outgoing_contract_txid,
            sweep_txid: None,
            recovery_txid: None,
            timelock,
            network,
            error_message: Some(error_message),
        }
    }

    /// Print the report to console
    pub fn print(&self) {
        println!(
            "\n================================================================================"
        );
        println!("                            MAKER SWAP REPORT");
        println!(
            "================================================================================\n"
        );

        println!("Swap ID           : {}", self.swap_id);
        println!("Status            : {}", self.status);

        if self.swap_duration_seconds > 0.0 {
            println!(
                "Duration          : {:.2} seconds",
                self.swap_duration_seconds
            );
        }

        println!(
            "\n--------------------------------------------------------------------------------"
        );
        println!("                              Swap Details");
        println!(
            "--------------------------------------------------------------------------------"
        );
        println!("Incoming Amount   : {} sats", self.incoming_amount);
        println!("Outgoing Amount   : {} sats", self.outgoing_amount);
        println!("Fee Earned        : {} sats", self.fee_earned);
        println!("Timelock          : {} blocks", self.timelock);
        println!("Network           : {}", self.network);

        println!(
            "\n--------------------------------------------------------------------------------"
        );
        println!("                            Transaction IDs");
        println!(
            "--------------------------------------------------------------------------------"
        );
        println!("Incoming Contract : {}", self.incoming_contract_txid);
        println!("Outgoing Contract : {}", self.outgoing_contract_txid);

        if let Some(ref sweep_txid) = self.sweep_txid {
            println!("Sweep Tx          : {}", sweep_txid);
        }

        if let Some(ref recovery_txid) = self.recovery_txid {
            println!("Recovery Tx       : {}", recovery_txid);
        }

        if let Some(ref error) = self.error_message {
            println!("\nError: {}", error);
        }

        println!(
            "\n================================================================================"
        );
        println!("                                END REPORT");
        println!(
            "================================================================================\n"
        );
    }
}

/// Persists a maker swap report to the data directory.
///
/// Reports are stored as a JSON array in `{data_dir}/wallets/{wallet_name}_maker_swap_reports.json`,
/// appending to existing reports to maintain history.
pub fn persist_maker_report(
    data_dir: &Path,
    wallet_name: &str,
    report: &MakerSwapReport,
) -> Result<(), WalletError> {
    let reports_path = data_dir
        .join("wallets")
        .join(format!("{}_maker_swap_reports.json", wallet_name));

    persist_report_to_array(&reports_path, report)
}

/// Generic helper to persist a report to a JSON array file.
fn persist_report_to_array<T>(reports_path: &Path, report: &T) -> Result<(), WalletError>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    // Ensure parent directory exists
    if let Some(parent) = reports_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Load existing reports or create empty vec
    let mut reports: Vec<T> = if reports_path.exists() {
        let file = std::fs::File::open(reports_path)?;
        match serde_json::from_reader(&file) {
            Ok(existing) => existing,
            Err(e) => {
                log::error!(
                    "Swap reports corrupted at {:?}. Starting fresh. Error: {:?}",
                    reports_path,
                    e
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // Append new report
    reports.push(report.clone());

    // Write back to file
    let file = std::fs::File::create(reports_path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &reports)?;

    Ok(())
}
