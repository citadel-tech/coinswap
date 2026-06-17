//! Watchtower backend: querying bitcoind, scanning mempool, and running discovery via REST API's.

use std::{
    collections::HashMap,
    fs,
    str::FromStr,
    sync::{Arc, Mutex, OnceLock},
};

use bitcoin::{consensus::deserialize, Block, BlockHash, OutPoint, Transaction, Txid};
use bitcoind::bitcoincore_rpc::{json::GetBlockchainInfoResult, jsonrpc::base64, Auth};
use electrum_client::Client as ElectrumClient;
use serde::de::DeserializeOwned;

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::FileRegistry, utils::process_transaction, watcher_error::WatcherError,
    },
};

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// Lightweight wrapper around bitcoind REST endpoints used by the watchtower (no longer uses JSON-RPC).
#[derive(Clone)]
pub struct BitcoinRest {
    base_url: String,
    auth_header: Option<String>,
    timeout_secs: u64,
}

impl BitcoinRest {
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

    /// Returns the block height at which an unspent output was confirmed.
    pub fn get_utxo_confirmation_height(&self, outpoint: &OutPoint) -> Result<u32, WatcherError> {
        #[derive(serde::Deserialize)]
        struct GetUtxosResponse {
            utxos: Vec<RestUtxo>,
        }

        #[derive(serde::Deserialize)]
        struct RestUtxo {
            height: u32,
        }

        let response: GetUtxosResponse = self.get_json(&format!(
            "/rest/getutxos/{}-{}.json",
            outpoint.txid, outpoint.vout
        ))?;
        response
            .utxos
            .first()
            .map(|utxo| utxo.height)
            .ok_or_else(|| WatcherError::General(format!("Fidelity UTXO {outpoint} not found")))
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

/// Process-wide pool of long-lived [`ElectrumClient`] connections keyed by URL.
///
// Opening a fresh TCP/SSL connection per request burns sockets and can trip rate-limits on public Electrum servers, so the helpers below cache one
// client per URL and reuse it. Dead entries are evicted on error so the next call reconnects.
static ELECTRUM_POOL: OnceLock<Mutex<HashMap<String, Arc<ElectrumClient>>>> = OnceLock::new();

fn electrum_pool() -> &'static Mutex<HashMap<String, Arc<ElectrumClient>>> {
    ELECTRUM_POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn electrum_client(url: &str) -> Result<Arc<ElectrumClient>, WatcherError> {
    let mut g = electrum_pool()
        .lock()
        .map_err(|_| WatcherError::General("electrum pool poisoned".into()))?;
    if let Some(c) = g.get(url) {
        return Ok(c.clone());
    }
    let c = Arc::new(ElectrumClient::new(url).map_err(|e| WatcherError::General(format!("{e}")))?);
    g.insert(url.to_string(), c.clone());
    Ok(c)
}

/// Evict a (presumed dead) client so the next call reconnects.
fn drop_electrum_client(url: &str) {
    if let Ok(mut g) = electrum_pool().lock() {
        g.remove(url);
    }
}

/// Run `f` against the pooled Electrum client for `url`. On error, evict the client (so the next call reconnects).
pub(crate) fn with_electrum_client<T, F>(url: &str, f: F) -> Result<T, WatcherError>
where
    F: FnOnce(&ElectrumClient) -> Result<T, electrum_client::Error>,
{
    let client = electrum_client(url)?;
    f(&client).map_err(|e| {
        drop_electrum_client(url);
        WatcherError::General(format!("{e}"))
    })
}

/// Match an Electrum server's `server_features().genesis_hash` against the known network genesis blocks.
pub fn network_from_electrum_genesis(genesis: &[u8]) -> Option<bitcoin::Network> {
    use bitcoin::hashes::Hash;
    for net in [
        bitcoin::Network::Bitcoin,
        bitcoin::Network::Testnet,
        bitcoin::Network::Signet,
        bitcoin::Network::Regtest,
    ] {
        let mut local = bitcoin::constants::genesis_block(net)
            .block_hash()
            .to_raw_hash()
            .to_byte_array();
        // Electrum returns `genesis_hash` in display (big-endian) byte order, while `to_byte_array()` returns internal little-endian bytes, hence the reverse.
        local.reverse();
        if local == genesis {
            return Some(net);
        }
    }
    None
}

/// Bitcoin Core's chain-name string for a network ("main"/"test"/"signet"/"regtest").
pub fn chain_name_for(net: bitcoin::Network) -> &'static str {
    match net {
        bitcoin::Network::Bitcoin => "main",
        bitcoin::Network::Testnet => "test",
        bitcoin::Network::Signet => "signet",
        bitcoin::Network::Regtest => "regtest",
        _ => "unknown",
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
