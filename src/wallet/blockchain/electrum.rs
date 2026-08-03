//! Electrum-protocol backend.
//!
//! Electrum servers index chain data by scripthash and have no server-side
//! wallet. This backend fakes the wallet-style queries the rest of the codebase
//! expects (UTXO listing, descriptor expansion, block-hash lookups) using local
//! maps, and drives the watchtower notification stream via
//! `blockchain.headers.subscribe` + per-script `scripthash.subscribe`. Coin
//! locking is handled wallet-side, not here.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, RwLock,
    },
    time::Duration,
};

use bitcoin::{
    address::NetworkUnchecked,
    bip32::{ChildNumber, Xpub},
    block::Header,
    consensus::encode::serialize,
    hashes::Hash,
    hex::DisplayHex,
    Address, BlockHash, CompressedPublicKey, OutPoint, Script, ScriptBuf, SignedAmount,
    Transaction, Txid,
};
use bitcoind::bitcoincore_rpc::json::{
    Bip125Replaceable, GetBlockchainInfoResult, GetRawTransactionResult,
    GetTransactionResultDetail, GetTransactionResultDetailCategory, GetTxOutResult,
    ListTransactionResult, ListUnspentResultEntry, WalletTxInfo,
};
use electrum_client::{
    Client as ElectrumClient, Config as ClientConfig, ConfigBuilder, ElectrumApi, Socks5Config,
};
use serde_json::{json, Value};

use super::{network_from_electrum_genesis, BlockRef, Blockchain, HdOrigin, WatchEvent};
use crate::wallet::error::WalletError;

/// Configuration for connecting to an Electrum-protocol server.
///
/// Electrum has no server-side wallet, so there is no wallet name here — the
/// on-disk wallet file name comes from the wallet path, not the backend.
#[derive(Debug, Clone)]
pub struct ElectrumConfig {
    /// Electrum server endpoint (e.g. `"tcp://localhost:50001"` or
    /// `"ssl://electrum.example.org:50002"`).
    pub url: String,
    /// SOCKS5 proxy to reach the server through, e.g. `"127.0.0.1:9050"` for Tor.
    /// Works for onion and clearnet servers alike. `None` connects directly, which
    /// cannot reach an onion address at all.
    pub socks5: Option<String>,
    /// Socket read timeout in seconds. `None` picks 30s direct and 120s through a
    /// proxy, since a slow large response over Tor must not be mistaken for a
    /// dead connection.
    pub timeout: Option<u8>,
    /// Seconds between notification pings. `None` picks 1s direct and 10s through
    /// a proxy, where each ping costs a circuit round-trip.
    pub poll_interval_secs: Option<u64>,
    /// Reconnect attempts after a transport failure before giving up. A dropped
    /// Tor circuit should not abort a swap.
    pub max_retries: u8,
}

impl Default for ElectrumConfig {
    fn default() -> Self {
        Self {
            url: "tcp://<your-electrum-server>:<port>".to_string(),
            socks5: None,
            timeout: None,
            poll_interval_secs: None,
            max_retries: 3,
        }
    }
}

/// Ping cadence on a direct connection.
pub const PING_INTERVAL_DIRECT: Duration = Duration::from_secs(1);
/// Ping cadence through a proxy. A Tor round-trip costs far more than a local
/// one, and every consumer of watchtower latency reacts on a block timescale.
pub const PING_INTERVAL_PROXIED: Duration = Duration::from_secs(10);
/// Read timeout for a direct connection.
pub const READ_TIMEOUT_DIRECT_SECS: u8 = 30;
/// Read timeout through a proxy. Generous on purpose: a large batched response
/// over a congested circuit must not be mistaken for a dead transport, or we
/// reconnect when we should have waited.
pub const READ_TIMEOUT_PROXIED_SECS: u8 = 120;

/// Host portion of an Electrum URL, without scheme or port.
fn url_host(url: &str) -> &str {
    let no_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    no_scheme
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(no_scheme)
}

/// Notification ping cadence for a config: explicit override, else derived from
/// whether a proxy is in play.
fn derive_ping_interval(cfg: &ElectrumConfig) -> Duration {
    cfg.poll_interval_secs
        .map(Duration::from_secs)
        .unwrap_or(if cfg.socks5.is_some() {
            PING_INTERVAL_PROXIED
        } else {
            PING_INTERVAL_DIRECT
        })
}

/// Socket read timeout for a config, in seconds. Proxied connections get a much
/// longer one; see [`READ_TIMEOUT_PROXIED_SECS`].
fn derive_timeout_secs(cfg: &ElectrumConfig) -> u8 {
    cfg.timeout.unwrap_or(if cfg.socks5.is_some() {
        READ_TIMEOUT_PROXIED_SECS
    } else {
        READ_TIMEOUT_DIRECT_SECS
    })
}

/// Build the `electrum_client` config from ours.
///
/// `retry(0)` is deliberate. The client's own retry rebuilds the connection and
/// then returns `Ok`, hiding the fact that every server-side script
/// subscription was dropped — which leaves the watchtower deaf. Reconnection is
/// handled by [`Electrum::call`] instead, which knows what to re-arm.
fn client_config(cfg: &ElectrumConfig) -> ClientConfig {
    let is_onion = url_host(&cfg.url).ends_with(".onion");
    ConfigBuilder::new()
        .socks5(cfg.socks5.as_deref().map(Socks5Config::new))
        .timeout(Some(derive_timeout_secs(cfg)))
        .retry(0)
        // A hidden-service name has no certificate to validate against.
        .validate_domain(!is_onion)
        .build()
}

/// How long to wait before retry `attempt` (1-based): 1s, 2s, 4s, then capped.
/// A Tor circuit needs a moment to rebuild, so the first retry is not immediate.
fn retry_backoff(attempt: u8) -> Duration {
    Duration::from_secs(1 << (attempt - 1).min(5))
}

