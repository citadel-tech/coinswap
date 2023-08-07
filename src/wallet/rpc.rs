use std::convert::TryFrom;

use bitcoin::{
    hashes::hex::{FromHex, ToHex},
    Address, Amount, Network, Script, Txid,
};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde_json::{json, Value};

use crate::{
    protocol::contract::redeemscript_to_scriptpubkey,
    utill::convert_json_rpc_bitcoin_to_satoshis,
    wallet::{wallet::KeychainKind, WalletSwapCoin},
};

use super::{error::WalletError, Wallet};

pub struct RPCConfig {
    /// The bitcoin node url
    pub url: String,
    /// The bitcoin node authentication mechanism
    pub auth: Auth,
    /// The network we are using (it will be checked the bitcoin node network matches this)
    pub network: Network,
    /// The wallet name in the bitcoin node, derive this from the descriptor.
    pub wallet_name: String,
}

const RPC_WALLET: &str = "teleport";
const RPC_HOSTPORT: &str = "localhost:18443";

impl Default for RPCConfig {
    fn default() -> Self {
        Self {
            url: RPC_HOSTPORT.to_string(),
            auth: Auth::UserPass("regtestrpcuser".to_string(), "regtestrpcpass".to_string()),
            network: Network::Regtest,
            wallet_name: RPC_WALLET.to_string(),
        }
    }
}

fn str_to_bitcoin_network(net_str: &str) -> Network {
    match net_str {
        "main" => Network::Bitcoin,
        "test" => Network::Testnet,
        "signet" => Network::Signet,
        "regtest" => Network::Regtest,
        _ => panic!("unknown network: {}", net_str),
    }
}

impl TryFrom<&RPCConfig> for Client {
    type Error = WalletError;
    fn try_from(config: &RPCConfig) -> Result<Self, WalletError> {
        let rpc = Client::new(
            format!("http://{}/wallet/{}", config.url, config.wallet_name),
            config.auth.clone(),
        )?;
        if config.network != str_to_bitcoin_network(rpc.get_blockchain_info()?.chain.as_str()) {
            return Err(WalletError::Protocol(
                "RPC Network not mathcing with RPCConfig".to_string(),
            ));
        }
        Ok(rpc)
    }
}

