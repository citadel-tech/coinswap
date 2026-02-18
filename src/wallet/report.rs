#![allow(clippy::too_many_arguments)]
//! Swap report for both Taker and Maker.
//!
//! This module provides a single [`SwapReport`] structure that captures all relevant
//! information about a coinswap transaction, regardless of whether the participant
//! is a taker or maker. It supports success, failure, and recovery scenarios.

use serde::{Deserialize, Serialize};
use std::{path::Path, str::FromStr, time::Instant};

/// Role of the participant in the swap.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwapRole {
    /// The initiator of the swap who sends coins first.
    Taker,
    /// The liquidity provider who responds to swap requests.
    Maker,
}

impl std::fmt::Display for SwapRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapRole::Taker => write!(f, "Taker"),
            SwapRole::Maker => write!(f, "Maker"),
        }
    }
}

/// Status of the swap indicating how it completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwapStatus {
    /// Swap completed successfully via cooperative key exchange.
    Success,
    /// Swap recovered using the hashlock script path.
    RecoveryHashlock,
    /// Swap recovered using the timelock script path.
    RecoveryTimelock,
    /// Swap failed and could not be completed or recovered.
    Failed,
}

impl FromStr for SwapStatus {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "success" => Ok(SwapStatus::Success),
            "recovery_hashlock" => Ok(SwapStatus::RecoveryHashlock),
            "recovery_timelock" => Ok(SwapStatus::RecoveryTimelock),
            "failed" => Ok(SwapStatus::Failed),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for SwapStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapStatus::Success => write!(f, "\x1b[1;32m SUCCESS\x1b[0m"),
            SwapStatus::RecoveryHashlock => write!(f, "\x1b[1;33m RECOVERED (hashlock)\x1b[0m"),
            SwapStatus::RecoveryTimelock => write!(f, "\x1b[1;33m RECOVERED (timelock)\x1b[0m"),
            SwapStatus::Failed => write!(f, "\x1b[1;31m FAILED\x1b[0m"),
        }
    }
}

/// Fee information for an individual maker in a swap route.
///
/// Used by taker reports to provide a detailed breakdown of fees
/// charged by each maker in the multi-hop swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerFeeInfo {
    /// Zero-based index of the maker in the swap route.
    pub maker_index: usize,
    /// Network address of the maker (e.g., onion address with port).
    pub maker_address: String,
    /// Fixed base fee charged by this maker (in satoshis).
    pub base_fee: f64,
    /// Fee proportional to the swap amount (in satoshis).
    pub amount_relative_fee: f64,
    /// Fee proportional to the timelock duration (in satoshis).
    pub time_relative_fee: f64,
    /// Total fee charged by this maker (sum of all fee components).
    pub total_fee: f64,
}

/// Swap report capturing all details of a coinswap transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapReport {
    /// Unique identifier for this swap (typically derived from preimage hash).
    pub swap_id: String,
    /// Role of the participant who generated this report.
    pub role: SwapRole,
    /// Final status of the swap.
    pub status: SwapStatus,
    /// Total duration of the swap in seconds.
    pub swap_duration_seconds: f64,
    /// Unix timestamp when the swap started.
    pub start_timestamp: u64,
    /// Unix timestamp when the swap completed.
    pub end_timestamp: u64,
    /// Bitcoin network used (mainnet, testnet, signet, regtest).
    pub network: String,
    /// Error message if the swap failed.
    pub error_message: Option<String>,

    /// Amount received in the incoming contract (satoshis).
    /// For makers: amount received from the previous hop.
    /// For takers: total output amount after swap.
    pub incoming_amount: u64,
    /// Amount sent in the outgoing contract (satoshis).
    /// For makers: amount sent to the next hop.
    /// For takers: target swap amount.
    pub outgoing_amount: u64,
    /// Net fee result (satoshis).
    /// Positive for makers (fee earned).
    /// Negative for takers (fee paid).
    pub fee_paid_or_earned: i64,

    /// Transaction ID of the incoming contract (maker only).
    pub incoming_contract_txid: Option<String>,
    /// Transaction ID of the outgoing contract (maker only).
    pub outgoing_contract_txid: Option<String>,
    /// Funding transaction IDs organized by hop (taker only).
    /// Each inner vector contains txids for one hop in the swap route.
    pub funding_txids: Vec<Vec<String>>,
    /// Transaction ID of the recovery transaction (hashlock/timelock).
    pub recovery_txid: Option<String>,

    /// Timelock value in blocks for the contract.
    pub timelock: u16,
    /// Number of makers involved in the swap (taker only).
    pub makers_count: Option<usize>,
    /// List of maker addresses used in the swap route (taker only).
    pub maker_addresses: Vec<String>,

    /// Detailed fee information for each maker (taker only).
    pub maker_fee_info: Vec<MakerFeeInfo>,
    /// Total fees paid to all makers (satoshis, taker only).
    pub total_maker_fees: u64,
    /// Mining fees paid for all transactions (satoshis, taker only).
    pub mining_fee: u64,
    /// Total fee as a percentage of the target amount (taker only).
    pub fee_percentage: f64,

    /// Input UTXO amounts consumed by the swap (satoshis).
    pub input_utxos: Vec<u64>,
    /// Change output amounts returned to regular wallet (satoshis, taker only).
    pub output_change_amounts: Vec<u64>,
    /// Swap output amounts received from the swap (satoshis, taker only).
    pub output_swap_amounts: Vec<u64>,
    /// Change UTXOs with amounts and addresses (amount in sats, address).
    pub output_change_utxos: Vec<(u64, String)>,
    /// Swap UTXOs with amounts and addresses (amount in sats, address).
    pub output_swap_utxos: Vec<(u64, String)>,
}