/// True for errors that mean the request was answered, not that the transport
/// failed. Reconnecting cannot help these. `NotSubscribed` is included because
/// it is local client state, and after a rebuild it means "re-arm", not "retry".
fn is_terminal(e: &electrum_client::Error) -> bool {
    matches!(
        e,
        electrum_client::Error::Protocol(_)
            | electrum_client::Error::AlreadySubscribed(_)
            | electrum_client::Error::NotSubscribed(_)
    )
}

/// Notification state for the watchtower path, mutated through a `Mutex` so the
/// backend stays `Sync` while exposing `&self` methods.
#[derive(Debug, Default)]
struct NotifierState {
    /// Highest header height seen, so duplicate tip notifications are ignored.
    last_height: i64,
    /// Txids already surfaced for each subscribed scriptPubKey.
    subscriptions: HashMap<ScriptBuf, HashSet<Txid>>,
    /// Scripts whose history still needs re-reading after a failed tx fetch. The
    /// server only notifies on status changes, so nothing else would revisit them.
    dirty: HashSet<ScriptBuf>,
    /// Events buffered between `poll_event` calls (one notification can yield
    /// many new transactions; we hand them back one at a time).
    pending: VecDeque<WatchEvent>,
    /// Last time the socket was pumped with a real RPC (see `poll_event`).
    last_ping: Option<std::time::Instant>,
}

/// Electrum-protocol backend. One owned connection serves both the wallet's
/// queries and the watchtower's notifications for a given consumer.
pub struct Electrum {
    /// Held for the process lifetime and reused by every call. Swappable only so
    /// [`Electrum::call`] can replace a dead connection in place — never
    /// re-established per call.
    inner: RwLock<ElectrumClient>,
    /// Connection config, retained so a fresh independent connection can be
    /// built via [`Electrum::reconnect`] (the watchtower needs its own).
    config: ElectrumConfig,
    /// Ping cadence for `poll_event`, resolved from the config at construction.
    ping_interval: Duration,
    /// Set when `call` rebuilds the client. The fresh socket carries no
    /// server-side script subscriptions, so `poll_event` must re-arm them.
    needs_rearm: AtomicBool,
    /// Times the transport was rebuilt. A healthy held connection leaves this at
    /// 0; used by tests to prove we do not reconnect per sync or watch call.
    reconnects: AtomicU64,
    /// Scripts the wallet asked us to track (Core's server-side wallet equivalent).
    watched: Mutex<HashSet<ScriptBuf>>,
    /// HD-origin hint per watched script, for UTXO classification without a descriptor.
    hd_paths: Mutex<HashMap<ScriptBuf, HdOrigin>>,
    /// height → hash cache to optimize `get_block_hash`. Only stores hashes for
    /// previously queried heights.
    height_to_hash: Mutex<HashMap<u64, BlockHash>>,
    /// Network derived from the server's reported genesis hash.
    network: bitcoin::Network,
    /// Watchtower notification bookkeeping.
    notifier: Mutex<NotifierState>,
}

impl std::fmt::Debug for Electrum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Electrum")
            .field("network", &self.network)
            .finish()
    }
}

impl Electrum {
    /// Connect to an Electrum server and derive the network from its genesis
    /// hash. Used by `AnyBlockchain::from_config`; each consumer gets its own
    /// connection.
    pub fn new(cfg: &ElectrumConfig) -> Result<Self, WalletError> {
        // Without a proxy an onion host has no route: it cannot be resolved, so
        // fail with the reason rather than an opaque DNS error.
        if cfg.socks5.is_none() && url_host(&cfg.url).ends_with(".onion") {
            return Err(electrum_err(format!(
                "electrum url {} is an onion address but no socks5 proxy is configured",
                cfg.url
            )));
        }
        let (inner, genesis, last_height) = Self::connect(cfg)?;
        let config = cfg.clone();
        // Not retried: a server on the wrong chain will keep saying so.
        let network = network_from_electrum_genesis(&genesis).ok_or_else(|| {
            electrum_err(format!(
                "unknown genesis from electrum server: {:?}",
                genesis
            ))
        })?;
        let ping_interval = derive_ping_interval(cfg);
        Ok(Self {
            inner: RwLock::new(inner),
            config,
            ping_interval,
            needs_rearm: AtomicBool::new(false),
            reconnects: AtomicU64::new(0),
            watched: Mutex::new(HashSet::new()),
            hd_paths: Mutex::new(HashMap::new()),
            height_to_hash: Mutex::new(HashMap::new()),
            network,
            notifier: Mutex::new(NotifierState {
                last_height,
                ..Default::default()
            }),
        })
    }

