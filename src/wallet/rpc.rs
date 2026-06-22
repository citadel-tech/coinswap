//! Manages connection with a Bitcoin Core RPC and an Electrum-protocol backend.
//!
//! Two concrete backends are provided behind the [`BlockchainBackend`] trait:
//! [`BitcoindBackend`] (an alias for `bitcoincore_rpc::Client`) and
//! [`ElectrumBackend`] (wraps `electrum_client::Client`, faking Bitcoin Core's
//! server-side wallet state with local maps).
use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Debug},
    sync::Mutex,
    thread,
};

use bitcoind::bitcoincore_rpc::{
    bitcoincore_rpc_json,
    json::{ListUnspentResultEntry, ScanningDetails},
    Auth, Error as RpcError, RawTx, Result as RpcResult, RpcApi,
};
use serde_json::{json, Value};

use crate::{utill::HEART_BEAT_INTERVAL, wallet::api::KeychainKind};

use bitcoin::{
    bip32::{ChildNumber, Xpub},
    block::Header,
    consensus::encode::serialize_hex,
    hashes::Hash,
    Address, BlockHash, CompressedPublicKey, OutPoint, Script, ScriptBuf, Transaction, Txid,
};
use electrum_client::{Client as ElectrumClient, ElectrumApi};
use serde::Deserialize;

use super::{error::WalletError, Wallet};

/// Bitcoin Core JSON-RPC backend
pub type BitcoindBackend = bitcoind::bitcoincore_rpc::Client;

/// Backend abstraction over a blockchain RPC source. Inherits [`RpcApi`] so wallet calls stay uniform.
pub trait BlockchainBackend: RpcApi + Debug + Send + Sync + 'static {
    /// Per-backend connection config (e.g. [`RPCConfig`], [`ElectrumConfig`]).
    type Config: Clone;
    /// True only for [`ElectrumBackend`]. Selects the sync implementation.
    const IS_ELECTRUM: bool = false;

    /// Construct the backend client from its config.
    fn from_config(config: &Self::Config) -> Result<Self, WalletError>
    where
        Self: Sized;

    /// Borrow the matching variant out of a [`BackendConfig`].
    fn from_backend_config(backend: &BackendConfig) -> Result<&Self::Config, WalletError>;

    /// Register a scriptPubKey for UTXO lookups. No-op on Bitcoin Core; Electrum  stores it locally along with an optional HD-origin hint.
    fn watch_script(&self, _script: &Script, _hd: Option<HdOrigin>) {}

    /// HD-origin recorded for `script_pubkey` (Electrum only). Bitcoin Core exposes the same info via the descriptor string on each UTXO, so the
    /// default returns `None`. Used by the wallet's UTXO classifier to recognise Electrum-sourced seed coins without round-tripping through a descriptor.
    fn hd_origin_for_script(&self, _script: &Script) -> Option<HdOrigin> {
        None
    }
}

impl BlockchainBackend for BitcoindBackend {
    type Config = RPCConfig;
    fn from_config(c: &RPCConfig) -> Result<Self, WalletError> {
        Ok(BitcoindBackend::new(
            &format!("http://{}/wallet/{}", c.url, c.wallet_name),
            c.auth.clone(),
        )?)
    }
    fn from_backend_config(b: &BackendConfig) -> Result<&RPCConfig, WalletError> {
        match b {
            BackendConfig::Bitcoind(c) => Ok(c),
            BackendConfig::Electrum(_) => Err(WalletError::General(
                "expected Bitcoind, got Electrum".into(),
            )),
        }
    }
}

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

impl BackendConfig {
    /// Borrow the wallet name from whichever variant is set.
    pub fn wallet_name(&self) -> &str {
        match self {
            BackendConfig::Bitcoind(c) => &c.wallet_name,
            BackendConfig::Electrum(c) => &c.wallet_name,
        }
    }

    /// Overwrite the wallet name on whichever variant is set. Used by
    /// interactive restore where the wallet name is derived from the restored path.
    pub fn set_wallet_name(&mut self, name: String) {
        match self {
            BackendConfig::Bitcoind(c) => c.wallet_name = name,
            BackendConfig::Electrum(c) => c.wallet_name = name,
        }
    }
}

/// HD-origin metadata for a watched script. Returned by
/// [`BlockchainBackend::hd_origin_for_script`] so the wallet's UTXO classifier
/// can identify an Electrum-sourced UTXO as a `SeedCoin` (or `SweptCoin`).
#[derive(Debug, Clone)]
pub struct HdOrigin {
    /// Fingerprint of the account-level xpub the script was derived under.
    pub fingerprint: String,
    /// 0 for external (receive) chain, 1 for internal (change) chain.
    pub keychain_idx: u32,
    /// Index along the chosen chain.
    pub index: u32,
    /// True if derived under the P2TR descriptor, false for P2WPKH.
    pub is_taproot: bool,
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

/// Top-level backend selection consumed by binaries / FFI.
#[derive(Debug, Clone)]
pub enum BackendConfig {
    /// Drive RPC through Bitcoin Core.
    Bitcoind(RPCConfig),
    /// Drive RPC through an Electrum-protocol server.
    Electrum(ElectrumConfig),
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
        if let Ok(cache) = self.height_to_hash.lock() {
            if let Some(h) = cache.get(&height) {
                return Ok(*h);
            }
        }
        Ok(self.header_at(height)?.block_hash())
    }

