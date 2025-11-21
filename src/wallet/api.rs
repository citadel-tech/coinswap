//! The Wallet API.
//!
//! Currently, wallet synchronization is exclusively performed through RPC for makers.
//! In the future, takers might adopt alternative synchronization methods, such as lightweight wallet solutions.

use std::{
    convert::TryFrom, fmt::Display, path::PathBuf, str::FromStr, thread, thread::sleep,
    time::Duration,
};

use std::collections::HashMap;

use crate::{
    security::KeyMaterial,
    taker::SwapParams,
    utill::{ContractMetadata, BLOCK_DELAY, HEART_BEAT_INTERVAL},
    wallet::Destination,
    watch_tower::service::WatchService,
};

use bip39::Mnemonic;
use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Xpriv, Xpub},
    hashes::hash160::Hash as Hash160,
    secp256k1,
    secp256k1::{Secp256k1, SecretKey},
    sighash::{EcdsaSighashType, SighashCache},
    Address, Amount, OutPoint, PublicKey, Script, ScriptBuf, Transaction, Txid, Weight,
};
use bitcoind::bitcoincore_rpc::{
    bitcoincore_rpc_json::ListUnspentResultEntry, json::ListTransactionResult, Client, RpcApi,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{
    protocol::contract,
    utill::{
        compute_checksum, generate_keypair, get_hd_path_from_descriptor,
        redeemscript_to_scriptpubkey, MIN_FEE_RATE,
    },
    wallet::split_utxos::MAX_SPLITS,
};

use rust_coinselect::{
    selectcoin::select_coin,
    types::{CoinSelectionOpt, ExcessStrategy, OutputGroup},
    utils::calculate_fee,
};

use super::{
    error::WalletError,
    rpc::RPCConfig,
    storage::WalletStore,
    swapcoin::{IncomingSwapCoin, OutgoingSwapCoin, SwapCoin, WalletSwapCoin},
};

// these subroutines are coded so that as much as possible they keep all their
// data in the bitcoin core wallet
// for example which privkey corresponds to a scriptpubkey is stored in hd paths

const HARDENDED_DERIVATION: &str = "m/84'/1'/0'";

/// Represents a Bitcoin wallet with associated functionality and data.
#[derive(Debug)]
pub struct Wallet {
    pub(crate) rpc: Client,
    pub(crate) wallet_file_path: PathBuf,
    pub(crate) store: WalletStore,
    /// Optional encryption material derived from the user’s passphrase.
    /// If present, wallet data will be encrypted/decrypted using AES-GCM.
    /// The original passphrase is never stored—only the derived key is kept in memory.
    pub(crate) store_enc_material: Option<KeyMaterial>,
}
/// Compares two wallets for cryptographic equivalence.
///
/// This comparison checks fields relevant to the cryptographic and functional
/// state of the wallet, intentionally excluding fields that are:
/// - related to file metadata (like `file_name`),
/// - transient or runtime-only (e.g., swap coins, sync height),
/// - dynamic (e.g., `prevout_to_contract_map`).
///
/// The fields checked include:
/// - `network`
/// - `master_key`
/// - `external_index`
/// - `offer_maxsize`
/// - `fidelity_bond`
/// - `wallet_birthday`
/// - `utxo_cache`
///
/// This allows comparing whether two wallets represent the same core cryptographic
/// identity and logic state, regardless of runtime or file system differences.
impl PartialEq for Wallet {
    fn eq(&self, other: &Self) -> bool {
        //self.store == other.store
        //avoided filename
        self.store.network == other.store.network &&
        self.store.master_key == other.store.master_key &&
        self.store.external_index == other.store.external_index &&
        self.store.offer_maxsize == other.store.offer_maxsize &&
        //avoided incoming_swapcoins
        //avoided outgoing_swapcoins
        //avoided prevout_to_contract_map
        self.store.fidelity_bond == other.store.fidelity_bond &&
        //avoided last_synced_height
        self.store.wallet_birthday == other.store.wallet_birthday &&
        self.store.utxo_cache == other.store.utxo_cache
    }
}

/// Specify the keychain derivation path from [`HARDENDED_DERIVATION`]
/// Each kind represents an unhardened index value. Starting with External = 0.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub(crate) enum KeychainKind {
    External = 0isize,
    Internal,
}

#[derive(Deserialize)]
struct LockedUtxo {
    txid: Txid,
    vout: u32,
}

impl KeychainKind {
    fn index_num(&self) -> u32 {
        match self {
            Self::External => 0,
            Self::Internal => 1,
        }
    }
}

const WATCH_ONLY_SWAPCOIN_LABEL: &str = "watchonly_swapcoin_label";

/// Enum representing additional data needed to spend a UTXO, in addition to `ListUnspentResultEntry`.
// data needed to find information  in addition to ListUnspentResultEntry
// about a UTXO required to spend it
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub enum UTXOSpendInfo {
    /// Seed Coin
    SeedCoin {
        /// HD derivation path for the private key
        path: String,
        /// UTXO value in satoshis
        input_value: Amount,
    },
    /// Coins that we have received in a swap
    IncomingSwapCoin {
        /// Multisig redeem script for spending (2-OF-2 MSIG)
        multisig_redeemscript: ScriptBuf,
    },
    /// Coins that we have sent in a swap
    OutgoingSwapCoin {
        /// Multisig redeem script for spending (2-OF-2 MSIG)
        multisig_redeemscript: ScriptBuf,
    },
    /// Timelock contract UTXO (can be claimed after locktime expiry)
    TimelockContract {
        /// Original swap multisig redeem script
        swapcoin_multisig_redeemscript: ScriptBuf,
        /// UTXO value in satoshis
        input_value: Amount,
    },
    /// Hashlock contract UTXO (requires hash preimage to spend)
    HashlockContract {
        /// Original swap multisig redeem script
        swapcoin_multisig_redeemscript: ScriptBuf,
        /// UTXO value in satoshis
        input_value: Amount,
    },
    /// Fidelity Bond Coin (time-locked)
    FidelityBondCoin {
        /// Bond index in wallet's fidelity bond list
        index: u32,
        /// UTXO value in satoshis
        input_value: Amount,
    },
    /// Swept incoming swap coin (recovered to regular wallet address at the end of the Swap)
    SweptCoin {
        /// HD derivation path for the swept address
        path: String,
        /// UTXO value in satoshis
        input_value: Amount,
        /// Original multisig script before sweeping
        original_multisig_redeemscript: ScriptBuf,
    },
}

impl UTXOSpendInfo {
    /// Estimates Witness Size for different types of UTXOs in the context of Coinswap
    pub fn estimate_witness_size(&self) -> usize {
        const P2PWPKH_WITNESS_SIZE: usize = 107;
        const P2WSH_MULTISIG_2OF2_WITNESS_SIZE: usize = 222;
        const FIDELITY_BOND_WITNESS_SIZE: usize = 115;
        const TIME_LOCK_CONTRACT_TX_WITNESS_SIZE: usize = 179;
        const HASH_LOCK_CONTRACT_TX_WITNESS_SIZE: usize = 211;

        match *self {
            Self::SeedCoin { .. } | Self::SweptCoin { .. } => P2PWPKH_WITNESS_SIZE,
            Self::IncomingSwapCoin { .. } | Self::OutgoingSwapCoin { .. } => {
                P2WSH_MULTISIG_2OF2_WITNESS_SIZE
            }
            Self::TimelockContract { .. } => TIME_LOCK_CONTRACT_TX_WITNESS_SIZE,
            Self::HashlockContract { .. } => HASH_LOCK_CONTRACT_TX_WITNESS_SIZE,
            Self::FidelityBondCoin { .. } => FIDELITY_BOND_WITNESS_SIZE,
        }
    }
}

impl Display for UTXOSpendInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            UTXOSpendInfo::SeedCoin { .. } => {
                write!(f, "regular")
            }
            UTXOSpendInfo::SweptCoin { .. } => write!(f, "swept-incoming-swap"),
            UTXOSpendInfo::FidelityBondCoin { .. } => write!(f, "fidelity-bond"),
            UTXOSpendInfo::HashlockContract { .. } => write!(f, "hashlock-contract"),
            UTXOSpendInfo::TimelockContract { .. } => write!(f, "timelock-contract"),
            UTXOSpendInfo::IncomingSwapCoin { .. } => write!(f, "incoming-swap"),
            UTXOSpendInfo::OutgoingSwapCoin { .. } => write!(f, "outgoing-swap"),
        }
    }
}

/// Represents total wallet balances of different categories.
#[derive(Serialize, Deserialize, Debug)]
pub struct Balances {
    /// All single signature regular wallet coins (seed balance).
    pub regular: Amount,
    /// All 2of2 multisig coins received in swaps.
    pub swap: Amount,
    /// All live contract transaction balance locked in timelocks.
    pub contract: Amount,
    /// All coins locked in fidelity bonds.
    pub fidelity: Amount,
    /// Spendable amount in wallet (regular + swap balance).
    pub spendable: Amount,
}

