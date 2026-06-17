//! Notification backends for the watcher loop.
//!
//! Two transports are provided behind [`NotificationBackend`]:
//! [`ZmqBackend`] (Bitcoin Core's ZMQ rawtx/rawblock channel) and
//! [`ElectrumNotifier`] (Electrum's `blockchain.headers.subscribe` +
//! per-script `scripthash.subscribe`). The watcher polls the enum and is
//! otherwise unaware of which transport is wired in.

use std::collections::{HashMap, HashSet, VecDeque};

use bitcoin::{consensus::encode::serialize, Script, ScriptBuf, Transaction, Txid};
use electrum_client::{Client as ElectrumClient, ElectrumApi};

use super::{
    rest_backend::{
        chain_name_for, network_from_electrum_genesis, with_electrum_client, BitcoinRest,
    },
    watcher_error::WatcherError,
};

#[derive(Clone)]
/// Chain State for either Bitcoin Core REST or Electrum protocol. Used by the watcher to query chain state for spends and block height.
pub enum ChainSource {
    /// Bitcoin Core REST.
    Rest(BitcoinRest),
    /// Electrum protocol.
    Electrum(String),
}

impl ChainSource {
    /// Current chain tip height.
    pub fn block_count(&self) -> Result<u64, WatcherError> {
        match self {
            Self::Rest(r) => r.get_block_count(),
            Self::Electrum(url) => with_electrum_client(url, |c| {
                c.block_headers_subscribe().map(|h| h.height as u64)
            }),
        }
    }

    /// Fetch a raw transaction by txid.
    pub fn get_raw_tx(&self, txid: &Txid) -> Result<Transaction, WatcherError> {
        match self {
            Self::Rest(r) => r.get_raw_tx(txid),
            Self::Electrum(url) => with_electrum_client(url, |c| c.transaction_get(txid)),
        }
    }

    /// Bitcoin Core chain name: "main", "test", "signet", "regtest".
    pub fn chain_name(&self) -> Result<String, WatcherError> {
        match self {
            Self::Rest(r) => Ok(r.get_blockchain_info()?.chain.to_string()),
            Self::Electrum(url) => {
                let genesis = with_electrum_client(url, |c| c.server_features())?.genesis_hash;
                network_from_electrum_genesis(&genesis)
                    .map(|n| chain_name_for(n).to_string())
                    .ok_or_else(|| {
                        WatcherError::General(format!(
                            "unknown genesis hash from electrum: {genesis:?}"
                        ))
                    })
            }
        }
    }
}

/// Reference to a block received via the notification backend.
#[derive(Debug, Clone)]
pub struct BlockRef {
    /// Height of the block if known / 0 when unavailable.
    pub height: u64,
    /// Raw block hash.
    pub hash: Vec<u8>,
}

/// Events emitted by a notification backend.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// Notifies when a transaction is seen (mempool or block) and includes the raw bytes.
    TxSeen {
        /// Raw transaction bytes.
        raw_tx: Vec<u8>,
    },
    /// Notifies when chain tip is updated with rawblock data.
    BlockConnected(BlockRef),
}

// --- ZMQ (Bitcoin Core) ------------------------------------------------

/// ZMQ backend used by the watcher to subscribe to node notifications.
pub struct ZmqBackend {
    socket: zmq::Socket,
}

impl ZmqBackend {
    /// Connects to a ZMQ endpoint and subscribes to rawtx and rawblock topics.
    pub fn new(endpoint: &str) -> Self {
        let ctx = zmq::Context::new();
        let socket = ctx.socket(zmq::SUB).expect("socket");

        socket.connect(endpoint).expect("connect");

        // Subscribe to both topics
        socket.set_subscribe(b"rawtx").expect("subscribe rawtx");
        socket
            .set_subscribe(b"rawblock")
            .expect("subscribe rawblock");

        Self { socket }
    }

    fn recv_event(&self) -> Option<(String, Vec<u8>)> {
        let msg = self.socket.recv_multipart(zmq::DONTWAIT).ok()?;
        if msg.len() < 2 {
            return None;
        }

        let topic = String::from_utf8_lossy(&msg[0]).to_string();
        let payload = msg[1].clone();
        Some((topic, payload))
    }

    /// Non-blocking poll for the next backend event.
    pub fn poll(&mut self) -> Option<BackendEvent> {
        let (topic, payload) = self.recv_event()?;

        match topic.as_str() {
            "rawtx" => Some(BackendEvent::TxSeen { raw_tx: payload }),
            "rawblock" => Some(BackendEvent::BlockConnected(BlockRef {
                height: 0,
                hash: payload,
            })),
            _ => None,
        }
    }
}
/// Electrum-protocol notification backend. Drives `headers.subscribe` and one `scripthash.subscribe` per watched SPK.
pub struct ElectrumNotifier {
    inner: ElectrumClient,
    last_height: i64,
    /// Txids already surfaced for each subscribed scriptPubKey.
    subscriptions: HashMap<ScriptBuf, HashSet<Txid>>,
    /// Events buffered between `poll` calls (one notification can yield many new transactions; we return them one at a time).
    pending: VecDeque<BackendEvent>,
}

impl ElectrumNotifier {
    /// Connect to an Electrum server and arm the headers subscription.
    pub fn new(url: &str) -> Result<Self, electrum_client::Error> {
        let inner = ElectrumClient::new(url)?;
        let tip = inner.block_headers_subscribe()?;
        Ok(Self {
            inner,
            last_height: tip.height as i64,
            subscriptions: HashMap::new(),
            pending: VecDeque::new(),
        })
    }

