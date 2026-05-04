#![allow(clippy::too_many_arguments)]
//! Unified swap report file for all coinswap participants.
//!
//! All reports are appended to a single `swap_reports.json` file located one
//! level above each node's data directory (the coinswap root, e.g. `~/.coinswap/`).
//!
//! The file has three sections:
//! - `taker`   – one entry per taker swap (success or failed)
//! - `maker`   – one array per maker node, keyed by node name
//! - `recovery`– one entry per recovery event (hashlock or timelock)
//! - `deniability_proofs` – reserved for future use

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::Instant,
};

// ---------------------------------------------------------------------------
// Timestamp helper
// ---------------------------------------------------------------------------

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

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

/// Final outcome of a swap.
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

impl std::str::FromStr for SwapStatus {
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

/// Per-maker fee breakdown stored in taker reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerFeeInfo {
    /// Zero-based index of the maker in the swap route.
    pub maker_index: usize,
    /// Network address of the maker.
    pub maker_address: String,
    /// Fixed base fee charged by this maker (satoshis).
    pub base_fee: f64,
    /// Fee proportional to the swap amount (satoshis).
    pub amount_relative_fee: f64,
    /// Fee proportional to the timelock duration (satoshis).
    pub time_relative_fee: f64,
    /// Sum of all fee components for this maker (satoshis).
    pub total_fee: f64,
}

// ---------------------------------------------------------------------------
// Report structs
// ---------------------------------------------------------------------------

/// Taker-perspective record for one swap (success or failed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakerReport {
    /// Unique swap identifier derived from the preimage hash.
    pub swap_id: String,
    /// Final outcome of the swap.
    pub status: SwapStatus,
    /// Bitcoin network (mainnet, testnet, signet, regtest).
    pub network: String,
    /// Total duration of the swap in seconds.
    pub swap_duration_seconds: f64,
    /// Unix timestamp when the swap started.
    pub start_timestamp: u64,
    /// Unix timestamp when the swap completed.
    pub end_timestamp: u64,
    /// Error description if the swap failed.
    pub error_message: Option<String>,

    /// Amount the taker sent (satoshis).
    pub outgoing_amount: u64,
    /// Amount the taker received back after the swap (satoshis).
    pub incoming_amount: u64,
    /// Total fee paid (outgoing − incoming, satoshis).
    pub fee_paid: u64,
    /// Mining fees paid for all transactions (satoshis).
    pub mining_fee: u64,
    /// Total fee as a percentage of the target amount.
    pub fee_percentage: f64,
    /// Total fees paid to all makers (satoshis).
    pub total_maker_fees: u64,

    /// Transaction ID of the taker's outgoing contract.
    pub outgoing_contract_txid: Option<String>,
    /// Transaction ID of the taker's incoming contract.
    pub incoming_contract_txid: Option<String>,
    /// Funding transaction IDs organised by hop.
    pub funding_txids: Vec<Vec<String>>,

    /// Number of makers in the swap route.
    pub makers_count: usize,
    /// Addresses of all makers in route order.
    pub maker_addresses: Vec<String>,
    /// Detailed fee breakdown per maker.
    pub maker_fee_info: Vec<MakerFeeInfo>,

    /// Input UTXO amounts consumed by the swap (satoshis).
    pub input_utxos: Vec<u64>,
    /// Change output amounts returned to the regular wallet (satoshis).
    pub output_change_amounts: Vec<u64>,
    /// Swap output amounts received from the swap (satoshis).
    pub output_swap_amounts: Vec<u64>,
    /// Change UTXOs with amounts and addresses.
    pub output_change_utxos: Vec<(u64, String)>,
    /// Swap UTXOs with amounts and addresses.
    pub output_swap_utxos: Vec<(u64, String)>,
}