impl Wallet {
    /// Initialize the wallet at a given path.
    ///
    /// The path should include the full path for a wallet file.
    /// If the wallet file doesn't exist it will create a new wallet file.
    pub fn init(
        path: &Path,
        rpc_config: &RPCConfig,
        store_enc_material: Option<KeyMaterial>,
    ) -> Result<Self, WalletError> {
        let rpc = Client::try_from(rpc_config)?;
        let network = rpc.get_blockchain_info()?.chain;

        // Generate Master key
        let master_key = {
            let mnemonic = Mnemonic::generate(12)?;
            let words = mnemonic.words().collect::<Vec<_>>();
            log::info!("Backup the Wallet Mnemonics. \n {words:?}");
            let seed = mnemonic.to_entropy();
            Xpriv::new_master(network, &seed)?
        };

        // Initialise wallet
        let file_name = path
            .file_name()
            .expect("file name expected")
            .to_str()
            .expect("expected")
            .to_string();

        let wallet_birthday = rpc.get_block_count()?;
        let store = WalletStore::init(
            file_name,
            path,
            network,
            master_key,
            Some(wallet_birthday),
            &store_enc_material,
        )?;

        Ok(Self {
            rpc,
            wallet_file_path: path.to_path_buf(),
            store,
            store_enc_material,
        })
    }
    /// Get the wallet name
    pub fn get_name(&self) -> &str {
        &self.store.file_name
    }

    /// Load wallet data from file and connect to a core RPC.
    /// The core rpc wallet name, and wallet_id field in the file should match.
    /// If encryption material is provided, decrypt the wallet store using it.
    pub(crate) fn load(path: &Path, rpc_config: &RPCConfig) -> Result<Wallet, WalletError> {
        let (store, store_enc_material) = WalletStore::read_from_disk(path)?;

        if rpc_config.wallet_name != store.file_name {
            return Err(WalletError::General(format!(
                "Wallet name of database file and core mismatch, expected {}, found {}",
                rpc_config.wallet_name, store.file_name
            )));
        }
        let rpc = Client::try_from(rpc_config)?;
        let network = rpc.get_blockchain_info()?.chain;

        // Check if the backend node is running on correct network. Or else hard error.
        if store.network != network {
            log::error!(
                "Wallet file is created for {}, backend Bitcoin Core is running on {}",
                store.network,
                network
            );
            return Err(WalletError::General("Wrong Bitcoin Network".to_string()));
        }
        log::debug!(
            "Loaded wallet file {} | External Index = {} | Incoming Swapcoins = {} | Outgoing Swapcoins = {}",
            store.file_name,
            store.external_index,
            store.incoming_swapcoins.len(),
            store.outgoing_swapcoins.len()
        );

        Ok(Self {
            rpc,
            wallet_file_path: path.to_path_buf(),
            store,
            store_enc_material,
        })
    }

    /// Loads an existing wallet from the given path or initializes a new one if none exists.
    ///
    /// Prompts the user for an encryption passphrase (unless running tests),
    /// derives encryption key material if a passphrase is provided,
    /// and either loads or creates the wallet accordingly.
    pub(crate) fn load_or_init_wallet(
        path: &Path,
        rpc_config: &RPCConfig,
    ) -> Result<Wallet, WalletError> {
        let wallet = if path.exists() {
            // wallet already exists, load the wallet
            let wallet = Wallet::load(path, rpc_config)?;
            log::info!("Wallet file at {path:?} successfully loaded.");
            wallet
        } else {
            // wallet doesn't exists at the given path, create a new one

            let store_enc_material = KeyMaterial::new_interactive(None);

            let wallet = Wallet::init(path, rpc_config, store_enc_material)?;

            log::info!("New Wallet created at : {path:?}");
            wallet
        };

        Ok(wallet)
    }

    /// Update the existing file. Error if path does not exist.
    pub(crate) fn save_to_disk(&self) -> Result<(), WalletError> {
        self.store
            .write_to_disk(&self.wallet_file_path, &self.store_enc_material)
    }

