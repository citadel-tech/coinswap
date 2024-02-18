//! The Wallet Storage Interface.
//!
//! Wallet data is currently written in unencrypted CBOR files which are not directly human readable.

use std::{collections::HashMap, convert::TryFrom, path::PathBuf};

use bip39::Mnemonic;
use bitcoin::{bip32::ExtendedPrivKey, Network, OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter},
};

use super::{error::WalletError, SwapCoin};

use super::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin};

use super::fidelity::generate_fidelity_scripts;

const WALLET_FILE_VERSION: u32 = 0;

#[derive(Serialize, Deserialize)]
struct FileData {
    file_name: String,
    version: u32,
    network: Network,
    seedphrase: String,
    passphrase: String,
    external_index: u32,
    incoming_swapcoins: Vec<IncomingSwapCoin>,
    outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    prevout_to_contract_map: HashMap<OutPoint, ScriptBuf>,
}

/// Represents the internal data store for a Bitcoin wallet.
#[derive(Debug, PartialEq)]
pub struct WalletStore {
    /// The file name associated with the wallet store.
    pub(crate) file_name: String,
    /// Network the wallet operates on.
    pub(crate) network: Network,
    /// The master key for the wallet.
    pub(super) master_key: ExtendedPrivKey,
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
    /// Map of fidelity bond scripts to index.
    pub(super) fidelity_scripts: HashMap<ScriptBuf, u32>,
    //TODO: Add last synced height and Wallet birthday.
}

impl TryFrom<FileData> for WalletStore {
    type Error = WalletError;
    fn try_from(file_data: FileData) -> Result<Self, WalletError> {
        let mnemonic = Mnemonic::parse(&file_data.seedphrase)?;
        let seed = mnemonic.to_seed(file_data.passphrase);
        let xprv = ExtendedPrivKey::new_master(file_data.network, &seed)?;

        let incoming_swapcoins = file_data
            .incoming_swapcoins
            .iter()
            .map(|sc| (sc.get_multisig_redeemscript(), sc.clone()))
            .collect::<HashMap<_, _>>();

        let outgoing_swapcoins = file_data
            .outgoing_swapcoins
            .iter()
            .map(|sc| (sc.get_multisig_redeemscript(), sc.clone()))
            .collect::<HashMap<_, _>>();

        let timelocked_script_index_map = generate_fidelity_scripts(&xprv);

        Ok(Self {
            file_name: file_data.file_name,
            network: file_data.network,
            master_key: xprv,
            external_index: file_data.external_index,
            incoming_swapcoins,
            outgoing_swapcoins,
            prevout_to_contract_map: file_data.prevout_to_contract_map,
            fidelity_scripts: timelocked_script_index_map,
            offer_maxsize: 0,
        })
    }
}

impl WalletStore {
    /// Initialize a store at a path (if path already exists, it will overwrite it).
    pub fn init(
        file_name: String,
        path: &PathBuf,
        network: Network,
        seedphrase: String,
        passphrase: String,
    ) -> Result<Self, WalletError> {
        FileData::init_new_file(path, file_name, network, seedphrase, passphrase)?;
        let store = WalletStore::read_from_disk(path)?;
        Ok(store)
    }

    /// Load existing file, updates it, writes it back (errors if path doesn't exist).
    pub fn write_to_disk(&self, path: &PathBuf) -> Result<(), WalletError> {
        let mut file_data = FileData::load_from_file(path)?;
        file_data.incoming_swapcoins = self
            .incoming_swapcoins
            .iter()
            .map(|is| is.1)
            .cloned()
            .collect();
        file_data.outgoing_swapcoins = self
            .outgoing_swapcoins
            .iter()
            .map(|os| os.1)
            .cloned()
            .collect();
        file_data.save_to_file(path)
    }

    /// Reads from a path (errors if path doesn't exist).
    pub fn read_from_disk(path: &PathBuf) -> Result<Self, WalletError> {
        let file_data = FileData::load_from_file(path)?;
        Self::try_from(file_data)
    }
}

impl FileData {
    /// Overwrites existing file or create a new one.
    fn init_new_file(
        path: &PathBuf,
        file_name: String,
        network: Network,
        seedphrase: String,
        passphrase: String,
    ) -> Result<(), WalletError> {
        let file_data = Self {
            file_name,
            version: WALLET_FILE_VERSION,
            network,
            seedphrase,
            passphrase,
            external_index: 0,
            incoming_swapcoins: Vec::new(),
            outgoing_swapcoins: Vec::new(),
            prevout_to_contract_map: HashMap::new(),
        };
        file_data.save_to_file(path)
    }

    /// File path should exist.
    fn load_from_file(path: &PathBuf) -> Result<Self, WalletError> {
        let wallet_file = File::open(path)?;
        let reader = BufReader::new(wallet_file);
        Ok(serde_cbor::from_reader(reader)?)
    }

    // Overwrite existing file or create a new one.
    fn save_to_file(&self, path: &PathBuf) -> Result<(), WalletError> {
        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        let writer = BufWriter::new(file);
        serde_cbor::to_writer(writer, &self)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bip39::Mnemonic;
    use bitcoin::Network;
    use bitcoind::tempfile::tempdir;

    #[test]
    fn test_write_and_read_wallet_to_disk() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("test_wallet.cbor");
        let mnemonic = Mnemonic::generate(12).unwrap().to_string();

        let original_wallet_store = WalletStore::init(
            "test_wallet".to_string(),
            &file_path,
            Network::Bitcoin,
            mnemonic,
            "passphrase".to_string(),
        )
        .unwrap();

        original_wallet_store.write_to_disk(&file_path).unwrap();

        let read_wallet = WalletStore::read_from_disk(&file_path).unwrap();
        assert_eq!(original_wallet_store, read_wallet);
    }
}
