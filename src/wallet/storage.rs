//! The Wallet Storage Interface.
//!
//! Wallet data is currently written in unencrypted CBOR files which are not directly human readable.

use super::{error::WalletError, fidelity::FidelityBond};

use bitcoin::{bip32::Xpriv, Network, OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::BufWriter,
    path::Path,
};

use super::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin};
use crate::wallet::{
    security::{encrypt_struct, load_sensitive_struct_interactive, KeyMaterial},
    SerdeCbor, UTXOSpendInfo,
};
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
    /// Map of swept incoming swap coins to prevent mixing with regular UTXOs
    /// Key: ScriptPubKey of swept UTXO, Value: Original multisig redeemscript
    pub(super) swept_incoming_swapcoins: HashMap<ScriptBuf, ScriptBuf>,
    /// Map for all the fidelity bond information.
    pub(crate) fidelity_bond: HashMap<u32, FidelityBond>,
    pub(super) last_synced_height: Option<u64>,

    pub(super) wallet_birthday: Option<u64>,

    /// Maps transaction outpoints to their associated UTXO and spend information.
    #[serde(default)] // Ensures deserialization works if `utxo_cache` is missing
    pub(super) utxo_cache: HashMap<OutPoint, (ListUnspentResultEntry, UTXOSpendInfo)>,
}

impl WalletStore {
    /// Initialize a store at a path (if path already exists, it will overwrite it).
    // TODO: FIX ENCRYPTION EVEN WHEN INIT
    pub(crate) fn init(
        file_name: String,
        path: &Path,
        network: Network,
        master_key: Xpriv,
        wallet_birthday: Option<u64>,
        store_enc_material: &Option<KeyMaterial>,
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
            swept_incoming_swapcoins: HashMap::new(),
            fidelity_bond: HashMap::new(),
            last_synced_height: None,
            wallet_birthday,
            utxo_cache: HashMap::new(),
        };

        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        // write: overwrites existing file.
        // create: creates new file if doesn't exist.
        File::create(path)?;

        store.write_to_disk(path, store_enc_material).unwrap();

        Ok(store)
    }

    /// Load existing file, updates it, writes it back (errors if path doesn't exist).
    pub(crate) fn write_to_disk(
        &self,
        path: &Path,
        store_enc_material: &Option<KeyMaterial>,
    ) -> Result<(), WalletError> {
        let wallet_file = fs::OpenOptions::new().write(true).open(path)?;
        let writer = BufWriter::new(wallet_file);

        match store_enc_material {
            Some(material) => {
                // Encryption branch: encrypt the serialized wallet before writing.

                let encrypted = encrypt_struct(self, material).unwrap();

                // Write encrypted wallet data to disk.
                serde_cbor::to_writer(writer, &encrypted)?;
            }
            None => {
                // No encryption: serialize and write the wallet directly.
                serde_cbor::to_writer(writer, &self)?;
            }
        }
        Ok(())
    }

    /// Reads from a path (errors if path doesn't exist).
    /// If `store_enc_material` is provided, attempts to decrypt the file using the
    /// provided key. Returns the deserialized `WalletStore` and the nonce.
    pub(crate) fn read_from_disk(path: &Path) -> Result<(Self, Option<KeyMaterial>), WalletError> {
        let (wallet_store, store_enc_material) =
            load_sensitive_struct_interactive::<Self, WalletError, SerdeCbor>(path)?;

        Ok((wallet_store, store_enc_material))
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
            &None,
        )
        .unwrap();

        original_wallet_store
            .write_to_disk(&file_path, &None)
            .unwrap();

        let (read_wallet, _nonce) = WalletStore::read_from_disk(&file_path).unwrap();
        assert_eq!(original_wallet_store, read_wallet);
    }
}