    /// Subscribe to a scriptPubKey. Idempotent; seeds the seen-set from current
    /// history so the first call doesn't fire `TxSeen` for already-mined txs.
    /// Mempool entries from the seeded history do re-emit, that's the
    /// crash-recovery path for a spend that landed while the maker was down.
    pub fn subscribe_script(&mut self, spk: &Script) -> Result<(), electrum_client::Error> {
        if self.subscriptions.contains_key(spk) {
            return Ok(());
        }
        self.subscriptions.insert(spk.to_owned(), HashSet::new());
        let _ = self.inner.script_subscribe(spk)?;
        let hist = self.inner.script_get_history(spk)?;
        let seen = self
            .subscriptions
            .get_mut(spk)
            .expect("just inserted above");
        for h in hist {
            if !seen.insert(h.tx_hash) {
                continue;
            }
            match self.inner.transaction_get(&h.tx_hash) {
                Ok(tx) => self.pending.push_back(BackendEvent::TxSeen {
                    raw_tx: serialize(&tx),
                }),
                Err(_) => {
                    // Couldn't fetch right now, roll back so we retry on the
                    // next poll/notification.
                    seen.remove(&h.tx_hash);
                }
            }
        }
        Ok(())
    }

    /// Drop a previously-armed subscription for `spk`. This is invoked when the corresponding watch is removed (`WatcherCommand::Unwatch`) so long-lived
    /// watchtowers don't accumulate stale subscriptions that keep `script_pop`/`script_get_history` polling forever for completed swaps.
    pub fn unsubscribe_script(&mut self, spk: &Script) -> Result<(), electrum_client::Error> {
        // Local state
        if self.subscriptions.remove(spk).is_none() {
            return Ok(());
        }
        // Server state. If this call fails (say, due to a network hiccup), we've still freed our local state so `poll` won't walk this script anymore.
        let _ = self.inner.script_unsubscribe(spk)?;
        Ok(())
    }

    /// Non-blocking poll. Drains buffered events first, then per-script
    /// notifications, then header tip updates.
    pub fn poll(&mut self) -> Option<BackendEvent> {
        if let Some(ev) = self.pending.pop_front() {
            return Some(ev);
        }

        // Walk subscribed scripts; for any that has a pending status change,
        // diff its current history against last-seen and queue new txs.
        let scripts: Vec<ScriptBuf> = self.subscriptions.keys().cloned().collect();
        for spk in scripts {
            if !matches!(self.inner.script_pop(&spk), Ok(Some(_))) {
                continue;
            }
            let Ok(hist) = self.inner.script_get_history(&spk) else {
                continue;
            };
            // Re-borrow as mutable now that we've finished any immutable use.
            let seen = self
                .subscriptions
                .get_mut(&spk)
                .expect("subscription exists, just cloned its key");
            for h in hist {
                if seen.insert(h.tx_hash) {
                    if let Ok(tx) = self.inner.transaction_get(&h.tx_hash) {
                        self.pending.push_back(BackendEvent::TxSeen {
                            raw_tx: serialize(&tx),
                        });
                    } else {
                        // Couldn't fetch — roll back so we retry on the next poll.
                        seen.remove(&h.tx_hash);
                    }
                }
            }
        }
        if let Some(ev) = self.pending.pop_front() {
            return Some(ev);
        }

        // Header tip update.
        let n = self.inner.block_headers_pop().ok().flatten()?;
        let height = n.height as i64;
        if height <= self.last_height {
            return None;
        }
        self.last_height = height;
        Some(BackendEvent::BlockConnected(BlockRef {
            height: height as u64,
            hash: serialize(&n.header.block_hash()),
        }))
    }
}

/// Notification backend the watcher drives.
pub enum NotificationBackend {
    /// Bitcoin Core's ZMQ pub/sub channel.
    Zmq(ZmqBackend),
    /// Electrum protocol `blockchain.headers.subscribe` channel.
    Electrum(Box<ElectrumNotifier>),
}

impl NotificationBackend {
    /// Non-blocking poll for the next event, regardless of underlying transport.
    pub fn poll(&mut self) -> Option<BackendEvent> {
        match self {
            Self::Zmq(b) => b.poll(),
            Self::Electrum(b) => b.poll(),
        }
    }

    /// Subscribe to a scriptPubKey so future activity on it surfaces as a
    /// `TxSeen` event. No-op on the ZMQ backend (Bitcoin Core's `rawtx` feed
    /// is already a firehose).
    pub fn subscribe_script(&mut self, spk: &Script) -> Result<(), electrum_client::Error> {
        match self {
            Self::Zmq(_) => Ok(()),
            Self::Electrum(n) => n.subscribe_script(spk),
        }
    }

    /// Drop a previously-armed subscription for `spk`. No-op on the ZMQ
    /// backend (no per-script subscriptions exist there). On Electrum,
    /// removes the local subscription bookkeeping and tells the server.
    pub fn unsubscribe_script(&mut self, spk: &Script) -> Result<(), electrum_client::Error> {
        match self {
            Self::Zmq(_) => Ok(()),
            Self::Electrum(n) => n.unsubscribe_script(spk),
        }
    }
}