    /// Finds an incoming swap coin with the specified multisig redeem script.
    pub(crate) fn find_incoming_swapcoin(
        &self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&IncomingSwapCoin> {
        self.store.incoming_swapcoins.get(multisig_redeemscript)
    }

    /// Finds an outgoing swap coin with the specified multisig redeem script.
    pub(crate) fn find_outgoing_swapcoin(
        &self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&OutgoingSwapCoin> {
        self.store.outgoing_swapcoins.get(multisig_redeemscript)
    }

    /// Finds an outgoing swap coin with the specified multisig redeem script.
    pub(crate) fn find_outgoing_swapcoin_mut(
        &mut self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&mut OutgoingSwapCoin> {
        self.store.outgoing_swapcoins.get_mut(multisig_redeemscript)
    }

    /// Finds a mutable reference to an incoming swap coin with the specified multisig redeem script.
    pub(crate) fn find_incoming_swapcoin_mut(
        &mut self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&mut IncomingSwapCoin> {
        self.store.incoming_swapcoins.get_mut(multisig_redeemscript)
    }

    /// Adds an incoming swap coin to the wallet.
    pub(crate) fn add_incoming_swapcoin(&mut self, coin: &IncomingSwapCoin) {
        self.store
            .incoming_swapcoins
            .insert(coin.get_multisig_redeemscript(), coin.clone());
    }

    /// Adds an outgoing swap coin to the wallet.
    pub(crate) fn add_outgoing_swapcoin(&mut self, coin: &OutgoingSwapCoin) {
        self.store
            .outgoing_swapcoins
            .insert(coin.get_multisig_redeemscript(), coin.clone());
    }

    /// Removes an incoming swap coin with the specified multisig redeem script from the wallet.
    pub(crate) fn remove_incoming_swapcoin(
        &mut self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Result<Option<IncomingSwapCoin>, WalletError> {
        Ok(self.store.incoming_swapcoins.remove(multisig_redeemscript))
    }

    /// Removes an outgoing swap coin with the specified multisig redeem script from the wallet.
    pub(crate) fn remove_outgoing_swapcoin(
        &mut self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Result<Option<OutgoingSwapCoin>, WalletError> {
        Ok(self.store.outgoing_swapcoins.remove(multisig_redeemscript))
    }

    /// Gets the total count of swap coins in the wallet.
    pub fn get_swapcoins_count(&self) -> usize {
        self.store.incoming_swapcoins.len() + self.store.outgoing_swapcoins.len()
    }

    /// Calculates the total balances of different categories in the wallet.
    /// Includes regular, swap, contract, fidelity, and spendable (regular + swap) utxos.
    /// Optionally takes in a list of UTXOs to reduce rpc call. If None is provided, the full list is fetched from core rpc.
    pub fn get_balances(&self) -> Result<Balances, WalletError> {
        let regular = self
            .list_descriptor_utxo_spend_info()
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        let contract = self
            .list_live_timelock_contract_spend_info()
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        let swap = self
            .list_swept_incoming_swap_utxos()
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        let fidelity = self
            .list_fidelity_spend_info()
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        let spendable = regular + swap;

        Ok(Balances {
            regular,
            swap,
            contract,
            fidelity,
            spendable,
        })
    }

    /// Checks if the previous output (prevout) matches the cached contract in the wallet.
    ///
    /// This function is used in two scenarios:
    /// 1. When the maker has received the message `signsendercontracttx`.
    /// 2. When the maker receives the message `proofoffunding`.
    ///
    /// ## Cases when receiving `signsendercontracttx`:
    /// - Case 1: Previous output in cache doesn't have any contract => Ok
    /// - Case 2: Previous output has a contract, and it matches the given contract => Ok
    /// - Case 3: Previous output has a contract, but it doesn't match the given contract => Reject
    ///
    /// ## Cases when receiving `proofoffunding`:
    /// - Case 1: Previous output doesn't have an entry => Weird, how did they get a signature?
    /// - Case 2: Previous output has an entry that matches the contract => Ok
    /// - Case 3: Previous output has an entry that doesn't match the contract => Reject
    ///
    /// The two cases are mostly the same, except for Case 1 in `proofoffunding`, which shouldn't happen.
    pub(crate) fn does_prevout_match_cached_contract(
        &self,
        prevout: &OutPoint,
        contract_scriptpubkey: &Script,
    ) -> Result<bool, WalletError> {
        //let wallet_file_data = Wallet::load_wallet_file_data(&self.wallet_file_path[..])?;
        Ok(match self.store.prevout_to_contract_map.get(prevout) {
            Some(c) => c == contract_scriptpubkey,
            None => true,
        })
    }

    /// Dynamic address import count function. 10 for tests, 5000 for production.
    pub(crate) fn get_addrss_import_count(&self) -> u32 {
        if cfg!(feature = "integration-test") {
            10
        } else {
            5000
        }
    }

    /// Stores an entry into [`WalletStore`]'s prevout-to-contract map.
    /// If the prevout already existed with a contract script, this will update the existing contract.
    pub(crate) fn cache_prevout_to_contract(
        &mut self,
        prevout: OutPoint,
        contract: ScriptBuf,
    ) -> Result<(), WalletError> {
        if let Some(contract) = self.store.prevout_to_contract_map.insert(prevout, contract) {
            log::warn!("Prevout to Contract map updated.\nExisting Contract: {contract}");
        }
        Ok(())
    }

    //pub(crate) fn get_recovery_phrase_from_file()

    /// Wallet descriptors are derivable. Currently only supports two KeychainKind. Internal and External.
    fn get_wallet_descriptors(&self) -> Result<HashMap<KeychainKind, String>, WalletError> {
        let secp = Secp256k1::new();
        let wallet_xpub = Xpub::from_priv(
            &secp,
            &self
                .store
                .master_key
                .derive_priv(&secp, &DerivationPath::from_str(HARDENDED_DERIVATION)?)?,
        );

        // Get descriptors for external and internal keychain. Other chains are not supported yet.
        [KeychainKind::External, KeychainKind::Internal]
            .iter()
            .map(|keychain| {
                let descriptor_without_checksum =
                    format!("wpkh({}/{}/*)", wallet_xpub, keychain.index_num());
                let decriptor = format!(
                    "{}#{}",
                    descriptor_without_checksum,
                    compute_checksum(&descriptor_without_checksum)?
                );
                Ok((*keychain, decriptor))
            })
            .collect()
    }

    /// Checks if the addresses derived from the wallet descriptor is imported upto full index range.
    /// Returns the list of descriptors not imported yet. Max index range is as below:
    /// Procution => 5000
    /// Integration Tests => 6
    pub(super) fn get_unimported_wallet_desc(&self) -> Result<Vec<String>, WalletError> {
        let mut unimported = Vec::new();
        for (_, descriptor) in self.get_wallet_descriptors()? {
            let first_addr = self.rpc.derive_addresses(&descriptor, Some([0, 0]))?[0].clone();

            let last_index = self.get_addrss_import_count() - 1;
            let last_addr = self
                .rpc
                .derive_addresses(&descriptor, Some([last_index, last_index]))?[0]
                .clone();

            let first_addr_imported = self
                .rpc
                .get_address_info(&first_addr.assume_checked())?
                .is_watchonly
                .unwrap_or(false);
            let last_addr_imported = self
                .rpc
                .get_address_info(&last_addr.assume_checked())?
                .is_watchonly
                .unwrap_or(false);

            if !first_addr_imported || !last_addr_imported {
                unimported.push(descriptor);
            }
        }

        Ok(unimported)
    }

    /// Gets the external index from the wallet.
    pub fn get_external_index(&self) -> &u32 {
        &self.store.external_index
    }

    /// Core wallet label is the master Xpub(crate) fingerint.
    pub(crate) fn get_core_wallet_label(&self) -> String {
        let secp = Secp256k1::new();
        let m_xpub = Xpub::from_priv(&secp, &self.store.master_key);
        m_xpub.fingerprint().to_string()
    }

    /// Locks the fidelity and live_contract utxos which are not considered for spending from the wallet.
    pub fn lock_unspendable_utxos(&self) -> Result<(), WalletError> {
        self.rpc.unlock_unspent_all()?;

        let all_unspents = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        let utxos_to_lock = &all_unspents
            .into_iter()
            .filter(|u| {
                self.check_and_derive_descriptor_utxo_or_swap_coin(u)
                    .unwrap()
                    .is_none()
            })
            .map(|u| OutPoint {
                txid: u.txid,
                vout: u.vout,
            })
            .collect::<Vec<OutPoint>>();
        self.rpc.lock_unspent(utxos_to_lock)?;
        Ok(())
    }

    fn list_lock_unspent(&self) -> Result<Vec<OutPoint>, WalletError> {
        // Call the RPC method "listlockunspent" with no parameters.
        let locked_utxos: Vec<LockedUtxo> = self.rpc.call("listlockunspent", &[])?;

        // Convert each LockedUtxo into an OutPoint.
        Ok(locked_utxos
            .into_iter()
            .map(|lu| OutPoint {
                txid: lu.txid,
                vout: lu.vout,
            })
            .collect())
    }

    /// Checks if a UTXO belongs to fidelity bonds, and then returns corresponding UTXOSpendInfo
    fn check_if_fidelity(&self, utxo: &ListUnspentResultEntry) -> Option<UTXOSpendInfo> {
        self.store.fidelity_bond.iter().find_map(|(i, bond)| {
            if bond.script_pub_key() == utxo.script_pub_key && bond.amount == utxo.amount {
                Some(UTXOSpendInfo::FidelityBondCoin {
                    index: *i,
                    input_value: bond.amount,
                })
            } else {
                None
            }
        })
    }

    /// Check if a UTXO is a swept incoming swap coin based on ScriptPubkey
    fn check_if_swept_incoming_swapcoin(
        &self,
        utxo: &ListUnspentResultEntry,
    ) -> Option<UTXOSpendInfo> {
        if let Some(original_multisig_redeemscript) = self
            .store
            .swept_incoming_swapcoins
            .get(&utxo.script_pub_key)
        {
            if let Some(descriptor) = &utxo.descriptor {
                if let Some((_, addr_type, index)) = get_hd_path_from_descriptor(descriptor) {
                    let path = format!("m/{addr_type}/{index}");
                    return Some(UTXOSpendInfo::SweptCoin {
                        input_value: utxo.amount,
                        path,
                        original_multisig_redeemscript: original_multisig_redeemscript.clone(),
                    });
                }
            }
        }
        None
    }

    /// Checks if a UTXO belongs to live contracts, and then returns corresponding UTXOSpendInfo
    /// ### Note
    /// This is a costly search and should be used with care.
    fn check_and_derive_live_contract_spend_info(
        &self,
        utxo: &ListUnspentResultEntry,
    ) -> Result<Option<UTXOSpendInfo>, WalletError> {
        if let Some((_, outgoing_swapcoin)) =
            self.store.outgoing_swapcoins.iter().find(|(_, og)| {
                redeemscript_to_scriptpubkey(&og.contract_redeemscript).unwrap()
                    == utxo.script_pub_key
            })
        {
            return Ok(Some(UTXOSpendInfo::TimelockContract {
                swapcoin_multisig_redeemscript: outgoing_swapcoin.get_multisig_redeemscript(),
                input_value: utxo.amount,
            }));
        } else if let Some((_, incoming_swapcoin)) =
            self.store.incoming_swapcoins.iter().find(|(_, ig)| {
                redeemscript_to_scriptpubkey(&ig.contract_redeemscript).unwrap()
                    == utxo.script_pub_key
            })
        {
            if incoming_swapcoin.is_hash_preimage_known() {
                return Ok(Some(UTXOSpendInfo::HashlockContract {
                    swapcoin_multisig_redeemscript: incoming_swapcoin.get_multisig_redeemscript(),
                    input_value: utxo.amount,
                }));
            }
        }
        Ok(None)
    }

    /// Checks if a UTXO belongs to descriptor or swap coin, and then returns corresponding UTXOSpendInfo
    /// ### Note
    /// This is a costly search and should be used with care.
    fn check_and_derive_descriptor_utxo_or_swap_coin(
        &self,
        utxo: &ListUnspentResultEntry,
    ) -> Result<Option<UTXOSpendInfo>, WalletError> {
        // First check if it's a swept incoming swap coin
        if let Some(swept_info) = self.check_if_swept_incoming_swapcoin(utxo) {
            return Ok(Some(swept_info));
        }

        // Existing logic for other UTXO types
        if let Some(descriptor) = &utxo.descriptor {
            // Descriptor logic here
            if let Some(ret) = get_hd_path_from_descriptor(descriptor) {
                //utxo is in a hd wallet
                let (fingerprint, addr_type, index) = ret;

                let secp = Secp256k1::new();
                let master_private_key = self
                    .store
                    .master_key
                    .derive_priv(&secp, &DerivationPath::from_str(HARDENDED_DERIVATION)?)?;
                if fingerprint == master_private_key.fingerprint(&secp).to_string() {
                    return Ok(Some(UTXOSpendInfo::SeedCoin {
                        path: format!("m/{addr_type}/{index}"),
                        input_value: utxo.amount,
                    }));
                }
            } else {
                //utxo might be one of our swapcoins
                if self
                    .find_incoming_swapcoin(
                        utxo.witness_script
                            .as_ref()
                            .unwrap_or(&ScriptBuf::default()),
                    )
                    .is_some_and(|sc| sc.other_privkey.is_some())
                {
                    return Ok(Some(UTXOSpendInfo::IncomingSwapCoin {
                        multisig_redeemscript: utxo
                            .witness_script
                            .as_ref()
                            .expect("witness script expected")
                            .clone(),
                    }));
                }

                if self
                    .find_outgoing_swapcoin(
                        utxo.witness_script
                            .as_ref()
                            .unwrap_or(&ScriptBuf::default()),
                    )
                    .is_some_and(|sc| sc.hash_preimage.is_some())
                {
                    return Ok(Some(UTXOSpendInfo::OutgoingSwapCoin {
                        multisig_redeemscript: utxo
                            .witness_script
                            .as_ref()
                            .expect("witness script expected")
                            .clone(),
                    }));
                }
            }
        }
        Ok(None)
    }

    /// Returns a list of all UTXOs tracked by the wallet. Including fidelity, live_contracts and swap coins.
    pub fn list_all_utxo(&self) -> Vec<ListUnspentResultEntry> {
        self.list_all_utxo_spend_info()
            .iter()
            .map(|(utxo, _)| utxo.clone())
            .collect()
    }

    /// Returns a list all utxos with their spend info tracked by the wallet.
    /// Optionally takes in an Utxo list to reduce RPC calls. If None is given, the
    /// full list of utxo is fetched from core rpc.
    pub fn list_all_utxo_spend_info(&self) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let processed_utxos = self
            .store
            .utxo_cache
            .values()
            .map(|(utxo, spend_info)| (utxo.clone(), spend_info.clone()))
            .collect();
        processed_utxos
    }

    /// Lists live contract UTXOs along with their Spend info.
    pub fn list_live_contract_spend_info(&self) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| {
                matches!(x.1, UTXOSpendInfo::HashlockContract { .. })
                    || matches!(x.1, UTXOSpendInfo::TimelockContract { .. })
            })
            .cloned()
            .collect();
        filtered_utxos
    }

    /// Lists live timelock contract UTXOs along with their Spend info.
    pub fn list_live_timelock_contract_spend_info(
        &self,
    ) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::TimelockContract { .. }))
            .cloned()
            .collect();
        filtered_utxos
    }
    /// Lists all live hashlock contract UTXOs along with their Spend info.
    pub fn list_live_hashlock_contract_spend_info(
        &self,
    ) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::HashlockContract { .. }))
            .cloned()
            .collect();
        filtered_utxos
    }

    /// Lists fidelity UTXOs along with their Spend info.
    pub fn list_fidelity_spend_info(&self) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::FidelityBondCoin { .. }))
            .cloned()
            .collect();
        filtered_utxos
    }

    /// Lists descriptor UTXOs along with their Spend info.
    pub fn list_descriptor_utxo_spend_info(&self) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::SeedCoin { .. }))
            .cloned()
            .collect();
        filtered_utxos
    }

    /// Lists swap coin UTXOs along with their Spend info.
    pub fn list_swap_coin_utxo_spend_info(&self) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| {
                matches!(
                    x.1,
                    UTXOSpendInfo::IncomingSwapCoin { .. } | UTXOSpendInfo::OutgoingSwapCoin { .. }
                )
            })
            .cloned()
            .collect();
        filtered_utxos
    }

    /// Lists all incoming swapcoin UTXOs along with their Spend info.
    pub fn list_incoming_swap_coin_utxo_spend_info(
        &self,
    ) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::IncomingSwapCoin { .. }))
            .cloned()
            .collect();
        filtered_utxos
    }
    /// Lists all swept incoming swapcoin UTXOs along with their Spend info.
    pub fn list_swept_incoming_swap_utxos(&self) -> Vec<(ListUnspentResultEntry, UTXOSpendInfo)> {
        let all_valid_utxo = self.list_all_utxo_spend_info();
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|(_, spend_info)| matches!(spend_info, UTXOSpendInfo::SweptCoin { .. }))
            .cloned()
            .collect();
        filtered_utxos
    }

    /// A simplification of `find_incomplete_coinswaps` function
    pub(crate) fn find_unfinished_swapcoins(
        &self,
    ) -> (Vec<IncomingSwapCoin>, Vec<OutgoingSwapCoin>) {
        let unfinished_incomings = self
            .store
            .incoming_swapcoins
            .iter()
            .filter_map(|(_, ic)| {
                if ic.other_privkey.is_none() {
                    Some(ic.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let unfinished_outgoings = self
            .store
            .outgoing_swapcoins
            .iter()
            .filter_map(|(_, oc)| {
                if oc.hash_preimage.is_none() {
                    Some(oc.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let inc_contract_txid = unfinished_incomings
            .iter()
            .map(|ic| ic.contract_tx.compute_txid())
            .collect::<Vec<_>>();
        let out_contract_txid = unfinished_outgoings
            .iter()
            .map(|oc| oc.contract_tx.compute_txid())
            .collect::<Vec<_>>();

        if !inc_contract_txid.is_empty() || !out_contract_txid.is_empty() {
            log::info!(
                "Unfinished swap contracts - Incoming: {} transactions, Outgoing: {} transactions",
                inc_contract_txid.len(),
                out_contract_txid.len()
            );
            if !inc_contract_txid.is_empty() {
                log::debug!("Unfinished incoming contract TxIDs: {inc_contract_txid:?}");
            }
            if !out_contract_txid.is_empty() {
                log::debug!("Unfinished outgoing contract TxIDs: {out_contract_txid:?}");
            }
        }

        (unfinished_incomings, unfinished_outgoings)
    }

    /// Finds the next unused index in the HD keychain.
    ///
    /// It will only return an unused address; i.e., an address that doesn't have a transaction associated with it.
    pub(super) fn find_hd_next_index(&self, keychain: KeychainKind) -> Result<u32, WalletError> {
        let mut max_index: i32 = -1;

        let mut utxos = self.list_descriptor_utxo_spend_info();
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info();
        utxos.append(&mut swap_coin_utxo);

        for (utxo, _) in utxos {
            if utxo.descriptor.is_none() {
                continue;
            }
            let descriptor = utxo.descriptor.expect("its not none");
            let ret = get_hd_path_from_descriptor(&descriptor);
            if ret.is_none() {
                continue;
            }
            let (_, addr_type, index) = ret.expect("its not none");
            if addr_type != keychain.index_num() {
                continue;
            }
            max_index = std::cmp::max(max_index, index);
        }
        Ok((max_index + 1) as u32)
    }

    /// Gets the next external address from the HD keychain. Saves the wallet to disk
    pub fn get_next_external_address(&mut self) -> Result<Address, WalletError> {
        let descriptors = self.get_wallet_descriptors()?;
        let receive_branch_descriptor = descriptors
            .get(&KeychainKind::External)
            .expect("external keychain expected");
        let receive_address = self.rpc.derive_addresses(
            receive_branch_descriptor,
            Some([self.store.external_index, self.store.external_index]),
        )?[0]
            .clone();
        self.store.external_index += 1;
        self.save_to_disk()?;
        Ok(receive_address.assume_checked())
    }

    /// Gets the next internal addresses from the HD keychain.
    pub fn get_next_internal_addresses(&self, count: u32) -> Result<Vec<Address>, WalletError> {
        let next_change_addr_index = self.find_hd_next_index(KeychainKind::Internal)?;
        let descriptors = self.get_wallet_descriptors()?;
        let change_branch_descriptor = descriptors
            .get(&KeychainKind::Internal)
            .expect("Internal Keychain expected");
        let addresses = self.rpc.derive_addresses(
            change_branch_descriptor,
            Some([next_change_addr_index, next_change_addr_index + count]),
        )?;

        Ok(addresses
            .into_iter()
            .map(|addrs| addrs.assume_checked())
            .collect())
    }

    /// Returns a list of recent Incoming Transactions (bydefault last 10)
    pub fn get_transactions(
        &self,
        count: Option<usize>,
        skip: Option<usize>,
    ) -> Result<Vec<ListTransactionResult>, WalletError> {
        Ok(self.rpc.list_transactions(None, count, skip, Some(true))?)
    }

    /// Refreshes the offer maximum size cache based on the current wallet's unspent transaction outputs (UTXOs).
    pub(crate) fn refresh_offer_maxsize_cache(&mut self) -> Result<(), WalletError> {
        let balance = self.get_balances()?.spendable;
        self.store.offer_maxsize = balance.to_sat();
        Ok(())
    }

    /// Gets a tweakable key pair from the master key of the wallet.
    pub(crate) fn get_tweakable_keypair(&self) -> Result<(SecretKey, PublicKey), WalletError> {
        let secp = Secp256k1::new();
        let privkey = self
            .store
            .master_key
            .derive_priv(&secp, &[ChildNumber::from_hardened_idx(0)?])?
            .private_key;

        let public_key = PublicKey {
            compressed: true,
            inner: privkey.public_key(&secp),
        };
        Ok((privkey, public_key))
    }

    /// Refreshes the UTXO cache by adding only new UTXOs while preserving existing ones.
    pub(crate) fn update_utxo_cache(&mut self, utxos: Vec<ListUnspentResultEntry>) {
        let mut new_entries = Vec::new();
        let existing_outpoints: std::collections::HashSet<OutPoint> = utxos
            .iter()
            .map(|utxo| OutPoint {
                txid: utxo.txid,
                vout: utxo.vout,
            })
            .collect();

        // Identify UTXOs to be removed (present in store but missing in utxos parameter passed)
        let mut to_remove = Vec::new();
        for existing_outpoint in self.store.utxo_cache.keys().cloned().collect::<Vec<_>>() {
            if !existing_outpoints.contains(&existing_outpoint) {
                to_remove.push(existing_outpoint);
            }
        }

        // Remove UTXOs that no longer exist in the received utxos list
        for outpoint in to_remove {
            self.store.utxo_cache.remove(&outpoint);
            log::debug!("[UTXO Cache] Removed UTXO: {outpoint:?}");
        }

        // Process and add only new UTXOs
        for utxo in utxos {
            let outpoint = OutPoint {
                txid: utxo.txid,
                vout: utxo.vout,
            };

            // Skip if the UTXO already exists in the cache
            if self.store.utxo_cache.contains_key(&outpoint) {
                continue;
            }

            // Process UTXOs to pair each with it's spend info using the wallet's private methods.
            let spend_info = self
                .check_if_fidelity(&utxo)
                .or_else(|| {
                    self.check_and_derive_live_contract_spend_info(&utxo)
                        .unwrap()
                })
                .or_else(|| {
                    self.check_and_derive_descriptor_utxo_or_swap_coin(&utxo)
                        .unwrap()
                });

            // If we found valid spend info, store it in the cache
            if let Some(info) = spend_info {
                log::debug!("[UTXO Cache] Added UTXO: {outpoint:?} -> {info:?}");
                new_entries.push((outpoint, (utxo, info)));
            }
        }

        // Insert only new entries into the cache
        for (outpoint, entry) in new_entries {
            self.store.utxo_cache.insert(outpoint, entry);
        }
    }

    /// Signs a transaction corresponding to the provided UTXO spend information.
    pub(crate) fn sign_transaction(
        &self,
        tx: &mut Transaction,
        inputs_info: impl Iterator<Item = UTXOSpendInfo>,
    ) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let master_private_key = self
            .store
            .master_key
            .derive_priv(&secp, &DerivationPath::from_str(HARDENDED_DERIVATION)?)?;
        let tx_clone = tx.clone();

        for (ix, (input, input_info)) in tx.input.iter_mut().zip(inputs_info).enumerate() {
            match input_info {
                UTXOSpendInfo::OutgoingSwapCoin { .. } => {
                    return Err(WalletError::General(
                        "Can't sign for outgoing swapcoins".to_string(),
                    ))
                }
                UTXOSpendInfo::IncomingSwapCoin {
                    multisig_redeemscript,
                } => {
                    self.find_incoming_swapcoin(&multisig_redeemscript)
                        .expect("incoming swapcoin missing")
                        .sign_transaction_input(ix, &tx_clone, input, &multisig_redeemscript)?;
                }
                UTXOSpendInfo::SeedCoin { path, input_value }
                | UTXOSpendInfo::SweptCoin {
                    path, input_value, ..
                } => {
                    let privkey = master_private_key
                        .derive_priv(&secp, &DerivationPath::from_str(&path)?)?
                        .private_key;
                    let pubkey = PublicKey {
                        compressed: true,
                        inner: privkey.public_key(&secp),
                    };
                    let scriptcode = ScriptBuf::new_p2wpkh(&pubkey.wpubkey_hash()?);
                    let sighash = SighashCache::new(&tx_clone).p2wpkh_signature_hash(
                        ix,
                        &scriptcode,
                        input_value,
                        EcdsaSighashType::All,
                    )?;
                    //use low-R value signatures for privacy
                    //https://en.bitcoin.it/wiki/Privacy#Wallet_fingerprinting
                    let signature = secp.sign_ecdsa_low_r(
                        &secp256k1::Message::from_digest_slice(&sighash[..])?,
                        &privkey,
                    );
                    let mut sig_serialised = signature.serialize_der().to_vec();
                    sig_serialised.push(EcdsaSighashType::All as u8);
                    input.witness.push(sig_serialised);
                    input.witness.push(pubkey.to_bytes());
                }
                UTXOSpendInfo::TimelockContract {
                    swapcoin_multisig_redeemscript,
                    input_value,
                } => self
                    .find_outgoing_swapcoin(&swapcoin_multisig_redeemscript)
                    .expect("Outgoing swapcoin expeted")
                    .sign_timelocked_transaction_input(ix, &tx_clone, input, input_value)?,
                UTXOSpendInfo::HashlockContract {
                    swapcoin_multisig_redeemscript,
                    input_value,
                } => self
                    .find_incoming_swapcoin(&swapcoin_multisig_redeemscript)
                    .expect("Incoming swapcoin expected")
                    .sign_hashlocked_transaction_input(ix, &tx_clone, input, input_value)?,
                UTXOSpendInfo::FidelityBondCoin { index, input_value } => {
                    let privkey = self.get_fidelity_keypair(index)?.secret_key();
                    let redeemscript = self.get_fidelity_reedemscript(index)?;
                    let sighash = SighashCache::new(&tx_clone).p2wsh_signature_hash(
                        ix,
                        &redeemscript,
                        input_value,
                        EcdsaSighashType::All,
                    )?;
                    let sig = secp.sign_ecdsa(
                        &secp256k1::Message::from_digest_slice(&sighash[..])?,
                        &privkey,
                    );

                    let mut sig_serialised = sig.serialize_der().to_vec();
                    sig_serialised.push(EcdsaSighashType::All as u8);
                    input.witness.push(sig_serialised);
                    input.witness.push(redeemscript.as_bytes());
                }
            }
        }
        Ok(())
    }

    /// Performs coin selection to choose UTXOs that sum to a target amount.
    ///
    /// Uses the rust-coinselect library to implement Bitcoin Core's coin selection algorithm.
    /// The algorithm tries to minimize the number of inputs while accounting for:
    /// - Transaction fees and weight
    /// - Long-term UTXO pool management
    /// - Change output costs
    /// - Privacy considerations
    ///
    /// Always prefers to spend reused addresses first to preserve privacy.
    /// Selects more UTXOs if total reused addresses amount isn't adequate.
    ///
    /// Seperates regular and swap UTXOs, and always chooses regular UTXOs first.
    /// Mixing regular and swap UTXOs is not allowed.
    ///
    /// # Arguments
    /// * `amount` - The target amount to select coins for
    /// * `feerate` - Fee rate in sats/vbyte
    ///
    /// # Returns
    /// * `Ok(Vec<(ListUnspentResultEntry, UTXOSpendInfo)>)` - Selected UTXOs and their spend info
    /// * `Err(WalletError)` - If coin selection fails or there are insufficient funds
    ///
    /// # Note
    /// Only considers spendable UTXOs (regular coins and swap coins), filtering out:
    /// - Fidelity bond UTXOs
    /// - Locked UTXOs
    /// - Unconfirmed UTXOs
    pub fn coin_select(
        &self,
        amount: Amount,
        feerate: f64,
        manually_selected_outpoints: Option<Vec<OutPoint>>,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        // P2WPKH Breaks down as:
        // Non-witness data (multiplied by 4):
        // - Previous txid (32 bytes) * 4     = 128 WU
        // - Prev vout (4 bytes) * 4          = 16 WU
        // - Script length (1 byte) * 4       = 4 WU
        // - Empty scriptsig (0 bytes) * 4    = 0 WU
        // - nSequence (4 bytes) * 4          = 16 WU
        // Subtotal non-witness:              = 164 WU

        // Witness data (counted as-is):
        // - Num witness elements (1 byte)    = 1 WU
        // - DER signature (~72 bytes)        = 72 WU
        // - Pubkey (33 bytes)                = 33 WU
        // Subtotal witness:                  = 106 WU

        // Total: 164 + 106 = 270 WU
        // Adding 2 bytes as a buffer : 270 + 2 = 272 WU
        const P2WPKH_INPUT_WEIGHT: u64 = 272; // Total weight units

        // P2WPKH script-pubkey size:
        // - OP_0 (1 byte)
        // - OP_PUSH_20 (1 byte)
        // - 20-byte pubkey hash
        // Total: 22 bytes
        const P2WPKH_SPK_SIZE: usize = 22;
        const LONG_TERM_FEERATE: f32 = 10.0;

        // Base transaction weight constants
        // VERSION_SIZE: 4 bytes - 16 WU
        // SEGWIT_MARKER_SIZE: 2 bytes - 2 WU
        // NUM_INPUTS_SIZE: 1 byte - 4 WU
        // NUM_OUTPUTS_SIZE: 1 byte - 4 WU
        // NUM_WITNESS_SIZE: 1 byte - 1 WU
        // LOCK_TIME_SIZE: 4 bytes - 16 WU
        // Total: (16 + 2 + 4 + 4 + 1 + 16 = 43 WU)
        // Source: https://docs.rs/bitcoin/latest/src/bitcoin/blockdata/transaction.rs.html#599-602
        const TX_BASE_WEIGHT: u64 = 43;

        // Estimated transaction weight for basic fee calculation
        // Assumes a typical transaction with 2 inputs(or manually selected inputs) and 2 outputs (target + change)
        // This is used for early fee estimation before actual coin selection
        let estimated_tx_weight = if manually_selected_outpoints.is_some() {
            (manually_selected_outpoints.iter().len() as u64 * P2WPKH_INPUT_WEIGHT)
                + TX_BASE_WEIGHT
                + CHANGE_OUTPUT_WEIGHT
                + TARGET_OUTPUT_WEIGHT
        } else {
            (2 * P2WPKH_INPUT_WEIGHT) + TX_BASE_WEIGHT + CHANGE_OUTPUT_WEIGHT + TARGET_OUTPUT_WEIGHT
        };

        // Convert weight units to virtual bytes for fee calculation
        // Weight is divided by 4 to get vbytes (BIP 141 standard)
        let estimated_tx_vbytes: u64 = estimated_tx_weight / 4;

        // P2WPKH input weight: OutPoint(32) + sequence(4) + vout(4) + empty_scriptsig(1) = 41 bytes
        const INPUT_BASE_WEIGHT: u64 = 32 + 4 + 4 + 1;

        // P2WPKH output weight: Amount(8) + VarInt(1) + script_pubkey(22) = 31 bytes
        const TARGET_OUTPUT_WEIGHT: u64 = Amount::SIZE as u64 + 1 + P2WPKH_SPK_SIZE as u64; // ~31 bytes
        const CHANGE_OUTPUT_WEIGHT: u64 = Amount::SIZE as u64 + 1 + P2WPKH_SPK_SIZE as u64; // ~31 bytes

        let locked_utxos = self.list_lock_unspent()?;
        let filter_locked = |utxos: Vec<(ListUnspentResultEntry, UTXOSpendInfo)>| {
            utxos
                .into_iter()
                .filter(|(utxo, _)| {
                    let outpoint = OutPoint::new(utxo.txid, utxo.vout);
                    !locked_utxos.contains(&outpoint)
                })
                .collect::<Vec<_>>()
        };

        // Get regular and swap UTXOs separately
        let available_regular_utxos = filter_locked(self.list_descriptor_utxo_spend_info());
        let available_swap_utxos = filter_locked(self.list_swept_incoming_swap_utxos());

        // Assert that no non-spendable UTXOs are included after filtering
        assert!(
        available_regular_utxos.iter().chain(available_swap_utxos.iter()).all(|(_, spend_info)| !matches!(
            spend_info,
            UTXOSpendInfo::FidelityBondCoin { .. }
                | UTXOSpendInfo::OutgoingSwapCoin { .. }
                | UTXOSpendInfo::TimelockContract { .. }
                | UTXOSpendInfo::HashlockContract { .. }
        )),
        "Fidelity, Outgoing Swapcoins, Hashlock and Timelock coins are not included in coin selection"
    );

        let estimated_fee = calculate_fee(estimated_tx_vbytes, feerate as f32)?;

        if available_regular_utxos.is_empty() && available_swap_utxos.is_empty() {
            log::error!("No spendable UTXOs available");
            return Err(WalletError::InsufficientFund {
                available: 0,
                required: amount.to_sat() + estimated_fee,
            });
        }

        // Calculate totals for each type
        let regular_total: u64 = available_regular_utxos
            .iter()
            .map(|(utxo, _)| utxo.amount.to_sat())
            .sum();
        let swap_total: u64 = available_swap_utxos
            .iter()
            .map(|(utxo, _)| utxo.amount.to_sat())
            .sum();
        let target_sats = amount.to_sat();

        // Determine which UTXO types can satisfy the target
        let can_use_regular = target_sats <= regular_total;
        let can_use_swap = target_sats <= swap_total;

        // Check manual UTXO selection constraints
        let (manual_regular_selected, manual_swap_selected) =
            if let Some(ref manual_outpoints) = manually_selected_outpoints {
                let manual_regular = available_regular_utxos.iter().any(|(utxo, _)| {
                    let outpoint = OutPoint::new(utxo.txid, utxo.vout);
                    manual_outpoints.contains(&outpoint)
                });

                let manual_swap = available_swap_utxos.iter().any(|(utxo, _)| {
                    let outpoint = OutPoint::new(utxo.txid, utxo.vout);
                    manual_outpoints.contains(&outpoint)
                });

                // Hard error if manual selection mixes regular and swap coins
                if manual_regular && manual_swap {
                    return Err(WalletError::General(
                        "Cannot mix regular and swap UTXOs in manual selection".to_string(),
                    ));
                }

                (manual_regular, manual_swap)
            } else {
                (false, false)
            };

        // Assert manual selection compatibility with available funds
        if manual_regular_selected && !can_use_regular {
            return Err(WalletError::General(
                "Manual regular UTXOs selected but insufficient regular funds available"
                    .to_string(),
            ));
        }
        if manual_swap_selected && !can_use_swap {
            return Err(WalletError::General(
                "Manual swap UTXOs selected but insufficient swap funds available".to_string(),
            ));
        }

        let change_weight = Weight::from_vb_unwrap(CHANGE_OUTPUT_WEIGHT);
        let cost_of_change = {
            let creation_cost = calculate_fee(change_weight.to_vbytes_ceil(), feerate as f32)?;
            let future_spending_cost = calculate_fee(P2WPKH_INPUT_WEIGHT / 4, LONG_TERM_FEERATE)?;
            creation_cost + future_spending_cost
        };

        let target_weight = Weight::from_vb_unwrap(TARGET_OUTPUT_WEIGHT);
        let avg_output_weight = (change_weight.to_wu() + target_weight.to_wu()) / 2;

        // Choose UTXO type
        let (utxo_type, unspents) = if can_use_regular {
            ("regular", &available_regular_utxos)
        } else if can_use_swap {
            ("swap", &available_swap_utxos)
        } else {
            return Err(WalletError::InsufficientFund {
                available: regular_total,
                required: target_sats + estimated_fee,
            });
        };

        let avg_input_weight = unspents
            .iter()
            .map(|(_, spend_info)| {
                let witness_weight = spend_info.estimate_witness_size();
                INPUT_BASE_WEIGHT + witness_weight as u64
            })
            .sum::<u64>()
            / unspents.len() as u64;

        // Segregate manually selected UTXOs from the unspents list
        let (manual_unspents, non_manual_unspents): (Vec<&_>, Vec<&_>) =
            unspents.iter().partition(|(utxo, _)| {
                let outpoint = OutPoint::new(utxo.txid, utxo.vout);
                manually_selected_outpoints
                    .as_ref()
                    .unwrap_or(&vec![])
                    .iter()
                    .any(|manual_utxo| {
                        OutPoint::new(manual_utxo.txid, manual_utxo.vout) == outpoint
                    })
            });

        let unspents = non_manual_unspents.into_iter().cloned().collect::<Vec<_>>();

        // Group UTXOs by address
        let mut address_groups: HashMap<String, Vec<(ListUnspentResultEntry, UTXOSpendInfo)>> =
            HashMap::new();
        for (utxo, spend_info) in unspents {
            let address_str = utxo
                .address
                .as_ref()
                .map(|addr| addr.clone().assume_checked().to_string())
                .unwrap_or_else(|| format!("script_{}", utxo.script_pub_key));
            address_groups
                .entry(address_str)
                .or_default()
                .push((utxo.clone(), spend_info.clone()));
        }

        // Separate addresses with multiple UTXOs from addresses with a single UTXO
        let (mut grouped_addresses, single_addresses): (Vec<_>, Vec<_>) = address_groups
            .into_values()
            .partition(|group| group.len() > 1);

        // Sort reused addresses by total value
        grouped_addresses
            .sort_by_key(|group| group.iter().map(|(u, _)| u.amount.to_sat()).sum::<u64>());

        // Insert manual UTXOs at the front if they exist
        if !manual_unspents.is_empty() {
            grouped_addresses.insert(
                0,
                manual_unspents
                    .clone()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>(),
            );

            // Assert that if manual_unspents is not empty, the first group in grouped_addresses
            // contains exactly the same outpoints as manual_unspents (order doesn't matter).
            let first_group_outpoints = grouped_addresses[0]
                .iter()
                .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                .collect::<Vec<_>>();

            // Verify all manual outpoints are present in the first group
            assert!(
                manually_selected_outpoints
                    .as_deref()
                    .unwrap()
                    .iter()
                    .all(|outpoint| {
                        first_group_outpoints
                            .iter()
                            .any(|first_outpoint| first_outpoint == outpoint)
                    }),
                "First group must contain all manual_unspents outpoints"
            );

            // Verify the first group contains only manual outpoints
            assert_eq!(
                manually_selected_outpoints.as_deref().unwrap().len(),
                first_group_outpoints.len(),
                "First group should contain exactly the manual UTXOs, no more, no less"
            );
        }

        // Single loop for address group selection
        let (selected_utxos, selected_total, selected_weight) = {
            let mut result_utxos = Vec::new();
            let mut result_total = 0u64;
            let mut result_weight = 0u64;

            for group in grouped_addresses {
                let group_total: u64 = group.iter().map(|(u, _)| u.amount.to_sat()).sum();
                let group_weight: u64 = group
                    .iter()
                    .map(|(_, spend_info)| {
                        INPUT_BASE_WEIGHT + spend_info.estimate_witness_size() as u64
                    })
                    .sum();

                // Add the reused address group to selection
                result_total += group_total;
                result_weight += group_weight;
                result_utxos.extend(group);

                // Check if reused addresses now cover target + fees
                if result_total >= target_sats + estimated_fee {
                    log::info!(
                    "Address grouping: Selected {} {} UTXOs (total: {} sats, target+fee: {} sats)",
                    result_utxos.len(),
                    utxo_type,
                    result_total,
                    target_sats + estimated_fee
                );
                    return Ok(result_utxos);
                }
            }
            (result_utxos, result_total, result_weight)
        };

        // Group selection worked but didn't cover the whole target, run coin selection on single addresses
        let single_output_groups = single_addresses
            .iter()
            .map(|single_address_utxos| {
                let total_value: u64 = single_address_utxos
                    .iter()
                    .map(|(utxo, _)| utxo.amount.to_sat())
                    .sum();
                let total_weight: u64 = single_address_utxos
                    .iter()
                    .map(|(_, spend_info)| 36 + spend_info.estimate_witness_size() as u64)
                    .sum();

                OutputGroup {
                    value: total_value,
                    weight: total_weight,
                    input_count: single_address_utxos.len(),
                    creation_sequence: None,
                }
            })
            .collect::<Vec<_>>();

        // Calculate base weight
        let tx_base_weight = TX_BASE_WEIGHT
            + selected_weight
            + MAX_SPLITS as u64 * (target_weight.to_wu() + change_weight.to_wu());

        // Create coin selection options with adjusted target
        let coin_selection_option = CoinSelectionOpt {
            target_value: amount.to_sat().saturating_sub(selected_total),
            target_feerate: feerate as f32,
            long_term_feerate: Some(LONG_TERM_FEERATE),
            min_absolute_fee: MIN_FEE_RATE as u64 * tx_base_weight,
            base_weight: tx_base_weight,
            change_weight: change_weight.to_wu(),
            change_cost: cost_of_change,
            avg_input_weight,
            avg_output_weight,
            min_change_value: 294, // Minimal NonDust value: 294
            excess_strategy: ExcessStrategy::ToChange,
        };

        // Run coin selection on single addresses only
        match select_coin(&single_output_groups, &coin_selection_option) {
            Ok(selection) => {
                let additional_utxos: Vec<_> = selection
                    .selected_inputs
                    .iter()
                    .flat_map(|&group_index| single_addresses[group_index].clone())
                    .collect();

                // Combine pre-selected groups + coin selection results
                let mut final_selection = selected_utxos;
                final_selection.extend(additional_utxos);

                log::info!("Selected {} {utxo_type} UTXOs", final_selection.len());
                Ok(final_selection)
            }
            Err(e) => {
                log::error!("Coin selection of {utxo_type} UTXOs failed: {e:?}");
                Err(WalletError::General(format!(
                    "Coin selection failed: {e:?}"
                )))
            }
        }
    }

    pub(crate) fn get_utxo(
        &self,
        (txid, vout): (Txid, u32),
    ) -> Result<Option<UTXOSpendInfo>, WalletError> {
        let mut seed_coin_utxo = self.list_descriptor_utxo_spend_info();
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info();
        seed_coin_utxo.append(&mut swap_coin_utxo);

        for utxo in seed_coin_utxo {
            if utxo.0.txid == txid && utxo.0.vout == vout {
                return Ok(Some(utxo.1));
            }
        }

        Ok(None)
    }

    fn create_and_import_coinswap_address(
        &mut self,
        other_pubkey: &PublicKey,
    ) -> Result<(Address, SecretKey), WalletError> {
        let (my_pubkey, my_privkey) = generate_keypair();

        let descriptor = self
            .rpc
            .get_descriptor_info(&format!("wsh(sortedmulti(2,{my_pubkey},{other_pubkey}))"))?
            .descriptor;
        self.import_descriptors(std::slice::from_ref(&descriptor), None)?;

        // redeemscript and descriptor show up in `getaddressinfo` only after
        // the address gets outputs on it-
        Ok((
            self.rpc.derive_addresses(&descriptor[..], None)?[0]
                .clone()
                .assume_checked(),
            my_privkey,
        ))
    }

    /// Initialize a Coinswap with the Other party.
    /// Returns, the Funding Transactions, [`OutgoingSwapCoin`]s and the Total Miner fees.
    pub(crate) fn initalize_coinswap(
        &mut self,
        swap_params: &SwapParams,
        other_multisig_pubkeys: &[PublicKey],
        hashlock_pubkeys: &[PublicKey],
        hashvalue: Hash160,
        locktime: u16,
        fee_rate: f64,
    ) -> Result<(Vec<Transaction>, Vec<OutgoingSwapCoin>, Amount), WalletError> {
        let (coinswap_addresses, my_multisig_privkeys): (Vec<_>, Vec<_>) = other_multisig_pubkeys
            .iter()
            .map(|other_key| self.create_and_import_coinswap_address(other_key))
            .collect::<Result<Vec<(Address, SecretKey)>, WalletError>>()?
            .into_iter()
            .unzip();

        let create_funding_txes_result = self.create_funding_txes(
            swap_params.send_amount,
            &coinswap_addresses,
            fee_rate,
            swap_params.manually_selected_outpoints.clone(),
        )?;

        let mut outgoing_swapcoins = Vec::<OutgoingSwapCoin>::new();
        for (
            (((my_funding_tx, &utxo_index), &my_multisig_privkey), &other_multisig_pubkey),
            hashlock_pubkey,
        ) in create_funding_txes_result
            .funding_txes
            .iter()
            .zip(create_funding_txes_result.payment_output_positions.iter())
            .zip(my_multisig_privkeys.iter())
            .zip(other_multisig_pubkeys.iter())
            .zip(hashlock_pubkeys.iter())
        {
            let (timelock_pubkey, timelock_privkey) = generate_keypair();
            let contract_redeemscript = contract::create_contract_redeemscript(
                hashlock_pubkey,
                &timelock_pubkey,
                &hashvalue,
                &locktime,
            );
            let funding_amount = my_funding_tx.output[utxo_index as usize].value;
            let my_senders_contract_tx = contract::create_senders_contract_tx(
                OutPoint {
                    txid: my_funding_tx.compute_txid(),
                    vout: utxo_index,
                },
                funding_amount,
                &contract_redeemscript,
            )?;

            // self.import_wallet_contract_redeemscript(&contract_redeemscript)?;
            outgoing_swapcoins.push(OutgoingSwapCoin::new(
                my_multisig_privkey,
                other_multisig_pubkey,
                my_senders_contract_tx,
                contract_redeemscript,
                timelock_privkey,
                funding_amount,
            )?);
        }

        Ok((
            create_funding_txes_result.funding_txes,
            outgoing_swapcoins,
            Amount::from_sat(create_funding_txes_result.total_miner_fee),
        ))
    }

    /// Imports a watch-only redeem script into the wallet.
    pub(crate) fn import_watchonly_redeemscript(
        &self,
        redeemscript: &ScriptBuf,
    ) -> Result<(), WalletError> {
        let spk = redeemscript_to_scriptpubkey(redeemscript)?;
        let descriptor = self
            .rpc
            .get_descriptor_info(&format!("raw({spk:x})"))?
            .descriptor;
        self.import_descriptors(&[descriptor], Some(WATCH_ONLY_SWAPCOIN_LABEL.to_string()))
    }

    pub(crate) fn descriptors_to_import(&self) -> Result<Vec<String>, WalletError> {
        let mut descriptors_to_import = Vec::new();

        descriptors_to_import.extend(self.get_unimported_wallet_desc()?);

        descriptors_to_import.extend(
            self.store
                .incoming_swapcoins
                .values()
                .map(|sc| {
                    let descriptor_without_checksum = format!(
                        "wsh(sortedmulti(2,{},{}))",
                        sc.get_other_pubkey(),
                        sc.get_my_pubkey()
                    );
                    Ok(format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum)?
                    ))
                })
                .collect::<Result<Vec<String>, WalletError>>()?,
        );

        descriptors_to_import.extend(
            self.store
                .outgoing_swapcoins
                .values()
                .map(|sc| {
                    let descriptor_without_checksum = format!(
                        "wsh(sortedmulti(2,{},{}))",
                        sc.get_other_pubkey(),
                        sc.get_my_pubkey()
                    );
                    Ok(format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum)?
                    ))
                })
                .collect::<Result<Vec<String>, WalletError>>()?,
        );

        descriptors_to_import.extend(
            self.store
                .incoming_swapcoins
                .values()
                .map(|sc| {
                    let contract_spk = redeemscript_to_scriptpubkey(&sc.contract_redeemscript)?;
                    let descriptor_without_checksum = format!("raw({contract_spk:x})");
                    Ok(format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum)?
                    ))
                })
                .collect::<Result<Vec<String>, WalletError>>()?,
        );
        descriptors_to_import.extend(
            self.store
                .outgoing_swapcoins
                .values()
                .map(|sc| {
                    let contract_spk = redeemscript_to_scriptpubkey(&sc.contract_redeemscript)?;
                    let descriptor_without_checksum = format!("raw({contract_spk:x})");
                    Ok(format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum)?
                    ))
                })
                .collect::<Result<Vec<String>, WalletError>>()?,
        );

        descriptors_to_import.extend(
            self.store
                .fidelity_bond
                .values()
                .map(|bond| {
                    let descriptor_without_checksum = format!("raw({:x})", bond.script_pub_key());
                    Ok(format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum)?
                    ))
                })
                .collect::<Result<Vec<String>, WalletError>>()?,
        );
        Ok(descriptors_to_import)
    }

    /// Uses internal RPC client to broadcast a transaction
    pub fn send_tx(&self, tx: &Transaction) -> Result<Txid, WalletError> {
        Ok(self.rpc.send_raw_transaction(tx)?)
    }
    /// Sweeps all completed incoming swapcoins to an internal wallet address, broadcasting transactions and recording their [`Txid`]s.
    pub fn sweep_incoming_swapcoins(&mut self, feerate: f64) -> Result<Vec<Txid>, WalletError> {
        let mut swept_txids = Vec::new();

        let completed_swapcoins: Vec<_> = self
            .store
            .incoming_swapcoins
            .iter()
            .filter_map(|(redeemscript, swapcoin)| {
                if swapcoin.other_privkey.is_some() {
                    Some((redeemscript.clone(), swapcoin.clone()))
                } else {
                    None
                }
            })
            .collect();

        if completed_swapcoins.is_empty() {
            log::info!("No completed incoming swap coins to sweep");
            return Ok(swept_txids);
        }

        log::info!(
            "Sweeping {} completed incoming swap coins",
            completed_swapcoins.len()
        );

        self.sync_no_fail();

        for (multisig_redeemscript, _) in completed_swapcoins {
            let utxo_info = self
                .list_incoming_swap_coin_utxo_spend_info()
                .into_iter()
                .find(|(_, spend_info)| {
                    matches!(spend_info, UTXOSpendInfo::IncomingSwapCoin {
                        multisig_redeemscript: rs
                    } if rs == &multisig_redeemscript)
                });

            if let Some((utxo, spend_info)) = utxo_info {
                let internal_address = self.get_next_internal_addresses(1)?[0].clone();
                log::info!(
                    "Sweeping incoming swap coin {} to internal address {}",
                    utxo.txid,
                    internal_address
                );
                let sweep_tx = self.spend_coins(
                    &[(utxo.clone(), spend_info)],
                    Destination::Sweep(internal_address.clone()),
                    feerate,
                )?;
                let txid = self.send_tx(&sweep_tx)?;
                let conf_height = self.wait_for_tx_confirmation(txid)?;
                log::info!("Sweep Transaction {txid} confirmed at blockheight: {conf_height}");

                swept_txids.push(txid);
                log::info!("Successfully swept incoming swap coin, txid: {txid}");
                self.remove_incoming_swapcoin(&multisig_redeemscript)?;
                log::info!("Successfully removed incoming swaps coins");

                let output_scriptpubkey = internal_address.script_pubkey();
                self.store
                    .swept_incoming_swapcoins
                    .insert(output_scriptpubkey, multisig_redeemscript.clone());
            } else {
                log::warn!("Could not find UTXO for completed incoming swap coin");
            }
        }
        self.save_to_disk()?;
        Ok(swept_txids)
    }

    /// Waits for a transaction to confirm and returns its block height.
    pub(crate) fn wait_for_tx_confirmation(&self, txid: Txid) -> Result<u32, WalletError> {
        let sleep_increment = 10;
        let mut sleep_multiplier = 0;

        let ht = loop {
            sleep_multiplier += 1;

            let get_tx_result = self.rpc.get_transaction(&txid, None)?;
            if let Some(ht) = get_tx_result.info.blockheight {
                log::info!("Transaction confirmed at blockheight: {ht}, txid : {txid}");
                break ht;
            } else {
                log::info!("Transaction seen in mempool,waiting for confirmation, txid: {txid}");
                let total_sleep = sleep_increment * sleep_multiplier.min(10 * 60); // Caps at 10 minutes
                log::info!("Next sync in {total_sleep:?} secs");
                thread::sleep(Duration::from_secs(total_sleep));
            }
        };

        Ok(ht)
    }

    ///Broadcasts all incoming contracts
    pub(crate) fn broadcast_incoming_contracts(
        &mut self,
        incomings: Vec<IncomingSwapCoin>,
    ) -> Result<ContractMetadata, WalletError> {
        // Return early with error if no incomings to broadcast
        if incomings.is_empty() {
            log::info!("No incoming contracts to broadcast");
            return Err(WalletError::General(
                "No incoming contracts to broadcast".to_string(),
            ));
        }
        let mut incoming_infos = Vec::with_capacity(incomings.len());

        for incoming in incomings {
            let contract_tx = incoming.get_fully_signed_contract_tx()?;
            let txid = contract_tx.compute_txid();
            if self.rpc.get_raw_transaction_info(&txid, None).is_ok() {
                log::info!("Incoming Contract already broadacsted. Txid : {txid}");
            } else {
                self.send_tx(&contract_tx)?;
                log::info!("Broadcasting Incoming Contract. Removing from wallet. Txid : {txid}");
            }
            let reedem_script = incoming.get_multisig_redeemscript();
            let next_internal = &self.get_next_internal_addresses(1)?[0];
            self.sync()?;

            let hashlock_spend =
                self.create_hashlock_spend(&incoming, next_internal, MIN_FEE_RATE)?;
            incoming_infos.push(((reedem_script, contract_tx), (0, hashlock_spend)));
        }
        self.sync_and_save()?;
        log::info!("Wallet file synced and saved.");

        Ok(incoming_infos)
    }

    ///Broadcasts all outgoing contracts
    pub(crate) fn broadcast_outgoing_contracts(
        &mut self,
        outgoings: Vec<OutgoingSwapCoin>,
    ) -> Result<ContractMetadata, WalletError> {
        let mut outgoing_infos = Vec::with_capacity(outgoings.len());

        for outgoing in outgoings {
            let contract_tx = outgoing.get_fully_signed_contract_tx()?;
            let txid = contract_tx.compute_txid();
            if self.rpc.get_raw_transaction_info(&txid, None).is_ok() {
                log::info!("Outgoing Contract already broadcasted | Txid: {txid}");
            } else {
                self.send_tx(&contract_tx)?;
                log::info!("Broadcasted Outgoing Contract | txid : {txid}");
            }
            let reedem_script = outgoing.get_multisig_redeemscript();
            let timelock = outgoing.get_timelock()?;
            let next_internal = &self.get_next_internal_addresses(1)?[0];
            self.sync()?;

            let timelock_spend =
                self.create_timelock_spend(&outgoing, next_internal, MIN_FEE_RATE)?;
            outgoing_infos.push(((reedem_script, contract_tx), (timelock, timelock_spend)));
        }
        self.sync_and_save()?;
        log::info!("Wallet file synced and saved.");

        Ok(outgoing_infos)
    }

    //Spend from hashlock contract
    pub(crate) fn spend_from_hashlock_contract(
        &mut self,
        incoming_infos: &ContractMetadata,
        watch_service: &WatchService,
    ) -> Result<Vec<Transaction>, WalletError> {
        let mut broadcasted = Vec::new();

        for ((ic_rs, contract), (_, hashlock_tx)) in incoming_infos.iter() {
            //We have already broadcasted this tx,so skip
            if broadcasted.contains(hashlock_tx) {
                continue;
            }
            let txid = contract.compute_txid();

            let Ok(info) = self.rpc.get_raw_transaction_info(&txid, None) else {
                continue;
            };
            log::info!(
                "Contract Tx : {}, reached confirmation : {:?}",
                txid,
                info.confirmations,
            );
            log::info!("Hashlock Contract Tx is confirmed : {txid}");
            log::info!("Broadcasting hashlocked tx: {}", hashlock_tx.compute_txid());
            self.send_tx(hashlock_tx)?;
            broadcasted.push(hashlock_tx.to_owned());

            // Add them as part of swapcoins, because they are technically output of a swap.
            self.store
                .swept_incoming_swapcoins
                .insert(hashlock_tx.output[0].script_pubkey.clone(), ic_rs.clone());

            let removed = self
                .remove_incoming_swapcoin(ic_rs)?
                .expect("incoming swapcoin expected");
            let contract_txid = removed.contract_tx.compute_txid();
            for (vout, _) in removed.contract_tx.output.iter().enumerate() {
                let outpoint = OutPoint {
                    txid: contract_txid,
                    vout: vout as u32,
                };
                watch_service.unwatch(outpoint);
            }
            log::info!(
                "Removed Incoming Swapcoin from Wallet, Contract Txid: {}",
                removed.contract_tx.compute_txid()
            );
            log::info!("Initializing Wallet sync and save");
            self.sync_and_save()?;
            log::info!("Completed wallet sync and save");
        }
        Ok(broadcasted)
    }

    //Spend from the timelock contract
    pub(crate) fn spend_from_timelock_contract(
        &mut self,
        outgoing_infos: &ContractMetadata,
        watch_service: &WatchService,
    ) -> Result<Vec<Transaction>, WalletError> {
        let mut broadcasted = Vec::new();
        for ((redeem_script, contract), (timelock, timelocked_tx)) in outgoing_infos.iter() {
            if broadcasted.contains(timelocked_tx) {
                continue;
            }
            let txid = contract.compute_txid();
            let Ok(info) = self.rpc.get_raw_transaction_info(&txid, None) else {
                continue;
            };
            let confirmations = match info.confirmations {
                Some(c) => {
                    if c > (*timelock as u32) {
                        c
                    } else {
                        log::info!("Contract Tx {txid} has {c} confirmations, waiting for {timelock} confirmations.");
                        sleep(BLOCK_DELAY);
                        continue;
                    }
                }
                _ => {
                    sleep(HEART_BEAT_INTERVAL);
                    continue;
                }
            };
            log::info!(
                "Contract Tx {txid} reached {confirmations} confirmations, required: {timelock}"
            );
            log::info!(
                "Timelock matured. Broadcasting timelocked tx: {}",
                timelocked_tx.compute_txid()
            );
            self.send_tx(timelocked_tx)?;
            broadcasted.push(timelocked_tx.to_owned());
            let removed = self
                .remove_outgoing_swapcoin(redeem_script)?
                .expect("Outgoing swapcoin expected");
            let contract_txid = removed.contract_tx.compute_txid();
            for (vout, _) in removed.contract_tx.output.iter().enumerate() {
                let outpoint = OutPoint {
                    txid: contract_txid,
                    vout: vout as u32,
                };
                watch_service.unwatch(outpoint);
            }
            log::info!(
                "Removed Outgoing Swapcoin. Contract Txid: {}",
                removed.contract_tx.compute_txid()
            );
            log::info!("Syncing and saving wallet...");
            self.sync_and_save()?;
            log::info!("Wallet sync and save complete.");
        }
        Ok(broadcasted)
    }

    /// Sends specified Amount of Satoshis to an External Address
    pub fn send_to_address(
        &mut self,
        amount: u64,
        address: String,
        fee_rate: Option<f64>,
    ) -> Result<Txid, WalletError> {
        let amount = Amount::from_sat(amount);

        let coins_to_spend = self.coin_select(amount, fee_rate.unwrap_or(MIN_FEE_RATE), None)?;

        let addr = Address::from_str(&address)
            .map_err(|e| WalletError::General(format!("Invalid address: {}", e)))?
            .assume_checked();
        let outputs = vec![(addr, amount)];
        let destination = Destination::Multi {
            outputs,
            op_return_data: None,
        };

        let tx = self.spend_from_wallet(
            fee_rate.unwrap_or(MIN_FEE_RATE),
            destination,
            &coins_to_spend,
        )?;

        let txid = self.send_tx(&tx).unwrap();

        println!("Send to Address TxId: {txid}");

        self.sync_no_fail();

        Ok(txid)
    }
}
