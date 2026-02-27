//! The Wallet Storage Interface.
//!
//! Wallet data is currently written in unencrypted CBOR files which are not directly human readable.

use crate::{
    security::{encrypt_struct, load_sensitive_struct, KeyMaterial, SerdeCbor},
    wallet::UTXOSpendInfo,
};

use super::{error::WalletError, fidelity::FidelityBond};

use bitcoin::{bip32::Xpriv, Network, OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    path::Path,
};

use super::{
    swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
    swapcoin2::{IncomingSwapCoinV2, OutgoingSwapCoinV2},
};

use bitcoind::bitcoincore_rpc::bitcoincore_rpc_json::ListUnspentResultEntry;

/// Address type supported by the wallet for HD address generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AddressType {
    /// BIP-84 Native SegWit (P2WPKH)
    #[default]
    P2WPKH,
    /// BIP-86 Taproot key-path only (P2TR)
    P2TR,
}

/// Represents the internal data store for a Bitcoin wallet.
#[derive(minicbor::Encode, minicbor::Decode, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WalletStore {
    /// The file name associated with the wallet store.
    #[n(0)]
    pub(crate) file_name: String,
    /// Network the wallet operates on.
    #[cbor(
        n(1),
        encode_with = "crate::wallet::backup::encode_network",
        decode_with = "crate::wallet::backup::decode_network"
    )]
    pub(crate) network: Network,
    /// The master key for the wallet.
    #[cbor(
        n(2),
        encode_with = "crate::wallet::backup::encode_xpriv",
        decode_with = "crate::wallet::backup::decode_xpriv"
    )]
    pub(super) master_key: Xpriv,
    /// The external index for the wallet.
    #[n(3)]
    pub(super) external_index: u32,
    /// The maximum size for an offer in the wallet.
    #[n(4)]
    pub(crate) offer_maxsize: u64,
    /// Map of multisig redeemscript to incoming swapcoins.
    #[cbor(
        n(5),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
    pub(super) incoming_swapcoins: HashMap<ScriptBuf, IncomingSwapCoin>,
    /// Map of multisig redeemscript to outgoing swapcoins.
    #[cbor(
        n(6),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
    pub(super) outgoing_swapcoins: HashMap<ScriptBuf, OutgoingSwapCoin>,
    /// Map of taproot contract txid to incoming taproot swapcoins.
    #[cbor(
        n(7),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
    pub(super) incoming_swapcoins_v2: HashMap<bitcoin::Txid, IncomingSwapCoinV2>,
    /// Map of taproot contract txid to outgoing taproot swapcoins.
    #[cbor(
        n(8),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
    pub(super) outgoing_swapcoins_v2: HashMap<bitcoin::Txid, OutgoingSwapCoinV2>,
    /// Map of prevout to contract redeemscript.
    #[cbor(
        n(9),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
    pub(super) prevout_to_contract_map: HashMap<OutPoint, ScriptBuf>,
    /// Set of swept incoming swap coin scriptpubkeys to prevent mixing with regular UTXOs
    #[cbor(
        n(10),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
    pub(crate) swept_incoming_swapcoins: HashSet<ScriptBuf>,
    /// Map for all the fidelity bond information.
    #[cbor(
        n(11),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
    pub(crate) fidelity_bond: HashMap<u32, FidelityBond>,
    #[n(12)]
    pub(super) last_synced_height: Option<u64>,

    #[n(13)]
    pub(super) wallet_birthday: Option<u64>,

    /// Maps transaction outpoints to their associated UTXO and spend information.
    #[serde(default)] // Ensures deserialization works if `utxo_cache` is missing
    #[cbor(
        n(14),
        encode_with = "encode_json_bytes",
        decode_with = "decode_json_bytes"
    )]
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
            incoming_swapcoins_v2: HashMap::new(),
            outgoing_swapcoins_v2: HashMap::new(),
            prevout_to_contract_map: HashMap::new(),
            swept_incoming_swapcoins: HashSet::new(),
            fidelity_bond: HashMap::new(),
            last_synced_height: None,
            wallet_birthday,
            utxo_cache: HashMap::new(),
        };

        std::fs::create_dir_all(path.parent().expect("Path should NOT be root!"))?;
        // write: overwrites existing file.
        // create: creates new file if doesn't exist.
        File::create(path)?;

        store.write_to_disk(path, store_enc_material)?;

        Ok(store)
    }

    /// Load existing file, updates it, writes it back (errors if path doesn't exist).
    pub(crate) fn write_to_disk(
        &self,
        path: &Path,
        store_enc_material: &Option<KeyMaterial>,
    ) -> Result<(), WalletError> {
        match store_enc_material {
            Some(material) => {
                // Encryption branch: encrypt the serialized wallet before writing.

                let encrypted = encrypt_struct(self, material).unwrap();

                // Write encrypted wallet data to disk.
                fs::write(
                    path,
                    minicbor::to_vec(&encrypted)
                        .map_err(|e| WalletError::General(e.to_string()))?,
                )?;
            }
            None => {
                // No encryption: serialize and write the wallet directly.
                fs::write(
                    path,
                    minicbor::to_vec(self).map_err(|e| WalletError::General(e.to_string()))?,
                )?;
            }
        }
        Ok(())
    }

    /// Reads from a path (errors if path doesn't exist).
    /// If `store_enc_material` is provided, attempts to decrypt the file using the
    /// provided key. Returns the deserialized `WalletStore` and the nonce.
    pub(crate) fn read_from_disk(
        backup_file_path: &Path,
        password: String,
    ) -> Result<(Self, Option<KeyMaterial>), WalletError> {
        let (wallet_store, store_enc_material) =
            load_sensitive_struct::<Self, SerdeCbor>(backup_file_path, Some(password));

        Ok((wallet_store, store_enc_material))
    }
}

// Inline minicbor helpers
#[allow(dead_code)]
fn encode_json_bytes<T: serde::Serialize, W: minicbor::encode::Write, C>(
    x: &T,
    e: &mut minicbor::Encoder<W>,
    _ctx: &mut C,
) -> Result<(), minicbor::encode::Error<W::Error>> {
    e.bytes(&serde_json::to_vec(x).map_err(|_| minicbor::encode::Error::message("json error"))?)?;
    Ok(())
}
#[allow(dead_code)]
fn decode_json_bytes<T: serde::de::DeserializeOwned, C>(
    d: &mut minicbor::Decoder<'_>,
    _ctx: &mut C,
) -> Result<T, minicbor::decode::Error> {
    serde_json::from_slice(d.bytes()?)
        .map_err(|_| minicbor::decode::Error::message("json decode error"))
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

        let (read_wallet, _nonce) = WalletStore::read_from_disk(&file_path, String::new()).unwrap();
        assert_eq!(original_wallet_store, read_wallet);
    }
}