    /// Open the connection and run the opening handshake, retrying on transport
    /// failure.
    ///
    /// A first connect over Tor often fails while the circuit or the onion
    /// descriptor is still settling, and that should not kill process startup.
    /// Returns the client, the server's genesis hash, and the tip height.
    fn connect(cfg: &ElectrumConfig) -> Result<(ElectrumClient, [u8; 32], i64), WalletError> {
        let once = || -> Result<(ElectrumClient, [u8; 32], i64), electrum_client::Error> {
            let inner = ElectrumClient::from_config(&cfg.url, client_config(cfg))?;
            // Some servers do not implement `server.features` (-32601 is the
            // JSON-RPC "method not found" code). Read the genesis header
            // instead, which every server answers.
            let genesis = match inner.server_features() {
                Ok(features) => features.genesis_hash,
                Err(electrum_client::Error::Protocol(ref value))
                    if value.get("code").and_then(Value::as_i64) == Some(-32601) =>
                {
                    let mut genesis = inner
                        .block_header(0)?
                        .block_hash()
                        .to_raw_hash()
                        .to_byte_array();
                    genesis.reverse();
                    genesis
                }
                Err(e) => return Err(e),
            };
            // Arm the header subscription so the watchtower's `poll_event` sees
            // tip updates; harmless for the wallet's instance, which never polls.
            let last_height = inner.block_headers_subscribe()?.height as i64;
            Ok((inner, genesis, last_height))
        };

        let mut last = match once() {
            Ok(v) => return Ok(v),
            Err(e) if is_terminal(&e) => return Err(e.into()),
            Err(e) => e,
        };

        for attempt in 1..=cfg.max_retries {
            let backoff = retry_backoff(attempt);
            log::warn!(
                "electrum connect to {} failed ({last:?}); retrying in {}s (attempt {attempt}/{})",
                cfg.url,
                backoff.as_secs(),
                cfg.max_retries
            );
            std::thread::sleep(backoff);
            match once() {
                Ok(v) => return Ok(v),
                Err(e) if is_terminal(&e) => return Err(e.into()),
                Err(e) => last = e,
            }
        }

        let attempts = cfg.max_retries.saturating_add(1);
        log::error!(
            "electrum backend {} unreachable at connect after {attempts} attempt(s): {last:?}",
            cfg.url
        );
        Err(WalletError::ElectrumUnreachable { attempts, last })
    }

    /// Open a fresh, independent Electrum connection with the same config.
    ///
    /// Used to give a separate consumer (e.g. the watchtower discovery thread)
    /// its own connection without threading the config around separately. This is
    /// not the reconnect-on-failure path, which replaces a dead socket in place.
    pub fn reconnect(&self) -> Result<Self, WalletError> {
        Self::new(&self.config)
    }

    /// Times the transport was rebuilt since construction.
    ///
    /// The connection is meant to be held, so a healthy run leaves this at 0.
    /// A non-zero value means the socket died and was replaced, not that calls
    /// failed.
    pub fn reconnect_count(&self) -> u64 {
        self.reconnects.load(Ordering::Relaxed)
    }

    /// Run a backend call, reconnecting and retrying on transport failure.
    ///
    /// The client is configured with `retry(0)`, so a dropped connection
    /// surfaces here instead of being papered over. A rebuilt Tor circuit or a
    /// restarted server must not abort a swap, so we reconnect and retry up to
    /// `max_retries` times with a widening backoff before failing loudly.
    ///
    /// The happy path takes only a read lock, so the held connection is shared
    /// by every caller and a successful call never reconnects.
    fn call<T>(
        &self,
        f: impl Fn(&ElectrumClient) -> Result<T, electrum_client::Error>,
    ) -> Result<T, WalletError> {
        let mut last = match self.try_call(&f) {
            Ok(v) => return Ok(v),
            Err(e) if is_terminal(&e) => return Err(e.into()),
            Err(e) => e,
        };

        for attempt in 1..=self.config.max_retries {
            let backoff = retry_backoff(attempt);
            // An idle server dropping the session is routine, so only the first
            // attempt is a warning; the rest are noise until we actually fail.
            let msg = format!(
                "electrum call to {} failed ({last:?}); reconnecting in {}s (attempt {attempt}/{})",
                self.config.url,
                backoff.as_secs(),
                self.config.max_retries
            );
            if attempt == 1 {
                log::warn!("{msg}");
            } else {
                log::debug!("{msg}");
            }
            std::thread::sleep(backoff);

            if let Err(e) = self.reconnect_client() {
                last = e;
                continue;
            }
            match self.try_call(&f) {
                Ok(v) => return Ok(v),
                Err(e) if is_terminal(&e) => return Err(e.into()),
                Err(e) => last = e,
            }
        }

        let attempts = self.config.max_retries.saturating_add(1);
        log::error!(
            "electrum backend {} unreachable after {attempts} attempt(s): {last:?}",
            self.config.url
        );
        Err(WalletError::ElectrumUnreachable { attempts, last })
    }

