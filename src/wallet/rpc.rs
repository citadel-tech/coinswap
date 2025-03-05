//! Manages connection with a Bitcoin Core RPC.
//!
use std::{convert::TryFrom, thread};

use bitcoind::bitcoincore_rpc::{Auth, Client, RpcApi};
use serde_json::{json, Value};

use crate::{utill::HEART_BEAT_INTERVAL, wallet::api::KeychainKind};

use serde::Deserialize;

use super::{error::WalletError, Wallet};

/// Configuration parameters for connecting to a Bitcoin node via RPC.
#[derive(Debug, Clone)]
pub struct RPCConfig {
    /// The bitcoin node url
    pub url: String,
    /// The bitcoin node authentication mechanism
    pub auth: Auth,
    /// The wallet name in the bitcoin node, derive this from the descriptor.
    pub wallet_name: String,
}

const RPC_HOSTPORT: &str = "localhost:18443";

impl Default for RPCConfig {
    fn default() -> Self {
        Self {
            url: RPC_HOSTPORT.to_string(),
            auth: Auth::UserPass("regtestrpcuser".to_string(), "regtestrpcpass".to_string()),
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
                    Value::Bool(true),  // Descriptor Wallet
                ];
                let _: Value = self.rpc.call("createwallet", &args)?;
            }

            log::debug!("wallet created: {}", wallet_name);
        }

        let descriptors_to_import = self.descriptors_to_import()?;

        if descriptors_to_import.is_empty() {
            return Ok(());
        }

        log::debug!("Importing Wallet spks/descriptors");

        self.import_descriptors(&descriptors_to_import, None)?;

        // Now run the scan
        log::debug!("Initializing TxOut scan. This may take a while.");

        // Sometimes in test multiple wallet scans can occur at same time, resulting in error.
        // Just retry after 3 sec.
        loop {
            let last_synced_height = self
                .store
                .last_synced_height
                .unwrap_or(0)
                .max(self.store.wallet_birthday.unwrap_or(0));
            let node_synced = self.rpc.get_block_count()?;
            log::debug!(
                "Re-scanning Blockchain from:{} to:{}",
                last_synced_height,
                node_synced
            );
            match self.rpc.rescan_blockchain(
                Some(last_synced_height as usize),
                Some(node_synced as usize),
            ) {
                Ok(_) => {
                    self.store.last_synced_height = Some(node_synced);
                    break;
                }

                Err(e) => {
                    log::warn!("Sync Error, Retrying: {}", e);
                    thread::sleep(HEART_BEAT_INTERVAL);
                    continue;
                }
            }
        }

        self.update_utxo_cache(self.get_all_utxo()?);

        let max_external_index = self.find_hd_next_index(KeychainKind::External)?;
        self.update_external_index(max_external_index)?;
        self.refresh_offer_maxsize_cache()?;
        Ok(())
    }

    /// Keep retrying sync until success and log failure.
    // This is useful to handle transient RPC errors.
    pub fn sync_no_fail(&mut self) {
        while let Err(e) = self.sync() {
            log::error!("Blockchain sync failed. Retrying. | {:?}", e);
        }
    }

    /// Import watch addresses into core wallet. Does not check if the address was already imported.
    pub(crate) fn import_descriptors(
        &self,
        descriptors_to_import: &[String],
        address_label: Option<String>,
    ) -> Result<(), WalletError> {
        let address_label = address_label.unwrap_or(self.get_core_wallet_label());

        let import_requests = descriptors_to_import
            .iter()
            .map(|desc| {
                if desc.contains("/*") {
                    return json!({
                        "timestamp": "now",
                        "desc": desc,
                        "range": (self.get_addrss_import_count() - 1)
                    });
                }
                json!({
                    "timestamp": "now",
                    "desc": desc,
                    "label": address_label
                })
            })
            .collect();
        let _res: Vec<Value> = self.rpc.call("importdescriptors", &[import_requests])?;
        Ok(())
    }
}
