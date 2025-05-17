//! The Wallet API.
//!
//! Currently, wallet synchronization is exclusively performed through RPC for makers.
//! In the future, takers might adopt alternative synchronization methods, such as lightweight wallet solutions.

use std::{convert::TryFrom, fmt::Display, path::PathBuf, str::FromStr};

use std::collections::HashMap;

use bip39::Mnemonic;
use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Xpriv, Xpub},
    hashes::hash160::Hash as Hash160,
    secp256k1,
    secp256k1::{Secp256k1, SecretKey},
    sighash::{EcdsaSighashType, SighashCache},
    Address, Amount, OutPoint, PublicKey, Script, ScriptBuf, Transaction, Txid, VarInt, Weight,
};
use bitcoind::bitcoincore_rpc::{bitcoincore_rpc_json::ListUnspentResultEntry, Client, RpcApi};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{
    protocol::contract,
    utill::{
        compute_checksum, generate_keypair, get_hd_path_from_descriptor,
        redeemscript_to_scriptpubkey,
    },
};

use rust_coinselect::{
    selectcoin::select_coin,
    types::{CoinSelectionOpt, ExcessStrategy, OutputGroup},
    utils::{calculate_base_weight_btc, calculate_fee},
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
    wallet_file_path: PathBuf,
    pub(crate) store: WalletStore,
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

use rust_coinselect::types::SelectionError;

impl From<SelectionError> for WalletError {
    fn from(err: SelectionError) -> Self {
        WalletError::General(format!("Coin selection error: {err}"))
    }
}

const WATCH_ONLY_SWAPCOIN_LABEL: &str = "watchonly_swapcoin_label";

/// Enum representing different types of addresses to display.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum DisplayAddressType {
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
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub enum UTXOSpendInfo {
    /// Seed Coin
    SeedCoin { path: String, input_value: Amount },
    /// Coins that we have received in a swap
    IncomingSwapCoin { multisig_redeemscript: ScriptBuf },
    /// Coins that we have sent in a swap
    OutgoingSwapCoin { multisig_redeemscript: ScriptBuf },
    /// Timelock Contract
    TimelockContract {
        swapcoin_multisig_redeemscript: ScriptBuf,
        input_value: Amount,
    },
    /// HashLockContract
    HashlockContract {
        swapcoin_multisig_redeemscript: ScriptBuf,
        input_value: Amount,
    },
    /// Fidelity Bond Coin
    FidelityBondCoin { index: u32, input_value: Amount },
}

impl UTXOSpendInfo {
    pub fn estimate_witness_size(&self) -> usize {
        const P2PWPKH_WITNESS_SIZE: usize = 107;
        const P2WSH_MULTISIG_2OF2_WITNESS_SIZE: usize = 222;
        const FIDELITY_BOND_WITNESS_SIZE: usize = 115;
        const CONTRACT_TX_WITNESS_SIZE: usize = 179;
        match *self {
            Self::SeedCoin { .. } => P2PWPKH_WITNESS_SIZE,
            Self::IncomingSwapCoin { .. } | Self::OutgoingSwapCoin { .. } => {
                P2WSH_MULTISIG_2OF2_WITNESS_SIZE
            }
            Self::TimelockContract { .. } | Self::HashlockContract { .. } => {
                CONTRACT_TX_WITNESS_SIZE
            }
            Self::FidelityBondCoin { .. } => FIDELITY_BOND_WITNESS_SIZE,
        }
    }
}

impl Display for UTXOSpendInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            UTXOSpendInfo::SeedCoin { .. } => write!(f, "regular"),
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
    pub fn init(path: &Path, rpc_config: &RPCConfig) -> Result<Self, WalletError> {
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
        let store = WalletStore::init(file_name, path, network, master_key, Some(wallet_birthday))?;

        Ok(Self {
            rpc,
            wallet_file_path: path.to_path_buf(),
            store,
        })
    }

    /// Load wallet data from file and connect to a core RPC.
    /// The core rpc wallet name, and wallet_id field in the file should match.
    pub(crate) fn load(path: &Path, rpc_config: &RPCConfig) -> Result<Wallet, WalletError> {
        let store = WalletStore::read_from_disk(path)?;
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
        })
    }

    /// Update external index and saves to disk.
    pub(crate) fn update_external_index(
        &mut self,
        new_external_index: u32,
    ) -> Result<(), WalletError> {
        self.store.external_index = new_external_index;
        self.save_to_disk()
    }

    /// Update the existing file. Error if path does not exist.
    pub(crate) fn save_to_disk(&self) -> Result<(), WalletError> {
        self.store.write_to_disk(&self.wallet_file_path)
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
            .list_descriptor_utxo_spend_info()?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        let contract = self
            .list_live_timelock_contract_spend_info()?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        let swap = self
            .list_incoming_swap_coin_utxo_spend_info()?
            .iter()
            .fold(Amount::ZERO, |sum, (utxo, _)| sum + utxo.amount);
        let fidelity = self
            .list_fidelity_spend_info()?
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
        self.store.fidelity_bond.iter().find_map(|(i, (bond, _))| {
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
    pub fn get_all_utxo(&self) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        self.rpc.unlock_unspent_all()?;
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
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let processed_utxos = self
            .store
            .utxo_cache
            .values()
            .map(|(utxo, spend_info)| (utxo.clone(), spend_info.clone()))
            .collect();
        Ok(processed_utxos)
    }

    /// Lists live contract UTXOs along with their [UTXOSpendInfo].
    pub fn list_live_contract_spend_info(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info()?;
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

    /// Lists live timelock contract UTXOs along with their [UTXOSpendInfo].
    pub fn list_live_timelock_contract_spend_info(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info()?;
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::TimelockContract { .. }))
            .cloned()
            .collect();
        Ok(filtered_utxos)
    }

    pub fn list_live_hashlock_contract_spend_info(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info()?;
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::HashlockContract { .. }))
            .cloned()
            .collect();
        Ok(filtered_utxos)
    }

    /// Lists fidelity UTXOs along with their [UTXOSpendInfo].
    pub fn list_fidelity_spend_info(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info()?;
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
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info()?;
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
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info()?;
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
        Ok(filtered_utxos)
    }

    /// Lists all incoming swapcoin UTXOs along with their [UTXOSpendInfo].
    pub fn list_incoming_swap_coin_utxo_spend_info(
        &self,
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        let all_valid_utxo = self.list_all_utxo_spend_info()?;
        let filtered_utxos: Vec<_> = all_valid_utxo
            .iter()
            .filter(|x| matches!(x.1, UTXOSpendInfo::IncomingSwapCoin { .. }))
            .cloned()
            .collect();
        Ok(filtered_utxos)
    }

    /// A simplification of `find_incomplete_coinswaps` function
    pub(crate) fn find_unfinished_swapcoins(
        &self,
    ) -> (Vec<IncomingSwapCoin>, Vec<OutgoingSwapCoin>) {
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

        let inc_contract_txid = unfinished_incomins
            .iter()
            .map(|ic| ic.contract_tx.compute_txid())
            .collect::<Vec<_>>();
        let out_contract_txid = unfinished_outgoings
            .iter()
            .map(|oc| oc.contract_tx.compute_txid())
            .collect::<Vec<_>>();

        log::info!("Unfinished incoming txids: {inc_contract_txid:?}");
        log::info!("Unfinished outgoing txids: {out_contract_txid:?}");

        (unfinished_incomins, unfinished_outgoings)
    }

    /// Finds the next unused index in the HD keychain.
    ///
    /// It will only return an unused address; i.e., an address that doesn't have a transaction associated with it.
    pub(super) fn find_hd_next_index(&self, keychain: KeychainKind) -> Result<u32, WalletError> {
        let mut max_index: i32 = -1;

        let mut utxos = self.list_descriptor_utxo_spend_info()?;
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info()?;
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
        Ok(receive_address.assume_checked()) // TODO: should we check the network or just assume_checked?
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
                UTXOSpendInfo::SeedCoin { path, input_value } => {
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
    /// # Arguments
    /// * `amount` - The target amount to select coins for
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
    ) -> Result<Vec<(ListUnspentResultEntry, UTXOSpendInfo)>, WalletError> {
        // TODO : Create a user input variable with the broader merge split refactor
        let num_outputs = 1; // Number of outputs

        // Get spendable UTXOs (regular coins and incoming swap coins)
        let mut unspents = self.list_descriptor_utxo_spend_info()?;
        unspents.extend(self.list_incoming_swap_coin_utxo_spend_info()?);

        // P2PWKH Breaks down as:
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

        // Filter out locked UTXOs
        let locked_utxos: Vec<OutPoint> = self.list_lock_unspent()?;
        let unspents = unspents
            .into_iter()
            .filter(|(utxo, _)| {
                let outpoint = OutPoint::new(utxo.txid, utxo.vout);
                !locked_utxos.contains(&outpoint)
            })
            .collect::<Vec<_>>();

        if unspents.is_empty() {
            log::warn!("No spendable UTXOs available, returning empty selection with 0 sats");
            return Ok(Vec::new());
        }

        // Assert: fidelity, outgoing swapcoins, hashlock and timelock coins are not included
        assert!(
            unspents.iter().all(|(_, spend_info)| !matches!(
                spend_info,
                UTXOSpendInfo::FidelityBondCoin { .. }
                    | UTXOSpendInfo::OutgoingSwapCoin { .. }
                    | UTXOSpendInfo::TimelockContract { .. }
                    | UTXOSpendInfo::HashlockContract { .. }
            )),
            "Fidelity, Outgoing Swapcoins, Hashlock and Timelock coins are not included in coin selection"
        );

        // Prepare CoinSelectionOpt: Calculate required weights and costs, additional metrics
        const LONG_TERM_FEERATE: f32 = 10.0;
        let change_weight = Weight::from_vb_unwrap(
            (Amount::SIZE // 8 bytes for amount
                    + VarInt::from(P2WPKH_SPK_SIZE).size() // VarInt size for script_pubkey
                    + P2WPKH_SPK_SIZE) as u64, // script_pubkey size
        );

        // Total cost associated with creating and later spending a change output in a transaction.
        // This includes the transaction fees for both the current transaction (where the change is created) and the future transaction (where the change is spent).
        // Calculate fee generally uses the units :- vbytes * sats/vbyte
        let cost_of_change = {
            let creation_cost = calculate_fee(change_weight.to_vbytes_ceil(), feerate as f32)
                .map_err(|e| WalletError::General(format!("Fee calculation failed: {e}")))?;

            let future_spending_cost = calculate_fee(P2WPKH_INPUT_WEIGHT / 4, LONG_TERM_FEERATE) // divide by 4 to convert weight to vbytes
                .map_err(|e| WalletError::General(format!("Fee calculation failed: {e}")))?;

            creation_cost + future_spending_cost
        };

        // Assuming target is a p2wpkh.
        // TODO: Add provision for other targets
        let target_weight = Weight::from_vb_unwrap(
            (Amount::SIZE + VarInt::from(P2WPKH_SPK_SIZE).size() + P2WPKH_SPK_SIZE) as u64,
        );
        let avg_output_weight =
            (change_weight.to_wu() + (target_weight.to_wu() * num_outputs)) / (1 + num_outputs);
        let avg_input_weight = unspents
            .iter()
            .map(|(_, spend_info)| {
                // Base weight of the input (OutPoint + sequence + empty script_sig)
                let base_weight = 32 + 4 + 4 + 1; // 32 bytes for OutPoint, 4 bytes for sequence,4 bytes for vout, 1 byte for empty script_sig
                let witness_weight = spend_info.estimate_witness_size();
                base_weight + witness_weight as u64
            })
            .sum::<u64>()
            / unspents.len() as u64;

        // Convert UTXOs to OutputGroups
        // TODO: Group UTXOs by address into single OutputGroups to mitigate privacy leaks from address reuse
        // TODO: Consider more sophisticated grouping policies in the future
        let output_groups: Vec<OutputGroup> = unspents
            .iter()
            .map(|(utxo, spend_info)| OutputGroup {
                value: utxo.amount.to_sat(),
                weight: 36 + spend_info.estimate_witness_size() as u64,
                input_count: 1,
                creation_sequence: None,
            })
            .collect();

        // Create coin selection options
        let coin_selection_option = CoinSelectionOpt {
            target_value: amount.to_sat(),
            target_feerate: feerate as f32,
            long_term_feerate: Some(LONG_TERM_FEERATE),
            min_absolute_fee: 500,
            base_weight: calculate_base_weight_btc(
                (target_weight.to_wu() * num_outputs) + change_weight.to_wu(),
            ),
            change_weight: change_weight.to_wu(),
            change_cost: cost_of_change,
            avg_input_weight,
            avg_output_weight,
            min_change_value: 100,
            excess_strategy: ExcessStrategy::ToChange,
        };

        match select_coin(&output_groups, &coin_selection_option) {
            Ok(selection) => {
                let selected_utxos: Vec<(ListUnspentResultEntry, UTXOSpendInfo)> = selection
                    .selected_inputs
                    .iter()
                    .map(|&index| unspents[index].clone())
                    .collect();
                log::info!("Coinselection concluded with {:?}", selection.waste);
                Ok(selected_utxos)
            }
            Err(e) => {
                // This is important for various tests and real life scenarios.
                log::warn!("Coin selection failed: {e}, attempting with available funds");

                // If error is insufficient funds, return all available UTXOs
                if e.to_string().contains("The Inputs funds are insufficient") {
                    let total_available = unspents
                        .iter()
                        .map(|(utxo, _)| utxo.amount.to_sat())
                        .sum::<u64>();

                    log::info!(
                        "Returning all available UTXOs totaling {} sats instead of requested {} sats",
                        total_available,
                        amount.to_sat()
                    );

                    Ok(unspents)
                } else {
                    // For other errors, return the original error
                    Err(WalletError::General(format!("Coin selection failed: {e}")))
                }
            }
        }
    }

    pub(crate) fn get_utxo(
        &self,
        (txid, vout): (Txid, u32),
    ) -> Result<Option<UTXOSpendInfo>, WalletError> {
        let mut seed_coin_utxo = self.list_descriptor_utxo_spend_info()?;
        let mut swap_coin_utxo = self.list_swap_coin_utxo_spend_info()?;
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
            //TODO should completely avoid derive_addresses
            //because it's slower and provides no benefit over using rust-bitcoin
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
        total_coinswap_amount: Amount,
        other_multisig_pubkeys: &[PublicKey],
        hashlock_pubkeys: &[PublicKey],
        hashvalue: Hash160,
        locktime: u16,
        fee_rate: Amount,
    ) -> Result<(Vec<Transaction>, Vec<OutgoingSwapCoin>, Amount), WalletError> {
        let (coinswap_addresses, my_multisig_privkeys): (Vec<_>, Vec<_>) = other_multisig_pubkeys
            .iter()
            .map(|other_key| self.create_and_import_coinswap_address(other_key))
            .collect::<Result<Vec<(Address, SecretKey)>, WalletError>>()?
            .into_iter()
            .unzip();

        let create_funding_txes_result =
            self.create_funding_txes(total_coinswap_amount, &coinswap_addresses, fee_rate)?;
        //for sweeping there would be another function, probably
        //probably have an enum called something like SendAmount which can be
        //an integer but also can be Sweep

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
                funding_amount,
                &contract_redeemscript,
                fee_rate,
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
                .map(|(bond, _)| {
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
}
