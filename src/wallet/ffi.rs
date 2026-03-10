//! FFI-compatible types for the Taker module.
//!
//! This module provides Foreign Function Interface (FFI) compatible data structures
//! for exposing swap functionality and reporting to other languages.

use crate::{
    security::{load_sensitive_struct, KeyMaterial, SerdeJson},
    utill::{get_taker_dir, MIN_FEE_RATE},
    wallet::{AddressType, Destination, RPCConfig, Wallet, WalletBackup, WalletError},
};
use bitcoin::{Address, Amount, OutPoint, Txid};
use bitcoind::bitcoincore_rpc::{json::ListTransactionResult, RpcApi};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

pub use super::report::{MakerFeeInfo, SwapReport, SwapRole, SwapStatus};

/// Restores a wallet from an encrypted or unencrypted JSON backup file for GUI/FFI applications.
///
/// This is a non-interactive restore method designed for programmatic use via FFI bindings.
/// Unlike `restore_wallet`, this function accepts a path to a JSON backup file and handles both
/// encrypted and unencrypted backups using [`load_sensitive_struct`].
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
    let (backup, encryption_material) = load_sensitive_struct::<WalletBackup, SerdeJson>(
        &backup_file_path,
        Some(password.unwrap_or_default()),
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
