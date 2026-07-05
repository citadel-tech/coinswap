//! Electrum-protocol wallet backend.
//!
//! Implements [`BlockchainBackend`] over an `electrum_client::Client`, faking
//! Bitcoin Core's server-side wallet + block APIs with local maps so the rest
//! of the wallet code can stay backend-agnostic. Bitcoin Core's own backend and
//! the shared [`BackendConfig`]/[`BlockchainBackend`] machinery live in
//! [`super::rpc`].

use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Debug},
    sync::Mutex,
};

use bitcoin::{
    bip32::{ChildNumber, Xpub},
    block::Header,
    consensus::encode::serialize_hex,
    hashes::Hash,
    Address, BlockHash, CompressedPublicKey, OutPoint, Script, ScriptBuf, Transaction, Txid,
};
use bitcoind::bitcoincore_rpc::{
    bitcoincore_rpc_json, json::ListUnspentResultEntry, Error as RpcError, RawTx,
    Result as RpcResult, RpcApi,
};
use electrum_client::{Client as ElectrumClient, ElectrumApi};
use serde_json::{json, Value};

use super::{
    error::WalletError,
    rpc::{BackendConfig, BlockchainBackend, HdOrigin},
};

impl BlockchainBackend for ElectrumBackend {
    type Config = ElectrumConfig;
    const IS_ELECTRUM: bool = true;
    fn from_config(c: &ElectrumConfig) -> Result<Self, WalletError> {
        ElectrumBackend::new(c)
    }
    fn from_backend_config(b: &BackendConfig) -> Result<&ElectrumConfig, WalletError> {
        match b {
            BackendConfig::Electrum(c) => Ok(c),
            BackendConfig::Bitcoind(_) => Err(WalletError::General(
                "expected Electrum, got Bitcoind".into(),
            )),
        }
    }
    fn watch_script(&self, script: &Script, hd: Option<HdOrigin>) {
        if let Ok(mut w) = self.watched.lock() {
            w.insert(script.to_owned());
        }
        if let Some(hd) = hd {
            if let Ok(mut paths) = self.hd_paths.lock() {
                paths.insert(script.to_owned(), hd);
            }
        }
    }
    fn hd_origin_for_script(&self, script: &Script) -> Option<HdOrigin> {
        self.hd_paths.lock().ok()?.get(script).cloned()
    }
}

/// Electrum-protocol backend.
///
/// Electrum servers expose chain data indexed by scripthash and have no
/// server-side wallet concept. This adapter therefore keeps a local
/// `watched` set of script_pubkeys to look up via Electrum, and a local
/// `locked` set since Electrum has no equivalent of `lockunspent`.
pub struct ElectrumBackend {
    pub(crate) inner: ElectrumClient,
    /// Scripts the wallet has asked us to track via [`BlockchainBackend::watch_script`].
    pub(crate) watched: Mutex<HashSet<ScriptBuf>>,
    /// HD-origin hint per watched script (when known), so UTXOs on those
    /// scripts can be classified as `SeedCoin` without a descriptor round-trip.
    pub(crate) hd_paths: Mutex<HashMap<ScriptBuf, HdOrigin>>,
    /// Outpoints the wallet has marked as locked (client-side only).
    pub(crate) locked: Mutex<HashSet<OutPoint>>,
    /// height → block hash cache to satisfy `get_block_hash`/`get_block_header`.
    pub(crate) height_to_hash: Mutex<HashMap<u64, BlockHash>>,
    /// reverse cache for `get_block_header(&hash)`.
    pub(crate) hash_to_height: Mutex<HashMap<BlockHash, u64>>,
    /// Network derived from the server's reported genesis hash.
    pub(crate) network: bitcoin::Network,
}

impl Debug for ElectrumBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElectrumBackend")
            .field("network", &self.network)
            .finish()
    }
}

/// Configuration for an Electrum-protocol backend.
#[derive(Debug, Clone)]
pub struct ElectrumConfig {
    /// Electrum server endpoint (e.g. `"tcp://localhost:50001"` or
    /// `"ssl://electrum.example.org:50002"`).
    pub url: String,
    /// On-disk wallet file name. Read via [`BackendConfig::wallet_name`] when
    /// the maker/taker init derives the wallet path.
    pub wallet_name: String,
}

impl Default for ElectrumConfig {
    fn default() -> Self {
        Self {
            url: "electrum1.bluewallet.io:50001".to_string(),
            wallet_name: "coinswap-wallet".to_string(),
        }
    }
}

