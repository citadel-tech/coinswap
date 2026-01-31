//! FFI-compatible types for the Taker module.
//!
//! This module provides Foreign Function Interface (FFI) compatible data structures
//! for exposing swap functionality and reporting to other languages. These types are
//! designed to be used by the FFI layer(uniffi, napi) and provide simplified,
//! language-agnostic representations of the core swap data structures.
//!
//! - [`MakerFeeInfo`]: Detailed fee breakdown for individual makers in a swap route
//! - [`TakerSwapReport`]: Comprehensive report of a completed swap transaction
//!
//! These structures use primitive types and standard collections (Vec, String) that
//! can be easily marshaled across FFI boundaries, avoiding Rust-specific types that
//! would be difficult to represent in other languages.

use crate::{
    security::{load_sensitive_struct_from_value, KeyMaterial, SerdeJson},
    utill::{get_taker_dir, MIN_FEE_RATE},
    wallet::{AddressType, Destination, RPCConfig, Wallet, WalletBackup, WalletError},
};
use bitcoin::{Address, Amount, OutPoint, Txid};
use bitcoind::bitcoincore_rpc::{json::ListTransactionResult, RpcApi};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

/// Information about individual maker fees in a swap
#[derive(Debug, Serialize)]
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

/// Complete swap report containing all swap information
#[derive(Debug, Serialize)]
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

/// Status of Maker swap report
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
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

impl FromStr for MakerSwapReportStatus {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "success" => Ok(MakerSwapReportStatus::Success),
            "recovery_hashlock" => Ok(MakerSwapReportStatus::RecoveryHashlock),
            "recovery_timelock" => Ok(MakerSwapReportStatus::RecoveryTimelock),
            "failed" => Ok(MakerSwapReportStatus::Failed),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for MakerSwapReportStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                MakerSwapReportStatus::Failed => "failed",
                MakerSwapReportStatus::Success => "success",
                MakerSwapReportStatus::RecoveryHashlock => "recovery_hashlock",
                MakerSwapReportStatus::RecoveryTimelock => "recovery_timelock",
            }
        )
    }
}

/// Represents a swap report for the maker
#[derive(Debug, Clone, Serialize)]
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

        Self {
            swap_id,
            status: MakerSwapReportStatus::from_str(&format!("recovery_{}", recovery_type))
                .unwrap(),
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

    /// Save the report to disk
    pub fn save_to_disk(&self, data_dir: &std::path::Path) -> std::io::Result<()> {
        let reports_dir = data_dir.join("swap_reports");
        std::fs::create_dir_all(&reports_dir)?;

        let timestamp = self.end_timestamp;
        let filename = format!("{}_{}.json", timestamp, self.swap_id);
        let filepath = reports_dir.join(filename);

        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&filepath, json)?;

        log::info!("Saved maker swap report to: {}", filepath.display());
        Ok(())
    }

    /// Print the report to console
    pub fn print(&self) {
        println!("\n\x1b[1;36m================================================================================");
        println!("                            MAKER SWAP REPORT");
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

        if let Some(ref sweep_txid) = self.sweep_txid {
            println!("\x1b[1;37mSweep Tx          :\x1b[0m {}", sweep_txid);
        }

        if let Some(ref recovery_txid) = self.recovery_txid {
            println!("\x1b[1;37mRecovery Tx       :\x1b[0m {}", recovery_txid);
        }

        if let Some(ref error) = self.error_message {
            println!("\n\x1b[1;31mError: {}\x1b[0m", error);
        }

        println!("\n\x1b[1;36m================================================================================");
        println!("                                END REPORT");
        println!("================================================================================\x1b[0m\n");
    }
}

