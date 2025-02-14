//! Send regular Bitcoin payments.
//!
//! This module provides functionality for managing wallet transactions, including the creation of
//! direct sends. It leverages Bitcoin Core's RPC for wallet synchronization and implements various
//! parsing mechanisms for transaction inputs and outputs.

use bitcoin::{Address, Amount, Transaction};
use bitcoind::bitcoincore_rpc::json::ListUnspentResultEntry;

use crate::wallet::api::UTXOSpendInfo;

use super::{error::WalletError, Wallet};

/// Represents different destination options for a transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    /// Sweep
    Sweep(Address),
    /// Multi
    Multi(Vec<(Address, Amount)>),
}

impl Wallet {
    /// API to perform spending from wallet UTXOs, including descriptor coins and swap coins.
    ///
    /// The caller needs to specify a list of UTXO data and their corresponding `spend_info`.
    /// These can be extracted using various `list_utxo_*` Wallet APIs.
    ///
    /// The caller must also specify a total fee and a destination address.
    /// Using [Destination::Wallet] will create a transaction to an internal wallet change address.
    ///
    /// ### Note
    /// This function should not be used to spend Fidelity Bonds or contract UTXOs
    /// (e.g., Hashlock or Timelock contracts). These UTXOs will be automatically skipped
    /// and not considered when creating the transaction.
    ///
    /// ### Behavior
    /// - If [Destination::Sweep] is used, the function creates a transaction for the maximum possible
    ///   value to the specified Address.
    /// - If [Destination::Multi] is used, a custom value is sent, and any remaining funds
    ///    are held in a change address, if applicable.
    pub fn spend_from_wallet(
        &mut self,
        feerate: Option<f64>,
        destination: Destination,
        coins_to_spend: &[(ListUnspentResultEntry, UTXOSpendInfo)],
    ) -> Result<Transaction, WalletError> {
        log::info!("Creating Direct-Spend from Wallet.");

        let mut coins = Vec::<&(ListUnspentResultEntry, UTXOSpendInfo)>::new();

        for coin in coins_to_spend {
            // filter all contract and fidelity utxos.
            if let UTXOSpendInfo::FidelityBondCoin { .. }
            | UTXOSpendInfo::HashlockContract { .. }
            | UTXOSpendInfo::TimelockContract { .. } = coin.1
            {
                log::warn!("Skipping Fidelity Bond or Contract UTXO.");
                continue;
            } else {
                coins.push(&coin);
            }
        }

        let tx = self.spend_coins(&coins, destination, feerate)?;

        Ok(tx)
    }
}
