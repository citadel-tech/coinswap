//! Watchtower backend: querying bitcoind, scanning mempool, and running discovery via REST API's.

use std::{fs, str::FromStr};

use bitcoin::{consensus::deserialize, Block, BlockHash, Transaction, Txid};
use bitcoind::bitcoincore_rpc::{json::GetBlockchainInfoResult, jsonrpc::base64, Auth};
use serde::de::DeserializeOwned;

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::FileRegistry, utils::process_transaction, watcher_error::WatcherError,
    },
};

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// Lightweight wrapper around bitcoind REST endpoints used by the watchtower (no longer uses JSON-RPC).
pub struct BitcoinRpc {
    base_url: String,
    auth_header: Option<String>,
    timeout_secs: u64,
}

impl BitcoinRpc {
    /// Constructs a new REST wrapper using the provided configuration.
    pub fn new(rpc_config: RPCConfig) -> Result<Self, WatcherError> {
        let base_url = normalize_rest_base_url(&rpc_config.url);
        let auth_header = build_auth_header(&rpc_config.auth)?;
        Ok(Self {
            base_url,
            auth_header,
            timeout_secs: DEFAULT_HTTP_TIMEOUT_SECS,
        })
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn http_get(&self, path: &str) -> Result<minreq::Response, WatcherError> {
        let url = self.url(path);
        let mut req = minreq::get(url).with_timeout(self.timeout_secs);
        if let Some(h) = &self.auth_header {
            req = req.with_header("Authorization", h);
        }
        let resp = req.send()?;
        if !(200..300).contains(&resp.status_code) {
            let body = resp
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| String::new());
            return Err(WatcherError::HttpStatus {
                status: resp.status_code,
                body,
            });
        }
        Ok(resp)
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, WatcherError> {
        Ok(self.http_get(path)?.json::<T>()?)
    }

    fn get_bytes(&self, path: &str) -> Result<Vec<u8>, WatcherError> {
        let resp = self.http_get(path)?;
        Ok(resp.as_bytes().to_vec())
    }

    /// Get txids of all transactions in the mempool.
    /// Uses `/rest/mempool/contents.json` and returns the map keys.
    pub fn get_raw_mempool(&self) -> Result<Vec<Txid>, WatcherError> {
        let obj: serde_json::Map<String, serde_json::Value> =
            self.get_json("/rest/mempool/contents.json")?;
        Ok(obj.keys().filter_map(|k| Txid::from_str(k).ok()).collect())
    }

    /// Fetches a full transaction by txid.
    /// Uses the binary endpoint `/rest/tx/<txid>.bin`.
    pub fn get_raw_tx(&self, txid: &Txid) -> Result<Transaction, WatcherError> {
        let bytes = self.get_bytes(&format!("/rest/tx/{txid}.bin"))?;
        Ok(deserialize::<Transaction>(&bytes)?)
    }

    /// Returns chain metadata.
    /// Uses `/rest/chaininfo.json`.
    pub fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WatcherError> {
        self.get_json("/rest/chaininfo.json")
    }

    /// Retrieves the block hash at a given height.
    /// Uses `/rest/blockhashbyheight/<height>.hex`.
    pub fn get_block_hash(&self, height: u64) -> Result<BlockHash, WatcherError> {
        let resp = self.http_get(&format!("/rest/blockhashbyheight/{height}.hex"))?;
        let hex = resp
            .as_str()
            .map_err(|_| WatcherError::ParsingError)?
            .trim();
        BlockHash::from_str(hex).map_err(|_| WatcherError::ParsingError)
    }

    /// Returns the current chain height.
    pub fn get_block_count(&self) -> Result<u64, WatcherError> {
        Ok(self.get_blockchain_info()?.blocks)
    }

    /// Fetches a block by hash.
    /// Uses the binary endpoint `/rest/block/<hash>.bin`.
    pub fn get_block(&self, hash: BlockHash) -> Result<Block, WatcherError> {
        let bytes = self.get_bytes(&format!("/rest/block/{hash}.bin"))?;
        Ok(deserialize::<Block>(&bytes)?)
    }

    /// Processes transactions in the mempool and updates registry.
    pub fn process_mempool(&self, registry: &mut FileRegistry) -> Result<(), WatcherError> {
        let txids = self.get_raw_mempool()?;
        for txid in &txids {
            let tx = self.get_raw_tx(txid)?;
            process_transaction(&tx, registry, false);
        }
        Ok(())
    }
}

fn normalize_rest_base_url(url: &str) -> String {
    // `RPCConfig::url` is often an RPC endpoint like:
    //   http://127.0.0.1:8332/wallet/<name>
    // REST endpoints ignore the wallet path, so we normalize to just the scheme + authority.
    let mut base = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("http://{url}")
    };

    // Strip wallet path if present.
    if let Some(idx) = base.find("/wallet/") {
        base.truncate(idx);
        return base;
    }

    // Strip any other path component after the host:port.
    let authority_start = base.find("://").map(|i| i + 3).unwrap_or(0);
    if let Some(rel) = base[authority_start..].find('/') {
        base.truncate(authority_start + rel);
    }

    base
}

fn build_auth_header(auth: &Auth) -> Result<Option<String>, WatcherError> {
    match auth {
        Auth::None => Ok(None),
        Auth::UserPass(u, p) => Ok(Some(format!(
            "Basic {}",
            base64::encode(format!("{u}:{p}").as_bytes())
        ))),
        Auth::CookieFile(path) => {
            let cookie = fs::read_to_string(path)?;
            let cookie = cookie.trim();
            Ok(Some(format!("Basic {}", base64::encode(cookie.as_bytes()))))
        }
    }
}