impl ElectrumBackend {
    /// Connect to an Electrum server and derive the network from its genesis hash.
    pub fn new(cfg: &ElectrumConfig) -> Result<Self, WalletError> {
        let inner = ElectrumClient::new(&cfg.url)
            .map_err(|e| WalletError::General(format!("electrum connect: {e}")))?;
        let features = inner
            .server_features()
            .map_err(|e| WalletError::General(format!("electrum server_features: {e}")))?;
        let network =
            crate::watch_tower::rest_backend::network_from_electrum_genesis(&features.genesis_hash)
                .ok_or_else(|| {
                    WalletError::General(format!(
                        "unknown genesis from electrum server: {:?}",
                        features.genesis_hash
                    ))
                })?;
        Ok(Self {
            inner,
            watched: Mutex::new(HashSet::new()),
            hd_paths: Mutex::new(HashMap::new()),
            locked: Mutex::new(HashSet::new()),
            height_to_hash: Mutex::new(HashMap::new()),
            hash_to_height: Mutex::new(HashMap::new()),
            network,
        })
    }

    /// Look up the block hash for a height, populating the local caches.
    fn header_at(&self, height: u64) -> RpcResult<Header> {
        let header = self
            .inner
            .block_header(height as usize)
            .map_err(electrum_err)?;
        let hash = header.block_hash();
        if let (Ok(mut h2h), Ok(mut hash_map)) =
            (self.height_to_hash.lock(), self.hash_to_height.lock())
        {
            h2h.insert(height, hash);
            hash_map.insert(hash, height);
        }
        Ok(header)
    }

    /// Resolve a block hash to its height. Electrum has no hash->header RPC, so
    /// we serve from the reverse cache, falling back to a bounded scan back from
    /// the tip (callers only ever pass a recent hash: a fresh tip or a fidelity
    /// bond's confirmation block). Populates the caches on a scan hit.
    fn height_for_hash(&self, hash: &BlockHash) -> RpcResult<u64> {
        if let Ok(rev) = self.hash_to_height.lock() {
            if let Some(h) = rev.get(hash) {
                return Ok(*h);
            }
        }
        // Bounded scan from the tip. `header_at` caches each height it touches.
        const MAX_SCAN: u64 = 1024;
        let tip = self.get_block_count()?;
        for height in (tip.saturating_sub(MAX_SCAN)..=tip).rev() {
            if self.header_at(height)?.block_hash() == *hash {
                return Ok(height);
            }
        }
        Err(RpcError::ReturnedError(format!(
            "electrum: unknown block hash {hash} (not within {MAX_SCAN} blocks of tip)"
        )))
    }

    /// Expand a wallet-emitted descriptor (`wpkh(<xpub>/<chain>/*)` or
    /// `tr(<xpub>/<chain>/*)`) to addresses at indices `start..=end`.
    /// Strips an optional `#checksum` suffix and the BIP-32 derivation `*` wildcard.
    fn derive_addresses_local(
        &self,
        descriptor: &str,
        start: u32,
        end: u32,
    ) -> RpcResult<Vec<Address>> {
        let bad = |msg: &str| RpcError::ReturnedError(format!("deriveaddresses: {msg}"));

        // Drop the optional `#csum` suffix.
        let body = descriptor.split('#').next().unwrap_or(descriptor);
        let (kind, inner) = if let Some(rest) = body.strip_prefix("wpkh(") {
            ("wpkh", rest)
        } else if let Some(rest) = body.strip_prefix("tr(") {
            ("tr", rest)
        } else {
            return Err(bad(&format!("unsupported descriptor: {descriptor}")));
        };
        let inner = inner
            .strip_suffix(')')
            .ok_or_else(|| bad(&format!("missing closing `)`: {descriptor}")))?;
        // Expect: `<xpub>/<chain>/*`
        let parts: Vec<&str> = inner.rsplitn(3, '/').collect();
        if parts.len() != 3 || parts[0] != "*" {
            return Err(bad(&format!("unexpected key form: {inner}")));
        }
        let chain_idx: u32 = parts[1].parse().map_err(|_| bad("chain index parse"))?;
        let xpub_str = parts[2];
        let xpub: Xpub = xpub_str.parse().map_err(|_| bad("xpub parse"))?;

        let secp = crate::utill::global_secp();
        let mut out = Vec::with_capacity((end - start + 1) as usize);
        for i in start..=end {
            let path = [
                ChildNumber::Normal { index: chain_idx },
                ChildNumber::Normal { index: i },
            ];
            let child = xpub
                .derive_pub(secp, &path)
                .map_err(|e| bad(&format!("derive: {e}")))?;
            let addr = match kind {
                "wpkh" => {
                    let pk = CompressedPublicKey(child.public_key);
                    Address::p2wpkh(&pk, self.network)
                }
                "tr" => {
                    let (xonly, _parity) = child.public_key.x_only_public_key();
                    Address::p2tr(secp, xonly, None, self.network)
                }
                _ => unreachable!(),
            };
            out.push(addr);
        }
        Ok(out)
    }
}

