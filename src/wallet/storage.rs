//! The Wallet Storage Interface.
//!
//! Wallet data is currently written in unencrypted CBOR files which are not directly human readable.

use super::{api::KeyMaterial, error::WalletError, fidelity::FidelityBond};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm,
    Key, // Or `Aes128Gcm`
};
use bitcoin::{bip32::Xpriv, Network, OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, read, File},
    io::BufWriter,
    path::Path,
};

use super::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin};
use crate::{utill, wallet::UTXOSpendInfo};
use bitcoind::bitcoincore_rpc::bitcoincore_rpc_json::ListUnspentResultEntry;

/// Wrapper struct for storing an encrypted wallet on disk.
///
/// The standard `WalletStore` is first serialized to CBOR, then encrypted using
/// [AES-GCM](https://en.wikipedia.org/wiki/Galois/Counter_Mode).
///
/// The resulting ciphertext is stored in `encrypted_wallet_store`, and the AES-GCM
/// nonce used for encryption is stored in `nonce`.
///
/// Note: The term “IV” (Initialization Vector) used in AES-GCM — including in the linked Wikipedia page —
/// refers to the same value as the nonce. They are conceptually the same in this context.
///
/// This wrapper itself is then serialized to CBOR and written to disk.
#[derive(Serialize, Deserialize, Debug)]
struct EncryptedWalletStore {
    /// Nonce used for AES-GCM encryption (must match during decryption).
    nonce: Vec<u8>,
    /// AES-GCM-encrypted CBOR-serialized `WalletStore` data.
    encrypted_wallet_store: Vec<u8>,
}

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

                // Serialize wallet data to bytes.
                let packed_store = serde_cbor::ser::to_vec(&self)?;

                // Extract nonce and key for AES-GCM.
                let material_nonce = material.nonce.as_ref().unwrap();
                let nonce = aes_gcm::Nonce::from_slice(material_nonce);
                let key = Key::<Aes256Gcm>::from_slice(&material.key);

                // Create AES-GCM cipher instance.
                let cipher = Aes256Gcm::new(key);

                // Encrypt the serialized wallet bytes.
                let ciphertext = cipher.encrypt(nonce, packed_store.as_ref()).unwrap();

                // Package encrypted data with nonce for storage.
                let encrypted = EncryptedWalletStore {
                    nonce: material_nonce.clone(),
                    encrypted_wallet_store: ciphertext,
                };

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
    pub(crate) fn read_from_disk(
        path: &Path,
        store_enc_material: &Option<KeyMaterial>,
    ) -> Result<(Self, Option<Vec<u8>>), WalletError> {
        let reader = read(path)?;

        match store_enc_material {
            Some(material) => {
                log::info!("Reading encrypted wallet");

                // Deserialize the outer EncryptedWalletStore wrapper.
                let encrypted_wallet: EncryptedWalletStore = utill::deserialize_from_cbor::<
                    EncryptedWalletStore,
                    WalletError,
                >(reader.clone())?;

                let nonce_vec = encrypted_wallet.nonce.clone();

                // Reconstruct AES-GCM cipher from the provided key and stored nonce.
                let key = Key::<Aes256Gcm>::from_slice(&material.key);
                let cipher = Aes256Gcm::new(key);
                let nonce = aes_gcm::Nonce::from_slice(&nonce_vec);

                // Decrypt the inner WalletStore CBOR bytes.
                let packed_wallet_store = cipher
                    .decrypt(nonce, encrypted_wallet.encrypted_wallet_store.as_ref())
                    .expect("Error decrypting wallet, wrong passphrase?");

                // Deserialize the decrypted WalletStore and return it with the nonce.
                Ok((
                    utill::deserialize_from_cbor::<Self, WalletError>(packed_wallet_store)?,
                    Some(nonce_vec.clone()),
                ))
            }
            None => {
                // If no encryption key is provided, deserialize the WalletStore directly.
                Ok((
                    utill::deserialize_from_cbor::<Self, WalletError>(reader)?,
                    None,
                ))
            }
        }
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

        original_wallet_store
            .write_to_disk(&file_path, &None)
            .unwrap();

        let (read_wallet, _nonce) = WalletStore::read_from_disk(&file_path, &None).unwrap();
        assert_eq!(original_wallet_store, read_wallet);
    }
}