impl TakerReport {
    /// Save to the shared report file.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let report = self.clone();
        write_to_file(data_dir, |f| f.taker.push(report))
    }

    /// Print a human-readable summary to stdout.
    pub fn print(&self) {
        println!("\n\x1b[1;36m================================================================================");
        println!("                           TAKER SWAP REPORT");
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
            self.fee_paid
        );
        println!(
            "\x1b[1;37mFee Percentage    :\x1b[0m {:.4}%",
            self.fee_percentage
        );
        if self.mining_fee > 0 {
            println!(
                "\x1b[1;37mMining Fee        :\x1b[0m {} sats",
                self.mining_fee
            );
        }
        println!("\x1b[1;37mNetwork           :\x1b[0m {}", self.network);
        println!("\x1b[1;37mMakers Used       :\x1b[0m {}", self.makers_count);
        for (i, addr) in self.maker_addresses.iter().enumerate() {
            println!("  Maker {}         : {}", i + 1, addr);
        }

        println!("\n\x1b[1;36m--------------------------------------------------------------------------------");
        println!("                            Transaction IDs");
        println!("--------------------------------------------------------------------------------\x1b[0m");

        if let Some(ref txid) = self.outgoing_contract_txid {
            println!("\x1b[1;37mOutgoing Contract :\x1b[0m {}", txid);
        }
        if let Some(ref txid) = self.incoming_contract_txid {
            println!("\x1b[1;37mIncoming Contract :\x1b[0m {}", txid);
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
                println!("    Base Fee             : {} sats", info.base_fee);
                println!(
                    "    Amount Relative Fee  : {} sats",
                    info.amount_relative_fee
                );
                println!("    Time Relative Fee    : {} sats", info.time_relative_fee);
                println!("    Total Fee            : {} sats", info.total_fee);
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

/// Maker-perspective record for one swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerReport {
    /// Unique swap identifier derived from the preimage hash.
    pub swap_id: String,
    /// Final outcome of the swap.
    pub status: SwapStatus,
    /// Bitcoin network (mainnet, testnet, signet, regtest).
    pub network: String,
    /// Total duration of the swap in seconds.
    pub swap_duration_seconds: f64,
    /// Unix timestamp when the swap started.
    pub start_timestamp: u64,
    /// Unix timestamp when the swap completed.
    pub end_timestamp: u64,

    /// Amount received in the incoming contract (satoshis).
    pub incoming_amount: u64,
    /// Amount sent in the outgoing contract (satoshis).
    pub outgoing_amount: u64,
    /// Fee earned (incoming − outgoing, satoshis).
    pub fee_earned: u64,

    /// Transaction ID of the incoming contract.
    pub incoming_contract_txid: String,
    /// Transaction ID of the outgoing contract.
    pub outgoing_contract_txid: String,
    /// Timelock value in blocks for the outgoing contract.
    pub timelock: u32,
}

impl MakerReport {
    /// Build a success report from raw swap state values.
    pub fn success(
        swap_id: String,
        start_time: Instant,
        incoming_amount: u64,
        outgoing_amount: u64,
        incoming_contract_txid: String,
        outgoing_contract_txid: String,
        timelock: u32,
        network: String,
    ) -> Self {
        let swap_duration_seconds = start_time.elapsed().as_secs_f64();
        let end = now_unix_secs();
        Self {
            swap_id,
            status: SwapStatus::Success,
            network,
            swap_duration_seconds,
            start_timestamp: end.saturating_sub(swap_duration_seconds as u64),
            end_timestamp: end,
            incoming_amount,
            outgoing_amount,
            fee_earned: incoming_amount.saturating_sub(outgoing_amount),
            incoming_contract_txid,
            outgoing_contract_txid,
            timelock,
        }
    }

    /// Save to the shared report file under this maker's node name.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let node_name = data_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let report = self.clone();
        write_to_file(data_dir, |f| {
            f.maker.entry(node_name).or_default().push(report);
        })
    }

    /// Print a human-readable summary to stdout.
    pub fn print(&self) {
        println!("\n\x1b[1;36m================================================================================");
        println!("                           MAKER SWAP REPORT");
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
            self.fee_earned
        );
        println!(
            "\x1b[1;37mTimelock          :\x1b[0m {} blocks",
            self.timelock
        );
        println!("\x1b[1;37mNetwork           :\x1b[0m {}", self.network);

        println!("\n\x1b[1;36m--------------------------------------------------------------------------------");
        println!("                            Transaction IDs");
        println!("--------------------------------------------------------------------------------\x1b[0m");

        println!(
            "\x1b[1;37mIncoming Contract :\x1b[0m {}",
            self.incoming_contract_txid
        );
        println!(
            "\x1b[1;37mOutgoing Contract :\x1b[0m {}",
            self.outgoing_contract_txid
        );

        println!("\n\x1b[1;36m================================================================================");
        println!("                                END REPORT");
        println!("================================================================================\x1b[0m\n");
    }
}

/// Recovery event record (written by whichever party performs the recovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    /// Unique swap identifier derived from the preimage hash.
    pub swap_id: String,
    /// Role of the party that performed the recovery.
    pub role: SwapRole,
    /// Maker node name when role is Maker; None for Taker.
    pub node: Option<String>,
    /// Recovery path used: `"hashlock"` or `"timelock"`.
    pub recovery_type: String,
    /// Transaction IDs of the recovery transactions.
    pub recovery_txids: Vec<String>,
    /// Bitcoin network (mainnet, testnet, signet, regtest).
    pub network: String,
    /// Unix timestamp of the recovery event.
    pub timestamp: u64,
}

