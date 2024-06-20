//! The Wallet API.
//!
//! Currently, wallet synchronization is exclusively performed through RPC for makers.
//! In the future, takers might adopt alternative synchronization methods, such as lightweight wallet solutions.

use std::{convert::TryFrom, fs, path::PathBuf, str::FromStr};

use std::collections::{HashMap, HashSet};

use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Xpub},
    hashes::{hash160::Hash as Hash160, hex::FromHex},
    secp256k1,
    secp256k1::{Secp256k1, SecretKey},
    sighash::{EcdsaSighashType, SighashCache},
    Address, Amount, OutPoint, PublicKey, Script, ScriptBuf, Transaction, Txid,
};

use bitcoind::bitcoincore_rpc::{bitcoincore_rpc_json::ListUnspentResultEntry, Client, RpcApi};

use crate::{
    protocol::contract,
    utill::{
        compute_checksum, generate_keypair, get_hd_path_from_descriptor,
        redeemscript_to_scriptpubkey,
    },
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
pub struct Wallet {
    pub(crate) rpc: Client,
    wallet_file_path: PathBuf,
    pub(crate) store: WalletStore,
}

/// Speicfy the keychain derivation path from [`HARDENDED_DERIVATION`]
/// Each kind represents an unhardened index value. Starting with External = 0.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum KeychainKind {
    External = 0isize,
    Internal,
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

/// Enum representing different types of addresses to display.
#[derive(Clone, PartialEq, Debug)]
pub enum DisplayAddressType {
    /// Display all types of addresses.
    All,
    /// Display information related to the master key.
    MasterKey,
    /// Display addresses derived from the seed.
    Seed,
    /// Display information related to incoming swap transactions.
    IncomingSwap,
    /// Display information related to outgoing swap transactions.
    OutgoingSwap,
    /// Display information related to swap transactions (both incoming and outgoing).
    Swap,
    /// Display information related to incoming contract transactions.
    IncomingContract,
    /// Display information related to outgoing contract transactions.
    OutgoingContract,
    /// Display information related to contract transactions (both incoming and outgoing).
    Contract,
    /// Display information related to fidelity bonds.
    FidelityBond,
}

impl FromStr for DisplayAddressType {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "all" => DisplayAddressType::All,
            "masterkey" => DisplayAddressType::MasterKey,
            "seed" => DisplayAddressType::Seed,
            "incomingswap" => DisplayAddressType::IncomingSwap,
            "outgoingswap" => DisplayAddressType::OutgoingSwap,
            "swap" => DisplayAddressType::Swap,
            "incomingcontract" => DisplayAddressType::IncomingContract,
            "outgoingcontract" => DisplayAddressType::OutgoingContract,
            "contract" => DisplayAddressType::Contract,
            "fidelitybond" => DisplayAddressType::FidelityBond,
            _ => Err("unknown type")?,
        })
    }
}

/// Enum representing additional data needed to spend a UTXO, in addition to `ListUnspentResultEntry`.
// data needed to find information  in addition to ListUnspentResultEntry
// about a UTXO required to spend it
#[derive(Debug, Clone)]
pub enum UTXOSpendInfo {
    SeedCoin {
        path: String,
        input_value: u64,
    },
    SwapCoin {
        multisig_redeemscript: ScriptBuf,
    },
    TimelockContract {
        swapcoin_multisig_redeemscript: ScriptBuf,
        input_value: u64,
    },
    HashlockContract {
        swapcoin_multisig_redeemscript: ScriptBuf,
        input_value: u64,
    },
    FidelityBondCoin {
        index: u32,
        input_value: u64,
    },
}