    fn get_block_header(&self, hash: &BlockHash) -> RpcResult<Header> {
        let height = self
            .hash_to_height
            .lock()
            .map_err(poisoned)?
            .get(hash)
            .copied()
            .ok_or_else(|| {
                RpcError::ReturnedError(format!(
                    "electrum: unknown block hash {hash} (no cached height)"
                ))
            })?;
        self.header_at(height)
    }

    fn get_block_header_info(
        &self,
        hash: &BlockHash,
    ) -> RpcResult<bitcoincore_rpc_json::GetBlockHeaderResult> {
        let height = self
            .hash_to_height
            .lock()
            .map_err(poisoned)?
            .get(hash)
            .copied()
            .ok_or_else(|| {
                RpcError::ReturnedError(format!(
                    "electrum: unknown block hash {hash} (no cached height)"
                ))
            })?;
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

/// Configuration parameters for connecting to a Bitcoin node via RPC.
#[derive(Debug, Clone)]
pub struct RPCConfig {
    /// The bitcoin node url
    pub url: String,
    /// The bitcoin node authentication mechanism
    pub auth: Auth,
    /// The wallet name in the bitcoin node, derive this from the descriptor.
    pub wallet_name: String,
    /// ZMQ endpoint for block/tx notifications (e.g. `"tcp://127.0.0.1:28332"`).
    /// Only used by components that need push notifications (maker watch service).
    pub zmq_addr: String,
}

const RPC_HOSTPORT: &str = "localhost:18443";

impl Default for RPCConfig {
    fn default() -> Self {
        Self {
            url: RPC_HOSTPORT.to_string(),
            auth: Auth::UserPass("regtestrpcuser".to_string(), "regtestrpcpass".to_string()),
            wallet_name: "random-wallet-name".to_string(),
            zmq_addr: "tcp://127.0.0.1:28332".to_string(),
        }
    }
}

fn list_wallet_dir<B: RpcApi>(client: &B) -> Result<Vec<String>, WalletError> {
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

fn get_wallet_scanning_details<B: RpcApi>(
    client: &B,
) -> Result<Option<ScanningDetails>, WalletError> {
    #[derive(Deserialize)]
    struct WalletInfoScanningOnly {
        scanning: Option<ScanningDetails>,
    }

    // Parse only the field we need so upstream schema removals (e.g. getwalletinfo v30 balance related fields removal)
    // do not break deserialization.
    let wallet_info: WalletInfoScanningOnly = client.call("getwalletinfo", &[])?;
    Ok(wallet_info.scanning)
}

impl<B: BlockchainBackend> Wallet<B> {
    /// Sync the wallet, then persist to disk.
    pub fn sync_and_save(&mut self) -> Result<(), WalletError> {
        log::info!("Sync Started for {:?}", &self.store.file_name);
        self.sync_no_fail();
        self.save_to_disk()?;
        log::info!("Synced & Saved {:?}", &self.store.file_name);
        Ok(())
    }

    /// Get all utxos tracked by the core rpc wallet.
    fn get_all_utxo_from_rpc(&self) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        self.rpc.unlock_unspent_all()?;
        let all_utxos = self
            .rpc
            .list_unspent(Some(0), Some(9999999), None, None, None)?;
        Ok(all_utxos)
    }

    /// Bitcoin Core's importdescriptors + scan vs Electrum's walks scripthhash history.
    fn sync(&mut self) -> Result<(), WalletError> {
        if B::IS_ELECTRUM {
            return self.sync_no_rescan();
        }
        // Create or load the watch-only bitcoin core wallet
        let wallet_name = &self.store.file_name;
        if self.rpc.list_wallets()?.contains(wallet_name) {
            log::debug!("wallet already loaded: {wallet_name}");
        } else if list_wallet_dir(&self.rpc)?.contains(wallet_name) {
            self.rpc.load_wallet(wallet_name)?;
            log::debug!("wallet loaded: {wallet_name}");
        } else {
            // pre-0.21 use legacy wallets
            if self.rpc.version()? < 210_000 {
                self.rpc
                    .create_wallet(wallet_name, Some(true), None, None, None)?;
            } else {
                // We cannot use the api directly right now.
                // https://github.com/rust-bitcoin/rust-bitcoincore-rpc/issues/225 is still open,
                // We can update to api call after moving to new corepc crate.
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

            log::debug!("wallet created: {wallet_name}");
        }

        let descriptors_to_import = self.descriptors_to_import()?;

        if descriptors_to_import.is_empty() {
            return Ok(());
        }

        // Sometimes in test multiple wallet scans can occur at same time, resulting in error.
        let mut last_synced_height = self
            .store
            .last_synced_height
            .unwrap_or(0)
            .max(self.store.wallet_birthday.unwrap_or(0));
        let node_synced = self.rpc.get_block_count()?;

        // If the chain is shorter than the wallet's last synced height (e.g. node
        // restarted with a fresh chain or a reorg), reset to rescan from the start.
        if last_synced_height > node_synced {
            log::warn!(
                "Wallet last_synced_height ({}) exceeds chain height ({}), resetting to 0",
                last_synced_height,
                node_synced
            );
            last_synced_height = 0;
            self.store.last_synced_height = Some(0);
        }

        log::info!("Re-scanning Blockchain from:{last_synced_height} to:{node_synced}");

        let block_hash = self.rpc.get_block_hash(last_synced_height)?;
        let Header { time, .. } = self.rpc.get_block_header(&block_hash)?;

        let _ = self.import_descriptors(&descriptors_to_import, Some(time), None);

        // Returns when the scanning is completed
        loop {
            match get_wallet_scanning_details(&self.rpc)? {
                Some(ScanningDetails::Scanning { duration, .. }) => {
                    // Todo: Show scan progress
                    log::info!("Scanning for {}s", duration);
                    thread::sleep(HEART_BEAT_INTERVAL);
                    continue;
                }
                Some(ScanningDetails::NotScanning(_)) => {
                    log::info!("Scanning completed");
                    break;
                }
                None => {
                    log::info!("No scan is in progress or Scanning completed");
                    break;
                }
            }
        }
        self.finalize_sync(node_synced)
    }

    /// Electrum-style sync: register every wallet-owned script client-side,
    /// then list UTXOs via per-scripthash queries.
    fn sync_no_rescan(&mut self) -> Result<(), WalletError> {
        self.populate_backend_watched_scripts()?;
        let tip = self.rpc.get_block_count()?;
        self.finalize_sync(tip)
    }

    /// Shared tail of both sync paths: record the synced tip, refresh the
    /// UTXO cache, advance the external index, and recompute the offer-max
    /// cache.
    fn finalize_sync(&mut self, tip: u64) -> Result<(), WalletError> {
        self.store.last_synced_height = Some(tip);
        self.update_utxo_cache(self.get_all_utxo_from_rpc()?);
        let max_external_index = self.find_hd_next_index(KeychainKind::External)?;
        self.store.external_index = max_external_index;
        self.refresh_offer_maxsize_cache()?;
        Ok(())
    }

    /// Retry sync forever; handles transient RPC errors.
    fn sync_no_fail(&mut self) {
        while let Err(e) = self.sync() {
            log::error!("Blockchain sync failed. Retrying. | {e:?}");
            thread::sleep(HEART_BEAT_INTERVAL);
        }
    }

    /// Import watch addresses into core wallet. Does not check if the address was already imported.
    /// Scans blocks from a given timestamp.
    pub(crate) fn import_descriptors(
        &self,
        descriptors_to_import: &[String],
        time: Option<u32>,
        address_label: Option<String>,
    ) -> Result<(), WalletError> {
        let address_label = address_label.unwrap_or(self.get_core_wallet_label());

        // Offset by +2h because import_descriptors applies a default -2h to the timestamp
        let time_stamp = time.map(|t| json!(t + 7200)).unwrap_or(json!("now"));

        let import_requests = descriptors_to_import
            .iter()
            .map(|desc| {
                if desc.contains("/*") {
                    return json!({
                        "timestamp": time_stamp,
                        "desc": desc,
                        "range": (self.get_addrss_import_count() - 1)
                    });
                }
                json!({
                    "timestamp": time_stamp,
                    "desc": desc,
                    "label": address_label
                })
            })
            .collect();
        let _res: Vec<Value> = self.rpc.call("importdescriptors", &[import_requests])?;
        Ok(())
    }

    /// Verify the SPV proof for a transaction.
    pub fn verify_tx_out_proof(
        &self,
        expected_txid: &bitcoin::Txid,
        proof_hex: &str,
    ) -> Result<(), WalletError> {
        let proof_txids: Vec<bitcoin::Txid> = self
            .rpc
            .call("verifytxoutproof", &[json!(proof_hex)])
            .map_err(WalletError::Rpc)?;

        if proof_txids != vec![*expected_txid] {
            return Err(WalletError::MerkleProofInvalid {
                expected: *expected_txid,
                got: proof_txids,
            });
        }

        Ok(())
    }
}