impl RecoveryReport {
    /// Emit a taker recovery report — prints to console and saves to file.
    pub fn emit_taker(
        data_dir: &Path,
        swap_id: String,
        network: String,
        recovery_type: String,
        recovery_txids: Vec<String>,
    ) {
        let report = Self {
            swap_id,
            role: SwapRole::Taker,
            node: None,
            recovery_type,
            recovery_txids,
            network,
            timestamp: now_unix_secs(),
        };
        report.print();
        if let Err(e) = report.save(data_dir) {
            log::warn!("Failed to save taker recovery report: {:?}", e);
        }
        log::info!(
            "For full details, see: {}",
            coinswap_root(data_dir).join("swap_reports.json").display()
        );
    }

    /// Emit a maker recovery report — prints to console and saves to file.
    pub fn emit_maker(
        data_dir: &Path,
        swap_id: String,
        network: String,
        recovery_type: String,
        recovery_txids: Vec<String>,
    ) {
        let node = data_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        let report = Self {
            swap_id,
            role: SwapRole::Maker,
            node,
            recovery_type,
            recovery_txids,
            network,
            timestamp: now_unix_secs(),
        };
        report.print();
        if let Err(e) = report.save(data_dir) {
            log::warn!("Failed to save maker recovery report: {:?}", e);
        }
    }

    fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let report = self.clone();
        write_to_file(data_dir, |f| f.recovery.push(report))
    }

    fn print(&self) {
        println!("\n\x1b[1;33m================================================================================");
        println!("                           RECOVERY REPORT");
        println!("================================================================================\x1b[0m\n");
        println!("\x1b[1;37mSwap ID       :\x1b[0m {}", self.swap_id);
        println!("\x1b[1;37mRole          :\x1b[0m {}", self.role);
        if let Some(ref node) = self.node {
            println!("\x1b[1;37mNode          :\x1b[0m {}", node);
        }
        println!("\x1b[1;37mRecovery Type :\x1b[0m {}", self.recovery_type);
        println!("\x1b[1;37mNetwork       :\x1b[0m {}", self.network);
        println!("\x1b[1;37mRecovery Txs  :\x1b[0m {:?}", self.recovery_txids);
        println!("\n\x1b[1;33m================================================================================");
        println!("                              END REPORT");
        println!("================================================================================\x1b[0m\n");
    }
}

// ---------------------------------------------------------------------------
// Top-level file structure
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SwapReportFile {
    pub taker: Vec<TakerReport>,
    pub maker: HashMap<String, Vec<MakerReport>>,
    pub recovery: Vec<RecoveryReport>,
    pub deniability_proofs: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// File I/O helpers
// ---------------------------------------------------------------------------

fn coinswap_root(data_dir: &Path) -> PathBuf {
    data_dir.parent().unwrap_or(data_dir).to_path_buf()
}

struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_lock(lock_path: &Path) -> std::io::Result<LockGuard> {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let mut deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => {
                return Ok(LockGuard {
                    path: lock_path.to_owned(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::time::Instant::now() > deadline {
                    let is_stale = std::fs::metadata(lock_path)
                        .and_then(|m| m.modified())
                        .map(|mtime| mtime.elapsed().unwrap_or_default() > TIMEOUT)
                        .unwrap_or(true);

                    if is_stale {
                        std::fs::remove_file(lock_path)?;
                    } else {
                        // Owner is still active — extend and keep waiting.
                        deadline = std::time::Instant::now() + TIMEOUT;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}

fn write_to_file<F>(data_dir: &Path, mutate: F) -> std::io::Result<()>
where
    F: FnOnce(&mut SwapReportFile),
{
    let root = coinswap_root(data_dir);
    std::fs::create_dir_all(&root)?;

    let file_path = root.join("swap_reports.json");
    let lock_path = root.join("swap_reports.json.lock");

    let _lock = acquire_lock(&lock_path)?;

    let mut report_file: SwapReportFile = if file_path.exists() {
        let content = std::fs::read_to_string(&file_path)?;
        serde_json::from_str(&content).map_err(std::io::Error::other)?
    } else {
        SwapReportFile::default()
    };

    mutate(&mut report_file);

    let json = serde_json::to_string_pretty(&report_file).map_err(std::io::Error::other)?;
    let tmp_path = file_path.with_extension("partial");
    {
        use std::io::Write;
        let mut tmp = std::fs::File::create(&tmp_path)?;
        tmp.write_all(json.as_bytes())?;
        tmp.sync_all()?;
    }
    std::fs::rename(&tmp_path, &file_path)?;

    log::info!("Saved swap report to: {}", file_path.display());
    Ok(())
}