// Custom type to handle complex return values.
type SwapCoinsInfo<'a> = (
    Vec<(&'a IncomingSwapCoin, ListUnspentResultEntry)>,
    Vec<(&'a OutgoingSwapCoin, ListUnspentResultEntry)>,
);

impl Wallet {
    /// Initialize the wallet at a given path.
    pub fn init(
        path: &PathBuf,
        rpc_config: &RPCConfig,
        seedphrase: String,
        passphrase: String,
    ) -> Result<Self, WalletError> {
        let file_name = path
            .file_name()
            .expect("file name expected")
            .to_str()
            .expect("expected")
            .to_string();
        let rpc = Client::try_from(rpc_config)?;
        let wallet_birthday = rpc.get_block_count()?;
        let store = WalletStore::init(
            file_name,
            path,
            rpc_config.network,
            seedphrase,
            passphrase,
            Some(wallet_birthday),
        )?;
        Ok(Self {
            rpc,
            wallet_file_path: path.clone(),
            store,
        })
    }

    /// Load wallet data from file and connects to a core RPC.
    /// The core rpc wallet name, and wallet_id field in the file should match.
    pub fn load(rpc_config: &RPCConfig, path: &PathBuf) -> Result<Wallet, WalletError> {
        let store = WalletStore::read_from_disk(path)?;
        if rpc_config.wallet_name != store.file_name {
            return Err(WalletError::Protocol(format!(
                "Wallet name of database file and core missmatch, expected {}, found {}",
                rpc_config.wallet_name, store.file_name
            )));
        }
        let rpc = Client::try_from(rpc_config)?;
        log::info!(
            "Loaded wallet file {} | External Index = {} | Incoming Swapcoins = {} | Outgoing Swapcoins = {}",
            store.file_name,
            store.external_index,
            store.incoming_swapcoins.len(),
            store.outgoing_swapcoins.len()
        );
        let wallet = Self {
            rpc,
            wallet_file_path: path.clone(),
            store,
        };
        Ok(wallet)
    }

    /// Deletes the wallet file and returns the result as `Ok(())` on success.
    pub fn delete_wallet_file(&self) -> Result<(), WalletError> {
        Ok(fs::remove_file(&self.wallet_file_path)?)
    }

    /// Returns a reference to the file path of the wallet.
    pub fn get_file_path(&self) -> &PathBuf {
        &self.wallet_file_path
    }

    /// Update external index and saves to disk.
    pub fn update_external_index(&mut self, new_external_index: u32) -> Result<(), WalletError> {
        self.store.external_index = new_external_index;
        self.save_to_disk()
    }

    // pub fn get_external_index(&self) -> u32 {
    //     self.external_index
    // }

    /// Update the existing file. Error if path does not exist.
    pub fn save_to_disk(&self) -> Result<(), WalletError> {
        self.store.write_to_disk(&self.wallet_file_path)
    }

    /// Finds an incoming swap coin with the specified multisig redeem script.
    pub fn find_incoming_swapcoin(
        &self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&IncomingSwapCoin> {
        self.store.incoming_swapcoins.get(multisig_redeemscript)
    }

    /// Finds an outgoing swap coin with the specified multisig redeem script.
    pub fn find_outgoing_swapcoin(
        &self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&OutgoingSwapCoin> {
        self.store.outgoing_swapcoins.get(multisig_redeemscript)
    }

    /// Finds a mutable reference to an incoming swap coin with the specified multisig redeem script.
    pub fn find_incoming_swapcoin_mut(
        &mut self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Option<&mut IncomingSwapCoin> {
        self.store.incoming_swapcoins.get_mut(multisig_redeemscript)
    }

    /// Adds an incoming swap coin to the wallet.
    pub fn add_incoming_swapcoin(&mut self, coin: &IncomingSwapCoin) {
        self.store
            .incoming_swapcoins
            .insert(coin.get_multisig_redeemscript(), coin.clone());
    }

    /// Adds an outgoing swap coin to the wallet.
    pub fn add_outgoing_swapcoin(&mut self, coin: &OutgoingSwapCoin) {
        self.store
            .outgoing_swapcoins
            .insert(coin.get_multisig_redeemscript(), coin.clone());
    }

    /// Removes an incoming swap coin with the specified multisig redeem script from the wallet.
    pub fn remove_incoming_swapcoin(
        &mut self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Result<Option<IncomingSwapCoin>, WalletError> {
        Ok(self.store.incoming_swapcoins.remove(multisig_redeemscript))
    }

    /// Removes an outgoing swap coin with the specified multisig redeem script from the wallet.
    pub fn remove_outgoing_swapcoin(
        &mut self,
        multisig_redeemscript: &ScriptBuf,
    ) -> Result<Option<OutgoingSwapCoin>, WalletError> {
        Ok(self.store.outgoing_swapcoins.remove(multisig_redeemscript))
    }

    /// Gets a reference to the list of incoming swap coins in the wallet.
    pub fn get_incoming_swapcoin_list(
        &self,
    ) -> Result<&HashMap<ScriptBuf, IncomingSwapCoin>, WalletError> {
        Ok(&self.store.incoming_swapcoins)
    }

    /// Gets a reference to the list of outgoing swap coins in the wallet.
    pub fn get_outgoing_swapcoin_list(
        &self,
    ) -> Result<&HashMap<ScriptBuf, OutgoingSwapCoin>, WalletError> {
        Ok(&self.store.outgoing_swapcoins)
    }

    /// Gets the total count of swap coins in the wallet.
    pub fn get_swapcoins_count(&self) -> usize {
        self.store.incoming_swapcoins.len() + self.store.outgoing_swapcoins.len()
    }

    /// Calculates the total balance of the wallet, including swap coins, live contracts and fidelity bonds.
    pub fn balance(&self) -> Result<Amount, WalletError> {
        Ok(self
            .list_all_utxo_spend_info(None)?
            .iter()
            .fold(Amount::ZERO, |a, (utxo, _)| a + utxo.amount))
    }

    /// Calculates the fidelity balance of the wallet.
    /// Optionally takes in a list of UTXOs to reduce rpc call. If None is provided, the full list is fetched from core rpc.
    pub fn balance_fidelity_bonds(
        &self,
        utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Amount, WalletError> {
        Ok(self
            .list_fidelity_spend_info(utxos)?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount))
    }

    /// Calculates live contract balance of the wallet.
    /// Optionally takes in a list of UTXOs to reduce rpc call. If None is provided, the full list is fetched from core rpc.
    pub fn balance_live_contract(
        &self,
        all_utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Amount, WalletError> {
        Ok(self
            .list_live_contract_spend_info(all_utxos)?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount))
    }

    /// Calculates the descriptor utxo balance of the wallet.
    /// Optionally takes in a list of UTXOs to reduce rpc call. If None is provided, the full list is fetched from core rpc.
    pub fn balance_descriptor_utxo(
        &self,
        all_utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Amount, WalletError> {
        Ok(self
            .list_descriptor_utxo_spend_info(all_utxos)?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount))
    }

    /// Calculates the swap coin balance of the wallet.
    /// Optionally takes in a list of UTXOs to reduce rpc call. If None is provided, the full list is fetched from core rpc.
    pub fn balance_swap_coins(
        &self,
        all_utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Amount, WalletError> {
        Ok(self
            .list_swap_coin_utxo_spend_info(all_utxos)?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount))
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
    pub fn does_prevout_match_cached_contract(
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
    pub fn get_addrss_import_count(&self) -> u32 {
        if cfg!(feature = "integration-test") {
            10
        } else {
            5000
        }
    }

    /// Stores an entry into [`WalletStore`]'s prevout-to-contract map.
    /// If the prevout already existed with a contract script, this will update the existing contract.
    pub fn cache_prevout_to_contract(
        &mut self,
        prevout: OutPoint,
        contract: ScriptBuf,
    ) -> Result<(), WalletError> {
        if let Some(contract) = self.store.prevout_to_contract_map.insert(prevout, contract) {
            log::warn!(
                "Prevout to Contract map updated.\nExisting Contract: {}",
                contract
            );
        }
        Ok(())
    }

    //pub fn get_recovery_phrase_from_file()

    /// Wallet descriptors are derivable. Currently only supports two KeychainKind. Internal and External.
    fn get_wallet_descriptors(&self) -> Result<HashMap<KeychainKind, String>, WalletError> {
        let secp = Secp256k1::new();
        let wallet_xpub = Xpub::from_priv(
            &secp,
            &self
                .store
                .master_key
                .derive_priv(
                    &secp,
                    &DerivationPath::from_str(HARDENDED_DERIVATION).unwrap(),
                )
                .unwrap(),
        );

        // Get descriptors for external and internal keychain. Other chains are not supported yet.
        let x = [KeychainKind::External, KeychainKind::Internal]
            .iter()
            .map(|keychain| {
                let descriptor_without_checksum =
                    format!("wpkh({}/{}/*)", wallet_xpub, keychain.index_num());
                let decriptor = format!(
                    "{}#{}",
                    descriptor_without_checksum,
                    compute_checksum(&descriptor_without_checksum).unwrap()
                );
                (*keychain, decriptor)
            })
            .collect::<HashMap<KeychainKind, String>>();

        Ok(x)
        //descriptors.map_err(|e| TeleportError::Rpc(e))
    }

    /// Checks if the addresses derived from the wallet descriptor is imported upto full index range.
    /// Returns the list of descriptors not imported yet
    /// Index range depend on [`WalletMode`].
    /// Normal => 5000
    /// Test => 6
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

    /// Core wallet label is the master XPub fingerint.
    pub fn get_core_wallet_label(&self) -> String {
        let secp = Secp256k1::new();
        let m_xpub = Xpub::from_priv(&secp, &self.store.master_key);
        m_xpub.fingerprint().to_string()
    }

    fn create_contract_scriptpubkey_outgoing_swapcoin_hashmap(
        &self,
    ) -> HashMap<ScriptBuf, &OutgoingSwapCoin> {
        self.store
            .outgoing_swapcoins
            .values()
            .map(|osc| {
                (
                    redeemscript_to_scriptpubkey(&osc.contract_redeemscript),
                    osc,
                )
            })
            .collect::<HashMap<_, _>>()
    }

    fn create_contract_scriptpubkey_incoming_swapcoin_hashmap(
        &self,
    ) -> HashMap<ScriptBuf, &IncomingSwapCoin> {
        self.store
            .incoming_swapcoins
            .values()
            .map(|isc| {
                (
                    redeemscript_to_scriptpubkey(&isc.contract_redeemscript),
                    isc,
                )
            })
            .collect::<HashMap<_, _>>()
    }

    /// Locks the fidelity and live_contract utxos which are not considered for spending from the wallet.
    pub fn lock_unspendable_utxos(&self) -> Result<(), WalletError> {
        self.rpc.unlock_unspent_all()?;

        let all_unspents = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        let utxos_to_lock = &all_unspents
            .into_iter()
            .filter(|u| self.check_descriptor_utxo_or_swap_coin(u).is_none())
            .map(|u| OutPoint {
                txid: u.txid,
                vout: u.vout,
            })
            .collect::<Vec<OutPoint>>();
        self.rpc.lock_unspent(utxos_to_lock)?;
        Ok(())
    }

    /// Checks if a UTXO belongs to fidelity bonds, and then returns corresponding UTXOSpendInfo
    fn check_if_fidelity(&self, utxo: &ListUnspentResultEntry) -> Option<UTXOSpendInfo> {
        self.store
            .fidelity_bond
            .iter()
            .find_map(|(i, (bond, _, _))| {
                if bond.script_pub_key() == utxo.script_pub_key
                    && bond.amount == utxo.amount.to_sat()
                {
                    Some(UTXOSpendInfo::FidelityBondCoin {
                        index: *i,
                        input_value: bond.amount,
                    })
                } else {
                    None
                }
            })
    }

    /// Checks if a UTXO belongs to live contracts, and then returns corresponding UTXOSpendInfo
    fn check_if_live_contract(&self, utxo: &ListUnspentResultEntry) -> Option<UTXOSpendInfo> {
        let (contract_scriptpubkeys_outgoing, contract_scriptpubkeys_incoming) = (
            self.create_contract_scriptpubkey_outgoing_swapcoin_hashmap(),
            self.create_contract_scriptpubkey_incoming_swapcoin_hashmap(),
        );

        if let Some(outgoing_swapcoin) = contract_scriptpubkeys_outgoing.get(&utxo.script_pub_key) {
            if utxo.confirmations >= outgoing_swapcoin.get_timelock().into() {
                return Some(UTXOSpendInfo::TimelockContract {
                    swapcoin_multisig_redeemscript: outgoing_swapcoin.get_multisig_redeemscript(),
                    input_value: utxo.amount.to_sat(),
                });
            }
        } else if let Some(incoming_swapcoin) =
            contract_scriptpubkeys_incoming.get(&utxo.script_pub_key)
        {
            if incoming_swapcoin.is_hash_preimage_known() && utxo.confirmations >= 1 {
                return Some(UTXOSpendInfo::HashlockContract {
                    swapcoin_multisig_redeemscript: incoming_swapcoin.get_multisig_redeemscript(),
                    input_value: utxo.amount.to_sat(),
                });
            }
        }
        None
    }

    /// Checks if a UTXO belongs to descriptor or swap coin, and then returns corresponding UTXOSpendInfo
    fn check_descriptor_utxo_or_swap_coin(
        &self,
        utxo: &ListUnspentResultEntry,
    ) -> Option<UTXOSpendInfo> {
        if let Some(descriptor) = &utxo.descriptor {
            // Descriptor logic here
            if let Some(ret) = get_hd_path_from_descriptor(descriptor) {
                //utxo is in a hd wallet
                let (fingerprint, addr_type, index) = ret;

                let secp = Secp256k1::new();
                let master_private_key = self
                    .store
                    .master_key
                    .derive_priv(
                        &secp,
                        &DerivationPath::from_str(HARDENDED_DERIVATION).unwrap(),
                    )
                    .unwrap();
                if fingerprint == master_private_key.fingerprint(&secp).to_string() {
                    return Some(UTXOSpendInfo::SeedCoin {
                        path: format!("m/{}/{}", addr_type, index),
                        input_value: utxo.amount.to_sat(),
                    });
                }
            } else {
                //utxo might be one of our swapcoins
                let found = self
                    .find_incoming_swapcoin(
                        utxo.witness_script
                            .as_ref()
                            .unwrap_or(&ScriptBuf::from(Vec::from_hex("").unwrap())),
                    )
                    .map_or(false, |sc| sc.other_privkey.is_some())
                    || self
                        .find_outgoing_swapcoin(
                            utxo.witness_script
                                .as_ref()
                                .unwrap_or(&ScriptBuf::from(Vec::from_hex("").unwrap())),
                        )
                        .map_or(false, |sc| sc.hash_preimage.is_some());
                if found {
                    return Some(UTXOSpendInfo::SwapCoin {
                        multisig_redeemscript: utxo.witness_script.as_ref().unwrap().clone(),
                    });
                }
            };
        }
        None
    }

    /// Returns a list of all UTXOs tracked by the wallet. Including fidelity, live_contracts and swap coins.
    pub fn get_all_utxo(&self) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        self.rpc.unlock_unspent_all()?;
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        Ok(all_utxos)
    }

    pub fn get_all_locked_utxo(&self) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        Ok(all_utxos)
    }
    /// Returns a list all utxos with their spend info tracked by the wallet.
    /// Optionally takes in an Utxo list to reduce RPC calls. If None is given, the
    /// full list of utxo is fetched from core rpc.
    pub fn list_all_utxo_spend_info(
        &self,
        utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let owned_utxo: Option<Vec<ListUnspentResultEntry>> = if utxos.is_none() {
            Some(self.get_all_utxo()?)
        } else {
            None
        };

        let all_utxos = utxos.unwrap_or_else(|| owned_utxo.as_ref().unwrap());

        let processed_utxos = all_utxos
            .iter()
            .filter_map(|utxo| {
                let mut spend_info = self.check_if_fidelity(utxo);
                if spend_info.is_none() {
                    spend_info = self.check_if_live_contract(utxo);
                }
                if spend_info.is_none() {
                    spend_info = self.check_descriptor_utxo_or_swap_coin(utxo);
                }
                spend_info.map(|info| (utxo.clone(), info))
            })
            .collect::<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>>();

        Ok(processed_utxos)
    }

    /// Lists live contract UTXOs along with their [UTXOSpendInfo].
    pub fn list_live_contract_spend_info(
        &self,
        all_utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info(all_utxos)?;
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| {
                matches!(x.1, UTXOSpendInfo::HashlockContract { .. })
                    || matches!(x.1, UTXOSpendInfo::TimelockContract { .. })
            })
            .cloned()
            .collect();
        Ok(filtered_utxos)
    }

    /// Lists fidelity UTXOs along with their [UTXOSpendInfo].
    pub fn list_fidelity_spend_info(
        &self,
        all_utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info(all_utxos)?;
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::FidelityBondCoin { .. }))
            .cloned()
            .collect();
        Ok(filtered_utxos)
    }

    /// Lists descriptor UTXOs along with their [UTXOSpendInfo].
    pub fn list_descriptor_utxo_spend_info(
        &self,
        all_utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info(all_utxos)?;
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::SeedCoin { .. }))
            .cloned()
            .collect();
        Ok(filtered_utxos)
    }

    /// Lists swap coin UTXOs along with their [UTXOSpendInfo].
    pub fn list_swap_coin_utxo_spend_info(
        &self,
        all_utxos: Option<&Vec<ListUnspentResultEntry>>,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info(all_utxos)?;
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::SwapCoin { .. }))
            .cloned()
            .collect();
        Ok(filtered_utxos)
    }

    /// Finds incomplete coin swaps in the wallet.
    pub fn find_incomplete_coinswaps(
        &self,
    ) -> Result<HashMap<Hash160, SwapCoinsInfo>, WalletError> {
        self.rpc.unlock_unspent_all()?;

        let completed_coinswap_hashvalues = self
            .store
            .incoming_swapcoins
            .values()
            .filter(|sc| sc.other_privkey.is_some())
            .map(|sc| sc.get_hashvalue())
            .collect::<HashSet<Hash160>>();

        let mut incomplete_swapcoin_groups = HashMap::<Hash160, SwapCoinsInfo>::new();
        let get_hashvalue = |s: &dyn SwapCoin| {
            let swapcoin_hashvalue = s.get_hashvalue();
            if completed_coinswap_hashvalues.contains(&swapcoin_hashvalue) {
                return None;
            }
            Some(swapcoin_hashvalue)
        };
        for utxo in self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?
        {
            if utxo.descriptor.is_none() {
                continue;
            }
            let multisig_redeemscript = if let Some(rs) = utxo.witness_script.as_ref() {
                rs
            } else {
                continue;
            };
            if let Some(s) = self.find_incoming_swapcoin(multisig_redeemscript) {
                if let Some(swapcoin_hashvalue) = get_hashvalue(s) {
                    incomplete_swapcoin_groups
                        .entry(swapcoin_hashvalue)
                        .or_insert((
                            Vec::<(&IncomingSwapCoin, ListUnspentResultEntry)>::new(),
                            Vec::<(&OutgoingSwapCoin, ListUnspentResultEntry)>::new(),
                        ))
                        .0
                        .push((s, utxo));
                }
            } else if let Some(s) = self.find_outgoing_swapcoin(multisig_redeemscript) {
                if let Some(swapcoin_hashvalue) = get_hashvalue(s) {
                    incomplete_swapcoin_groups
                        .entry(swapcoin_hashvalue)
                        .or_insert((
                            Vec::<(&IncomingSwapCoin, ListUnspentResultEntry)>::new(),
                            Vec::<(&OutgoingSwapCoin, ListUnspentResultEntry)>::new(),
                        ))
                        .1
                        .push((s, utxo));
                }
            } else {
                continue;
            };
        }
        Ok(incomplete_swapcoin_groups)
    }

    /// A simplification of `find_incomplete_coinswaps` function
    pub fn find_unfinished_swapcoins(&self) -> (Vec<IncomingSwapCoin>, Vec<OutgoingSwapCoin>) {
        let unfinished_incomins = self
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

        (unfinished_incomins, unfinished_outgoings)
    }

    /// Finds live contract unspent outputs in the wallet.
    // live contract refers to a contract tx which has been broadcast
    // i.e. where there are UTXOs protected by contract_redeemscript's that we know about
    pub fn find_live_contract_unspents(&self) -> Result<SwapCoinsInfo, WalletError> {
        // populate hashmaps where key is contract scriptpubkey and value is the swapcoin
        let contract_scriptpubkeys_incoming_swapcoins =
            self.create_contract_scriptpubkey_incoming_swapcoin_hashmap();
        let contract_scriptpubkeys_outgoing_swapcoins =
            self.create_contract_scriptpubkey_outgoing_swapcoin_hashmap();

        self.rpc.unlock_unspent_all()?;
        let listunspent = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;

        let (incoming_swapcoins_utxos, outgoing_swapcoins_utxos): (Vec<_>, Vec<_>) = listunspent
            .iter()
            .map(|u| {
                (
                    contract_scriptpubkeys_incoming_swapcoins.get(&u.script_pub_key),
                    contract_scriptpubkeys_outgoing_swapcoins.get(&u.script_pub_key),
                    u,
                )
            })
            .filter(|isc_osc_u| (isc_osc_u.0.is_some() || isc_osc_u.1.is_some()))
            .partition(|isc_osc_u| isc_osc_u.0.is_some());

        Ok((
            incoming_swapcoins_utxos
                .iter()
                .map(|isc_osc_u| (*isc_osc_u.0.unwrap(), isc_osc_u.2.clone()))
                .collect::<Vec<(&IncomingSwapCoin, ListUnspentResultEntry)>>(),
            outgoing_swapcoins_utxos
                .iter()
                .map(|isc_osc_u| (*isc_osc_u.1.unwrap(), isc_osc_u.2.clone()))
                .collect::<Vec<(&OutgoingSwapCoin, ListUnspentResultEntry)>>(),
        ))
    }

    /// Finds the next unused index in the HD keychain.
    pub(super) fn find_hd_next_index(&self, keychain: KeychainKind) -> Result<u32, WalletError> {
        let mut max_index: i32 = -1;
        let all_utxos = self.get_all_utxo()?;
        let mut utxos = self.list_descriptor_utxo_spend_info(Some(&all_utxos))?;
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info(Some(&all_utxos))?;
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

    /// Gets the next external address from the HD keychain.
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
        self.update_external_index(self.store.external_index + 1)?;
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

    /// Refreshes the offer maximum size cache based on the current wallet's unspent transaction outputs (UTXOs).
    pub fn refresh_offer_maxsize_cache(&mut self) -> Result<(), WalletError> {
        let all_utxos = self.get_all_utxo()?;
        let mut utxos = self.list_descriptor_utxo_spend_info(Some(&all_utxos))?;
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info(Some(&all_utxos))?;
        utxos.append(&mut swap_coin_utxo);
        let balance: Amount = utxos.iter().fold(Amount::ZERO, |acc, u| acc + u.0.amount);
        self.store.offer_maxsize = balance.to_sat();
        Ok(())
    }

    /// Gets the offer maximum size from the cached value.
    pub fn get_offer_maxsize(&self) -> u64 {
        self.store.offer_maxsize
    }

    /// Gets a tweakable key pair from the master key of the wallet.
    pub fn get_tweakable_keypair(&self) -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        let privkey = self
            .store
            .master_key
            .derive_priv(&secp, &[ChildNumber::from_hardened_idx(0).unwrap()])
            .unwrap()
            .private_key;

        let public_key = PublicKey {
            compressed: true,
            inner: privkey.public_key(&secp),
        };
        (privkey, public_key)
    }

    /// Signs a transaction corresponding to the provided UTXO spend information.
    pub fn sign_transaction(
        &self,
        tx: &mut Transaction,
        inputs_info: impl Iterator<Item = UTXOSpendInfo>,
    ) -> Result<(), WalletError> {
        let secp = Secp256k1::new();
        let master_private_key = self
            .store
            .master_key
            .derive_priv(
                &secp,
                &DerivationPath::from_str(HARDENDED_DERIVATION).unwrap(),
            )
            .unwrap();
        let tx_clone = tx.clone();

        for (ix, (input, input_info)) in tx.input.iter_mut().zip(inputs_info).enumerate() {
            match input_info {
                UTXOSpendInfo::SwapCoin {
                    multisig_redeemscript,
                } => {
                    self.find_incoming_swapcoin(&multisig_redeemscript)
                        .unwrap()
                        .sign_transaction_input(ix, &tx_clone, input, &multisig_redeemscript)
                        .unwrap();
                }
                UTXOSpendInfo::SeedCoin { path, input_value } => {
                    let privkey = master_private_key
                        .derive_priv(&secp, &DerivationPath::from_str(&path).unwrap())
                        .unwrap()
                        .private_key;
                    let pubkey = PublicKey {
                        compressed: true,
                        inner: privkey.public_key(&secp),
                    };
                    let scriptcode = ScriptBuf::new_p2wpkh(&pubkey.wpubkey_hash().unwrap());
                    let sighash = SighashCache::new(&tx_clone)
                        .p2wpkh_signature_hash(
                            ix,
                            &scriptcode,
                            Amount::from_sat(input_value),
                            EcdsaSighashType::All,
                        )
                        .unwrap();
                    //use low-R value signatures for privacy
                    //https://en.bitcoin.it/wiki/Privacy#Wallet_fingerprinting
                    let signature = secp.sign_ecdsa_low_r(
                        &secp256k1::Message::from_digest_slice(&sighash[..]).unwrap(),
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
                    .unwrap()
                    .sign_timelocked_transaction_input(ix, &tx_clone, input, input_value)
                    .unwrap(),
                UTXOSpendInfo::HashlockContract {
                    swapcoin_multisig_redeemscript,
                    input_value,
                } => self
                    .find_incoming_swapcoin(&swapcoin_multisig_redeemscript)
                    .unwrap()
                    .sign_hashlocked_transaction_input(ix, &tx_clone, input, input_value)
                    .unwrap(),
                UTXOSpendInfo::FidelityBondCoin { index, input_value } => {
                    let privkey = self.get_fidelity_keypair(index)?.secret_key();
                    let redeemscript = self.get_fidelity_reedemscript(index)?;
                    let sighash = SighashCache::new(&tx_clone)
                        .p2wsh_signature_hash(
                            ix,
                            &redeemscript,
                            Amount::from_sat(input_value),
                            EcdsaSighashType::All,
                        )
                        .unwrap();
                    let sig = secp.sign_ecdsa(
                        &secp256k1::Message::from_digest_slice(&sighash[..]).unwrap(),
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

    pub fn coin_select(
        &self,
        amount: Amount,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_utxos = self.get_all_locked_utxo()?;

        let mut seed_coin_utxo = self.list_descriptor_utxo_spend_info(Some(&all_utxos))?;
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info(Some(&all_utxos))?;
        seed_coin_utxo.append(&mut swap_coin_utxo);

        // Fetch utxos, filter out existing fidelity coins
        let mut unspents = seed_coin_utxo
            .into_iter()
            .filter(|(_, spend_info)| !matches!(spend_info, UTXOSpendInfo::FidelityBondCoin { .. }))
            .collect::<Vec<_>>();

        unspents.sort_by(|a, b| b.0.amount.cmp(&a.0.amount));

        let mut selected_utxo = Vec::new();
        let mut remaining = amount;

        // the simplest largest first coinselection.
        for unspent in unspents {
            if remaining.checked_sub(unspent.0.amount).is_none() {
                selected_utxo.push(unspent);
                break;
            } else {
                remaining -= unspent.0.amount;
                selected_utxo.push(unspent);
            }
        }
        Ok(selected_utxo)
    }

    pub fn get_utxo(
        &self,
        (txid, vout): (Txid, u32),
    ) -> Result<Option<UTXOSpendInfo>, WalletError> {
        let all_utxos = self.get_all_utxo()?;

        let mut seed_coin_utxo = self.list_descriptor_utxo_spend_info(Some(&all_utxos))?;
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info(Some(&all_utxos))?;
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
    ) -> (Address, SecretKey) {
        let (my_pubkey, my_privkey) = generate_keypair();

        let descriptor = self
            .rpc
            .get_descriptor_info(&format!(
                "wsh(sortedmulti(2,{},{}))",
                my_pubkey, other_pubkey
            ))
            .unwrap()
            .descriptor;
        self.import_descriptors(&[descriptor.clone()], None)
            .unwrap();

        //redeemscript and descriptor show up in `getaddressinfo` only after
        // the address gets outputs on it-
        (
            //TODO should completely avoid derive_addresses
            //because its slower and provides no benefit over using rust-bitcoin
            self.rpc.derive_addresses(&descriptor[..], None).unwrap()[0]
                .clone()
                .assume_checked(),
            my_privkey,
        )
    }

    /// Initialize a Coinswap with the Other party.
    /// Returns, the Funding Transactions, [`OutgoingSwapCoin`]s and the Total Miner fees.
    pub fn initalize_coinswap(
        &mut self,
        total_coinswap_amount: u64,
        other_multisig_pubkeys: &[PublicKey],
        hashlock_pubkeys: &[PublicKey],
        hashvalue: Hash160,
        locktime: u16,
        fee_rate: u64,
    ) -> Result<(Vec<Transaction>, Vec<OutgoingSwapCoin>, u64), WalletError> {
        let (coinswap_addresses, my_multisig_privkeys): (Vec<_>, Vec<_>) = other_multisig_pubkeys
            .iter()
            .map(|other_key| self.create_and_import_coinswap_address(other_key))
            .unzip();

        let create_funding_txes_result =
            self.create_funding_txes(total_coinswap_amount, &coinswap_addresses, fee_rate)?;
        //for sweeping there would be another function, probably
        //probably have an enum called something like SendAmount which can be
        // an integer but also can be Sweep

        //TODO: implement the idea where a maker will send its own privkey back to the
        //taker in this situation, so if a taker gets their own funding txes mined
        //but it turns out the maker cant fulfil the coinswap, then the taker gets both
        //privkeys, so it can try again without wasting any time and only a bit of miner fees

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
                funding_amount.to_sat(),
                &contract_redeemscript,
            );

            // self.import_wallet_contract_redeemscript(&contract_redeemscript)?;
            outgoing_swapcoins.push(OutgoingSwapCoin::new(
                my_multisig_privkey,
                other_multisig_pubkey,
                my_senders_contract_tx,
                contract_redeemscript,
                timelock_privkey,
                funding_amount.to_sat(),
            ));
        }

        Ok((
            create_funding_txes_result.funding_txes,
            outgoing_swapcoins,
            create_funding_txes_result.total_miner_fee,
        ))
    }

    /// Imports a watch-only redeem script into the wallet.
    pub fn import_watchonly_redeemscript(
        &self,
        redeemscript: &ScriptBuf,
    ) -> Result<(), WalletError> {
        let spk = redeemscript_to_scriptpubkey(redeemscript);
        let descriptor = self
            .rpc
            .get_descriptor_info(&format!("raw({:x})", spk))
            .unwrap()
            .descriptor;
        self.import_descriptors(&[descriptor], Some(WATCH_ONLY_SWAPCOIN_LABEL.to_string()))
    }

    pub fn descriptors_to_import(&self) -> Result<Vec<String>, WalletError> {
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
                    format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum).unwrap()
                    )
                })
                .collect::<Vec<String>>(),
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
                    format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum).unwrap()
                    )
                })
                .collect::<Vec<String>>(),
        );

        descriptors_to_import.extend(
            self.store
                .incoming_swapcoins
                .values()
                .map(|sc| {
                    let contract_spk = redeemscript_to_scriptpubkey(&sc.contract_redeemscript);
                    let descriptor_without_checksum = format!("raw({:x})", contract_spk);
                    format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum).unwrap()
                    )
                })
                .collect::<Vec<_>>(),
        );
        descriptors_to_import.extend(
            self.store
                .outgoing_swapcoins
                .values()
                .map(|sc| {
                    let contract_spk = redeemscript_to_scriptpubkey(&sc.contract_redeemscript);
                    let descriptor_without_checksum = format!("raw({:x})", contract_spk);
                    format!(
                        "{}#{}",
                        descriptor_without_checksum,
                        compute_checksum(&descriptor_without_checksum).unwrap()
                    )
                })
                .collect::<Vec<_>>(),
        );

        descriptors_to_import.extend(self.store.fidelity_bond.iter().map(|(_, (_, spk, _))| {
            let descriptor_without_checksum = format!("raw({:x})", spk);
            format!(
                "{}#{}",
                descriptor_without_checksum,
                compute_checksum(&descriptor_without_checksum).unwrap()
            )
        }));

        Ok(descriptors_to_import)
    }
}
