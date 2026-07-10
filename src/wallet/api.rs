//! The Wallet API.
//!
//! Currently, wallet synchronization is exclusively performed through RPC for makers.
//! In the future, takers might adopt alternative synchronization methods, such as lightweight wallet solutions.

use std::{
    cmp::max, convert::TryFrom, fmt::Display, path::PathBuf, str::FromStr, thread, time::Duration,
};

use std::collections::{HashMap, HashSet};

use crate::security::KeyMaterial;

use bip39::Mnemonic;
use bitcoin::{
    bip32::{ChainCode, ChildNumber, DerivationPath, Xpriv, Xpub},
    hashes::{sha512, Hash},
    key::TapTweak,
    secp256k1,
    secp256k1::{Keypair, Secp256k1, SecretKey},
    sighash::{EcdsaSighashType, Prevouts, SighashCache, TapSighashType},
    Address, Amount, OutPoint, PublicKey, Script, ScriptBuf, Transaction, TxOut, Txid, Weight,
};
use bitcoind::bitcoincore_rpc::{bitcoincore_rpc_json::ListUnspentResultEntry, Client, RpcApi};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::utill::{
    compute_checksum, generate_keypair, get_hd_path_from_descriptor, redeemscript_to_scriptpubkey,
};

use rust_coinselect::{
    selectcoin::select_coin,
    types::{CoinSelectionOpt, ExcessStrategy, OutputGroup, SelectionError},
    utils::calculate_fee,
};

use super::{
    error::WalletError,
    rpc::RPCConfig,
    storage::{AddressType, WalletStore},
};

// these subroutines are coded so that as much as possible they keep all their
// data in the bitcoin core wallet
// for example which privkey corresponds to a scriptpubkey is stored in hd paths

/// BIP-84 derivation path for P2WPKH (Native SegWit)
const HARDENDED_DERIVATION_P2WPKH: &str = "m/84'/1'/0'";
/// BIP-86 derivation path for P2TR (Taproot key-path)
const HARDENDED_DERIVATION_P2TR: &str = "m/86'/1'/0'";
/// P2WSH ECDSA: 2 sigs/sig+preimage + full redeemscript (~149)
const LEGACY_CONTRACT_SPEND_VSIZE: u64 = 150;
/// key-path: one 64B Schnorr sig, no script (~111)
const TAPROOT_KEYPATH_VSIZE: u64 = 112;
/// script-path: sig+preimage+script+control_block (~154)
const TAPROOT_SCRIPTPATH_VSIZE: u64 = 155;
/// ≈141 + 1 (growing block-height)
const LEGACY_TIMELOCK_VSIZE: u64 = 142;
/// ≈138 + 2 (growing block-height)
const TAPROOT_TIMELOCK_VSIZE: u64 = 140;

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
        self.store.internal_index == other.store.internal_index &&
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

/// Specify the keychain derivation path from [`HARDENDED_DERIVATION_P2WPKH`] or [`HARDENDED_DERIVATION_P2TR`]
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

/// Enum representing additional data needed to spend a UTXO, in addition to `ListUnspentResultEntry`.
// data needed to find information  in addition to ListUnspentResultEntry
// about a UTXO required to spend it
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub enum UTXOSpendInfo {
    /// Seed Coin (regular wallet UTXO from HD derivation)
    SeedCoin {
        /// HD derivation path for the private key
        path: String,
        /// UTXO value in satoshis
        input_value: Amount,
        /// Address type (P2WPKH or P2TR)
        #[serde(default)]
        address_type: AddressType,
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
        /// Address type (P2WPKH or P2TR)
        #[serde(default)]
        address_type: AddressType,
    },
}

impl UTXOSpendInfo {
    /// Estimates Witness Size for different types of UTXOs in the context of Coinswap
    pub fn estimate_witness_size(&self) -> usize {
        const P2WPKH_WITNESS_SIZE: usize = 107; // 1 + 72 (sig) + 33 (pubkey) + 1 (count)
        const P2TR_WITNESS_SIZE: usize = 66; // 1 (witness items) + 1 (Signature length) + 64 (Schnorr sig)
        const P2WSH_MULTISIG_2OF2_WITNESS_SIZE: usize = 218; //1 + 1 + 72 + 72 + 72
        const FIDELITY_BOND_WITNESS_SIZE: usize = 115; // 1 (count) + 1 (sig len) + 71 (sig) + 1 (script len) + 40 (script) = ~114
        match self {
            Self::SeedCoin { address_type, .. } | Self::SweptCoin { address_type, .. } => {
                match address_type {
                    AddressType::P2WPKH => P2WPKH_WITNESS_SIZE,
                    AddressType::P2TR => P2TR_WITNESS_SIZE,
                }
            }
            Self::IncomingSwapCoin { .. } | Self::OutgoingSwapCoin { .. } => {
                P2WSH_MULTISIG_2OF2_WITNESS_SIZE
            }
            Self::TimelockContract { .. } => 179,
            Self::HashlockContract { .. } => 211,
            Self::FidelityBondCoin { .. } => FIDELITY_BOND_WITNESS_SIZE,
        }
    }
}

/// Spend path of a swap transaction, for protocol-aware vsize estimation.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SpendKind {
    /// Incoming swapcoin spend; `cooperative` = key-path/2-of-2 vs hashlock path.
    ContractSpend { cooperative: bool },
    /// Timelock recovery of an outgoing swapcoin.
    Timelock,
}

