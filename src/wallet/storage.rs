//! The Wallet Storage Interface.
//!
//! Wallet data is currently written in unencrypted CBOR files which are not directly human readable.

use bitcoin::{bip32::Xpriv, Network, OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, read, File},
    io::BufWriter,
    path::Path,
};

use super::{error::WalletError, fidelity::FidelityBond};

use super::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin};
use crate::wallet::UTXOSpendInfo;
use bitcoind::bitcoincore_rpc::bitcoincore_rpc_json::ListUnspentResultEntry;

/// Represents the internal data store for a Bitcoin wallet.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WalletStore {
    /// The file name associated with the wallet store.
    pub(crate) file_name: String,
    /// Network the wallet operates on.
    pub(crate) network: Network,
    /// The master key for the wallet.
    pub(super) master_key: Xpriv,
    /// The external index for the wallet.
    pub(super) external_index: u32,
    /// The maximum size for an offer in the wallet.
    pub(crate) offer_maxsize: u64,
    /// Map of multisig redeemscript to incoming swapcoins.
    pub(super) incoming_swapcoins: HashMap<ScriptBuf, IncomingSwapCoin>,
    /// Map of multisig redeemscript to outgoing swapcoins.
    pub(super) outgoing_swapcoins: HashMap<ScriptBuf, OutgoingSwapCoin>,
    /// Map of prevout to contract redeemscript.
    pub(super) prevout_to_contract_map: HashMap<OutPoint, ScriptBuf>,
    /// Map for all the fidelity bond information. (index, (Bond, redeemed)).
    pub(crate) fidelity_bond: HashMap<u32, (FidelityBond, bool)>,
    pub(super) last_synced_height: Option<u64>,

    pub(super) wallet_birthday: Option<u64>,

    /// Maps transaction outpoints to their associated UTXO and spend information.
    #[serde(default)] // Ensures deserialization works if `utxo_cache` is missing
    pub(super) utxo_cache: HashMap<OutPoint, (ListUnspentResultEntry, UTXOSpendInfo)>,
}

impl WalletStore {
    /// Initialize a store at a path (if path already exists, it will overwrite it).
    pub(crate) fn init(
        file_name: String,
        path: &Path,
        network: Network,
        master_key: Xpriv,
        wallet_birthday: Option<u64>,
    ) -> Result<Self, WalletError> {
        let store = Self {
            file_name,
            network,
            master_key,
            external_index: 0,
            offer_maxsize: 0,
            incoming_swapcoins: HashMap::new(),
            outgoing_swapcoins: HashMap::new(),
            prevout_to_contract_map: HashMap::new(),
            fidelity_bond: HashMap::new(),
            last_synced_height: None,
            wallet_birthday,
            utxo_cache: HashMap::new(),
        };

        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        // write: overwrites existing file.
        // create: creates new file if doesn't exist.
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_cbor::to_writer(writer, &store)?;

        Ok(store)
    }

    /// Load existing file, updates it, writes it back (errors if path doesn't exist).
    pub(crate) fn write_to_disk(&self, path: &Path) -> Result<(), WalletError> {
        let wallet_file = fs::OpenOptions::new().write(true).open(path)?;
        let writer = BufWriter::new(wallet_file);
        Ok(serde_cbor::to_writer(writer, &self)?)
    }

    /// Reads from a path (errors if path doesn't exist).
    pub(crate) fn read_from_disk(path: &Path) -> Result<Self, WalletError> {
        //let wallet_file = File::open(path)?;
        let mut reader = read(path)?;
        let store = match serde_cbor::from_slice::<Self>(&reader) {
            Ok(store) => store,
            Err(e) => {
                let err_string = format!("{e:?}");
                if err_string.contains("code: TrailingData") {
                    log::info!("Wallet file has trailing data, trying to restore");
                    loop {
                        // pop the last byte and try again.
                        reader.pop();
                        match serde_cbor::from_slice::<Self>(&reader) {
                            Ok(store) => break store,
                            Err(_) => continue,
                        }
                    }
                } else {
                    return Err(e.into());
                }
            }
        };
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bip39::rand::{thread_rng, Rng};
    use bitcoind::tempfile::tempdir;

    #[test]
    fn test_write_and_read_wallet_to_disk() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("test_wallet.cbor");

        let master_key = {
            let seed: [u8; 16] = thread_rng().gen();
            Xpriv::new_master(Network::Bitcoin, &seed).unwrap()
        };

        let original_wallet_store = WalletStore::init(
            "test_wallet".to_string(),
            &file_path,
            Network::Bitcoin,
            master_key,
            None,
        )
        .unwrap();

        original_wallet_store.write_to_disk(&file_path).unwrap();

        let read_wallet = WalletStore::read_from_disk(&file_path).unwrap();
        assert_eq!(original_wallet_store, read_wallet);
    }
}
