//! Manages connection with a Bitcoin Core RPC.
//!
use std::convert::TryFrom;

use bitcoin::{Address, Amount, Network, Txid};
use bitcoind::bitcoincore_rpc::{Auth, Client, RpcApi};
use serde_json::{json, Value};

use crate::{
    utill::{
        convert_json_rpc_bitcoin_to_satoshis, redeemscript_to_scriptpubkey, str_to_bitcoin_network,
        to_hex,
    },
    wallet::{api::KeychainKind, WalletSwapCoin},
};

use serde::Deserialize;

use super::{error::WalletError, Wallet};

/// Configuration parameters for connecting to a Bitcoin node via RPC.
#[derive(Debug, Clone)]
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

const RPC_HOSTPORT: &str = "localhost:18443";

impl Default for RPCConfig {
    fn default() -> Self {
        Self {
            url: RPC_HOSTPORT.to_string(),
            auth: Auth::UserPass("regtestrpcuser".to_string(), "regtestrpcpass".to_string()),
            network: Network::Regtest,
            wallet_name: "random-wallet-name".to_string(),
        }
    }
}

impl TryFrom<&RPCConfig> for Client {
    type Error = WalletError;
    fn try_from(config: &RPCConfig) -> Result<Self, WalletError> {
        let rpc = Client::new(
            format!(
                "http://{}/wallet/{}",
                config.url.as_str(),
                config.wallet_name.as_str()
            )
            .as_str(),
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

fn list_wallet_dir(client: &Client) -> Result<Vec<String>, WalletError> {
    #[derive(Deserialize)]
    struct Name {
        name: String,
    }
    #[derive(Deserialize)]
    struct CallResult {
        wallets: Vec<Name>,
    }

    let result: CallResult = client.call("listwalletdir", &[])?;
    Ok(result.wallets.into_iter().map(|n| n.name).collect())
}

impl Wallet {
    /// Sync the wallet with the configured Bitcoin Core RPC. Save data to disk.
    pub fn sync(&mut self) -> Result<(), WalletError> {
        // Create or load the watch-only bitcoin core wallet
        let wallet_name = &self.store.file_name;
        if self.rpc.list_wallets()?.contains(wallet_name) {
            log::debug!("wallet already loaded: {}", wallet_name);
        } else if list_wallet_dir(&self.rpc)?.contains(wallet_name) {
            self.rpc.load_wallet(wallet_name)?;
            log::debug!("wallet loaded: {}", wallet_name);
        } else {
            // pre-0.21 use legacy wallets
            if self.rpc.version()? < 210_000 {
                self.rpc
                    .create_wallet(wallet_name, Some(true), None, None, None)?;
            } else {
                // TODO: move back to api call when https://github.com/rust-bitcoin/rust-bitcoincore-rpc/issues/225 is closed
                let args = [
                    Value::String(wallet_name.clone()),
                    Value::Bool(true),  // Disable Private Keys
                    Value::Bool(false), // Create a blank wallet
                    Value::Null,        // Optional Passphrase
                    Value::Bool(false), // Avoid Reuse
                    Value::Bool(false), // Descriptor Wallet
                ];
                let _: Value = self.rpc.call("createwallet", &args)?;
            }

            log::debug!("wallet created: {}", wallet_name);
        }

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
            .filter(|d| !self.is_swapcoin_descriptor_imported(d))
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
                .filter(|d| !self.is_swapcoin_descriptor_imported(d)),
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
            .collect::<Vec<_>>();

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
                .collect::<Vec<_>>(),
        );

        let is_fidelity_addrs_imported = {
            let mut spks = self
                .store
                .fidelity_bond
                .iter()
                .map(|(_, (b, _, _))| b.script_pub_key());
            let (first_addr, last_addr) = (spks.next(), spks.last());

            let is_first_imported = if let Some(spk) = first_addr {
                let ad = Address::from_script(&spk, self.store.network)?;
                log::debug!("First Fidelity Addrs: {}", ad);
                self.rpc
                    .get_address_info(&ad)?
                    .is_watchonly
                    .unwrap_or(false)
            } else {
                true // mark true if theres no spk to import
            };

            let is_last_imported = if let Some(spk) = last_addr {
                let ad = Address::from_script(&spk, self.store.network)?;
                log::debug!("Last Fidelity Addr: {}", ad);
                self.rpc
                    .get_address_info(&ad)?
                    .is_watchonly
                    .unwrap_or(false)
            } else {
                true // mark true if theres no spks to import
            };

            is_first_imported && is_last_imported
        };

        if hd_descriptors_to_import.is_empty()
            && swapcoin_descriptors_to_import.is_empty()
            && contract_scriptpubkeys_to_import.is_empty()
            && is_fidelity_addrs_imported
        {
            return Ok(());
        }

        log::debug!("Importing Wallet spks/descriptors");
        log::debug!("HD descriptors: {:?}", hd_descriptors_to_import);
        log::debug!("Swapcoin descriptors: {:?}", swapcoin_descriptors_to_import);
        log::debug!("Contract SPKs: {:?}", contract_scriptpubkeys_to_import);

        self.import_addresses(
            &hd_descriptors_to_import,
            &swapcoin_descriptors_to_import,
            &contract_scriptpubkeys_to_import,
        )?;

        // Abort a previous scan, if any
        self.rpc.call::<Value>("scantxoutset", &[json!("abort")])?;

        // The final descriptor list to import
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
            .chain(self.store.fidelity_bond.iter().map(|(_, (bond, _, _))| {
                let spk = bond.script_pub_key();
                json!({ "desc": format!("raw({:x})", spk) })
            }))
            .collect::<Vec<Value>>();

        // Now run the scan
        log::debug!("Initializing TxOut scan. This may take a while.");
        let scantxoutset_result: Value = self
            .rpc
            .call("scantxoutset", &[json!("start"), json!(desc_list)])?;
        if !scantxoutset_result["success"].as_bool().unwrap() {
            return Err(WalletError::Rpc(
                bitcoind::bitcoincore_rpc::Error::UnexpectedStructure,
            ));
        }
        log::debug!(
            "TxOut set scan complete, found {} btc",
            Amount::from_sat(convert_json_rpc_bitcoin_to_satoshis(
                &scantxoutset_result["total_amount"]
            )),
        );
        let unspent_list = scantxoutset_result["unspents"].as_array().unwrap();
        log::debug!(
            "Found \ncoins={} \ntxouts={} \nheight={} \nbestblock={}",
            unspent_list.len(),
            scantxoutset_result["txouts"].as_u64().unwrap(),
            scantxoutset_result["height"].as_u64().unwrap(),
            scantxoutset_result["bestblock"].as_str().unwrap(),
        );
        for unspent in unspent_list {
            let blockhash = self
                .rpc
                .get_block_hash(unspent["height"].as_u64().unwrap())?;
            let txid = unspent["txid"].as_str().unwrap().parse::<Txid>().unwrap();
            let rawtx = self.rpc.get_raw_transaction_hex(&txid, Some(&blockhash))?;
            let merkleproof = to_hex(&self.rpc.get_tx_out_proof(&[txid], Some(&blockhash))?);
            self.rpc.call(
                "importprunedfunds",
                &[Value::String(rawtx), Value::String(merkleproof)],
            )?;
        }

        let max_external_index = self.find_hd_next_index(KeychainKind::External)?;
        self.update_external_index(max_external_index)?;
        Ok(())
    }
}