/// Adapt an Electrum-client error into a `bitcoincore_rpc::Error` so the
/// [`RpcApi`] surface stays uniform.
fn electrum_err(e: electrum_client::Error) -> RpcError {
    RpcError::ReturnedError(format!("electrum: {e}"))
}

impl RpcApi for ElectrumBackend {
    /// Catch-all dispatch for JSON-RPC method names the wallet still calls
    /// via [`RpcApi::call`]. Methods that don't translate cleanly (e.g.
    /// `importdescriptors`) are handled here as local-state mutations.
    fn call<T: for<'a> serde::de::Deserialize<'a>>(
        &self,
        cmd: &str,
        args: &[Value],
    ) -> RpcResult<T> {
        let v: Value = match cmd {
            // Descriptor import is a no-op on Electrum — the wallet pre-derives
            // scripts and registers them via `ElectrumBackend::watch_script`.
            "importdescriptors" => {
                let count = args
                    .first()
                    .and_then(|v| v.as_array())
                    .map_or(0, |a| a.len());
                Value::Array(
                    (0..count)
                        .map(|_| json!({ "success": true }))
                        .collect::<Vec<_>>(),
                )
            }
            // Local lock set, formatted to match Core's `listlockunspent`.
            "listlockunspent" => {
                let locked = self.locked.lock().map_err(poisoned)?;
                Value::Array(
                    locked
                        .iter()
                        .map(|op| json!({ "txid": op.txid, "vout": op.vout }))
                        .collect(),
                )
            }
            // Local descriptor expansion: `wpkh(<xpub>/<keychain>/*)#<csum>` or
            // `tr(<xpub>/<keychain>/*)#<csum>` over a `[start, end]` range.
            "deriveaddresses" => {
                let descriptor = args.first().and_then(|v| v.as_str()).ok_or_else(|| {
                    RpcError::ReturnedError("deriveaddresses: missing descriptor".into())
                })?;
                let range = args
                    .get(1)
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        let start = a.first().and_then(|n| n.as_u64()).unwrap_or(0) as u32;
                        let end = a.get(1).and_then(|n| n.as_u64()).unwrap_or(start as u64) as u32;
                        (start, end)
                    })
                    .unwrap_or((0, 0));
                let addresses = self.derive_addresses_local(descriptor, range.0, range.1)?;
                Value::Array(
                    addresses
                        .into_iter()
                        .map(|a| Value::String(a.to_string()))
                        .collect(),
                )
            }
            other => {
                return Err(RpcError::ReturnedError(format!(
                    "ElectrumBackend: rpc method '{other}' not supported"
                )));
            }
        };
        Ok(serde_json::from_value(v)?)
    }

    fn get_block_count(&self) -> RpcResult<u64> {
        let tip = self.inner.block_headers_subscribe().map_err(electrum_err)?;
        Ok(tip.height as u64)
    }

    fn get_block_hash(&self, height: u64) -> RpcResult<BlockHash> {
        // Cache hit: return the hash, but first make sure the reverse
        // (hash -> height) mapping also exists. `header_at` populates both
        // caches, but a forward entry can predate a reverse one (e.g. seeded
        // by a sync); a later `get_block_header(&hash)` would otherwise miss.
        if let Ok(cache) = self.height_to_hash.lock() {
            if let Some(h) = cache.get(&height) {
                let h = *h;
                drop(cache);
                if let Ok(mut rev) = self.hash_to_height.lock() {
                    rev.entry(h).or_insert(height);
                }
                return Ok(h);
            }
        }
        Ok(self.header_at(height)?.block_hash())
    }

    fn get_block_header(&self, hash: &BlockHash) -> RpcResult<Header> {
        let height = self.height_for_hash(hash)?;
        self.header_at(height)
    }

    fn get_block_header_info(
        &self,
        hash: &BlockHash,
    ) -> RpcResult<bitcoincore_rpc_json::GetBlockHeaderResult> {
        let height = self.height_for_hash(hash)?;
        let header = self.header_at(height)?;
        let tip = self.get_block_count()?;
        let confirmations = ((tip + 1).saturating_sub(height)) as i32;
        Ok(bitcoincore_rpc_json::GetBlockHeaderResult {
            hash: *hash,
            confirmations,
            height: height as usize,
            version: header.version,
            version_hex: None,
            merkle_root: header.merkle_root,
            time: header.time as usize,
            median_time: None,
            nonce: header.nonce,
            bits: format!("{:08x}", header.bits.to_consensus()),
            difficulty: 0.0,
            chainwork: vec![],
            n_tx: 0,
            previous_block_hash: Some(header.prev_blockhash),
            next_block_hash: None,
        })
    }

    fn get_blockchain_info(&self) -> RpcResult<bitcoincore_rpc_json::GetBlockchainInfoResult> {
        let tip = self.inner.block_headers_subscribe().map_err(electrum_err)?;
        let best = self.get_block_hash(tip.height as u64)?;
        // Construct via serde so we don't have to enumerate every field
        // (some are private or non-Default in upstream).
        let v = json!({
            "chain": match self.network {
                bitcoin::Network::Bitcoin => "main",
                bitcoin::Network::Testnet => "test",
                bitcoin::Network::Signet => "signet",
                bitcoin::Network::Regtest => "regtest",
                _ => "regtest",
            },
            "blocks": tip.height as u64,
            "headers": tip.height as u64,
            "bestblockhash": best,
            "difficulty": 0.0,
            "mediantime": 0u64,
            "verificationprogress": 1.0,
            "initialblockdownload": false,
            "chainwork": "00",
            "size_on_disk": 0u64,
            "pruned": false,
            "softforks": {},
            "warnings": "",
        });
        Ok(serde_json::from_value(v)?)
    }

    fn send_raw_transaction<R: RawTx>(&self, tx: R) -> RpcResult<Txid> {
        let raw = tx.raw_hex();
        let bytes: Vec<u8> = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(&raw)
            .map_err(|e| RpcError::ReturnedError(format!("hex decode: {e}")))?;
        let tx: Transaction = bitcoin::consensus::deserialize(&bytes)
            .map_err(|e| RpcError::ReturnedError(format!("tx decode: {e}")))?;
        self.inner.transaction_broadcast(&tx).map_err(electrum_err)
    }

    fn get_raw_transaction(
        &self,
        txid: &Txid,
        _block_hash: Option<&BlockHash>,
    ) -> RpcResult<Transaction> {
        self.inner.transaction_get(txid).map_err(electrum_err)
    }

    fn get_raw_transaction_info(
        &self,
        txid: &Txid,
        _block_hash: Option<&BlockHash>,
    ) -> RpcResult<bitcoincore_rpc_json::GetRawTransactionResult> {
        // Ask electrs for the verbose form so we can read `confirmations` directly.
        // We only fill the few fields the wallet actually reads (chiefly `confirmations`);
        // everything else is set to a sensible default to satisfy serde.
        let value: Value = self
            .inner
            .raw_call(
                "blockchain.transaction.get",
                vec![
                    electrum_client::Param::String(txid.to_string()),
                    electrum_client::Param::Bool(true),
                ],
            )
            .map_err(electrum_err)?;
        let confirmations = value
            .get("confirmations")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let hex = value.get("hex").and_then(|v| v.as_str()).ok_or_else(|| {
            RpcError::ReturnedError(format!(
                "electrum: missing `hex` field for transaction {txid}"
            ))
        })?;
        let bytes: Vec<u8> = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(hex).map_err(|e| {
            RpcError::ReturnedError(format!("electrum: invalid transaction hex for {txid}: {e}"))
        })?;
        let tx: Transaction = bitcoin::consensus::deserialize(&bytes).map_err(|e| {
            RpcError::ReturnedError(format!(
                "electrum: undecodable transaction bytes for {txid}: {e}"
            ))
        })?;
        let stub = json!({
            "in_active_chain": null,
            "hex": hex,
            "txid": txid,
            "hash": tx.compute_wtxid(),
            "size": bytes.len(),
            "vsize": bytes.len(),
            "version": tx.version.0 as u32,
            "locktime": tx.lock_time.to_consensus_u32(),
            "vin": [],
            "vout": [],
            "blockhash": value.get("blockhash"),
            "confirmations": confirmations,
            "time": value.get("time"),
            "blocktime": value.get("blocktime"),
        });
        Ok(serde_json::from_value(stub)?)
    }

    fn get_tx_out(
        &self,
        txid: &Txid,
        vout: u32,
        _include_mempool: Option<bool>,
    ) -> RpcResult<Option<bitcoincore_rpc_json::GetTxOutResult>> {
        // Bitcoin Core's `gettxout` returns `None` once the output is spent —
        // callers (taker recovery loop, maker funding-confirmation check) rely on
        // this transition. Electrum's `blockchain.transaction.get` always returns
        // the tx whether or not the output is still in the UTXO set, so we have
        // to confirm liveness explicitly via `scripthash.listunspent`.
        let value: Value = match self.inner.raw_call(
            "blockchain.transaction.get",
            vec![
                electrum_client::Param::String(txid.to_string()),
                electrum_client::Param::Bool(true),
            ],
        ) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let hex = value.get("hex").and_then(|v| v.as_str()).unwrap_or("");
        let bytes: Vec<u8> = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(hex)
            .map_err(|e| RpcError::ReturnedError(format!("get_tx_out hex decode: {e}")))?;
        let tx: Transaction = bitcoin::consensus::deserialize(&bytes)
            .map_err(|e| RpcError::ReturnedError(format!("get_tx_out tx decode: {e}")))?;
        let txout = match tx.output.get(vout as usize) {
            Some(o) => o.clone(),
            None => return Ok(None),
        };

        // Is this specific outpoint still unspent at its scriptPubKey?
        let still_unspent = self
            .inner
            .script_list_unspent(txout.script_pubkey.as_script())
            .map(|entries| {
                entries
                    .iter()
                    .any(|e| e.tx_hash == *txid && e.tx_pos as u32 == vout)
            })
            .unwrap_or(false);
        if !still_unspent {
            return Ok(None);
        }

        let confirmations = value
            .get("confirmations")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let v = json!({
            "bestblock": BlockHash::all_zeros(),
            "confirmations": confirmations,
            "value": txout.value.to_btc(),
            "scriptPubKey": {
                "asm": "",
                "hex": serialize_hex(&txout.script_pubkey),
                "type": null,
            },
            "coinbase": false,
        });
        Ok(Some(serde_json::from_value(v)?))
    }

    fn list_unspent(
        &self,
        minconf: Option<usize>,
        _maxconf: Option<usize>,
        _addresses: Option<&[&bitcoin::Address]>,
        _include_unsafe: Option<bool>,
        _query_options: Option<bitcoincore_rpc_json::ListUnspentQueryOptions>,
    ) -> RpcResult<Vec<ListUnspentResultEntry>> {
        let watched: Vec<ScriptBuf> = self
            .watched
            .lock()
            .map_err(poisoned)?
            .iter()
            .cloned()
            .collect();
        let locked = self.locked.lock().map_err(poisoned)?.clone();
        let tip = self.get_block_count()?;
        let min_conf = minconf.unwrap_or(0) as u32;

        // Batch the per-script queries via JSON-RPC batching so N watched scripts
        // become ~N/BATCH round-trips instead of N. Some servers cap individual
        // batch payload size, so we chunk rather than send everything in one shot.
        const LIST_UNSPENT_BATCH: usize = 200;
        let mut out = Vec::new();
        for chunk in watched.chunks(LIST_UNSPENT_BATCH) {
            let refs: Vec<&Script> = chunk.iter().map(|s| s.as_script()).collect();
            let results = self
                .inner
                .batch_script_list_unspent(refs.iter().copied())
                .map_err(electrum_err)?;
            for (script, entries) in chunk.iter().zip(results) {
                for e in entries {
                    let outpoint = OutPoint {
                        txid: e.tx_hash,
                        vout: e.tx_pos as u32,
                    };
                    if locked.contains(&outpoint) {
                        continue;
                    }
                    let confirmations = if e.height == 0 {
                        0
                    } else {
                        (tip + 1).saturating_sub(e.height as u64) as u32
                    };
                    if confirmations < min_conf {
                        continue;
                    }
                    // HD-origin lookup happens later via
                    // `BlockchainBackend::hd_origin_for_script`, so the
                    // `descriptor` field is left empty here.
                    out.push(ListUnspentResultEntry {
                        txid: e.tx_hash,
                        vout: e.tx_pos as u32,
                        address: None,
                        label: None,
                        redeem_script: None,
                        witness_script: None,
                        script_pub_key: script.clone(),
                        amount: bitcoin::Amount::from_sat(e.value),
                        confirmations,
                        spendable: !locked.contains(&outpoint),
                        solvable: true,
                        descriptor: None,
                        safe: true,
                    });
                }
            }
        }
        Ok(out)
    }

    fn lock_unspent(&self, outputs: &[OutPoint]) -> RpcResult<bool> {
        let mut locked = self.locked.lock().map_err(poisoned)?;
        for op in outputs {
            locked.insert(*op);
        }
        Ok(true)
    }

    fn unlock_unspent_all(&self) -> RpcResult<bool> {
        self.locked.lock().map_err(poisoned)?.clear();
        Ok(true)
    }
}

fn poisoned<T>(_e: T) -> RpcError {
    RpcError::ReturnedError("ElectrumBackend: mutex poisoned".into())
}
