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
    sync::Mutex,
    time::Duration,
};

use bitcoin::{
    address::NetworkUnchecked,
    bip32::{ChildNumber, Xpub},
    block::Header,
    consensus::encode::{serialize, serialize_hex},
    hashes::Hash,
    Address, BlockHash, CompressedPublicKey, Script, ScriptBuf, SignedAmount, Transaction, Txid,
};
use bitcoind::bitcoincore_rpc::json::{
    Bip125Replaceable, GetBlockchainInfoResult, GetRawTransactionResult,
    GetTransactionResultDetail, GetTransactionResultDetailCategory, GetTxOutResult,
    ListTransactionResult, ListUnspentResultEntry, WalletTxInfo,
};
use electrum_client::{Client as ElectrumClient, ElectrumApi, Param};
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
}

impl Default for ElectrumConfig {
    fn default() -> Self {
        Self {
            url: "tcp://electrum1.bluewallet.io:50001".to_string(),
        }
    }
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
    inner: ElectrumClient,
    /// Connection config, retained so a fresh independent connection can be
    /// built via [`Electrum::reconnect`] (the watchtower needs its own).
    config: ElectrumConfig,
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
        let inner = ElectrumClient::new(&cfg.url)?;
        let config = cfg.clone();
        let features = inner.server_features()?;
        let network = network_from_electrum_genesis(&features.genesis_hash).ok_or_else(|| {
            electrum_err(format!(
                "unknown genesis from electrum server: {:?}",
                features.genesis_hash
            ))
        })?;
        // Arm the header subscription so the watchtower's `poll_event` sees tip
        // updates; harmless for the wallet's instance, which never polls.
        let last_height = inner.block_headers_subscribe()?.height as i64;
        Ok(Self {
            inner,
            config,
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

    /// Open a fresh, independent Electrum connection with the same config.
    ///
    /// Used to give a separate consumer (e.g. the watchtower discovery thread)
    /// its own connection without threading the config around separately.
    pub fn reconnect(&self) -> Result<Self, WalletError> {
        Self::new(&self.config)
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
        let mut out = Vec::with_capacity((end - start + 1) as usize);
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
        let tip = self.inner.block_headers_subscribe()?;
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
        let tip = self.inner.block_headers_subscribe()?;
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
        let header = self.inner.block_header(height as usize)?;
        if let Ok(mut cache) = self.height_to_hash.lock() {
            cache.insert(height, header.block_hash());
        }
        Ok(header)
    }

    fn tx_block_height(&self, txid: &Txid) -> Result<Option<u64>, WalletError> {
        // Electrum has no txid→height index, but `scripthash.get_history` reports
        // the mined height of every tx touching a script, so any output of this tx
        // serves as the lookup key. That height is the server's own answer — no
        // arithmetic against a tip that may have moved — and works at any depth,
        // for scripts inside or outside our watch set.
        let tx = self.inner.transaction_get(txid)?;
        for txout in &tx.output {
            // Unspendable outputs (OP_RETURN) have no scripthash history.
            if txout.script_pubkey.is_op_return() {
                continue;
            }
            let Ok(history) = self
                .inner
                .script_get_history(txout.script_pubkey.as_script())
            else {
                continue;
            };
            if let Some(entry) = history.iter().find(|e| e.tx_hash == *txid) {
                // `height` is 0 for a mempool tx and -1 for one with unconfirmed
                // parents; both mean "not mined yet".
                return Ok((entry.height > 0).then_some(entry.height as u64));
            }
        }
        Ok(None)
    }

    fn get_raw_transaction(
        &self,
        txid: &Txid,
        _block_hash: Option<&BlockHash>,
    ) -> Result<Transaction, WalletError> {
        Ok(self.inner.transaction_get(txid)?)
    }

    fn get_raw_transaction_info(
        &self,
        txid: &Txid,
        _block_hash: Option<&BlockHash>,
    ) -> Result<GetRawTransactionResult, WalletError> {
        // Ask electrs for the verbose form so we can read `confirmations` directly.
        // Only the few fields the wallet actually reads are populated.
        let value: Value = self.inner.raw_call(
            "blockchain.transaction.get",
            vec![Param::String(txid.to_string()), Param::Bool(true)],
        )?;
        let confirmations = value
            .get("confirmations")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let hex = value.get("hex").and_then(|v| v.as_str()).ok_or_else(|| {
            electrum_err(format!(
                "electrum: missing `hex` field for transaction {txid}"
            ))
        })?;
        let bytes: Vec<u8> = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(hex).map_err(|e| {
            electrum_err(format!("electrum: invalid transaction hex for {txid}: {e}"))
        })?;
        let tx: Transaction = bitcoin::consensus::deserialize(&bytes)?;

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
                        "hex": serialize_hex(&o.script_pubkey),
                        "type": null,
                    },
                })
            })
            .collect();
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
            "vout": vout,
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
        include_mempool: Option<bool>,
    ) -> Result<Option<GetTxOutResult>, WalletError> {
        // Core's `gettxout` returns `None` once the output is spent — recovery and
        // funding-confirmation logic rely on this transition. Electrum's
        // `transaction.get` always returns the tx, so we confirm liveness via
        // `scripthash.listunspent`.
        let value: Value = match self.inner.raw_call(
            "blockchain.transaction.get",
            vec![Param::String(txid.to_string()), Param::Bool(true)],
        ) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let hex = value.get("hex").and_then(|v| v.as_str()).unwrap_or("");
        let bytes: Vec<u8> = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(hex)
            .map_err(|e| electrum_err(format!("get_tx_out hex decode: {e}")))?;
        let tx: Transaction = bitcoin::consensus::deserialize(&bytes)?;
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
            })?;
        if !still_unspent {
            return Ok(None);
        }

        let confirmations = value
            .get("confirmations")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
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
            let results = self.inner.batch_script_list_unspent(refs.iter().copied())?;
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
        Ok(self.inner.transaction_broadcast(tx)?)
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
            for entries in self.inner.batch_script_get_history(refs.iter().copied())? {
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
            let tx = self.inner.transaction_get(&txid)?;

            // Value our own inputs: that is what separates a spend from a
            // receive, and the input total gives the fee.
            let mut input_total = bitcoin::Amount::ZERO;
            let mut spent_by_us = bitcoin::Amount::ZERO;
            let is_coinbase = tx.is_coinbase();
            if !is_coinbase {
                for input in &tx.input {
                    let prev = self.inner.transaction_get(&input.previous_output.txid)?;
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
                    // Change coming back from our own spend is not a payment to
                    // us, and Core leaves it out of the history too.
                    let is_change = self
                        .hd_origin_for_script(&txout.script_pubkey)
                        .is_some_and(|hd| hd.keychain_idx == 1);
                    if is_send && is_change {
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
        if state.subscriptions.contains_key(spk) {
            return Ok(());
        }
        // Record the subscription only once both calls succeed, so a failure here
        // doesn't make every retry an early-return no-op.
        if let Err(e) = self.inner.script_subscribe(spk) {
            if !matches!(e, electrum_client::Error::AlreadySubscribed(_)) {
                return Err(e.into());
            }
        }
        let hist = self.inner.script_get_history(spk)?;

        // Backend subscription works. Now record it as seen.
        let seen = state.subscriptions.entry(spk.to_owned()).or_default();
        for h in hist {
            if !seen.insert(h.tx_hash) {
                continue;
            }
            match self.inner.transaction_get(&h.tx_hash) {
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
        self.inner.script_unsubscribe(spk)?;
        Ok(())
    }

    fn poll_event(&self) -> Option<WatchEvent> {
        let mut guard = self.notifier.lock().ok()?;
        let state = &mut *guard;
        if let Some(ev) = state.pending.pop_front() {
            return Some(ev);
        }

        // The electrum client only reads from the socket while waiting for a
        // reply to its own request, so notifications never arrive on an idle
        // connection. Ping periodically to trigger a socket read.
        const PING_INTERVAL: Duration = Duration::from_secs(1);
        let ping_due = state
            .last_ping
            .is_none_or(|at| at.elapsed() >= PING_INTERVAL);
        if ping_due {
            state.last_ping = Some(std::time::Instant::now());
            if let Err(e) = self.inner.ping() {
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
            if !flagged && !matches!(self.inner.script_pop(&spk), Ok(Some(_))) {
                continue;
            }
            let Ok(hist) = self.inner.script_get_history(&spk) else {
                state.dirty.insert(spk.clone());
                continue;
            };
            let seen = state
                .subscriptions
                .get_mut(&spk)
                .expect("subscription exists, just cloned its key");
            for h in hist {
                if seen.insert(h.tx_hash) {
                    if let Ok(tx) = self.inner.transaction_get(&h.tx_hash) {
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
        let n = self.inner.block_headers_pop().ok().flatten()?;
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