/// Returns the estimated vsize (virtual bytes) for cooperative keypath, preimage (hashlock),
/// and timelock recovery spend transactions only.
pub(crate) fn contract_and_timelock_vsize(
    protocol: crate::protocol::ProtocolVersion,
    kind: SpendKind,
) -> u64 {
    use crate::protocol::ProtocolVersion::{Legacy, Taproot};
    use SpendKind::{ContractSpend, Timelock};

    match (protocol, kind) {
        (Legacy, ContractSpend { .. }) => LEGACY_CONTRACT_SPEND_VSIZE,
        (Taproot, ContractSpend { cooperative: true }) => TAPROOT_KEYPATH_VSIZE,
        (Taproot, ContractSpend { cooperative: false }) => TAPROOT_SCRIPTPATH_VSIZE,
        (Legacy, Timelock) => LEGACY_TIMELOCK_VSIZE,
        (Taproot, Timelock) => TAPROOT_TIMELOCK_VSIZE,
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

pub(crate) fn infer_address_type(script_pubkey: &Script) -> AddressType {
    if script_pubkey.is_p2wpkh() {
        AddressType::P2WPKH
    } else {
        // P2TR and P2WSH both have 34-byte scriptpubkeys; default non-P2WPKH to P2TR.
        AddressType::P2TR
    }
}

/// Results of sweep/recovery operations with per-contract detail.
#[derive(Debug, Default, Clone)]
pub struct RecoveryOutcome {
    /// (contract_txid, spending_txid) for contracts we successfully spent.
    pub resolved: Vec<(Txid, Txid)>,
    /// Contract txids that were discarded (already spent or never broadcast).
    pub discarded: Vec<Txid>,
}

impl RecoveryOutcome {
    /// Returns true if no contracts were resolved or discarded.
    pub fn is_empty(&self) -> bool {
        self.resolved.is_empty() && self.discarded.is_empty()
    }

    /// Total number of contracts handled (resolved + discarded).
    pub fn len(&self) -> usize {
        self.resolved.len() + self.discarded.len()
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
    #[hotpath::measure]
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
        let last_synced_height_val = match store.last_synced_height {
            Some(height) => height.to_string(),
            None => "None".to_string(),
        };

        log::info!(
            "Wallet birth_height = {}, wallet last_sync_height = {}",
            wallet_birthday,
            last_synced_height_val
        );

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

    /// Verify the deniability proof for a specific swap in this wallet's report file.
    pub fn verify_deniability(&self, swap_id: &str) -> Result<bool, std::io::Error> {
        let stem = self
            .wallet_file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| std::io::Error::other("wallet path has no valid file stem"))?;
        let report_path = self
            .wallet_file_path
            .parent()
            .ok_or_else(|| std::io::Error::other("wallet path has no parent directory"))?
            .join(format!("{stem}_swap_report.json"));
        crate::wallet::deniability::verify_deniability(&report_path, &self.rpc, swap_id)
    }

    /// Load wallet data from file and connect to a core RPC.
    /// The core rpc wallet name, and wallet_id field in the file should match.
    /// If encryption material is provided, decrypt the wallet store using it.
    pub(crate) fn load(
        path: &Path,
        rpc_config: &RPCConfig,
        password: Option<String>,
    ) -> Result<Wallet, WalletError> {
        let (store, store_enc_material) =
            WalletStore::read_from_disk(path, password.unwrap_or_default())?;

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
            "Loaded wallet file {} | External Index = {} | Incoming = {} | Outgoing = {}",
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
        password: Option<String>,
    ) -> Result<Wallet, WalletError> {
        let wallet = if path.exists() {
            // wallet already exists, load the wallet
            let wallet = Wallet::load(path, rpc_config, password)?;
            log::info!("Wallet file at {path:?} successfully loaded.");
            wallet
        } else {
            // wallet doesn't exists at the given path, create a new one

            let store_enc_material = KeyMaterial::new_from_password(password);

            let wallet = Wallet::init(path, rpc_config, store_enc_material)?;

            log::info!("New Wallet created at : {path:?}");
            wallet
        };

        Ok(wallet)
    }

    /// Persist wallet data to disk, creating missing parent directories and file as needed.
    pub(crate) fn save_to_disk(&self) -> Result<(), WalletError> {
        self.store
            .write_to_disk(&self.wallet_file_path, &self.store_enc_material)
    }

    /// Adds a incoming swap coin to the wallet.
    pub(crate) fn add_incoming_swapcoin(&mut self, coin: &super::swapcoin::IncomingSwapCoin) {
        // Use contract txid as key to ensure each swapcoin has a unique entry,
        // even when multiple incoming swapcoins share the same swap_id.
        let key = coin.contract_tx.compute_txid().to_string();
        self.store
            .incoming_swapcoins
            .insert(key.clone(), coin.clone());
        log::info!(
            "Added incoming swapcoin to wallet store: {} (total: {})",
            key,
            self.store.incoming_swapcoins.len()
        );
    }

    /// Adds a outgoing swap coin to the wallet.
    pub(crate) fn add_outgoing_swapcoin(&mut self, coin: &super::swapcoin::OutgoingSwapCoin) {
        // Use contract txid as key to ensure each swapcoin has a unique entry,
        // even when multiple outgoing swapcoins share the same swap_id.
        let key = coin.contract_tx.compute_txid().to_string();
        self.store
            .outgoing_swapcoins
            .insert(key.clone(), coin.clone());
        log::info!(
            "Added outgoing swapcoin to wallet store: {} (total: {})",
            key,
            self.store.outgoing_swapcoins.len()
        );
    }

    /// Finds a incoming swap coin by swap_id.
    #[allow(dead_code)]
    pub(crate) fn find_incoming_swapcoin(
        &self,
        contract_txid: &str,
    ) -> Option<&super::swapcoin::IncomingSwapCoin> {
        self.store.incoming_swapcoins.get(contract_txid)
    }

    /// Finds a incoming swap coin by contract txid (mutable).
    pub(crate) fn find_incoming_swapcoin_mut(
        &mut self,
        contract_txid: &str,
    ) -> Option<&mut super::swapcoin::IncomingSwapCoin> {
        self.store.incoming_swapcoins.get_mut(contract_txid)
    }

    /// Finds a outgoing swap coin by multisig redeemscript.
    pub(crate) fn find_outgoing_swapcoin_by_multisig(
        &self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&super::swapcoin::OutgoingSwapCoin> {
        for swapcoin in self.store.outgoing_swapcoins.values() {
            // Only check Legacy swapcoins which have my_pubkey and other_pubkey
            if swapcoin.protocol == crate::protocol::ProtocolVersion::Legacy {
                if let (Some(my_pubkey), Some(other_pubkey)) =
                    (swapcoin.my_pubkey, swapcoin.other_pubkey)
                {
                    let computed_script = crate::protocol::contract::create_multisig_redeemscript(
                        &my_pubkey,
                        &other_pubkey,
                    );
                    if &computed_script == multisig_redeemscript {
                        return Some(swapcoin);
                    }
                }
            }
        }
        None
    }

    /// Finds a incoming swap coin by multisig redeemscript.
    pub(crate) fn find_incoming_swapcoin_by_multisig(
        &self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&super::swapcoin::IncomingSwapCoin> {
        for swapcoin in self.store.incoming_swapcoins.values() {
            if swapcoin.protocol == crate::protocol::ProtocolVersion::Legacy {
                if let (Some(my_pubkey), Some(other_pubkey)) =
                    (swapcoin.my_pubkey, swapcoin.other_pubkey)
                {
                    let computed_script = crate::protocol::contract::create_multisig_redeemscript(
                        &my_pubkey,
                        &other_pubkey,
                    );
                    if &computed_script == multisig_redeemscript {
                        return Some(swapcoin);
                    }
                }
            }
        }
        None
    }

    /// Removes a incoming swap coin by contract txid.
    pub(crate) fn remove_incoming_swapcoin(
        &mut self,
        contract_txid: &str,
    ) -> Option<super::swapcoin::IncomingSwapCoin> {
        let removed = self.store.incoming_swapcoins.remove(contract_txid);
        if removed.is_some() {
            log::info!(
                "Removed incoming swapcoin from wallet store: {} (remaining: {})",
                contract_txid,
                self.store.incoming_swapcoins.len()
            );
        }
        removed
    }

    /// Adds watch-only swapcoins for a given swap.
    pub(crate) fn add_watchonly_swapcoins(
        &mut self,
        swap_id: &str,
        coins: Vec<super::swapcoin::WatchOnlySwapCoin>,
    ) {
        let count = coins.len();
        self.store
            .watchonly_swapcoins
            .entry(swap_id.to_string())
            .or_default()
            .extend(coins);
        log::info!("Added {} watch-only swapcoins for swap {}", count, swap_id);
    }

    /// Removes watch-only swapcoins for a given swap.
    pub(crate) fn remove_watchonly_swapcoins(
        &mut self,
        swap_id: &str,
    ) -> Option<Vec<super::swapcoin::WatchOnlySwapCoin>> {
        self.store.watchonly_swapcoins.remove(swap_id)
    }

    /// Gets the count of incoming swap coins.
    pub fn get_incoming_swapcoins_count(&self) -> usize {
        self.store.incoming_swapcoins.len()
    }

    /// Gets the count of outgoing swap coins.
    pub fn get_outgoing_swapcoins_count(&self) -> usize {
        self.store.outgoing_swapcoins.len()
    }

    /// Returns contract outpoints for all persisted outgoing swapcoins.
    pub(crate) fn outgoing_contract_outpoints(&self) -> Vec<OutPoint> {
        self.store
            .outgoing_swapcoins
            .values()
            .map(|sc| OutPoint {
                txid: sc.contract_tx.compute_txid(),
                vout: 0,
            })
            .collect()
    }

    /// Returns contract outpoints for all persisted incoming swapcoins.
    pub(crate) fn incoming_contract_outpoints(&self) -> Vec<OutPoint> {
        self.store
            .incoming_swapcoins
            .values()
            .map(|sc| OutPoint {
                txid: sc.contract_tx.compute_txid(),
                vout: 0,
            })
            .collect()
    }

    /// Remove a outgoing swapcoin by contract txid.
    pub(crate) fn remove_outgoing_swapcoin(&mut self, contract_txid: &str) {
        if self
            .store
            .outgoing_swapcoins
            .remove(contract_txid)
            .is_some()
        {
            log::info!(
                "Removed outgoing swapcoin: {} (remaining: {})",
                contract_txid,
                self.store.outgoing_swapcoins.len()
            );
        }
    }

    /// Returns contract_txid keys of outgoing swapcoins matching a swap_id.
    pub(crate) fn outgoing_keys_for_swap(&self, swap_id: &str) -> Vec<String> {
        self.store
            .outgoing_swapcoins
            .iter()
            .filter(|(_, sc)| sc.swap_id.as_deref() == Some(swap_id))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Attempt to recover timelocked outgoing swapcoins.
    #[hotpath::measure]
    pub fn recover_timelocked_swapcoins(
        &mut self,
        fee_rate: f64,
    ) -> Result<RecoveryOutcome, WalletError> {
        let mut outcome = RecoveryOutcome::default();
        let mut recovered_keys = Vec::new();

        let current_height = self.rpc.get_block_count()? as u32;

        let mut to_recover = Vec::new();

        log::info!(
            "recover_timelocked: {} outgoing swapcoins in store at height {}",
            self.store.outgoing_swapcoins.len(),
            current_height
        );

        for (swap_id, swapcoin) in &self.store.outgoing_swapcoins {
            if swapcoin.my_privkey.is_some() {
                if let Some(timelock) = swapcoin.get_timelock() {
                    if swapcoin.protocol == crate::protocol::ProtocolVersion::Taproot {
                        // Taproot uses CLTV (absolute height).
                        if current_height >= timelock {
                            log::info!(
                                "Outgoing swapcoin {} ready for timelock recovery (current: {}, CLTV: {})",
                                swap_id, current_height, timelock
                            );
                            to_recover.push(swap_id.clone());
                        } else {
                            log::debug!(
                                "Outgoing swapcoin {} not yet ready (current: {}, CLTV: {})",
                                swap_id,
                                current_height,
                                timelock
                            );
                        }
                    } else {
                        // Legacy uses CSV (relative to contract tx confirmation).
                        // Can't filter by height alone — the downstream confirmation
                        // count check (line 938) is the real gate.
                        log::debug!(
                            "Outgoing swapcoin {} queued for timelock recovery (CSV: {} blocks)",
                            swap_id,
                            timelock
                        );
                        to_recover.push(swap_id.clone());
                    }
                }
            }
        }

        let mut discarded = Vec::new();

        for swap_id in to_recover {
            if let Some(swapcoin) = self.store.outgoing_swapcoins.get(&swap_id).cloned() {
                // Ensure the contract tx is on-chain before attempting timelock spend.
                let contract_txid = swapcoin.contract_tx.compute_txid();
                let contract_vout = swapcoin.get_contract_output_vout();
                match self
                    .rpc
                    .get_tx_out(&contract_txid, contract_vout, Some(false))
                {
                    Ok(Some(_)) => {
                        log::info!(
                            "Contract tx {} already on-chain for {}",
                            contract_txid,
                            swap_id
                        );
                    }
                    _ => {
                        // get_tx_out returned None — either the UTXO was spent or
                        // the contract tx was never broadcast.

                        // First, check if the contract tx exists on-chain at all.
                        let contract_tx_on_chain = self
                            .rpc
                            .get_raw_transaction_info(&contract_txid, None)
                            .ok()
                            .and_then(|info| info.confirmations)
                            .unwrap_or(0)
                            > 0;

                        if contract_tx_on_chain {
                            // Contract tx IS on-chain but UTXO is spent — someone
                            // already claimed this output (hashlock or timelock).
                            // Nothing left to recover.
                            log::info!(
                                "Contract UTXO for {} was already spent — discarding swapcoin",
                                swap_id
                            );
                            discarded.push(swap_id.clone());
                            continue;
                        }

                        // Contract tx not on-chain. Check if the wallet UTXOs
                        // (inputs to the contract tx) are still unspent — if so,
                        // the tx was never broadcast and funds are still ours.
                        let input_outpoint = swapcoin.contract_tx.input[0].previous_output;
                        let input_still_unspent = matches!(
                            self.rpc.get_tx_out(
                                &input_outpoint.txid,
                                input_outpoint.vout,
                                Some(true)
                            ),
                            Ok(Some(_))
                        );

                        if input_still_unspent
                            && swapcoin.protocol == crate::protocol::ProtocolVersion::Taproot
                        {
                            // For Taproot, contract_tx IS the funding tx.
                            // If its input (wallet UTXO) is still unspent, funds are still ours.
                            log::info!(
                                "Contract tx for {} was never broadcast — wallet UTXOs still unspent, discarding swapcoin",
                                swap_id
                            );
                            discarded.push(swap_id.clone());
                            continue;
                        }
                        // For Legacy, the input is the 2-of-2 multisig funding output,
                        // not a wallet UTXO. Fall through to broadcast the contract tx.

                        // Inputs are spent but contract output isn't on-chain.
                        // For Legacy, the contract tx (pre-signed insurance) may
                        // not have been broadcast yet — sign and push it so the
                        // timelock output exists.
                        log::info!(
                            "Signing and broadcasting contract tx for {} before timelock recovery",
                            swap_id
                        );
                        match swapcoin.create_signed_contract_tx() {
                            Ok(signed_contract_tx) => match self.send_tx(&signed_contract_tx) {
                                Ok(_) => {
                                    log::info!(
                                        "Contract tx {} broadcast successfully",
                                        signed_contract_tx.compute_txid()
                                    );
                                }
                                Err(e) => {
                                    let err_str = format!("{:?}", e);
                                    // RPC error -27 means "Transaction already in block chain"
                                    // — the contract tx IS on-chain, so proceed with recovery.
                                    let is_already_in_chain = err_str.contains("-27")
                                        || err_str.contains("already in utxo set");
                                    // RPC error -25 means inputs are missing or already spent
                                    // — the funding tx was never broadcast (e.g. SkipFundingBroadcast),
                                    // so this swapcoin is permanently unrecoverable. Discard it.
                                    let is_inputs_missing = err_str.contains("-25")
                                        || err_str.contains("bad-txns-inputs-missingorspent");
                                    if is_already_in_chain {
                                        log::info!(
                                            "Contract tx for {} already on-chain, proceeding with timelock recovery",
                                            swap_id
                                        );
                                    } else if is_inputs_missing {
                                        log::warn!(
                                            "Contract tx for {} has missing/spent inputs — discarding swapcoin: {}",
                                            swap_id,
                                            err_str
                                        );
                                        discarded.push(swap_id.clone());
                                        continue;
                                    } else {
                                        log::warn!(
                                            "Failed to broadcast contract tx for {}: {:?} — skipping recovery",
                                            swap_id,
                                            e
                                        );
                                        continue;
                                    }
                                }
                            },
                            Err(e) => {
                                log::warn!(
                                    "Failed to sign contract tx for {}: {:?} — skipping recovery",
                                    swap_id,
                                    e
                                );
                                continue;
                            }
                        }
                    }
                }

                // Verify the contract UTXO is confirmed and the timelock is satisfied.
                //
                // Legacy uses BIP68 CSV (relative): the recovery tx sets
                // Sequence::from_height(timelock), requiring `timelock` confirmations.
                //
                // Taproot uses BIP65 CLTV (absolute): the recovery tx sets
                // nLockTime to the absolute height. We just need the UTXO to be
                // confirmed (at least 1 confirmation).
                let timelock_value = swapcoin.get_timelock().unwrap_or(0);
                let required_confirmations =
                    if swapcoin.protocol == crate::protocol::ProtocolVersion::Taproot {
                        1 // CLTV only needs the UTXO to exist; height check is at lines 744-745
                    } else {
                        timelock_value // CSV needs this many confirmations
                    };
                match self
                    .rpc
                    .get_tx_out(&contract_txid, contract_vout, Some(false))
                {
                    Ok(Some(utxo_info)) if utxo_info.confirmations >= required_confirmations => {
                        log::info!(
                            "Contract tx {} has {} confirmations (need {}), proceeding with recovery",
                            contract_txid,
                            utxo_info.confirmations,
                            required_confirmations
                        );
                    }
                    Ok(Some(utxo_info)) => {
                        log::info!(
                            "Contract tx {} has {} confirmations, need {} — waiting",
                            contract_txid,
                            utxo_info.confirmations,
                            required_confirmations
                        );
                        continue;
                    }
                    _ => {
                        log::info!(
                            "Contract tx {} not yet confirmed, skipping recovery attempt",
                            contract_txid
                        );
                        continue;
                    }
                }

                match self.create_timelock_recovery_tx(&swapcoin, fee_rate) {
                    Ok(recovery_tx) => {
                        let txid = recovery_tx.compute_txid();
                        match self.send_tx(&recovery_tx) {
                            Ok(_) => {
                                log::info!("Broadcast timelock recovery tx: {}", txid);
                                outcome.resolved.push((contract_txid, txid));
                                recovered_keys.push(swap_id.clone());
                            }
                            Err(e) => {
                                log::warn!(
                                    "Failed to broadcast recovery tx for {}: {:?}",
                                    swap_id,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to create recovery tx for {}: {:?}", swap_id, e);
                    }
                }
            }
        }

        for id in &discarded {
            if let Some(sc) = self.store.outgoing_swapcoins.get(id) {
                outcome.discarded.push(sc.contract_tx.compute_txid());
            }
            self.store.outgoing_swapcoins.remove(id);
        }
        for key in &recovered_keys {
            self.store.outgoing_swapcoins.remove(key);
        }

        if !outcome.is_empty() || !discarded.is_empty() {
            self.save_to_disk()?;
        }

        if !outcome.is_empty() {
            #[cfg(debug_assertions)]
            log::debug!(
                "[RECOVERY_STATE] Wallet: {} | Action: recover_timelocked | Resolved: {} | Discarded: {} | OutgoingRemaining: {}",
                self.store.file_name,
                outcome.resolved.len(),
                outcome.discarded.len(),
                self.store.outgoing_swapcoins.len()
            );
        }
        Ok(outcome)
    }

    /// Create a recovery transaction for a timelocked outgoing swapcoin.
    #[hotpath::measure]
    fn create_timelock_recovery_tx(
        &mut self,
        swapcoin: &super::swapcoin::OutgoingSwapCoin,
        fee_rate: f64,
    ) -> Result<bitcoin::Transaction, WalletError> {
        use bitcoin::{locktime::absolute::LockTime, transaction::Version, Sequence, TxIn, TxOut};

        let timelock = swapcoin.get_timelock().ok_or_else(|| {
            WalletError::General("Could not extract timelock from swapcoin".to_string())
        })?;
        let contract_txid = swapcoin.contract_tx.compute_txid();
        let contract_vout = swapcoin.get_contract_output_vout();

        let contract_output = swapcoin
            .contract_tx
            .output
            .get(contract_vout as usize)
            .ok_or_else(|| WalletError::General("No output in contract tx".to_string()))?;

        let vsize = contract_and_timelock_vsize(swapcoin.protocol, SpendKind::Timelock);

        let fee = Amount::from_sat((vsize as f64 * fee_rate) as u64);
        let output_amount = contract_output.value.checked_sub(fee).ok_or_else(|| {
            WalletError::General("Insufficient funds for recovery fee".to_string())
        })?;

        let recovery_address = self
            .get_next_internal_addresses(1, AddressType::P2TR)?
            .into_iter()
            .next()
            .ok_or_else(|| WalletError::General("Failed to get recovery address".to_string()))?;

        // Legacy (CSV): nSequence encodes relative locktime, nLockTime = 0.
        // Taproot (CLTV): nLockTime = absolute height, nSequence enables locktime.
        let (lock_time, sequence) =
            if swapcoin.protocol == crate::protocol::ProtocolVersion::Taproot {
                (
                    LockTime::from_height(timelock).unwrap_or(LockTime::ZERO),
                    Sequence::ENABLE_LOCKTIME_NO_RBF,
                )
            } else {
                (LockTime::ZERO, Sequence::from_height(timelock as u16))
            };

        let recovery_tx = bitcoin::Transaction {
            version: Version::TWO,
            lock_time,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: contract_txid,
                    vout: contract_vout,
                },
                script_sig: bitcoin::ScriptBuf::new(),
                sequence,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![TxOut {
                value: output_amount,
                script_pubkey: recovery_address.script_pubkey(),
            }],
        };

        swapcoin.sign_timelock_recovery(recovery_tx)
    }

    /// Calculates the total balances of different categories in the wallet.
    /// Includes regular, swap, contract, fidelity, and spendable (regular + swap) utxos.
    /// Optionally takes in a list of UTXOs to reduce rpc call. If None is provided, the full list is fetched from core rpc.
    #[hotpath::measure]
    pub fn get_balances(&self) -> Result<Balances, WalletError> {
        let regular = self
            .list_descriptor_utxo_spend_info()
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        // Contract balance: outgoing swapcoins whose contract TX is still unspent on-chain.
        // These are OUR funds locked in a contract, recoverable via timelock.
        // This is already covered by list_live_timelock_contract_spend_info() which
        // checks outgoing_swapcoins in check_and_derive_live_contract_spend_info().
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
    /// 1. When the Maker has received the message `signsendercontracttx`.
    /// 2. When the Maker receives the message `proofoffunding`.
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

    /// Returns the derivation path for the given address type
    fn get_derivation_path(address_type: AddressType) -> &'static str {
        match address_type {
            AddressType::P2WPKH => HARDENDED_DERIVATION_P2WPKH,
            AddressType::P2TR => HARDENDED_DERIVATION_P2TR,
        }
    }

    /// Wallet descriptors are derivable. Currently only supports two KeychainKind. Internal and External.
    fn get_wallet_descriptors(
        &self,
        address_type: AddressType,
    ) -> Result<HashMap<KeychainKind, String>, WalletError> {
        let secp = Secp256k1::new();
        let derivation_path = Self::get_derivation_path(address_type);
        let wallet_xpub = Xpub::from_priv(
            &secp,
            &self
                .store
                .master_key
                .derive_priv(&secp, &DerivationPath::from_str(derivation_path)?)?,
        );

        // Get descriptors for external and internal keychain. Other chains are not supported yet.
        [KeychainKind::External, KeychainKind::Internal]
            .iter()
            .map(|keychain| {
                let descriptor_without_checksum = match address_type {
                    AddressType::P2WPKH => {
                        format!("wpkh({}/{}/*)", wallet_xpub, keychain.index_num())
                    }
                    AddressType::P2TR => {
                        format!("tr({}/{}/*)", wallet_xpub, keychain.index_num())
                    }
                };
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
    /// Production => 5000
    /// Integration Tests => 6
    pub(super) fn get_unimported_wallet_desc(
        &self,
        address_type: AddressType,
    ) -> Result<Vec<String>, WalletError> {
        let mut unimported = Vec::new();
        for (_, descriptor) in self.get_wallet_descriptors(address_type)? {
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
        self.store
            .fidelity_bond
            .iter()
            .enumerate()
            .find_map(|(i, bond)| {
                if bond.script_pub_key() == utxo.script_pub_key && bond.amount == utxo.amount {
                    Some(UTXOSpendInfo::FidelityBondCoin {
                        index: i as u32,
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
        if self
            .store
            .swept_incoming_swapcoins
            .contains(&utxo.script_pub_key)
        {
            if let Some(descriptor) = &utxo.descriptor {
                if let Some((_, addr_type, index)) = get_hd_path_from_descriptor(descriptor) {
                    let path = format!("m/{addr_type}/{index}");
                    let address_type = if descriptor.starts_with("tr(") {
                        AddressType::P2TR
                    } else {
                        AddressType::P2WPKH
                    };
                    return Some(UTXOSpendInfo::SweptCoin {
                        input_value: utxo.amount,
                        path,
                        address_type,
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
        // Check outgoing swapcoins for timelock contracts
        for outgoing in self.store.outgoing_swapcoins.values() {
            let contract_txid = outgoing.contract_tx.compute_txid();
            let vout = outgoing.get_contract_output_vout();
            if utxo.txid == contract_txid && utxo.vout == vout {
                return Ok(Some(UTXOSpendInfo::TimelockContract {
                    swapcoin_multisig_redeemscript: outgoing
                        .contract_redeemscript
                        .clone()
                        .unwrap_or_default(),
                    input_value: utxo.amount,
                }));
            }
        }

        // Check incoming swapcoins for hashlock contracts
        for incoming in self.store.incoming_swapcoins.values() {
            let contract_txid = incoming.contract_tx.compute_txid();
            let vout = incoming.get_contract_output_vout();
            if utxo.txid == contract_txid && utxo.vout == vout && incoming.is_preimage_known() {
                return Ok(Some(UTXOSpendInfo::HashlockContract {
                    swapcoin_multisig_redeemscript: incoming
                        .contract_redeemscript
                        .clone()
                        .unwrap_or_default(),
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
        // First check if it's a swept incoming swap coin (V1)
        if let Some(swept_info) = self.check_if_swept_incoming_swapcoin(utxo) {
            return Ok(Some(swept_info));
        }

        // Existing logic for other UTXO types
        if let Some(descriptor) = &utxo.descriptor {
            // Descriptor logic here
            if let Some(ret) = get_hd_path_from_descriptor(descriptor) {
                //utxo is in a hd wallet
                let (fingerprint, addr_type, index) = ret;

                let address_type = if descriptor.starts_with("tr(") {
                    AddressType::P2TR
                } else {
                    AddressType::P2WPKH
                };

                let secp = Secp256k1::new();
                let derivation_path = Self::get_derivation_path(address_type);
                let master_private_key = self
                    .store
                    .master_key
                    .derive_priv(&secp, &DerivationPath::from_str(derivation_path)?)?;
                if fingerprint == master_private_key.fingerprint(&secp).to_string() {
                    return Ok(Some(UTXOSpendInfo::SeedCoin {
                        path: format!("m/{addr_type}/{index}"),
                        input_value: utxo.amount,
                        address_type,
                    }));
                }
            } else {
                //utxo might be one of our swapcoins
                let default_script = ScriptBuf::default();
                let witness_script = utxo.witness_script.as_ref().unwrap_or(&default_script);

                if self
                    .find_incoming_swapcoin_by_multisig(witness_script)
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
                    .find_outgoing_swapcoin_by_multisig(witness_script)
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
    #[hotpath::measure]
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

    /// Finds unfinished swapcoins.
    /// Incoming unfinished: `other_privkey` is None.
    /// Outgoing unfinished: `hash_preimage` is None.
    pub(crate) fn find_unfinished_swapcoins(
        &self,
    ) -> (
        Vec<super::swapcoin::IncomingSwapCoin>,
        Vec<super::swapcoin::OutgoingSwapCoin>,
    ) {
        let unfinished_incomings: Vec<_> = self
            .store
            .incoming_swapcoins
            .values()
            .filter(|ic| ic.other_privkey.is_none())
            .cloned()
            .collect();
        let unfinished_outgoings: Vec<_> = self
            .store
            .outgoing_swapcoins
            .values()
            .filter(|oc| oc.hash_preimage.is_none())
            .cloned()
            .collect();
        if !unfinished_incomings.is_empty() || !unfinished_outgoings.is_empty() {
            log::info!(
                "Unfinished swaps - Incoming: {}, Outgoing: {}",
                unfinished_incomings.len(),
                unfinished_outgoings.len()
            );
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
    pub fn get_next_external_address(
        &mut self,
        address_type: AddressType,
    ) -> Result<Address, WalletError> {
        let descriptors = self.get_wallet_descriptors(address_type)?;
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

    /// Gets the next internal addresses from the HD keychain. Index saved to disk
    pub fn get_next_internal_addresses(
        &mut self,
        count: u32,
        address_type: AddressType,
    ) -> Result<Vec<Address>, WalletError> {
        let start = self.store.internal_index;
        let descriptors = self.get_wallet_descriptors(address_type)?;
        let change_branch_descriptor = descriptors
            .get(&KeychainKind::Internal)
            .expect("Internal Keychain expected");
        let addresses = self
            .rpc
            .derive_addresses(change_branch_descriptor, Some([start, start + count - 1]))?;

        self.store.internal_index += count;
        self.save_to_disk()?;

        Ok(addresses
            .into_iter()
            .map(|addrs| addrs.assume_checked())
            .collect())
    }

    /// Refreshes the offer maximum size cache based on the current wallet's unspent transaction outputs (UTXOs).
    pub(crate) fn refresh_offer_maxsize_cache(&mut self) -> Result<(), WalletError> {
        let Balances { swap, regular, .. } = self.get_balances()?;
        self.store.offer_maxsize = max(swap, regular).to_sat();
        Ok(())
    }

    //expose a deterministically-derived 64-byte Ed25519-V3 Tor key
    // built from the wallet's master_key
    pub(crate) fn derive_tor_key(&self) -> [u8; 64] {
        // Hash the 32-byte secp256k1 private key bytes RFC 8032 per 5.1.5,
        // then clamp into a valid Ed25519 expanded key.
        let mut tor_key =
            *sha512::Hash::hash(&self.store.master_key.private_key.secret_bytes()).as_byte_array();
        tor_key[0] &= 248;
        tor_key[31] &= 127;
        tor_key[31] |= 64;
        tor_key
    }

    /// Gets a tweakable key pair from the master key of the wallet.
    pub(crate) fn get_tweakable_keypair(
        &self,
    ) -> Result<(SecretKey, PublicKey, ChainCode), WalletError> {
        let secp = Secp256k1::new();
        let Xpriv {
            private_key,
            chain_code,
            ..
        } = self
            .store
            .master_key
            .derive_priv(&secp, &[ChildNumber::from_hardened_idx(175)?])?;

        let public_key = PublicKey {
            compressed: true,
            inner: private_key.public_key(&secp),
        };
        Ok((private_key, public_key, chain_code))
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
        for outpoint in &to_remove {
            self.store.utxo_cache.remove(outpoint);
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
                new_entries.push((outpoint, (utxo, info)));
            }
        }

        // Insert only new entries into the cache
        #[cfg(debug_assertions)]
        if !new_entries.is_empty() || !to_remove.is_empty() {
            log::debug!(
                "[UTXO_STATE] Source: wallet::api::update_utxo_cache | Wallet: {} | Added: {} | Removed: {} | CachedUtxos: {} -> {} | IncomingSwapcoins: {} | OutgoingSwapcoins: {}",
                self.store.file_name,
                new_entries.len(),
                to_remove.len(),
                self.store.utxo_cache.len() + to_remove.len(),
                self.store.utxo_cache.len() + new_entries.len(),
                self.store.incoming_swapcoins.len(),
                self.store.outgoing_swapcoins.len()
            );
        }
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
        let tx_clone = tx.clone();

        let inputs_info: Vec<UTXOSpendInfo> = inputs_info.collect();

        // Build all prevouts for taproot sighash computation (BIP-341 requires all prevouts)
        let prevouts: Vec<TxOut> = inputs_info
            .iter()
            .map(|info| match info {
                UTXOSpendInfo::SeedCoin {
                    path,
                    input_value,
                    address_type,
                }
                | UTXOSpendInfo::SweptCoin {
                    path,
                    input_value,
                    address_type,
                    ..
                } => {
                    let base_derivation = match address_type {
                        AddressType::P2WPKH => HARDENDED_DERIVATION_P2WPKH,
                        AddressType::P2TR => HARDENDED_DERIVATION_P2TR,
                    };
                    let master_private_key = self
                        .store
                        .master_key
                        .derive_priv(&secp, &DerivationPath::from_str(base_derivation).unwrap())
                        .unwrap();
                    let privkey = master_private_key
                        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
                        .unwrap()
                        .private_key;

                    let script_pubkey = match address_type {
                        AddressType::P2WPKH => {
                            let pubkey = PublicKey {
                                compressed: true,
                                inner: privkey.public_key(&secp),
                            };
                            ScriptBuf::new_p2wpkh(&pubkey.wpubkey_hash().unwrap())
                        }
                        AddressType::P2TR => {
                            let keypair = Keypair::from_secret_key(&secp, &privkey);
                            let (x_only_pubkey, _) = keypair.x_only_public_key();
                            ScriptBuf::new_p2tr(&secp, x_only_pubkey, None)
                        }
                    };
                    TxOut {
                        script_pubkey,
                        value: *input_value,
                    }
                }
                UTXOSpendInfo::FidelityBondCoin { index, input_value } => {
                    let redeemscript = self.get_fidelity_reedemscript(*index).unwrap();
                    TxOut {
                        script_pubkey: redeemscript.to_p2wsh(),
                        value: *input_value,
                    }
                }
                _ => TxOut {
                    script_pubkey: ScriptBuf::new(),
                    value: Amount::ZERO,
                },
            })
            .collect();

        if tx.input.len() != inputs_info.len() {
            return Err(WalletError::General(format!(
                "Mismatched signer/input counts: tx has {} inputs but signer metadata has {} entries",
                tx.input.len(),
                inputs_info.len()
            )));
        }

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
                    let sc = self
                        .find_incoming_swapcoin_by_multisig(&multisig_redeemscript)
                        .expect("incoming swapcoin missing");
                    let spend_tx = sc.sign_spend_transaction(
                        sc.funding_amount,
                        &tx.output[0].script_pubkey,
                        1.0,
                    )?;
                    input.witness = spend_tx.input[0].witness.clone();
                }
                UTXOSpendInfo::SeedCoin {
                    path,
                    input_value,
                    address_type,
                }
                | UTXOSpendInfo::SweptCoin {
                    path,
                    input_value,
                    address_type,
                    ..
                } => {
                    let base_derivation = match address_type {
                        AddressType::P2WPKH => HARDENDED_DERIVATION_P2WPKH,
                        AddressType::P2TR => HARDENDED_DERIVATION_P2TR,
                    };
                    let master_private_key = self
                        .store
                        .master_key
                        .derive_priv(&secp, &DerivationPath::from_str(base_derivation)?)?;
                    let privkey = master_private_key
                        .derive_priv(&secp, &DerivationPath::from_str(&path)?)?
                        .private_key;

                    match address_type {
                        AddressType::P2WPKH => {
                            // P2WPKH signing (existing logic)
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
                        AddressType::P2TR => {
                            let keypair = Keypair::from_secret_key(&secp, &privkey);

                            // Calculate taproot key-spend sighash using all prevouts
                            let sighash = SighashCache::new(&tx_clone)
                                .taproot_key_spend_signature_hash(
                                    ix,
                                    &Prevouts::All(&prevouts),
                                    TapSighashType::Default,
                                )?;

                            let tweaked_keypair = keypair.tap_tweak(&secp, None);
                            let msg = secp256k1::Message::from(sighash);
                            let signature = secp.sign_schnorr(&msg, &tweaked_keypair.to_keypair());

                            input.witness.push(signature.as_ref());
                        }
                    }
                }
                UTXOSpendInfo::TimelockContract {
                    swapcoin_multisig_redeemscript,
                    ..
                } => {
                    let sc = self
                        .find_outgoing_swapcoin_by_multisig(&swapcoin_multisig_redeemscript)
                        .expect("Outgoing swapcoin expected");
                    let signed_tx = sc.sign_timelock_recovery(tx_clone.clone())?;
                    input.witness = signed_tx.input[0].witness.clone();
                }
                UTXOSpendInfo::HashlockContract {
                    swapcoin_multisig_redeemscript,
                    ..
                } => {
                    let sc = self
                        .find_incoming_swapcoin_by_multisig(&swapcoin_multisig_redeemscript)
                        .expect("Incoming swapcoin expected");
                    let spend_tx = sc.sign_spend_transaction(
                        sc.funding_amount,
                        &tx.output[0].script_pubkey,
                        1.0,
                    )?;
                    input.witness = spend_tx.input[0].witness.clone();
                }
                UTXOSpendInfo::FidelityBondCoin { index, input_value } => {
                    let privkey = self.get_fidelity_keypair(index)?.secret_key();
                    let redeemscript = self.get_fidelity_reedemscript(index)?;
                    let sighash = SighashCache::new(&tx_clone).p2wsh_signature_hash(
                        ix,
                        &redeemscript,
                        input_value,
                        EcdsaSighashType::All,
                    )?;
                    let sig = secp.sign_ecdsa_low_r(
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
    #[hotpath::measure]
    pub fn coin_select(
        &self,
        amount: Amount,
        feerate: f64,
        output_address_type: AddressType,
        manually_selected_outpoints: Option<Vec<OutPoint>>,
        excluded_outpoints: Option<Vec<OutPoint>>,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        const LONG_TERM_FEERATE: f32 = 10.0;
        // (version 4 + input varint 1 + output varint 1 + locktime 4) * 4 + marker 1 + flag 1 = 42 WU
        const BASE_TXN_ONLY_WEIGHT: u64 = 42;
        // Non-witness data (multiplied by 4):
        // - Previous txid (32 bytes) * 4     = 128 WU
        // - Prev vout (4 bytes) * 4          = 16 WU
        // - Script length (1 byte) * 4       = 4 WU
        // - Empty scriptsig (0 bytes) * 4    = 0 WU
        // - nSequence (4 bytes) * 4          = 16 WU
        // Subtotal non-witness:              = 164 WU
        const INPUT_BASE_WEIGHT: u64 = (32 + 4 + 4 + 1) * 4;
        let output_script_pubkey_size: u64 = match output_address_type {
            // OP_0 (1 byte) + OP_PUSH_20 (1 byte) + 20-byte pubkey hash = 22
            AddressType::P2WPKH => 22,
            // OP_1 (1 byte) +  OP_PUSH_32 (1 byte) + 32-byte x-only pubkey = 34
            AddressType::P2TR => 34,
        };
        let target_output_weight = (Amount::SIZE as u64 + 1 + output_script_pubkey_size) * 4;
        // Change always goes to a P2TR address for now.
        // TODO : Have a combined policy for change to choose it's type depending on the wallet state.
        let change_output_weight = (Amount::SIZE as u64 + 1 + 34u64) * 4;

        // 1. Drop locked and explicitly excluded UTXOs from consideration.
        let locked_utxos = self.list_lock_unspent()?;
        let excluded: std::collections::HashSet<OutPoint> =
            excluded_outpoints.unwrap_or_default().into_iter().collect();
        let filter_locked = |utxos: Vec<(ListUnspentResultEntry, UTXOSpendInfo)>| {
            utxos
                .into_iter()
                .filter(|(utxo, _)| {
                    let outpoint = OutPoint::new(utxo.txid, utxo.vout);
                    !locked_utxos.contains(&outpoint) && !excluded.contains(&outpoint)
                })
                .collect::<Vec<_>>()
        };

        // 2. Segregate spendable UTXOs into two pools: regular and swap.
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

        let target = amount.to_sat();
        let target_feerate_wu = feerate as f32 / 4.0;

        let input_weight = |utxo_data: &(ListUnspentResultEntry, UTXOSpendInfo)| -> u64 {
            let (_, spend_info) = utxo_data;
            INPUT_BASE_WEIGHT + spend_info.estimate_witness_size() as u64
        };

        // 3. Validate manual selection: all outpoints must exist and stay within one pool.
        let manually_selected_outpoints =
            manually_selected_outpoints.filter(|outpoints| !outpoints.is_empty());

        let (manual_utxo_type, manual_outpoints) = if let Some(ref manual_outpoints) =
            manually_selected_outpoints
        {
            let requested_outpoints: HashSet<_> = manual_outpoints.iter().copied().collect();

            let matched_manual_regular_utxos = available_regular_utxos
                .iter()
                .filter(|(utxo, _)| {
                    requested_outpoints.contains(&OutPoint::new(utxo.txid, utxo.vout))
                })
                .collect::<Vec<_>>();
            let matched_manual_swap_utxos = available_swap_utxos
                .iter()
                .filter(|(utxo, _)| {
                    requested_outpoints.contains(&OutPoint::new(utxo.txid, utxo.vout))
                })
                .collect::<Vec<_>>();

            let matched_manual_count =
                matched_manual_regular_utxos.len() + matched_manual_swap_utxos.len();
            if matched_manual_count != requested_outpoints.len() {
                return Err(WalletError::General(
                    "Some manually selected UTXOs are unavailable, locked, or excluded".to_string(),
                ));
            }

            if !matched_manual_regular_utxos.is_empty() && !matched_manual_swap_utxos.is_empty() {
                return Err(WalletError::General(
                    "Cannot mix regular and swap UTXOs in manual selection".to_string(),
                ));
            }

            let utxo_type = if !matched_manual_regular_utxos.is_empty() {
                "regular"
            } else {
                "swap"
            };

            (Some(utxo_type), requested_outpoints)
        } else {
            (None, HashSet::new())
        };

        let change_weight = Weight::from_wu(change_output_weight);
        let cost_of_change = {
            let creation_cost = calculate_fee(change_weight.to_vbytes_ceil(), feerate as f32);
            let future_spending_cost =
                calculate_fee((INPUT_BASE_WEIGHT + 66).div_ceil(4), LONG_TERM_FEERATE);
            creation_cost + future_spending_cost
        };

        let tx_weight_with_selected_inputs =
            |selected_weight: u64| BASE_TXN_ONLY_WEIGHT + selected_weight + target_output_weight;
        let required_total = |selected_weight: u64| {
            target
                + calculate_fee(
                    tx_weight_with_selected_inputs(selected_weight),
                    target_feerate_wu,
                )
        };
        // 4. Try each pool in order: manual pins one pool, else regular first then swap.
        let utxo_types_to_try = if manual_utxo_type == Some("regular") {
            vec![("regular", &available_regular_utxos)]
        } else if manual_utxo_type == Some("swap") {
            vec![("swap", &available_swap_utxos)]
        } else {
            vec![
                ("regular", &available_regular_utxos),
                ("swap", &available_swap_utxos),
            ]
        };

        let mut insufficient_funds = None;
        let mut other_selection_error = None;
        for (utxo_type, unspents) in utxo_types_to_try {
            // 5. Force-include manually selected UTXOs; the rest are free candidates.
            let (forced_utxos, candidate_utxos): (Vec<&_>, Vec<&_>) =
                unspents.iter().partition(|(utxo, _)| {
                    let outpoint = OutPoint::new(utxo.txid, utxo.vout);
                    manual_outpoints.contains(&outpoint)
                });

            let unspents = candidate_utxos.into_iter().cloned().collect::<Vec<_>>();

            // 6. Group candidates by address so reused addresses are spent together.
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

            // 6. Split reused addresses (>1 UTXO) from singletons, both sorted ascending by value.
            let (mut grouped_addresses, mut single_addresses): (Vec<_>, Vec<_>) = address_groups
                .into_values()
                .partition(|group| group.len() > 1);

            grouped_addresses
                .sort_by_key(|group| group.iter().map(|(u, _)| u.amount.to_sat()).sum::<u64>());

            single_addresses
                .sort_by_key(|group| group.iter().map(|(u, _)| u.amount.to_sat()).sum::<u64>());

            let (selected_utxos, selected_total, selected_weight) = {
                let mut result_utxos = forced_utxos.into_iter().cloned().collect::<Vec<_>>();
                let mut result_total = result_utxos
                    .iter()
                    .map(|(utxo, _)| utxo.amount.to_sat())
                    .sum::<u64>();
                let mut result_weight = result_utxos.iter().map(&input_weight).sum::<u64>();

                let required_for_selected = required_total(result_weight);
                if result_total >= required_for_selected {
                    log::info!(
                        "Manual selection: Selected {} {} UTXOs (total: {} sats, target+fee: {} sats)",
                        result_utxos.len(),
                        utxo_type,
                        result_total,
                        required_for_selected
                    );
                    return Ok(result_utxos);
                }

                for group in grouped_addresses {
                    let group_total: u64 = group.iter().map(|(u, _)| u.amount.to_sat()).sum();
                    let group_weight: u64 = group.iter().map(&input_weight).sum();

                    result_total += group_total;
                    result_weight += group_weight;
                    result_utxos.extend(group);

                    let required_for_selected = required_total(result_weight);
                    if result_total >= required_for_selected {
                        log::info!(
                    "Address grouping: Selected {} {} UTXOs (total: {} sats, target+fee: {} sats)",
                    result_utxos.len(),
                    utxo_type,
                    result_total,
                    required_for_selected
                );
                        #[cfg(debug_assertions)]
                        log::debug!(
                            "[COIN_SELECTION] Source: wallet::api::coin_select | Wallet: {} | Type: {} | Inputs: {} | Selected: {} | TargetWithFee: {} | Strategy: grouped",
                            self.store.file_name,
                            utxo_type,
                            result_utxos.len(),
                            result_total,
                            required_for_selected
                        );
                        return Ok(result_utxos);
                    }
                }
                (result_utxos, result_total, result_weight)
            };

            // 7. Run rust-coinselect over the remaining single-address UTXOs.
            let single_output_groups = single_addresses
                .iter()
                .map(|single_address_utxos| {
                    let total_value: u64 = single_address_utxos
                        .iter()
                        .map(|(utxo, _)| utxo.amount.to_sat())
                        .sum();
                    let total_weight: u64 = single_address_utxos.iter().map(&input_weight).sum();

                    OutputGroup {
                        value: total_value,
                        weight: total_weight,
                        input_count: single_address_utxos.len(),
                        creation_sequence: None,
                    }
                })
                .collect::<Vec<_>>();

            let base_weight = tx_weight_with_selected_inputs(selected_weight);

            let remaining_target = target.saturating_sub(selected_total).max(1);

            let coin_selection_option = CoinSelectionOpt {
                target_value: remaining_target,
                target_feerate: target_feerate_wu, //sats per wu
                long_term_feerate: Some(LONG_TERM_FEERATE),
                min_absolute_fee: 0,
                base_weight,
                change_weight: change_weight.to_wu(),
                change_cost: cost_of_change,
                min_change_value: 330, // P2TR dust threshold (since P2WPKH's 294)
                excess_strategy: ExcessStrategy::ToChange,
            };

            match select_coin(&single_output_groups, &coin_selection_option) {
                Ok(results) => {
                    let (_, result) = results.into_iter().next().unwrap();
                    let _fee = result.fee;
                    let additional_utxos: Vec<_> = result
                        .selected_inputs
                        .iter()
                        .flat_map(|&group_index| single_addresses[group_index].clone())
                        .collect();

                    let mut final_selection = selected_utxos;
                    final_selection.extend(additional_utxos);

                    log::info!("Selected {} {utxo_type} UTXOs", final_selection.len());
                    #[cfg(debug_assertions)]
                    log::debug!(
                        "[COIN_SELECTION] Source: wallet::api::coin_select | Wallet: {} | Type: {} | Inputs: {} | Selected: {} | TargetWithFee: {} | Strategy: coinselect",
                        self.store.file_name,
                        utxo_type,
                        final_selection.len(),
                        final_selection
                            .iter()
                            .map(|(utxo, _)| utxo.amount.to_sat())
                            .sum::<u64>(),
                        required_total(
                            selected_weight
                                + result
                                    .selected_inputs
                                    .iter()
                                    .map(|&group_index| single_output_groups[group_index].weight)
                                    .sum::<u64>(),
                        )
                    );
                    return Ok(final_selection);
                }
                Err(e) => {
                    if let SelectionError::InsufficientFunds {
                        available,
                        required,
                    } = e
                    {
                        // coinselect sees (target - selected_total) as its target and
                        // base_weight already includes manual+grouped input weights.
                        // Add selected_total back to restore both to wallet-level figures.
                        let available = available + selected_total;
                        let required = required + selected_total;
                        // Each pool (regular, swap) may report insufficient funds. Keep the
                        // one with the smallest deficit (required - available) so the final
                        // error reflects the pool closest to covering the target, rather than
                        // whichever pool happened to be tried last.
                        let deficit = required.saturating_sub(available);
                        let is_better = insufficient_funds
                            .map(|(prev_avail, prev_req): (u64, u64)| {
                                deficit < prev_req.saturating_sub(prev_avail)
                            })
                            .unwrap_or(true);
                        if is_better {
                            insufficient_funds = Some((available, required));
                        }
                        log::warn!(
                            "Coin selection with {utxo_type} UTXOs failed: insufficient funds (available={available}, required={required})"
                        );
                    } else {
                        log::warn!("Coin selection with {utxo_type} UTXOs failed: {e:?}");
                        other_selection_error = Some(e);
                    }
                }
            }
        }
        // 8. All pools exhausted: report the pool that came closest to covering the target.
        if let Some((available, required)) = insufficient_funds {
            Err(WalletError::InsufficientFund {
                available,
                required,
            })
        } else if let Some(e) = other_selection_error {
            Err(WalletError::Selection(e))
        } else {
            Err(WalletError::General(
                "coin selection failed without returning an error".to_string(),
            ))
        }
    }

    pub(crate) fn create_and_import_coinswap_address(
        &mut self,
        other_pubkey: &PublicKey,
    ) -> Result<(Address, SecretKey), WalletError> {
        let (my_pubkey, my_privkey) = generate_keypair();

        let descriptor = self
            .rpc
            .get_descriptor_info(&format!("wsh(sortedmulti(2,{my_pubkey},{other_pubkey}))"))?
            .descriptor;
        self.import_descriptors(std::slice::from_ref(&descriptor), None, None)?;

        // redeemscript and descriptor show up in `getaddressinfo` only after
        // the address gets outputs on it-
        Ok((
            self.rpc.derive_addresses(&descriptor[..], None)?[0]
                .clone()
                .assume_checked(),
            my_privkey,
        ))
    }

    pub(crate) fn descriptors_to_import(&self) -> Result<Vec<String>, WalletError> {
        let mut descriptors_to_import = Vec::new();

        // Import both P2WPKH and P2TR descriptors to support both address types
        descriptors_to_import.extend(self.get_unimported_wallet_desc(AddressType::P2WPKH)?);
        descriptors_to_import.extend(self.get_unimported_wallet_desc(AddressType::P2TR)?);

        // Import swapcoin descriptors (Legacy only — multisig + contract redeemscripts)
        for sc in self.store.incoming_swapcoins.values() {
            if let (Some(my_pubkey), Some(other_pubkey)) = (sc.my_pubkey, sc.other_pubkey) {
                let descriptor_without_checksum =
                    format!("wsh(sortedmulti(2,{},{}))", other_pubkey, my_pubkey);
                descriptors_to_import.push(format!(
                    "{}#{}",
                    descriptor_without_checksum,
                    compute_checksum(&descriptor_without_checksum)?
                ));
            }
            if let Some(ref redeemscript) = sc.contract_redeemscript {
                let contract_spk = redeemscript_to_scriptpubkey(redeemscript)?;
                let descriptor_without_checksum = format!("raw({contract_spk:x})");
                descriptors_to_import.push(format!(
                    "{}#{}",
                    descriptor_without_checksum,
                    compute_checksum(&descriptor_without_checksum)?
                ));
            }
        }

        for sc in self.store.outgoing_swapcoins.values() {
            if let (Some(my_pubkey), Some(other_pubkey)) = (sc.my_pubkey, sc.other_pubkey) {
                let descriptor_without_checksum =
                    format!("wsh(sortedmulti(2,{},{}))", other_pubkey, my_pubkey);
                descriptors_to_import.push(format!(
                    "{}#{}",
                    descriptor_without_checksum,
                    compute_checksum(&descriptor_without_checksum)?
                ));
            }
            if let Some(ref redeemscript) = sc.contract_redeemscript {
                let contract_spk = redeemscript_to_scriptpubkey(redeemscript)?;
                let descriptor_without_checksum = format!("raw({contract_spk:x})");
                descriptors_to_import.push(format!(
                    "{}#{}",
                    descriptor_without_checksum,
                    compute_checksum(&descriptor_without_checksum)?
                ));
            }
        }

        descriptors_to_import.extend(
            self.store
                .fidelity_bond
                .iter()
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
    #[hotpath::measure]
    pub fn send_tx(&self, tx: &Transaction) -> Result<Txid, WalletError> {
        Ok(self.rpc.send_raw_transaction(tx)?)
    }
    /// Sweeps all completed incoming swap coins.
    #[hotpath::measure]
    pub fn sweep_incoming_swapcoins(
        &mut self,
        feerate: f64,
    ) -> Result<RecoveryOutcome, WalletError> {
        let mut outcome = RecoveryOutcome::default();

        let completed_swapcoins: Vec<_> = self
            .store
            .incoming_swapcoins
            .iter()
            .filter(|(_, swapcoin)| {
                swapcoin.other_privkey.is_some() || swapcoin.hash_preimage.is_some()
            })
            .map(|(swap_id, swapcoin)| (swap_id.clone(), swapcoin.clone()))
            .collect();

        if completed_swapcoins.is_empty() {
            log::info!("No completed incoming swap coins to sweep");
            return Ok(outcome);
        }

        log::info!(
            "Sweeping {} completed incoming swap coins",
            completed_swapcoins.len()
        );

        self.sync_and_save()?;

        let internal_addresses =
            self.get_next_internal_addresses(completed_swapcoins.len() as u32, AddressType::P2TR)?;
        for (i, (swap_id, swapcoin)) in completed_swapcoins.into_iter().enumerate() {
            let contract_txid = swapcoin.contract_tx.compute_txid();
            // Determine which UTXO to spend based on protocol and spending path.
            let (utxo_txid, utxo_vout, input_value) = match swapcoin.protocol {
                crate::protocol::ProtocolVersion::Legacy => {
                    if swapcoin.other_privkey.is_some() {
                        // Legacy cooperative: spend from funding output
                        let funding_outpoint = match swapcoin.contract_tx.input.first() {
                            Some(input) => input.previous_output,
                            None => {
                                log::warn!(
                                    "Contract tx has no input for swap {} - skipping sweep",
                                    swap_id
                                );
                                continue;
                            }
                        };
                        (
                            funding_outpoint.txid,
                            funding_outpoint.vout,
                            swapcoin.funding_amount,
                        )
                    } else {
                        // Legacy hashlock: spend from contract output
                        let contract_txid = swapcoin.contract_tx.compute_txid();
                        let contract_output = match swapcoin.contract_tx.output.first() {
                            Some(output) => output,
                            None => {
                                log::warn!(
                                    "No output found in contract tx for swap {} - skipping sweep",
                                    swap_id
                                );
                                continue;
                            }
                        };
                        (contract_txid, 0, contract_output.value)
                    }
                }
                crate::protocol::ProtocolVersion::Taproot => {
                    // Taproot: contract_tx IS the funding tx, spend from its P2TR output.
                    // Find the correct output index by matching the funding amount.
                    let contract_txid = swapcoin.contract_tx.compute_txid();
                    let vout = swapcoin
                        .contract_tx
                        .output
                        .iter()
                        .position(|o| o.value == swapcoin.funding_amount)
                        .unwrap_or(0) as u32;
                    (contract_txid, vout, swapcoin.funding_amount)
                }
            };

            // Verify the UTXO actually exists on chain before attempting to spend.
            // First check confirmed UTXOs, then fall back to mempool.
            let utxo_confirmed = matches!(
                self.rpc.get_tx_out(&utxo_txid, utxo_vout, Some(false)),
                Ok(Some(_))
            );

            if !utxo_confirmed {
                // UTXO not yet confirmed. Check if it's at least in the mempool.
                let in_mempool = matches!(
                    self.rpc.get_tx_out(&utxo_txid, utxo_vout, None),
                    Ok(Some(_))
                );

                if in_mempool {
                    // The incoming contract tx is broadcast but unconfirmed.
                    // Wait for it to confirm before sweeping.
                    log::info!(
                        "Incoming contract tx {}:{} is in mempool for {} — waiting for confirmation",
                        utxo_txid,
                        utxo_vout,
                        swap_id
                    );
                    // Poll get_tx_out with confirmed-only until the UTXO appears.
                    // We can't use wait_for_tx_confirmation here because that
                    // requires the tx to be in our wallet's transaction history,
                    // but this tx was broadcast by another party.
                    let mut wait_secs = 0u64;
                    loop {
                        if matches!(
                            self.rpc.get_tx_out(&utxo_txid, utxo_vout, Some(false)),
                            Ok(Some(_))
                        ) {
                            log::info!(
                                "Incoming contract tx {}:{} confirmed for {}",
                                utxo_txid,
                                utxo_vout,
                                swap_id
                            );
                            break;
                        }
                        wait_secs += 10;
                        if wait_secs > 600 {
                            log::warn!(
                                "Timed out waiting for contract tx {}:{} to confirm for {}",
                                utxo_txid,
                                utxo_vout,
                                swap_id
                            );
                            break;
                        }
                        log::info!(
                            "Still waiting for {}:{} to confirm ({}s elapsed)",
                            utxo_txid,
                            utxo_vout,
                            wait_secs
                        );
                        std::thread::sleep(std::time::Duration::from_secs(10));
                    }
                } else if swapcoin.other_privkey.is_none() && swapcoin.others_contract_sig.is_some()
                {
                    log::info!(
                        "Contract output not on-chain for {} — broadcasting signed contract tx",
                        swap_id
                    );
                    match swapcoin.create_signed_contract_tx() {
                        Ok(signed_contract_tx) => match self.send_tx(&signed_contract_tx) {
                            Ok(txid) => {
                                log::info!(
                                    "Broadcast incoming contract tx {} for {}",
                                    txid,
                                    swap_id
                                );
                            }
                            Err(e) => {
                                log::warn!(
                                    "Failed to broadcast incoming contract tx for {}: {:?}",
                                    swap_id,
                                    e
                                );
                                continue;
                            }
                        },
                        Err(e) => {
                            log::warn!(
                                "Failed to create signed incoming contract tx for {}: {:?}",
                                swap_id,
                                e
                            );
                            continue;
                        }
                    }

                    // Re-check UTXO availability (including mempool) after broadcast
                    let utxo_available = matches!(
                        self.rpc.get_tx_out(&utxo_txid, utxo_vout, Some(true)),
                        Ok(Some(_))
                    );
                    if !utxo_available {
                        log::info!(
                            "Contract output still not available for {} after broadcast — will retry later",
                            swap_id
                        );
                        continue;
                    }
                } else {
                    log::info!(
                        "Skipping sweep for {} - UTXO not available on chain",
                        swap_id
                    );
                    continue;
                }
            }

            // Receive the swept funds at this coin's pre-allocated distinct address.
            let internal_address = internal_addresses[i].clone();

            log::info!(
                "Sweeping incoming swap coin {} (utxo: {}:{}) to internal address {}",
                swap_id,
                utxo_txid,
                utxo_vout,
                internal_address
            );

            match swapcoin.sign_spend_transaction(
                input_value,
                &internal_address.script_pubkey(),
                feerate,
            ) {
                Ok(spend_tx) => {
                    match self.send_tx(&spend_tx) {
                        Ok(txid) => {
                            let conf_height =
                                self.wait_for_tx_confirmation(&[txid], 1, None, None)?;
                            log::info!(
                                "Sweep transaction {} confirmed at blockheight: {}",
                                txid,
                                conf_height
                            );

                            outcome.resolved.push((contract_txid, txid));
                            log::info!("Successfully swept incoming swap coin: {}", swap_id);

                            // Remove the swapcoin from wallet
                            self.remove_incoming_swapcoin(&swap_id);

                            // Track the output scriptpubkey to prevent mixing with regular UTXOs
                            let output_scriptpubkey = internal_address.script_pubkey();
                            self.store
                                .swept_incoming_swapcoins
                                .insert(output_scriptpubkey);
                        }
                        Err(e) => {
                            log::warn!(
                                "Failed to broadcast sweep tx for swapcoin {}: {:?}",
                                swap_id,
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "Failed to create spend tx for swapcoin {}: {:?}",
                        swap_id,
                        e
                    );
                }
            }
        }

        self.save_to_disk()?;
        if !outcome.is_empty() {
            #[cfg(debug_assertions)]
            log::debug!(
                "[RECOVERY_STATE] Wallet: {} | Action: sweep_incoming | Resolved: {} | Discarded: {} | IncomingRemaining: {}",
                self.store.file_name,
                outcome.resolved.len(),
                outcome.discarded.len(),
                self.store.incoming_swapcoins.len()
            );
        }
        Ok(outcome)
    }

    /// Wait for one or more transactions to reach the required number of confirmations.
    ///
    /// Returns the highest block height at which any of the transactions was confirmed
    /// (i.e. `current_height - confirmations + 1`). Returns 0 if `required_confirms` is 0
    /// or `txids` is empty.
    ///
    /// If a `shutdown` flag is provided, the wait is interrupted when it becomes `true`.
    /// If an `abort_check` is provided, it is polled during the wait; if it returns `true`,
    /// the wait is interrupted.
    #[hotpath::measure]
    pub fn wait_for_tx_confirmation(
        &self,
        txids: &[Txid],
        required_confirms: u32,
        shutdown: Option<&std::sync::atomic::AtomicBool>,
        abort_check: Option<&dyn Fn() -> bool>,
    ) -> Result<u32, WalletError> {
        if required_confirms == 0 || txids.is_empty() {
            return Ok(0);
        }

        log::info!(
            "Waiting for {} confirmation(s) on {} transaction(s)...",
            required_confirms,
            txids.len()
        );

        // cap at ~1 block interval
        let max_backoff_secs: u64 = 600;
        let sleep_increment_secs: u64 = 10;
        let mut attempt: u64 = 0;

        loop {
            if shutdown.is_some_and(|s| s.load(std::sync::atomic::Ordering::Relaxed)) {
                return Err(WalletError::Interrupted("Shutdown requested"));
            }
            if abort_check.is_some_and(|f| f()) {
                return Err(WalletError::Interrupted("Abort requested"));
            }

            attempt = attempt.saturating_add(1);

            let mut all_confirmed = true;
            let mut max_confirm_height: u32 = 0;

            for txid in txids {
                match self.rpc.get_raw_transaction_info(txid, None) {
                    Ok(tx_info) => {
                        let confirms: u32 = tx_info.confirmations.unwrap_or(0);
                        if confirms < required_confirms {
                            log::debug!(
                                "Tx {} has {} confirmations (need {})",
                                txid,
                                confirms,
                                required_confirms
                            );
                            all_confirmed = false;
                        } else {
                            // QA: Derive the mined height from the block itself;
                            // tip-height arithmetic can race with a newly mined block.
                            let block_hash = tx_info.blockhash.ok_or_else(|| {
                                WalletError::General(format!(
                                    "Confirmed transaction {txid} has no block hash"
                                ))
                            })?;
                            let confirm_height =
                                self.rpc.get_block_header_info(&block_hash)?.height as u32;
                            max_confirm_height = max_confirm_height.max(confirm_height);
                        }
                    }
                    Err(e) => {
                        log::debug!("Error getting tx info for {}: {:?}", txid, e);
                        all_confirmed = false;
                    }
                }
            }

            if all_confirmed {
                log::info!(
                    "All transactions confirmed (latest at height {})",
                    max_confirm_height
                );
                return Ok(max_confirm_height);
            }

            let total_sleep = sleep_increment_secs
                .saturating_mul(attempt)
                .min(max_backoff_secs);
            log::info!("Next sync in {} secs", total_sleep);

            // Sleep in 1-second increments so we can check shutdown/abort.
            for _ in 0..total_sleep {
                if shutdown.is_some_and(|s| s.load(std::sync::atomic::Ordering::Relaxed)) {
                    return Err(WalletError::Interrupted("Shutdown requested"));
                }
                if abort_check.is_some_and(|f| f()) {
                    return Err(WalletError::Interrupted("Abort requested"));
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    }

    /// Separates coins by sending them to a specific address with a large OP_RETURN.
    /// This is useful to split coins across chain forks (e.g., Core vs BIP-128).
    pub fn separate_chain_coins(
        &mut self,
        feerate: f64,
        destination_address: Option<Address>,
        op_return_payload: &[u8],
    ) -> Result<Txid, WalletError> {
        let mut coins_to_spend = self.list_descriptor_utxo_spend_info();
        coins_to_spend.extend(self.list_swept_incoming_swap_utxos());

        if coins_to_spend.is_empty() {
            return Err(WalletError::General("No spendable coins to separate".to_owned()));
        }

        let change_type = AddressType::P2WPKH;
        let dest_addr = match destination_address {
            Some(addr) => addr,
            None => self.get_next_internal_addresses(1, change_type)?[0].clone(),
        };

        let destination = crate::wallet::spend::Destination::Sweep(dest_addr, Some(op_return_payload.into()));
        
        let tx = self.spend_coins(&coins_to_spend, destination, feerate)?;
        let txid = self.send_tx(&tx)?;

        log::info!("Separation transaction broadcasted. txid: {}", txid);
        Ok(txid)
    }
}