    /// One attempt against the currently held client.
    ///
    /// A poisoned lock is recovered rather than propagated: the guarded value is
    /// a live socket, and refusing to touch it would brick the backend for the
    /// process lifetime with no way for `reconnect_client` to heal it.
    fn try_call<T>(
        &self,
        f: &impl Fn(&ElectrumClient) -> Result<T, electrum_client::Error>,
    ) -> Result<T, electrum_client::Error> {
        let client = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&client)
    }

    /// Replace the held client with a fresh connection and flag subscriptions
    /// for re-arming, since the new socket carries none.
    fn reconnect_client(&self) -> Result<(), electrum_client::Error> {
        let fresh = ElectrumClient::from_config(&self.config.url, client_config(&self.config))?;
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = fresh;
        self.needs_rearm.store(true, Ordering::SeqCst);
        self.reconnects.fetch_add(1, Ordering::Relaxed);
        log::info!("reconnected to electrum backend {}", self.config.url);
        Ok(())
    }

    /// Arm the server-side scripthash subscription, treating an existing one as
    /// success. Shared by the initial subscribe and the post-reconnect re-arm.
    fn arm_subscription(&self, spk: &Script) -> Result<(), WalletError> {
        match self.call(|c| c.script_subscribe(spk).map(|_| ())) {
            Ok(()) => Ok(()),
            Err(WalletError::Electrum(electrum_client::Error::AlreadySubscribed(_))) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Mined height of an already-fetched transaction, or `None` while it is
    /// unconfirmed.
    ///
    /// Electrum has no txid→height index, but `scripthash.get_history` reports
    /// the mined height of every tx touching a script, so any output of this tx
    /// serves as the lookup key. That height is the server's own answer — no
    /// arithmetic against a tip that may have moved — and works at any depth,
    /// for scripts inside or outside our watch set.
    fn mined_height(&self, tx: &Transaction) -> Result<Option<u64>, WalletError> {
        let txid = tx.compute_txid();
        for txout in &tx.output {
            // Unspendable outputs (OP_RETURN) have no scripthash history.
            if txout.script_pubkey.is_op_return() {
                continue;
            }
            // A missing entry is a legitimate "not mined"; a failed read is a
            // server problem and must not be collapsed into that answer.
            let history = self.call(|c| c.script_get_history(txout.script_pubkey.as_script()))?;
            if let Some(entry) = history.iter().find(|e| e.tx_hash == txid) {
                // `height` is 0 for a mempool tx and -1 for one with unconfirmed
                // parents; both mean "not mined yet".
                return Ok((entry.height > 0).then_some(entry.height as u64));
            }
        }
        Ok(None)
    }

    /// Confirmation count for a mined height against the current tip, and `0`
    /// for an unconfirmed transaction.
    fn confirmations_at(&self, height: Option<u64>) -> Result<u32, WalletError> {
        let Some(height) = height else { return Ok(0) };
        let tip = self.get_block_count()?;
        Ok((tip + 1).saturating_sub(height) as u32)
    }

    /// Expand a wallet-emitted descriptor (`wpkh(<xpub>/<chain>/*)` or
    /// `tr(<xpub>/<chain>/*)`) to addresses at indices `start..=end`. Strips an
    /// optional `#checksum` suffix and the BIP-32 `*` wildcard.
    fn derive_addresses_local(
        &self,
        descriptor: &str,
        start: u32,
        end: u32,
    ) -> Result<Vec<Address>, WalletError> {
        let bad = |msg: String| electrum_err(format!("deriveaddresses: {msg}"));

        if start > end {
            return Err(bad(format!("inverted range: start {start} > end {end}")));
        }

        let body = descriptor.split('#').next().unwrap_or(descriptor);
        let (kind, inner) = if let Some(rest) = body.strip_prefix("wpkh(") {
            ("wpkh", rest)
        } else if let Some(rest) = body.strip_prefix("tr(") {
            ("tr", rest)
        } else {
            return Err(bad(format!("unsupported descriptor: {descriptor}")));
        };
        let inner = inner
            .strip_suffix(')')
            .ok_or_else(|| bad(format!("missing closing `)`: {descriptor}")))?;
        // Expect: `<xpub>/<chain>/*`
        let parts: Vec<&str> = inner.rsplitn(3, '/').collect();
        if parts.len() != 3 || parts[0] != "*" {
            return Err(bad(format!("unexpected key form: {inner}")));
        }
        let chain_idx: u32 = parts[1]
            .parse()
            .map_err(|_| bad("chain index parse".to_string()))?;
        let xpub: Xpub = parts[2]
            .parse()
            .map_err(|_| bad("xpub parse".to_string()))?;

        let secp = crate::utill::global_secp();
        let mut out = Vec::new();
        for i in start..=end {
            let path = [
                ChildNumber::Normal { index: chain_idx },
                ChildNumber::Normal { index: i },
            ];
            let child = xpub
                .derive_pub(secp, &path)
                .map_err(|e| bad(format!("derive: {e}")))?;
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

/// Build a [`WalletError`] for a backend-side condition that has no
/// Electrum-client error of its own (unexpected responses, poisoned locks).
/// Raw client errors convert directly via `From<electrum_client::Error>`.
fn electrum_err(msg: String) -> WalletError {
    WalletError::Electrum(electrum_client::Error::Message(msg))
}

/// Map a poisoned mutex into a [`WalletError`].
fn poisoned<T>(_e: T) -> WalletError {
    electrum_err("Electrum: mutex poisoned".into())
}

impl Blockchain for Electrum {
    fn is_electrum(&self) -> bool {
        true
    }

    fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WalletError> {
        let tip = self.call(|c| c.block_headers_subscribe())?;
        let best = self.get_block_hash(tip.height as u64)?;
        // Build via serde so we don't have to enumerate every field (some are
        // private or non-`Default` upstream).
        let v = json!({
            "chain": super::chain_name_for(self.network),
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

    fn get_block_count(&self) -> Result<u64, WalletError> {
        let tip = self.call(|c| c.block_headers_subscribe())?;
        Ok(tip.height as u64)
    }

    fn get_block_hash(&self, height: u64) -> Result<BlockHash, WalletError> {
        if let Ok(cache) = self.height_to_hash.lock() {
            if let Some(h) = cache.get(&height) {
                return Ok(*h);
            }
        }
        Ok(self.header_at_height(height)?.block_hash())
    }

    fn header_at_height(&self, height: u64) -> Result<Header, WalletError> {
        let header = self.call(|c| c.block_header(height as usize))?;
        if let Ok(mut cache) = self.height_to_hash.lock() {
            cache.insert(height, header.block_hash());
        }
        Ok(header)
    }

    fn tx_block_height(&self, txid: &Txid) -> Result<Option<u64>, WalletError> {
        // A txid the server does not know cannot be mined; that is the
        // routine answer for a contract nobody broadcast, not a failure.
        let tx = match self.call(|c| c.transaction_get(txid)) {
            Ok(tx) => tx,
            Err(WalletError::Electrum(electrum_client::Error::Protocol(_))) => return Ok(None),
            Err(e) => return Err(e),
        };
        self.mined_height(&tx)
    }

    fn is_confirmed_spend(
        &self,
        outpoint: &OutPoint,
        script: &Script,
    ) -> Result<bool, WalletError> {
        // listunspent folds mempool spends in, so a missing output alone is
        // ambiguous. The script's history is not: a second confirmed tx
        // touching it means the spend is mined.
        let history = self.call(|c| c.script_get_history(script))?;
        Ok(history
            .iter()
            .any(|e| e.tx_hash != outpoint.txid && e.height > 0))
    }

    fn get_raw_transaction(
        &self,
        txid: &Txid,
        _block_hash: Option<&BlockHash>,
    ) -> Result<Transaction, WalletError> {
        self.call(|c| c.transaction_get(txid))
    }

    fn get_raw_transaction_info(
        &self,
        txid: &Txid,
        _block_hash: Option<&BlockHash>,
    ) -> Result<GetRawTransactionResult, WalletError> {
        // The verbose form of `transaction.get` carries `confirmations` and block
        // info, but plenty of servers refuse it. Fetch the plain tx every server
        // serves and derive the rest from its mined height.
        // Only the few fields the wallet actually reads are populated.
        let tx = self.call(|c| c.transaction_get(txid))?;
        let hex = serialize(&tx).to_lower_hex_string();
        let height = self.mined_height(&tx)?;
        let confirmations = self.confirmations_at(height)?;
        let (blockhash, blocktime) = match height {
            Some(h) => {
                let header = self.header_at_height(h)?;
                (Some(header.block_hash()), Some(header.time as u64))
            }
            None => (None, None),
        };

        let vout: Vec<Value> = tx
            .output
            .iter()
            .enumerate()
            .map(|(n, o)| {
                json!({
                    "value": o.value.to_btc(),
                    "n": n,
                    "scriptPubKey": {
                        "asm": "",
                        "hex": o.script_pubkey.as_bytes().to_lower_hex_string(),
                        "type": null,
                    },
                })
            })
            .collect();

        let is_coinbase = tx.is_coinbase();
        let vin: Vec<Value> = tx
            .input
            .iter()
            .map(|i| {
                let script_sig = i.script_sig.as_bytes().to_lower_hex_string();
                // Core prints a coinbase input as a bare `coinbase` script with no
                // outpoint behind it; every other input carries txid/vout/scriptSig.
                let mut v = if is_coinbase {
                    json!({ "coinbase": script_sig, "sequence": i.sequence.0 })
                } else {
                    json!({
                        "txid": i.previous_output.txid,
                        "vout": i.previous_output.vout,
                        "scriptSig": { "asm": "", "hex": script_sig },
                        "sequence": i.sequence.0,
                    })
                };
                // Core omits the field entirely for a non-witness input.
                if !i.witness.is_empty() {
                    let items: Vec<String> =
                        i.witness.iter().map(|w| w.to_lower_hex_string()).collect();
                    v["txinwitness"] = json!(items);
                }
                v
            })
            .collect();

        let stub = json!({
            "in_active_chain": null,
            "hex": hex,
            "txid": txid,
            "hash": tx.compute_wtxid(),
            "size": tx.total_size(),
            // BIP141 weight units, rounded up to whole vbytes — same as Core's
            // `GetVirtualTransactionSize`.
            "vsize": tx.vsize(),
            "version": tx.version.0 as u32,
            "locktime": tx.lock_time.to_consensus_u32(),
            "vin": vin,
            "vout": vout,
            "blockhash": blockhash,
            "confirmations": confirmations,
            // Core reports the block time in both fields once a tx is mined, and
            // the receive time before that, which no Electrum server tracks.
            "time": blocktime,
            "blocktime": blocktime,
        });
        Ok(serde_json::from_value(stub)?)
    }

    fn get_tx_out(
        &self,
        txid: &Txid,
        vout: u32,
        include_mempool: Option<bool>,
    ) -> Result<Option<GetTxOutResult>, WalletError> {
        // Core's `gettxout` returns `None` once the output is spent — recovery and
        // funding-confirmation logic rely on this transition. Electrum's
        // `transaction.get` always returns the tx, so we confirm liveness via
        // `scripthash.listunspent`, which also hands back the mined height.
        // The server answering "no such transaction" is a real absence, like Core.
        // Any other failure must surface: `Ok(None)` reads as "spent", the
        // fail-dangerous answer for those callers.
        let tx = match self.call(|c| c.transaction_get(txid)) {
            Ok(tx) => tx,
            Err(WalletError::Electrum(electrum_client::Error::Protocol(_))) => return Ok(None),
            Err(e) => return Err(e),
        };
        let txout = match tx.output.get(vout as usize) {
            Some(o) => o.clone(),
            None => return Ok(None),
        };

        // Is this specific outpoint still unspent at its scriptPubKey?
        let entries = self.call(|c| c.script_list_unspent(txout.script_pubkey.as_script()))?;
        let Some(entry) = entries
            .iter()
            .find(|e| e.tx_hash == *txid && e.tx_pos as u32 == vout)
        else {
            return Ok(None);
        };

        // `height` is 0 while the output sits in the mempool.
        let height = (entry.height > 0).then_some(entry.height as u64);
        let confirmations = self.confirmations_at(height)?;
        // with `include_mempool = false`, only confirmed outputs are returned.
        if include_mempool == Some(false) && confirmations == 0 {
            return Ok(None);
        }
        let v = json!({
            "bestblock": BlockHash::all_zeros(),
            "confirmations": confirmations,
            "value": txout.value.to_btc(),
            "scriptPubKey": {
                "asm": "",
                "hex": txout.script_pubkey.as_bytes().to_lower_hex_string(),
                "type": null,
            },
            "coinbase": false,
        });
        Ok(Some(serde_json::from_value(v)?))
    }

    fn list_unspent(
        &self,
        minconf: Option<usize>,
        maxconf: Option<usize>,
    ) -> Result<Vec<ListUnspentResultEntry>, WalletError> {
        let watched: Vec<ScriptBuf> = self
            .watched
            .lock()
            .map_err(poisoned)?
            .iter()
            .cloned()
            .collect();
        let tip = self.get_block_count()?;
        let min_conf = minconf.unwrap_or(0) as u32;
        // Mirror Bitcoin Core's `listunspent` default: no upper bound.
        let max_conf = maxconf.map(|c| c as u32).unwrap_or(u32::MAX);

        // Batch the per-script queries so N watched scripts become ~N/BATCH
        // round-trips. Some servers cap batch payload size, so we chunk.
        const LIST_UNSPENT_BATCH: usize = 200;
        let mut out = Vec::new();
        for chunk in watched.chunks(LIST_UNSPENT_BATCH) {
            let refs: Vec<&Script> = chunk.iter().map(|s| s.as_script()).collect();
            let results = self.call(|c| c.batch_script_list_unspent(refs.iter().copied()))?;
            for (script, entries) in chunk.iter().zip(results) {
                for e in entries {
                    let confirmations = if e.height == 0 {
                        0
                    } else {
                        (tip + 1).saturating_sub(e.height as u64) as u32
                    };
                    if confirmations < min_conf || confirmations > max_conf {
                        continue;
                    }
                    // HD-origin is surfaced out-of-band via `hd_origin_for_script`,
                    // so `descriptor` is left empty here. Coin locking is handled
                    // wallet-side (see `Wallet`'s lock set), not by this backend.
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
                        spendable: true,
                        solvable: true,
                        descriptor: None,
                        safe: true,
                    });
                }
            }
        }
        Ok(out)
    }

    fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid, WalletError> {
        self.call(|c| c.transaction_broadcast(tx))
    }

    fn derive_addresses(
        &self,
        descriptor: &str,
        range: Option<[u32; 2]>,
    ) -> Result<Vec<Address<NetworkUnchecked>>, WalletError> {
        let (start, end) = range.map(|r| (r[0], r[1])).unwrap_or((0, 0));
        Ok(self
            .derive_addresses_local(descriptor, start, end)?
            .into_iter()
            .map(|a| a.as_unchecked().clone())
            .collect())
    }

    fn list_transactions(
        &self,
        _label: Option<&str>,
        count: Option<usize>,
        skip: Option<usize>,
        _include_watchonly: Option<bool>,
    ) -> Result<Vec<ListTransactionResult>, WalletError> {
        let watched: Vec<ScriptBuf> = self
            .watched
            .lock()
            .map_err(poisoned)?
            .iter()
            .cloned()
            .collect();
        let watched_set: HashSet<&ScriptBuf> = watched.iter().collect();

        // Every transaction touching a watched script, with the height the server
        // mined it at (0 in the mempool, -1 with unconfirmed parents).
        const HISTORY_BATCH: usize = 200;
        let mut heights: HashMap<Txid, i32> = HashMap::new();
        for chunk in watched.chunks(HISTORY_BATCH) {
            let refs: Vec<&Script> = chunk.iter().map(|s| s.as_script()).collect();
            for entries in self.call(|c| c.batch_script_get_history(refs.iter().copied()))? {
                for e in entries {
                    heights.insert(e.tx_hash, e.height);
                }
            }
        }

        // Page over the txid list before fetching anything, so a long history
        // costs one batched round-trip instead of a `transaction.get` per entry.
        let mut ordered: Vec<(Txid, i32)> = heights.into_iter().collect();
        let rank = |height: i32| if height > 0 { height } else { i32::MAX };
        ordered.sort_by(|a, b| rank(b.1).cmp(&rank(a.1)).then(a.0.cmp(&b.0)));
        let mut page: Vec<(Txid, i32)> = ordered
            .into_iter()
            .skip(skip.unwrap_or(0))
            .take(count.unwrap_or(10))
            .collect();
        // Core hands back the requested window oldest-first.
        page.reverse();

        let tip = self.get_block_count()?;
        let mut headers: HashMap<u64, Header> = HashMap::new();
        let mut out = Vec::new();
        for (txid, height) in page {
            let tx = self.call(|c| c.transaction_get(&txid))?;

            // Value our own inputs: that is what separates a spend from a
            // receive, and the input total gives the fee.
            let mut input_total = bitcoin::Amount::ZERO;
            let mut spent_by_us = bitcoin::Amount::ZERO;
            let is_coinbase = tx.is_coinbase();
            if !is_coinbase {
                for input in &tx.input {
                    let prev = self.call(|c| c.transaction_get(&input.previous_output.txid))?;
                    let prev_out = prev
                        .output
                        .get(input.previous_output.vout as usize)
                        .ok_or_else(|| {
                            electrum_err(format!(
                                "electrum: {} spends missing output {}",
                                txid, input.previous_output
                            ))
                        })?;
                    input_total += prev_out.value;
                    if watched_set.contains(&prev_out.script_pubkey) {
                        spent_by_us += prev_out.value;
                    }
                }
            }
            let is_send = spent_by_us > bitcoin::Amount::ZERO;
            let output_total: bitcoin::Amount = tx.output.iter().map(|o| o.value).sum();
            let fee = (is_send && !is_coinbase).then(|| {
                SignedAmount::from_sat(output_total.to_sat() as i64 - input_total.to_sat() as i64)
            });

            let confirmations = if height > 0 {
                (tip + 1).saturating_sub(height as u64) as i32
            } else {
                0
            };
            let (blockhash, blocktime, blockheight) = if height > 0 {
                let header = match headers.get(&(height as u64)) {
                    Some(h) => *h,
                    None => {
                        let h = self.header_at_height(height as u64)?;
                        headers.insert(height as u64, h);
                        h
                    }
                };
                (
                    Some(header.block_hash()),
                    Some(header.time as u64),
                    Some(height as u32),
                )
            } else {
                (None, None, None)
            };

            for (vout, txout) in tx.output.iter().enumerate() {
                let ours = watched_set.contains(&txout.script_pubkey);
                let category = if ours {
                    // Paying ourselves on a send is not a payment to us, and
                    // Core leaves such outputs out of the history too.
                    if is_send {
                        continue;
                    }
                    GetTransactionResultDetailCategory::Receive
                } else if is_send {
                    GetTransactionResultDetailCategory::Send
                } else {
                    continue;
                };
                let value = SignedAmount::from_sat(txout.value.to_sat() as i64);
                let amount = if ours { value } else { -value };
                out.push(ListTransactionResult {
                    info: WalletTxInfo {
                        confirmations,
                        blockhash,
                        blockindex: None,
                        blocktime,
                        blockheight,
                        txid,
                        time: blocktime.unwrap_or(0),
                        timereceived: blocktime.unwrap_or(0),
                        bip125_replaceable: Bip125Replaceable::Unknown,
                        wallet_conflicts: Vec::new(),
                    },
                    detail: GetTransactionResultDetail {
                        address: Address::from_script(&txout.script_pubkey, self.network)
                            .ok()
                            .map(|a| a.as_unchecked().clone()),
                        category,
                        amount,
                        label: None,
                        vout: vout as u32,
                        fee: if ours { None } else { fee },
                        abandoned: None,
                    },
                    trusted: None,
                    comment: None,
                });
            }
        }
        Ok(out)
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

    fn subscribe_script(&self, spk: &Script) -> Result<(), WalletError> {
        let mut guard = self.notifier.lock().map_err(poisoned)?;
        let state = &mut *guard;
        // Arm the server side first, even when a local entry already exists: a
        // reconnect leaves the entry behind with no subscription under it.
        self.arm_subscription(spk)?;
        if state.subscriptions.contains_key(spk) {
            return Ok(());
        }
        let hist = self.call(|c| c.script_get_history(spk))?;

        // Backend subscription works. Now record it as seen.
        let seen = state.subscriptions.entry(spk.to_owned()).or_default();
        for h in hist {
            if !seen.insert(h.tx_hash) {
                continue;
            }
            match self.call(|c| c.transaction_get(&h.tx_hash)) {
                Ok(tx) => state.pending.push_back(WatchEvent::TxSeen {
                    raw_tx: serialize(&tx),
                }),
                // Couldn't fetch now. Forget it and flag the script, so a later poll
                // re-reads this history instead of waiting for a notification.
                Err(_) => {
                    seen.remove(&h.tx_hash);
                    state.dirty.insert(spk.to_owned());
                }
            }
        }
        Ok(())
    }

    fn unsubscribe_script(&self, spk: &Script) -> Result<(), WalletError> {
        let mut guard = self.notifier.lock().map_err(poisoned)?;
        guard.subscriptions.remove(spk);
        guard.dirty.remove(spk);
        // A partial subscribe failure can leave the server subscribed with no
        // local entry, so always tell the server to unsubscribe also.
        self.call(|c| c.script_unsubscribe(spk).map(|_| ()))?;
        Ok(())
    }

    fn poll_event(&self) -> Option<WatchEvent> {
        let mut guard = self.notifier.lock().ok()?;
        let state = &mut *guard;
        if let Some(ev) = state.pending.pop_front() {
            return Some(ev);
        }

        // A reconnect hands us a socket with no server-side subscriptions.
        // Re-arm them and force a history re-read so anything that happened
        // while we were deaf still surfaces.
        if self.needs_rearm.swap(false, Ordering::SeqCst) {
            let armed: Vec<ScriptBuf> = state.subscriptions.keys().cloned().collect();
            for spk in armed {
                if let Err(e) = self.arm_subscription(&spk) {
                    log::warn!("re-arm after reconnect failed: {e:?}");
                    self.needs_rearm.store(true, Ordering::SeqCst);
                }
                state.dirty.insert(spk);
            }
            // The rebuilt socket lost the header subscription too; without it
            // `block_headers_pop` stays empty and tip updates silently stop.
            if let Err(e) = self.call(|c| c.block_headers_subscribe().map(|_| ())) {
                log::warn!("re-arm header subscription after reconnect failed: {e:?}");
                self.needs_rearm.store(true, Ordering::SeqCst);
            }
        }

        // The electrum client only reads from the socket while waiting for a
        // reply to its own request, so notifications never arrive on an idle
        // connection. Ping periodically to trigger a socket read. The ping also
        // keeps the held connection alive against server-side idle timeouts.
        let ping_due = state
            .last_ping
            .is_none_or(|at| at.elapsed() >= self.ping_interval);
        if ping_due {
            state.last_ping = Some(std::time::Instant::now());
            if let Err(e) = self.call(|c| c.ping()) {
                log::warn!("electrum notification ping failed: {e:?}");
                return None;
            }
        }

        // Walk subscribed scripts; for any with a pending status change, diff its
        // current history against last-seen and queue new txs.
        let scripts: Vec<ScriptBuf> = state.subscriptions.keys().cloned().collect();
        for spk in scripts {
            // A flagged script is re-read without a fresh notification; the server
            // won't send one just because our last fetch failed. Rate-limited to the
            // ping tick so a broken server isn't hammered.
            let flagged = ping_due && state.dirty.remove(&spk);
            if !flagged {
                match self.call(|c| c.script_pop(&spk)) {
                    Ok(Some(_)) => {}
                    Ok(None) => continue,
                    // The client has no record of this subscription, so it was
                    // rebuilt under us. Re-arm and re-read on the next tick
                    // rather than skipping this script forever.
                    Err(_) => {
                        if let Err(e) = self.arm_subscription(&spk) {
                            log::warn!("re-arm after script_pop failure: {e:?}");
                        }
                        state.dirty.insert(spk.clone());
                        continue;
                    }
                }
            }
            let Ok(hist) = self.call(|c| c.script_get_history(&spk)) else {
                state.dirty.insert(spk.clone());
                continue;
            };
            let seen = state
                .subscriptions
                .get_mut(&spk)
                .expect("subscription exists, just cloned its key");
            for h in hist {
                if seen.insert(h.tx_hash) {
                    if let Ok(tx) = self.call(|c| c.transaction_get(&h.tx_hash)) {
                        state.pending.push_back(WatchEvent::TxSeen {
                            raw_tx: serialize(&tx),
                        });
                    } else {
                        seen.remove(&h.tx_hash);
                        state.dirty.insert(spk.clone());
                    }
                }
            }
        }
        if let Some(ev) = state.pending.pop_front() {
            return Some(ev);
        }

        // Header tip update.
        let n = self.call(|c| c.block_headers_pop()).ok().flatten()?;
        let height = n.height as i64;
        if height <= state.last_height {
            return None;
        }
        state.last_height = height;
        Some(WatchEvent::BlockConnected(BlockRef {
            height: height as u64,
            hash: serialize(&n.header.block_hash()),
        }))
    }

    fn chain_name(&self) -> Result<String, WalletError> {
        Ok(super::chain_name_for(self.network).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str, socks5: Option<&str>) -> ElectrumConfig {
        ElectrumConfig {
            url: url.to_string(),
            socks5: socks5.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn url_host_strips_scheme_and_port() {
        assert_eq!(url_host("tcp://localhost:50001"), "localhost");
        assert_eq!(
            url_host("ssl://electrum.example.org:50002"),
            "electrum.example.org"
        );
        assert_eq!(url_host("tcp://abcdef.onion:21"), "abcdef.onion");
        // No scheme and no port are both tolerated.
        assert_eq!(url_host("abcdef.onion"), "abcdef.onion");
    }

    #[test]
    fn ping_cadence_follows_proxy() {
        assert_eq!(
            derive_ping_interval(&cfg("tcp://localhost:50001", None)),
            PING_INTERVAL_DIRECT
        );
        assert_eq!(
            derive_ping_interval(&cfg("tcp://x.onion:21", Some("127.0.0.1:9050"))),
            PING_INTERVAL_PROXIED
        );
    }

    #[test]
    fn ping_cadence_override_wins() {
        let mut c = cfg("tcp://x.onion:21", Some("127.0.0.1:9050"));
        c.poll_interval_secs = Some(2);
        assert_eq!(derive_ping_interval(&c), Duration::from_secs(2));

        let mut c = cfg("tcp://localhost:50001", None);
        c.poll_interval_secs = Some(45);
        assert_eq!(derive_ping_interval(&c), Duration::from_secs(45));
    }

    #[test]
    fn read_timeout_is_generous_when_proxied() {
        assert_eq!(
            derive_timeout_secs(&cfg("tcp://localhost:50001", None)),
            READ_TIMEOUT_DIRECT_SECS
        );
        // A slow large response over Tor must not look like a dead transport.
        assert_eq!(
            derive_timeout_secs(&cfg("tcp://x.onion:21", Some("127.0.0.1:9050"))),
            READ_TIMEOUT_PROXIED_SECS
        );
        let mut c = cfg("tcp://x.onion:21", Some("127.0.0.1:9050"));
        c.timeout = Some(7);
        assert_eq!(derive_timeout_secs(&c), 7);
    }

    #[test]
    fn proxy_does_not_require_an_onion_url() {
        // Tor reaches clearnet through an exit node, so a proxy suits any host.
        let clearnet = client_config(&cfg(
            "ssl://electrum.example.org:50002",
            Some("127.0.0.1:9050"),
        ));
        assert!(clearnet.socks5().is_some());
        // A real domain has a real certificate, so keep checking it.
        assert!(clearnet.validate_domain());

        // An onion host has no certificate, so validation is dropped there only.
        let onion = client_config(&cfg("tcp://x.onion:21", Some("127.0.0.1:9050")));
        assert!(!onion.validate_domain());
    }

    #[test]
    fn onion_without_proxy_is_rejected_before_connecting() {
        // The guard runs before any socket work, so this needs no server.
        let err = Electrum::new(&cfg("tcp://abcdef.onion:21", None)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("onion") && msg.contains("socks5"),
            "expected an explicit onion/proxy error, got: {}",
            msg
        );
    }

    #[test]
    fn clearnet_default_has_no_proxy() {
        let c = ElectrumConfig::default();
        assert!(c.socks5.is_none());
        assert_eq!(derive_ping_interval(&c), PING_INTERVAL_DIRECT);
        assert_eq!(derive_timeout_secs(&c), READ_TIMEOUT_DIRECT_SECS);
        assert_eq!(c.max_retries, 3);
    }

    #[test]
    fn terminal_errors_are_not_retried() {
        use electrum_client::Error;
        // Answered-but-negative, or local state: reconnecting cannot help.
        assert!(is_terminal(&Error::AlreadySubscribed([0u8; 32].into())));
        assert!(is_terminal(&Error::NotSubscribed([0u8; 32].into())));
        // A transport-shaped error must stay retryable.
        assert!(!is_terminal(&Error::Message("socket closed".into())));
    }

    #[test]
    fn unreachable_error_names_the_attempt_count() {
        let e = WalletError::ElectrumUnreachable {
            attempts: 4,
            last: electrum_client::Error::Message("no route".into()),
        };
        let msg = e.to_string();
        assert!(msg.contains("unreachable"), "got: {}", msg);
        assert!(msg.contains('4'), "got: {}", msg);
        assert!(msg.contains("no route"), "got: {}", msg);
    }
}