impl Wallet {
    pub fn sync(&mut self) -> Result<(), WalletError> {
        //TODO many of these unwraps to be replaced with proper error handling
        let hd_descriptors_to_import = self.get_unimoprted_wallet_desc()?;

        let mut swapcoin_descriptors_to_import = self
            .store
            .incoming_swapcoins
            .values()
            .map(|sc| {
                format!(
                    "wsh(sortedmulti(2,{},{}))",
                    sc.get_other_pubkey(),
                    sc.get_my_pubkey()
                )
            })
            .map(|d| self.rpc.get_descriptor_info(&d).unwrap().descriptor)
            .filter(|d| !self.is_swapcoin_descriptor_imported(&d))
            .collect::<Vec<String>>();

        swapcoin_descriptors_to_import.extend(
            self.store
                .outgoing_swapcoins
                .values()
                .map(|sc| {
                    format!(
                        "wsh(sortedmulti(2,{},{}))",
                        sc.get_other_pubkey(),
                        sc.get_my_pubkey()
                    )
                })
                .map(|d| self.rpc.get_descriptor_info(&d).unwrap().descriptor)
                .filter(|d| !self.is_swapcoin_descriptor_imported(&d)),
        );

        let mut contract_scriptpubkeys_to_import = self
            .store
            .incoming_swapcoins
            .values()
            .filter_map(|sc| {
                let contract_spk = redeemscript_to_scriptpubkey(&sc.contract_redeemscript);
                let addr_info = self
                    .rpc
                    .get_address_info(
                        &Address::from_script(&contract_spk, self.store.network)
                            .expect("address wrong"),
                    )
                    .unwrap();
                if addr_info.is_watchonly.is_none() {
                    Some(contract_spk)
                } else {
                    None
                }
            })
            .collect::<Vec<Script>>();

        contract_scriptpubkeys_to_import.extend(
            self.store
                .outgoing_swapcoins
                .values()
                .filter_map(|sc| {
                    let contract_spk = redeemscript_to_scriptpubkey(&sc.contract_redeemscript);
                    let addr_info = self
                        .rpc
                        .get_address_info(
                            &Address::from_script(&contract_spk, self.store.network)
                                .expect("address wrong"),
                        )
                        .unwrap();
                    if addr_info.is_watchonly.is_none() {
                        Some(contract_spk)
                    } else {
                        None
                    }
                })
                .collect::<Vec<Script>>(),
        );

        //get first and last timelocked script, check if both are imported
        let first_timelocked_addr = Address::p2wsh(
            &self.get_timelocked_redeemscript_from_index(0),
            self.store.network,
        );
        let last_timelocked_addr = Address::p2wsh(
            &self.get_timelocked_redeemscript_from_index(
                super::fidelity::TIMELOCKED_ADDRESS_COUNT - 1,
            ),
            self.store.network,
        );
        log::debug!(target: "wallet", "first_timelocked_addr={} last_timelocked_addr={}",
            first_timelocked_addr, last_timelocked_addr);
        let is_timelock_branch_imported = self
            .rpc
            .get_address_info(&first_timelocked_addr)?
            .is_watchonly
            .unwrap_or(false)
            && self
                .rpc
                .get_address_info(&last_timelocked_addr)?
                .is_watchonly
                .unwrap_or(false);

        log::debug!(target: "wallet",
            concat!("hd_descriptors_to_import.len = {} swapcoin_descriptors_to_import.len = {}",
                " contract_scriptpubkeys_to_import = {} is_timelock_branch_imported = {}"),
            hd_descriptors_to_import.len(), swapcoin_descriptors_to_import.len(),
            contract_scriptpubkeys_to_import.len(),
            is_timelock_branch_imported);
        if hd_descriptors_to_import.is_empty()
            && swapcoin_descriptors_to_import.is_empty()
            && contract_scriptpubkeys_to_import.is_empty()
            && is_timelock_branch_imported
        {
            return Ok(());
        }

        log::info!(target: "wallet", "New wallet detected, synchronizing balance...");
        self.import_addresses(
            &hd_descriptors_to_import,
            &swapcoin_descriptors_to_import,
            &contract_scriptpubkeys_to_import,
        )?;

        self.rpc.call::<Value>("scantxoutset", &[json!("abort")])?;
        let desc_list = hd_descriptors_to_import
            .iter()
            .map(|d| {
                json!(
                {"desc": d,
                "range": self.get_addrss_import_count() -1})
            })
            .chain(swapcoin_descriptors_to_import.iter().map(|d| json!(d)))
            .chain(
                contract_scriptpubkeys_to_import
                    .iter()
                    .map(|spk| json!({ "desc": format!("raw({:x})", spk) })),
            )
            .chain(
                self.store
                    .fidelity_scripts
                    .keys()
                    .map(|spk| json!({ "desc": format!("raw({:x})", spk) })),
            )
            .collect::<Vec<Value>>();

        let scantxoutset_result: Value = self
            .rpc
            .call("scantxoutset", &[json!("start"), json!(desc_list)])?;
        if !scantxoutset_result["success"].as_bool().unwrap() {
            return Err(WalletError::Rpc(
                bitcoincore_rpc::Error::UnexpectedStructure,
            ));
        }
        log::info!(target: "wallet", "TxOut set scan complete, found {} btc",
            Amount::from_sat(convert_json_rpc_bitcoin_to_satoshis(&scantxoutset_result["total_amount"])),
        );
        let unspent_list = scantxoutset_result["unspents"].as_array().unwrap();
        log::debug!(target: "wallet", "scantxoutset found_coins={} txouts={} height={} bestblock={}",
            unspent_list.len(),
            scantxoutset_result["txouts"].as_u64().unwrap(),
            scantxoutset_result["height"].as_u64().unwrap(),
            scantxoutset_result["bestblock"].as_str().unwrap(),
        );
        for unspent in unspent_list {
            let blockhash = self
                .rpc
                .get_block_hash(unspent["height"].as_u64().unwrap())?;
            let txid = Txid::from_hex(unspent["txid"].as_str().unwrap()).unwrap();
            let rawtx = self.rpc.get_raw_transaction_hex(&txid, Some(&blockhash));
            if let Ok(rawtx_hex) = rawtx {
                log::debug!(target: "wallet", "found coin {}:{} {} height={} {}",
                    txid,
                    unspent["vout"].as_u64().unwrap(),
                    Amount::from_sat(convert_json_rpc_bitcoin_to_satoshis(&unspent["amount"])),
                    unspent["height"].as_u64().unwrap(),
                    unspent["desc"].as_str().unwrap(),
                );
                let merkleproof = self
                    .rpc
                    .get_tx_out_proof(&[txid], Some(&blockhash))?
                    .to_hex();
                self.rpc.call(
                    "importprunedfunds",
                    &[Value::String(rawtx_hex), Value::String(merkleproof)],
                )?;
            } else {
                log::error!(target: "wallet", "block pruned, TODO add UTXO to wallet file");
                panic!("teleport doesnt work with pruning yet, try rescanning");
            }
        }

        let max_external_index = self.find_hd_next_index(KeychainKind::External)?;
        self.update_external_index(max_external_index)?;
        Ok(())
    }
}
