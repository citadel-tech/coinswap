//! The Wallet API.
//!
//! Currently, wallet synchronization is exclusively performed through RPC for makers.
//! In the future, takers might adopt alternative synchronization methods, such as lightweight wallet solutions.

use std::{convert::TryFrom, fs, path::PathBuf, str::FromStr};

use std::collections::{HashMap, HashSet};

use bitcoin::{
    absolute::LockTime,
    bip32::{ChildNumber, DerivationPath, ExtendedPubKey},
    blockdata::script::Builder,
    consensus::encode::serialize_hex,
    hashes::{hash160::Hash as Hash160, hex::FromHex},
    secp256k1,
    secp256k1::{Secp256k1, SecretKey},
    sighash::{EcdsaSighashType, SighashCache},
    Address, Amount, OutPoint, PublicKey, Script, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
    Txid, Witness,
};

use bitcoind::bitcoincore_rpc::{
    core_rpc_json::{
        ImportMultiOptions, ImportMultiRequest, ImportMultiRequestScriptPubkey,
        ListUnspentResultEntry, Timestamp,
    },
    Client, RpcApi,
};
use serde_json::Value;

use crate::{
    protocol::contract,
    utill::{
        convert_json_rpc_bitcoin_to_satoshis, generate_keypair, get_hd_path_from_descriptor,
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
    /// Displays addresses based on the specified `DisplayAddressType`.
    pub fn display_addresses(&self, types: DisplayAddressType) -> Result<(), WalletError> {
        if types == DisplayAddressType::All || types == DisplayAddressType::MasterKey {
            println!(
                "master key = {}, external_index = {}",
                self.store.master_key, self.store.external_index
            );
        }
        let secp = Secp256k1::new();

        if types == DisplayAddressType::All || types == DisplayAddressType::Seed {
            let top_branch = ExtendedPubKey::from_priv(
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
            for c in 0..2 {
                println!(
                    "{} branch from seed",
                    if c == 0 { "Receive" } else { "Change" }
                );
                let recv_or_change_branch = top_branch
                    .ckd_pub(&secp, ChildNumber::Normal { index: c })
                    .unwrap();
                for i in 0..self.get_addrss_import_count() {
                    let pubkey = PublicKey {
                        compressed: true,
                        inner: recv_or_change_branch
                            .ckd_pub(&secp, ChildNumber::Normal { index: i })
                            .unwrap()
                            .public_key,
                    };
                    let addr = Address::p2wpkh(&pubkey, self.store.network).unwrap();
                    println!("{} from seed {}/{}/{}", addr, HARDENDED_DERIVATION, c, i);
                }
            }
        }

        if types == DisplayAddressType::All
            || types == DisplayAddressType::IncomingSwap
            || types == DisplayAddressType::Swap
        {
            println!(
                "incoming swapcoin count = {}",
                self.store.incoming_swapcoins.len()
            );
            for (multisig_redeemscript, swapcoin) in &self.store.incoming_swapcoins {
                println!(
                    "{} incoming_swapcoin other_privkey={} contract_txid={}",
                    Address::p2wsh(multisig_redeemscript, self.store.network),
                    if swapcoin.other_privkey.is_some() {
                        "known  "
                    } else {
                        "unknown"
                    },
                    swapcoin.contract_tx.txid()
                );
            }
        }

        if types == DisplayAddressType::All
            || types == DisplayAddressType::OutgoingSwap
            || types == DisplayAddressType::Swap
        {
            println!(
                "outgoing swapcoin count = {}",
                self.store.outgoing_swapcoins.len()
            );
            for (multisig_redeemscript, swapcoin) in &self.store.outgoing_swapcoins {
                println!(
                    "{} outgoing_swapcoin contract_txid={}",
                    Address::p2wsh(multisig_redeemscript, self.store.network),
                    swapcoin.contract_tx.txid()
                );
            }
        }

        if types == DisplayAddressType::All
            || types == DisplayAddressType::IncomingContract
            || types == DisplayAddressType::Contract
        {
            println!(
                "incoming swapcoin count = {}",
                self.store.incoming_swapcoins.len()
            );
            for swapcoin in self.store.incoming_swapcoins.values() {
                println!(
                    "{} incoming_swapcoin_contract hashvalue={} locktime={} contract_txid={}",
                    Address::p2wsh(&swapcoin.contract_redeemscript, self.store.network),
                    swapcoin.get_hashvalue(),
                    swapcoin.get_timelock(),
                    swapcoin.contract_tx.txid()
                );
            }
        }

        if types == DisplayAddressType::All
            || types == DisplayAddressType::OutgoingContract
            || types == DisplayAddressType::Contract
        {
            println!(
                "outgoing swapcoin count = {}",
                self.store.outgoing_swapcoins.len()
            );
            for swapcoin in self.store.outgoing_swapcoins.values() {
                println!(
                    "{} outgoing_swapcoin_contract hashvalue={} locktime={} contract_txid={}",
                    Address::p2wsh(&swapcoin.contract_redeemscript, self.store.network),
                    swapcoin.get_hashvalue(),
                    swapcoin.get_timelock(),
                    swapcoin.contract_tx.txid()
                );
            }
        }

        if types == DisplayAddressType::All || types == DisplayAddressType::FidelityBond {
            for (bond, _, _) in self.store.fidelity_bond.values() {
                let locktime = bond.lock_time;
                println!(
                    "[{}] locktime={}",
                    Address::from_script(&bond.script_pub_key(), self.store.network).unwrap(),
                    locktime
                );
            }
        }
        Ok(())
    }

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
        let store = WalletStore::init(file_name, path, rpc_config.network, seedphrase, passphrase)?;
        let rpc = Client::try_from(rpc_config)?;
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
        log::debug!(target: "wallet",
            "loaded wallet file, external_index={} incoming_swapcoins={} outgoing_swapcoins={}",
            store.external_index,
            store.incoming_swapcoins.len(), store.outgoing_swapcoins.len());
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

    /// Calculates the total balance of the wallet, considering live contracts and fidelity bonds.
    pub fn balance(
        &self,
        live_contracts: bool,
        fidelity_bonds: bool,
    ) -> Result<Amount, WalletError> {
        Ok(self
            .list_unspent_from_wallet(live_contracts, fidelity_bonds)?
            .iter()
            .fold(Amount::ZERO, |a, (utxo, _)| a + utxo.amount))
    }

    pub fn balance_fidelity_bonds(&self) -> Result<Amount, WalletError> {
        Ok(self
            .list_fidelity_unspend_from_wallet()?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount))
    }

    pub fn balance_live_transaction(&self) -> Result<Amount, WalletError> {
        Ok(self
            .list_live_contract_unspend_from_wallet()?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount))
    }

    pub fn balance_descriptor_utxo_or_swap_coins(&self) -> Result<Amount, WalletError> {
        Ok(self
            .list_descriptor_utxo_or_swap_coin_unspend_from_wallet()?
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
            log::debug!(target: "Wallet:cache_prevout_to_contract", "prevout-contract map updated.\nExisting contract: {}", contract);
        }
        Ok(())
    }

    //pub fn get_recovery_phrase_from_file()

    /// Wallet descriptors are derivable. Currently only supports two KeychainKind. Internal and External.
    fn get_wallet_descriptors(&self) -> Result<HashMap<KeychainKind, String>, WalletError> {
        let secp = Secp256k1::new();
        let wallet_xpub = ExtendedPubKey::from_priv(
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
                let desc_info = self
                    .rpc
                    .get_descriptor_info(&format!(
                        "wpkh({}/{}/*)",
                        wallet_xpub,
                        keychain.index_num()
                    ))
                    .unwrap();
                (*keychain, desc_info.descriptor)
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
    pub(super) fn get_unimoprted_wallet_desc(&self) -> Result<Vec<String>, WalletError> {
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

    /// Checks if the first derived address from a swapcoin descriptor is imported.
    /// swapcoin descriptors are non-derivable.
    pub(super) fn is_swapcoin_descriptor_imported(&self, descriptor: &str) -> bool {
        let addr = self.rpc.derive_addresses(descriptor, None).unwrap()[0].clone();
        self.rpc
            .get_address_info(&addr.assume_checked())
            .unwrap()
            .is_watchonly
            .unwrap_or(false)
    }

    /// Core wallet label is the master XPub fingerint.
    pub fn get_core_wallet_label(&self) -> String {
        let secp = Secp256k1::new();
        let m_xpub = ExtendedPubKey::from_priv(&secp, &self.store.master_key);
        m_xpub.fingerprint().to_string()
    }

    /// Import watch addresses into core wallet. Does not check if the address was already imported.
    pub(super) fn import_addresses(
        &self,
        hd_descriptors: &[String],
        swapcoin_descriptors: &[String],
        contract_scriptpubkeys: &[ScriptBuf],
    ) -> Result<(), WalletError> {
        log::debug!(target: "Wallet: ",
            "import_initial_addresses with initial_address_import_count = {}",
            self.get_addrss_import_count());
        let address_label = self.get_core_wallet_label();

        let import_requests = hd_descriptors
            .iter()
            .map(|desc| ImportMultiRequest {
                timestamp: Timestamp::Now,
                descriptor: Some(desc),
                range: Some((0, (self.get_addrss_import_count() - 1) as usize)),
                watchonly: Some(true),
                label: Some(&address_label),
                ..Default::default()
            })
            .chain(swapcoin_descriptors.iter().map(|desc| ImportMultiRequest {
                timestamp: Timestamp::Now,
                descriptor: Some(desc),
                watchonly: Some(true),
                label: Some(&address_label),
                ..Default::default()
            }))
            .chain(contract_scriptpubkeys.iter().map(|spk| ImportMultiRequest {
                timestamp: Timestamp::Now,
                script_pubkey: Some(ImportMultiRequestScriptPubkey::Script(spk)),
                watchonly: Some(true),
                label: Some(&address_label),
                ..Default::default()
            }))
            .chain(
                self.store
                    .fidelity_bond
                    .iter()
                    .map(|(_, (_, spk, _))| ImportMultiRequest {
                        timestamp: Timestamp::Now,
                        script_pubkey: Some(ImportMultiRequestScriptPubkey::Script(spk)),
                        watchonly: Some(true),
                        label: Some(&address_label),
                        ..Default::default()
                    }),
            )
            .collect::<Vec<ImportMultiRequest>>();

        let result = self.rpc.import_multi(
            &import_requests,
            Some(
                &(ImportMultiOptions {
                    rescan: Some(false),
                }),
            ),
        )?;

        // Only hard error if it errors, or else log the warning
        for r in result {
            if !r.success {
                log::warn!(target: "Wallet:import_addresses", "{:?}", r.warnings);
                if let Some(e) = r.error {
                    return Err(WalletError::Protocol(e.message));
                }
            }
        }
        Ok(())
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

    /// Locks all non-wallet unspent transaction outputs (UTXOs) & returns a `WalletError`
    /// if there is an issue with the RPC call or locking the UTXOs.
    pub fn lock_all_nonwallet_unspents(&self) -> Result<(), WalletError> {
        //rpc.unlock_unspent(&[])?;
        //https://github.com/rust-bitcoin/rust-bitcoincore-rpc/issues/148
        self.rpc
            .call::<Value>("lockunspent", &[Value::Bool(true)])?;

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

    fn check_fidelity_bonds(&self, utxo: &ListUnspentResultEntry) -> Option<UTXOSpendInfo> {
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

    fn check_live_contracts(
        &self,
        utxo: &ListUnspentResultEntry,
        contract_scriptpubkeys_outgoing: &HashMap<ScriptBuf, &OutgoingSwapCoin>,
        contract_scriptpubkeys_incoming: &HashMap<ScriptBuf, &IncomingSwapCoin>,
    ) -> Option<UTXOSpendInfo> {
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

    /// Lists UTXOs from the wallet, optionally including live contracts and fidelity bonds.
    /// Simplified method to list UTXOs from the wallet, integrating the logic directly.
    pub fn list_unspent_from_wallet(
        &self,
        include_live_contracts: bool,
        include_fidelity_bonds: bool,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let (contract_scriptpubkeys_outgoing, contract_scriptpubkeys_incoming) =
            if include_live_contracts {
                (
                    self.create_contract_scriptpubkey_outgoing_swapcoin_hashmap(),
                    self.create_contract_scriptpubkey_incoming_swapcoin_hashmap(),
                )
            } else {
                (HashMap::new(), HashMap::new())
            };

        self.rpc
            .call::<Value>("lockunspent", &[Value::Bool(true)])?;
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;

        let processed_utxos = all_utxos
            .iter()
            .filter_map(|utxo| {
                let mut spend_info = if include_fidelity_bonds {
                    self.check_fidelity_bonds(utxo)
                } else {
                    None
                };

                if spend_info.is_none() && utxo.descriptor.is_none() {
                    spend_info = self.check_live_contracts(
                        utxo,
                        &contract_scriptpubkeys_outgoing,
                        &contract_scriptpubkeys_incoming,
                    );
                }

                if spend_info.is_none() {
                    spend_info = self.check_descriptor_utxo_or_swap_coin(utxo);
                }

                spend_info.map(|info| (utxo.clone(), info))
            })
            .collect::<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>>();

        Ok(processed_utxos)
    }

    pub fn list_live_contract_unspend_from_wallet(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let (contract_scriptpubkeys_outgoing, contract_scriptpubkeys_incoming) = (
            self.create_contract_scriptpubkey_outgoing_swapcoin_hashmap(),
            self.create_contract_scriptpubkey_incoming_swapcoin_hashmap(),
        );
        self.rpc
            .call::<Value>("lockunspent", &[Value::Bool(true)])?;
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        let processed_utxos = all_utxos
            .iter()
            .filter_map(|utxo| {
                let spend_info = self.check_live_contracts(
                    utxo,
                    &contract_scriptpubkeys_outgoing,
                    &contract_scriptpubkeys_incoming,
                );

                spend_info.map(|info| (utxo.clone(), info))
            })
            .collect::<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>>();

        Ok(processed_utxos)
    }

    pub fn list_fidelity_unspend_from_wallet(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        self.rpc
            .call::<Value>("lockunspent", &[Value::Bool(true)])?;
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        let processed_utxos = all_utxos
            .iter()
            .filter_map(|utxo| {
                let spend_info = self.check_fidelity_bonds(utxo);

                spend_info.map(|info| (utxo.clone(), info))
            })
            .collect::<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>>();

        Ok(processed_utxos)
    }

    pub fn list_descriptor_utxo_or_swap_coin_unspend_from_wallet(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        self.rpc
            .call::<Value>("lockunspent", &[Value::Bool(true)])?;
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        let processed_utxos = all_utxos
            .iter()
            .filter_map(|utxo| {
                let spend_info = self.check_descriptor_utxo_or_swap_coin(utxo);
                spend_info.map(|info| (utxo.clone(), info))
            })
            .collect::<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>>();

        Ok(processed_utxos)
    }

    /// Finds incomplete coin swaps in the wallet.
    pub fn find_incomplete_coinswaps(
        &self,
    ) -> Result<HashMap<Hash160, SwapCoinsInfo>, WalletError> {
        self.rpc
            .call::<Value>("lockunspent", &[Value::Bool(true)])?;

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

        self.rpc
            .call::<Value>("lockunspent", &[Value::Bool(true)])?;
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
        //TODO error handling
        let utxos = self.list_descriptor_utxo_or_swap_coin_unspend_from_wallet()?;
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
        let utxos = self.list_descriptor_utxo_or_swap_coin_unspend_from_wallet()?;
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
            .ckd_priv(&secp, ChildNumber::from_hardened_idx(0).unwrap())
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
            log::debug!(target: "wallet", "signing with input_info = {:?}", input_info);
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
                    let scriptcode = ScriptBuf::new_p2pkh(&pubkey.pubkey_hash());
                    let sighash = SighashCache::new(&tx_clone)
                        .segwit_signature_hash(ix, &scriptcode, input_value, EcdsaSighashType::All)
                        .unwrap();
                    //use low-R value signatures for privacy
                    //https://en.bitcoin.it/wiki/Privacy#Wallet_fingerprinting
                    let signature = secp.sign_ecdsa_low_r(
                        &secp256k1::Message::from_slice(&sighash[..]).unwrap(),
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
                        .segwit_signature_hash(
                            ix,
                            &redeemscript,
                            input_value,
                            EcdsaSighashType::All,
                        )
                        .unwrap();
                    let sig = secp.sign_ecdsa(
                        &secp256k1::Message::from_slice(&sighash[..]).unwrap(),
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

    /// Converts a PSBT (Partially Signed Bitcoin Transaction) created by the wallet
    /// into a fully signed transaction.
    pub fn from_walletcreatefundedpsbt_to_tx(
        &self,
        psbt: &String,
    ) -> Result<Transaction, WalletError> {
        //TODO rust-bitcoin handles psbt, use those functions instead
        let decoded_psbt = self
            .rpc
            .call::<Value>("decodepsbt", &[Value::String(psbt.to_string())])?;
        log::debug!(target: "wallet", "decoded_psbt = {:?}", decoded_psbt);

        //TODO proper error handling, theres many unwrap()s here
        //make this function return Result<>
        let inputs = decoded_psbt["tx"]["vin"]
            .as_array()
            .unwrap()
            .iter()
            .map(|vin| TxIn {
                previous_output: OutPoint {
                    txid: vin["txid"].as_str().unwrap().parse::<Txid>().unwrap(),
                    vout: vin["vout"].as_u64().unwrap() as u32,
                },
                sequence: Sequence::ZERO,
                witness: Witness::new(),
                script_sig: ScriptBuf::new(),
            })
            .collect::<Vec<TxIn>>();
        let outputs = decoded_psbt["tx"]["vout"]
            .as_array()
            .unwrap()
            .iter()
            .map(|vout| TxOut {
                script_pubkey: Builder::from(
                    Vec::from_hex(vout["scriptPubKey"]["hex"].as_str().unwrap()).unwrap(),
                )
                .into_script(),
                value: convert_json_rpc_bitcoin_to_satoshis(&vout["value"]),
            })
            .collect::<Vec<TxOut>>();

        let mut tx = Transaction {
            input: inputs,
            output: outputs,
            lock_time: LockTime::ZERO,
            version: 2,
        };
        log::debug!(target: "wallet", "tx = {:?}", tx);

        let mut inputs_info = decoded_psbt["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input_info| (input_info, input_info["bip32_derivs"].as_array().unwrap()))
            .map(|(input_info, bip32_info)| {
                if bip32_info.len() == 2 {
                    UTXOSpendInfo::SwapCoin {
                        multisig_redeemscript: Builder::from(
                            Vec::from_hex(input_info["witness_script"]["hex"].as_str().unwrap())
                                .unwrap(),
                        )
                        .into_script(),
                    }
                } else {
                    UTXOSpendInfo::SeedCoin {
                        path: bip32_info[0]["path"].as_str().unwrap().to_string(),
                        input_value: convert_json_rpc_bitcoin_to_satoshis(
                            &input_info["witness_utxo"]["amount"],
                        ),
                    }
                }
            });
        log::debug!(target: "wallet", "inputs_info = {:?}", inputs_info);
        self.sign_transaction(&mut tx, &mut inputs_info)?;

        log::debug!(target: "wallet",
            "txhex = {}",
            bitcoin::consensus::encode::serialize_hex(&tx)
        );
        Ok(tx)
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

        self.import_multisig_redeemscript_descriptor(
            &my_pubkey,
            other_pubkey,
            &self.get_core_wallet_label(),
        )
        .unwrap();

        //redeemscript and descriptor show up in `getaddressinfo` only after
        // the address gets outputs on it
        (
            //TODO should completely avoid derive_addresses
            //because its slower and provides no benefit over using rust-bitcoin
            self.rpc.derive_addresses(&descriptor[..], None).unwrap()[0]
                .clone()
                .assume_checked(),
            my_privkey,
        )
    }

    /// Imports a contract redeem script into the wallet.
    pub fn import_wallet_contract_redeemscript(
        &self,
        redeemscript: &ScriptBuf,
    ) -> Result<(), WalletError> {
        self.import_redeemscript(redeemscript, &self.get_core_wallet_label())
    }

    /// Imports a multisig redeem script into the wallet using two public keys.
    pub fn import_wallet_multisig_redeemscript(
        &self,
        pubkey1: &PublicKey,
        pubkey2: &PublicKey,
    ) -> Result<(), WalletError> {
        self.import_multisig_redeemscript_descriptor(
            pubkey1,
            pubkey2,
            &self.get_core_wallet_label(),
        )
    }

    /// Imports a transaction along with its merkle proof into the wallet.
    pub fn import_tx_with_merkleproof(
        &self,
        tx: &Transaction,
        merkleproof: &str,
    ) -> Result<(), WalletError> {
        let rawtx_hex = serialize_hex(&tx);
        self.rpc.call(
            "importprunedfunds",
            &[
                Value::String(rawtx_hex),
                Value::String(merkleproof.to_owned()),
            ],
        )?;
        log::debug!(target: "wallet", "import_tx_with_merkleproof txid={}", tx.txid());
        Ok(())
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
        log::debug!(target: "wallet", "coinswap_addresses = {:?}", coinswap_addresses);

        // TODO: Instead of options, return results.
        let create_funding_txes_result =
            self.create_funding_txes(total_coinswap_amount, &coinswap_addresses, fee_rate)?;
        //for sweeping there would be another function, probably
        //probably have an enum called something like SendAmount which can be
        // an integer but also can be Sweep

        if create_funding_txes_result.is_none() {
            return Err(WalletError::Protocol(
                "Unable to create funding transactions, not enough funds".to_string(),
            ));
            //TODO: implement the idea where a maker will send its own privkey back to the
            //taker in this situation, so if a taker gets their own funding txes mined
            //but it turns out the maker cant fulfil the coinswap, then the taker gets both
            //privkeys, so it can try again without wasting any time and only a bit of miner fees
        }
        let create_funding_txes_result = create_funding_txes_result.unwrap();

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
                    txid: my_funding_tx.txid(),
                    vout: utxo_index,
                },
                funding_amount,
                &contract_redeemscript,
            );

            self.import_wallet_contract_redeemscript(&contract_redeemscript)?;
            outgoing_swapcoins.push(OutgoingSwapCoin::new(
                my_multisig_privkey,
                other_multisig_pubkey,
                my_senders_contract_tx,
                contract_redeemscript,
                timelock_privkey,
                funding_amount,
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
        self.import_redeemscript(redeemscript, &WATCH_ONLY_SWAPCOIN_LABEL.to_string())
    }

    /// Imports a multisig redeem script with a descriptor into the wallet.
    fn import_multisig_redeemscript_descriptor(
        &self,
        pubkey1: &PublicKey,
        pubkey2: &PublicKey,
        address_label: &String,
    ) -> Result<(), WalletError> {
        let descriptor = self
            .rpc
            .get_descriptor_info(&format!("wsh(sortedmulti(2,{},{}))", pubkey1, pubkey2))?
            .descriptor;
        let result = self
            .rpc
            .import_multi(
                &[ImportMultiRequest {
                    timestamp: Timestamp::Now,
                    descriptor: Some(&descriptor),
                    watchonly: Some(true),
                    label: Some(address_label),
                    ..Default::default()
                }],
                Some(
                    &(ImportMultiOptions {
                        rescan: Some(false),
                    }),
                ),
            )
            .unwrap();
        for r in result {
            if !r.success {
                log::warn!(target: "Wallet:import_addresses", "{:?}", r.warnings);
                if let Some(e) = r.error {
                    return Err(WalletError::Protocol(e.message));
                }
            }
        }
        Ok(())
    }

    /// Imports a redeem script into the wallet.
    pub fn import_redeemscript(
        &self,
        redeemscript: &ScriptBuf,
        address_label: &String,
    ) -> Result<(), WalletError> {
        let spk = redeemscript_to_scriptpubkey(redeemscript);
        let result = self.rpc.import_multi(
            &[ImportMultiRequest {
                timestamp: Timestamp::Now,
                script_pubkey: Some(ImportMultiRequestScriptPubkey::Script(&spk)),
                redeem_script: Some(redeemscript),
                watchonly: Some(true),
                label: Some(address_label),
                ..Default::default()
            }],
            Some(
                &(ImportMultiOptions {
                    rescan: Some(false),
                }),
            ),
        )?;
        for r in result {
            if !r.success {
                log::warn!(target: "Wallet:import_addresses", "{:?}", r.warnings);
                if let Some(e) = r.error {
                    return Err(WalletError::Protocol(e.message));
                }
            }
        }
        Ok(())
    }
}