impl SwapReport {
    /// Create a successful maker swap report.
    pub fn maker_success(
        swap_id: String,
        start_time: Instant,
        incoming_amount: u64,
        outgoing_amount: u64,
        incoming_contract_txid: String,
        outgoing_contract_txid: String,
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
            role: SwapRole::Maker,
            status: SwapStatus::Success,
            swap_duration_seconds: swap_duration.as_secs_f64(),
            start_timestamp: end_timestamp.saturating_sub(swap_duration.as_secs()),
            end_timestamp,
            network,
            error_message: None,
            incoming_amount,
            outgoing_amount,
            fee_paid_or_earned: (incoming_amount as i64) - (outgoing_amount as i64),
            incoming_contract_txid: Some(incoming_contract_txid),
            outgoing_contract_txid: Some(outgoing_contract_txid),
            funding_txids: vec![],
            recovery_txid: None,
            timelock,
            makers_count: None,
            maker_addresses: vec![],
            maker_fee_info: vec![],
            total_maker_fees: 0,
            mining_fee: 0,
            fee_percentage: 0.0,
            input_utxos: vec![],
            output_change_amounts: vec![],
            output_swap_amounts: vec![],
            output_change_utxos: vec![],
            output_swap_utxos: vec![],
        }
    }

    /// Create a maker recovery report (hashlock or timelock).
    pub fn maker_recovery(
        swap_id: String,
        recovery_type: &str,
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
            "hashlock" => SwapStatus::RecoveryHashlock,
            "timelock" => SwapStatus::RecoveryTimelock,
            _ => SwapStatus::Failed,
        };

        Self {
            swap_id,
            role: SwapRole::Maker,
            status,
            swap_duration_seconds: 0.0,
            start_timestamp: end_timestamp,
            end_timestamp,
            network,
            error_message: None,
            incoming_amount,
            outgoing_amount,
            fee_paid_or_earned: 0,
            incoming_contract_txid: Some(incoming_contract_txid),
            outgoing_contract_txid: Some(outgoing_contract_txid),
            funding_txids: vec![],
            recovery_txid: Some(recovery_txid),
            timelock,
            makers_count: None,
            maker_addresses: vec![],
            maker_fee_info: vec![],
            total_maker_fees: 0,
            mining_fee: 0,
            fee_percentage: 0.0,
            input_utxos: vec![],
            output_change_amounts: vec![],
            output_swap_amounts: vec![],
            output_change_utxos: vec![],
            output_swap_utxos: vec![],
        }
    }

    /// Create a failed maker swap report.
    pub fn maker_failed(
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
            role: SwapRole::Maker,
            status: SwapStatus::Failed,
            swap_duration_seconds: 0.0,
            start_timestamp: end_timestamp,
            end_timestamp,
            network,
            error_message: Some(error_message),
            incoming_amount,
            outgoing_amount,
            fee_paid_or_earned: 0,
            incoming_contract_txid: Some(incoming_contract_txid),
            outgoing_contract_txid: Some(outgoing_contract_txid),
            funding_txids: vec![],
            recovery_txid: None,
            timelock,
            makers_count: None,
            maker_addresses: vec![],
            maker_fee_info: vec![],
            total_maker_fees: 0,
            mining_fee: 0,
            fee_percentage: 0.0,
            input_utxos: vec![],
            output_change_amounts: vec![],
            output_swap_amounts: vec![],
            output_change_utxos: vec![],
            output_swap_utxos: vec![],
        }
    }

    /// Create a successful taker swap report.
    #[allow(clippy::too_many_arguments)]
    pub fn taker_success(
        swap_id: String,
        swap_duration_seconds: f64,
        target_amount: u64,
        total_output: u64,
        makers_count: usize,
        maker_addresses: Vec<String>,
        funding_txids: Vec<Vec<String>>,
        total_fee: u64,
        total_maker_fees: u64,
        mining_fee: u64,
        fee_percentage: f64,
        maker_fee_info: Vec<MakerFeeInfo>,
        input_utxos: Vec<u64>,
        output_change_amounts: Vec<u64>,
        output_swap_amounts: Vec<u64>,
        output_change_utxos: Vec<(u64, String)>,
        output_swap_utxos: Vec<(u64, String)>,
        network: String,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let end_timestamp = now.as_secs();
        let start_timestamp = end_timestamp - (swap_duration_seconds as u64);

        Self {
            swap_id,
            role: SwapRole::Taker,
            status: SwapStatus::Success,
            swap_duration_seconds,
            start_timestamp,
            end_timestamp,
            network,
            error_message: None,
            incoming_amount: total_output,
            outgoing_amount: target_amount,
            fee_paid_or_earned: -(total_fee as i64),
            incoming_contract_txid: None,
            outgoing_contract_txid: None,
            funding_txids,
            recovery_txid: None,
            timelock: 0,
            makers_count: Some(makers_count),
            maker_addresses,
            maker_fee_info,
            total_maker_fees,
            mining_fee,
            fee_percentage,
            input_utxos,
            output_change_amounts,
            output_swap_amounts,
            output_change_utxos,
            output_swap_utxos,
        }
    }

    /// Save the report to disk as a JSON file.
    ///
    /// The report is saved to `{data_dir}/swap_reports/{role}_{timestamp}_{swap_id}.json`.
    pub fn save_to_disk(&self, data_dir: &Path) -> std::io::Result<()> {
        let reports_dir = data_dir.join("swap_reports");
        std::fs::create_dir_all(&reports_dir)?;

        let role_prefix = match self.role {
            SwapRole::Taker => "taker",
            SwapRole::Maker => "maker",
        };

        let sanitized_swap_id: String = self
            .swap_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(64)
            .collect();

        let safe_swap_id = if sanitized_swap_id.is_empty() {
            "unknown".to_string()
        } else {
            sanitized_swap_id
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let timestamp_ms = now.as_millis();

        let filename = format!(
            "{}_{}_{}_{}.json",
            role_prefix, self.end_timestamp, timestamp_ms, safe_swap_id
        );
        let filepath = reports_dir.join(filename);

        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&filepath, json)?;

        log::info!("Saved swap report to: {}", filepath.display());
        Ok(())
    }

    /// Print the report to console with colored formatting.
    ///
    /// Displays a human-readable summary of the swap including status,
    /// amounts, transaction IDs, and fee breakdown (for taker reports).
    pub fn print(&self) {
        let role_str = self.role.to_string().to_uppercase();

        println!("\n\x1b[1;36m================================================================================");
        println!("                         {} SWAP REPORT", role_str);
        println!("================================================================================\x1b[0m\n");

        println!("\x1b[1;37mSwap ID           :\x1b[0m {}", self.swap_id);
        println!("\x1b[1;37mStatus            :\x1b[0m {}", self.status);

        if self.swap_duration_seconds > 0.0 {
            println!(
                "\x1b[1;37mDuration          :\x1b[0m {:.2} seconds",
                self.swap_duration_seconds
            );
        }

        println!("\n\x1b[1;36m--------------------------------------------------------------------------------");
        println!("                              Swap Details");
        println!("--------------------------------------------------------------------------------\x1b[0m");

        match self.role {
            SwapRole::Maker => {
                println!(
                    "\x1b[1;37mIncoming Amount   :\x1b[0m {} sats",
                    self.incoming_amount
                );
                println!(
                    "\x1b[1;37mOutgoing Amount   :\x1b[0m {} sats",
                    self.outgoing_amount
                );
                println!(
                    "\x1b[1;37mFee Earned        :\x1b[0m \x1b[1;32m{} sats\x1b[0m",
                    self.fee_paid_or_earned
                );
            }
            SwapRole::Taker => {
                println!(
                    "\x1b[1;37mTarget Amount     :\x1b[0m {} sats",
                    self.outgoing_amount
                );
                println!(
                    "\x1b[1;37mTotal Output      :\x1b[0m {} sats",
                    self.incoming_amount
                );
                println!(
                    "\x1b[1;37mTotal Fee Paid    :\x1b[0m \x1b[1;31m{} sats\x1b[0m",
                    -self.fee_paid_or_earned
                );
                if self.mining_fee > 0 {
                    println!(
                        "\x1b[1;37mMining Fee        :\x1b[0m {} sats",
                        self.mining_fee
                    );
                }
            }
        }

        if self.timelock > 0 {
            println!(
                "\x1b[1;37mTimelock          :\x1b[0m {} blocks",
                self.timelock
            );
        }
        println!("\x1b[1;37mNetwork           :\x1b[0m {}", self.network);

        if let Some(count) = self.makers_count {
            println!("\x1b[1;37mMakers Used       :\x1b[0m {}", count);
            for (i, addr) in self.maker_addresses.iter().enumerate() {
                println!("  Maker {}         : {}", i + 1, addr);
            }
        }

        println!("\n\x1b[1;36m--------------------------------------------------------------------------------");
        println!("                            Transaction IDs");
        println!("--------------------------------------------------------------------------------\x1b[0m");

        if let Some(ref txid) = self.incoming_contract_txid {
            println!("\x1b[1;37mIncoming Contract :\x1b[0m {}", txid);
        }
        if let Some(ref txid) = self.outgoing_contract_txid {
            println!("\x1b[1;37mOutgoing Contract :\x1b[0m {}", txid);
        }
        if !self.funding_txids.is_empty() {
            println!("\x1b[1;37mFunding Txs       :\x1b[0m");
            for (hop_idx, hop_txids) in self.funding_txids.iter().enumerate() {
                println!("  Hop {}:", hop_idx + 1);
                for (i, txid) in hop_txids.iter().enumerate() {
                    println!("    {}. {}", i + 1, txid);
                }
            }
        }
        if let Some(ref txid) = self.recovery_txid {
            println!("\x1b[1;37mRecovery Tx       :\x1b[0m {}", txid);
        }

        if !self.maker_fee_info.is_empty() {
            println!("\n\x1b[1;36m--------------------------------------------------------------------------------");
            println!("                           Maker Fee Breakdown");
            println!("--------------------------------------------------------------------------------\x1b[0m");
            for info in &self.maker_fee_info {
                println!(
                    "\n\x1b[1;33mMaker {}:\x1b[0m {}",
                    info.maker_index + 1,
                    info.maker_address
                );
                println!("    Base Fee             : {:.2}", info.base_fee);
                println!("    Amount Relative Fee  : {:.2}", info.amount_relative_fee);
                println!("    Time Relative Fee    : {:.2}", info.time_relative_fee);
                println!("    Total Fee            : {:.2}", info.total_fee);
            }
        }

        if !self.input_utxos.is_empty() || !self.output_swap_utxos.is_empty() {
            println!("\n\x1b[1;36m--------------------------------------------------------------------------------");
            println!("                              UTXO Information");
            println!("--------------------------------------------------------------------------------\x1b[0m");
            if !self.input_utxos.is_empty() {
                println!(
                    "\x1b[1;37mInput UTXOs       :\x1b[0m {:?}",
                    self.input_utxos
                );
            }
            if !self.output_change_utxos.is_empty() {
                println!(
                    "\x1b[1;37mChange UTXOs      :\x1b[0m {:?}",
                    self.output_change_utxos
                );
            }
            if !self.output_swap_utxos.is_empty() {
                println!(
                    "\x1b[1;37mSwap UTXOs        :\x1b[0m {:?}",
                    self.output_swap_utxos
                );
            }
        }

        if let Some(ref error) = self.error_message {
            println!("\n\x1b[1;31mError: {}\x1b[0m", error);
        }

        println!("\n\x1b[1;36m================================================================================");
        println!("                                END REPORT");
        println!("================================================================================\x1b[0m\n");
    }
}