/// Restores a wallet from an encrypted or unencrypted JSON backup file for GUI/FFI applications.
///
/// This is a non-interactive restore method designed for programmatic use via FFI bindings.
/// Unlike `restore_wallet`, this function accepts a path to a JSON backup file and handles both
/// encrypted and unencrypted backups using [`load_sensitive_struct_from_value`].
///
/// # Behavior
///
/// 1. Reads and parses the JSON backup file into a [`WalletBackup`] structure
/// 2. If encrypted, decrypts using the provided password and preserves encryption material
/// 3. Constructs the wallet path: `{data_dir_or_default}/wallets/{wallet_file_name_or_default}`
/// 4. Calls [`Wallet::restore`] to reconstruct the wallet with all UTXOs and metadata
///
/// # Parameters
///
/// - `data_dir`: Target directory, defaults to `~/.coinswap/taker`
/// - `wallet_file_name`: Restored wallet filename, defaults to name from backup if empty
/// - `backup_file_path`: Path to the JSON file containing the wallet backup (encrypted or plain)
/// - `password`: Required if backup is encrypted, ignored otherwise
pub fn restore_wallet_gui_app(
    data_dir: Option<PathBuf>,
    wallet_file_name: Option<String>,
    rpc_config: RPCConfig,
    backup_file_path: PathBuf,
    password: Option<String>,
) {
    let (backup, encryption_material) = load_sensitive_struct_from_value::<WalletBackup, SerdeJson>(
        &backup_file_path,
        password.unwrap_or_default(),
    );
    let restored_wallet_filename = wallet_file_name.unwrap_or("".to_string());

    let restored_wallet_path = data_dir
        .clone()
        .unwrap_or(get_taker_dir())
        .join("wallets")
        .join(restored_wallet_filename);

    if let Err(e) = Wallet::restore(
        &backup,
        &restored_wallet_path,
        &rpc_config,
        encryption_material,
    ) {
        log::error!("Wallet restore failed: {e:?}");
    } else {
        println!("Wallet restore succeeded!");
    }
}

impl Wallet {
    /// Creates a wallet backup for GUI/FFI applications with optional encryption.
    ///
    /// This is a ffi-only wrapper around [`Wallet::backup`] that handles encryption
    /// material generation internally based on whether a password is provided.
    ///
    /// # Behavior
    ///
    /// - If `password` is `Some(pwd)` and not empty: Creates encrypted backup using the password
    /// - If `password` is `None` or empty string: Creates unencrypted backup (logs warning)
    /// - The backup is written as a `.json` file at the specified path
    ///
    /// # Parameters
    ///
    /// - `destination_path`: Destination file path for the backup (`.json`)
    /// - `password`: Optional password for encryption. Use `None` or empty string for plaintext backup
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Encrypted backup
    /// wallet.backup_gui_app("/path/to/backup".to_string(), Some("my_password".to_string()))?;
    ///
    /// // Unencrypted backup
    /// wallet.backup_gui_app("/path/to/backup".to_string(), None)?;
    pub fn backup_wallet_gui_app(
        &self,
        destination_path: String,
        password: Option<String>,
    ) -> Result<(), WalletError> {
        let km = KeyMaterial::new_from_password(password);
        let backup_path = Path::new(&destination_path);
        self.backup(backup_path, km)?;

        Ok(())
    }

    /// Checks whether wallet is encrypted or not.
    pub fn is_wallet_encrypted(wallet_path: &Path) -> Result<bool, WalletError> {
        if !wallet_path.exists() {
            return Ok(false); // No wallet = not encrypted
        }

        let content = std::fs::read(wallet_path).map_err(WalletError::IO)?;

        // Try to deserialize as EncryptedData using CBOR
        // If it succeeds, the wallet is encrypted
        // If it fails, the wallet is plaintext
        match serde_cbor::from_slice::<crate::security::EncryptedData>(&content) {
            Ok(_) => Ok(true),   // Successfully parsed as EncryptedData = encrypted
            Err(_) => Ok(false), // Failed to parse as EncryptedData = plaintext
        }
    }

    /// Returns a list of recent Incoming Transactions (bydefault last 10)
    pub fn get_transactions(
        &self,
        count: Option<usize>,
        skip: Option<usize>,
    ) -> Result<Vec<ListTransactionResult>, WalletError> {
        Ok(self.rpc.list_transactions(None, count, skip, Some(true))?)
    }

    /// Sends specified Amount of Satoshis to an External Address
    pub fn send_to_address(
        &mut self,
        amount: u64,
        address: String,
        fee_rate: Option<f64>,
        manually_selected_outpoints: Option<Vec<OutPoint>>,
    ) -> Result<Txid, WalletError> {
        let amount = Amount::from_sat(amount);

        let coins_to_spend = self.coin_select(
            amount,
            fee_rate.unwrap_or(MIN_FEE_RATE),
            manually_selected_outpoints,
        )?;

        let addr = Address::from_str(&address)
            .map_err(|e| WalletError::General(format!("Invalid address: {}", e)))?
            .assume_checked();
        let outputs = vec![(addr, amount)];
        let destination = Destination::Multi {
            outputs,
            op_return_data: None,
            change_address_type: AddressType::P2WPKH,
        };

        let tx = self.spend_from_wallet(
            fee_rate.unwrap_or(MIN_FEE_RATE),
            destination,
            &coins_to_spend,
        )?;

        let txid = self.send_tx(&tx).unwrap();
        self.sync_and_save()?;
        println!("Send to Address TxId: {txid}");

        Ok(txid)
    }
}
