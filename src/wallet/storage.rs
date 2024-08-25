//! The Wallet Storage Interface.
//!
//! Wallet data is currently written in unencrypted CBOR files which are not directly human readable.

use bitcoin::{
    bip32::{Fingerprint, Xpriv},
    Network, OutPoint, ScriptBuf,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use super::{error::WalletError, fidelity::FidelityBond};

use super::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin};

/// Represents the internal data store for a Bitcoin wallet.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct WalletStore {
    /// The file name associated with the wallet store.
    pub(crate) wallet_id: Fingerprint,
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
    /// Map for all the fidelity bond information. (index, (Bond, script_pubkey, is_spent)).
    pub(super) fidelity_bond: HashMap<u32, (FidelityBond, ScriptBuf, bool)>,
    //TODO: Add last synced height and Wallet birthday.
    pub(super) last_synced_height: Option<u64>,

    pub(super) wallet_birthday: Option<u64>,
}

impl WalletStore {
    /// Initialize a store at a path (if path already exists, it will overwrite it).
    pub fn init(
        wallet_id: Fingerprint,
        data_directory: &Path,
        network: Network,
        master_key: Xpriv,
        wallet_birthday: Option<u64>,
    ) -> Result<Self, WalletError> {
        let store = Self {
            wallet_id,
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
        };
        let wallet_file_path = data_directory.join(format!("{}", wallet_id));

        std::fs::create_dir_all(data_directory)?;

        // write: overwrites existing file.
        // create: creates new file if doesn't exist.
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(wallet_file_path)?;
        let writer = BufWriter::new(file);
        serde_cbor::to_writer(writer, &store)?;

        Ok(store)
    }

    /// Load existing file, updates it, writes it back (errors if path doesn't exist).
    pub fn write_to_disk(&self, path: &PathBuf) -> Result<(), WalletError> {
        let wallet_file = OpenOptions::new().write(true).open(path)?;
        let writer = BufWriter::new(wallet_file);
        Ok(serde_cbor::to_writer(writer, &self)?)
    }

    /// Reads from a path (errors if path doesn't exist).
    pub fn read_from_disk(path: &PathBuf) -> Result<Self, WalletError> {
        let wallet_file = OpenOptions::new().read(true).open(path)?;
        let reader = BufReader::new(wallet_file);
        let store: Self = serde_cbor::from_reader(reader)?;
        Ok(store)
    }
}

// This is fixed but not committed as this part keeps changing.

#[cfg(test)]
mod tests {
    use super::*;
    use bip39::Mnemonic;
    use bitcoind::tempfile::tempdir;

    #[test]
    fn test_write_and_read_wallet_to_disk() {
        let temp_dir = tempdir().unwrap();
        let data_directory = temp_dir.path().to_path_buf();
        let mnemonic = Mnemonic::generate(12).unwrap().to_string();
        let fingerprint = Fingerprint::default();
        let original_wallet_store = WalletStore::init(
            fingerprint,
            &data_directory,
            Network::Bitcoin,
            Xpriv::new_master(Network::Bitcoin, mnemonic.as_bytes()).unwrap(),
            None,
        )
        .unwrap();

        let wallet_file_path = data_directory.join(fingerprint.to_string());
        original_wallet_store
            .write_to_disk(&wallet_file_path)
            .unwrap();

        let read_wallet = WalletStore::read_from_disk(&wallet_file_path).unwrap();
        assert_eq!(original_wallet_store, read_wallet);
    }
}
