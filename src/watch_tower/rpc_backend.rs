//! Watchtower RPC backend: querying bitcoind, scanning mempool, and running discovery.

use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use bitcoincore_rpc::{
    bitcoin::{Block, BlockHash, Transaction, Txid},
    json::GetBlockchainInfoResult,
    Client, RpcApi,
};
use bitcoind::bitcoincore_rpc;
use nostr::{
    event::Kind,
    filter::Filter,
    message::{ClientMessage, RelayMessage, SubscriptionId},
    util::JsonUtil,
};
use tungstenite::{stream::MaybeTlsStream, Message};

use crate::{
    nostr_coinswap::{COINSWAP_KIND, NOSTR_RELAYS},
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::FileRegistry,
        utils::{parse_fidelity_event, process_fidelity, process_transaction},
        watcher_error::WatcherError,
    },
};

/// Lightweight wrapper around bitcoind RPC calls used by the watchtower.
pub struct BitcoinRpc {
    client: Client,
}

impl BitcoinRpc {
    /// Constructs a new RPC wrapper using the provided configuration.
    pub fn new(rpc_config: RPCConfig) -> Result<Self, WatcherError> {
        let client = Client::new(&rpc_config.url, rpc_config.auth)?;
        Ok(Self { client })
    }

    /// Wraps an existing bitcoind RPC client.
    pub fn new_client(client: Client) -> Self {
        Self { client }
    }

    /// Get txids of all transactions in the mempool.
    pub fn get_raw_mempool(&self) -> Result<Vec<Txid>, WatcherError> {
        let raw_mempool = self.client.get_raw_mempool()?;
        Ok(raw_mempool)
    }

    /// Fetches a full transaction by txid.
    pub fn get_raw_tx(&self, txid: &Txid) -> Result<Transaction, WatcherError> {
        let tx = self.client.get_raw_transaction(txid, None)?;
        Ok(tx)
    }

    /// Returns chain metadata.
    pub fn get_blockchain_info(&self) -> Result<GetBlockchainInfoResult, WatcherError> {
        let blockchain_info = self.client.get_blockchain_info()?;
        Ok(blockchain_info)
    }

    /// Retrieves the block hash at a given height.
    pub fn get_block_hash(&self, height: u64) -> Result<BlockHash, WatcherError> {
        let block_hash = self.client.get_block_hash(height)?;
        Ok(block_hash)
    }

    /// Fetches a block by hash.
    pub fn get_block_count(&self) -> Result<u64, WatcherError> {
        let height = self.client.get_block_count()?;
        Ok(height)
    }

    /// Fetches a block by hash.
    pub fn get_block(&self, hash: BlockHash) -> Result<Block, WatcherError> {
        let block = self.client.get_block(&hash)?;
        Ok(block)
    }

    /// Processes transactions in the mempool and updates registry.
    pub fn process_mempool(&mut self, registry: &mut FileRegistry) -> Result<(), WatcherError> {
        let txids = self.get_raw_mempool()?;
        for txid in &txids {
            let tx = self.get_raw_tx(txid)?;
            process_transaction(&tx, registry, false);
        }
        Ok(())
    }

    // ## TODO: Create a nostr module and move these set of methods there.
    // ## TODO: Improve error handling, currently we are just using messing
    //          unwraps and general error
    // ## TODO: Instead of looping over relay's have a connection Pool.
    /// Discovers maker fidelity bonds by subscribing to Nostr events (kind 37777).
    pub fn run_discovery(
        self,
        registry: FileRegistry,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(), WatcherError> {
        log::info!("Starting market discovery via Nostr");

        let registry = Arc::new(Mutex::new(registry));
        let this = Arc::new(Mutex::new(self));

        for relay in NOSTR_RELAYS {
            let relay = relay.to_string();
            let shutdown = shutdown.clone();
            let registry = Arc::clone(&registry);
            let this = Arc::clone(&this);

            std::thread::Builder::new()
                .name(format!("nostr-session-{}", relay))
                .spawn(move || {
                    Self::run_nostr_session_for_relay(&relay.clone(), registry, shutdown, this);
                })
                .map_err(|e| WatcherError::General(e.to_string()))?;
        }

        Ok(())
    }

    /// Runs a long-lived Nostr session for a single relay.
    /// Reconnects automatically until shutdown is requested.
    fn run_nostr_session_for_relay(
        relay_url: &str,
        registry: Arc<Mutex<FileRegistry>>,
        shutdown: Arc<AtomicBool>,
        bitcoin_rpc: Arc<Mutex<BitcoinRpc>>,
    ) {
        log::info!("Starting Nostr session for relay {}", relay_url);

        while !shutdown.load(Ordering::SeqCst) {
            match Self::connect_and_run_once(
                relay_url,
                registry.clone(),
                shutdown.clone(),
                bitcoin_rpc.clone(),
            ) {
                Ok(()) => {
                    // Likely exited due to shutdown
                    break;
                }
                Err(e) => {
                    log::warn!(
                        "Nostr session error on {}: {:?}, retrying in 5s",
                        relay_url,
                        e
                    );
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
            }
        }

        log::info!("Stopped Nostr session for relay {}", relay_url);
    }

    /// Establishes a single Nostr connection and processes events until error or shutdown.
    fn connect_and_run_once(
        relay_url: &str,
        registry: Arc<Mutex<FileRegistry>>,
        shutdown: Arc<AtomicBool>,
        bitcoin_rpc: Arc<Mutex<BitcoinRpc>>,
    ) -> Result<(), WatcherError> {
        let (mut socket, _) =
            tungstenite::connect(relay_url).map_err(|e| WatcherError::General(e.to_string()))?;

        let filter = Filter::new().kind(Kind::Custom(COINSWAP_KIND));
        let req = ClientMessage::Req {
            subscription_id: Cow::Owned(SubscriptionId::new(format!(
                "market-discovery-{}",
                relay_url
            ))),
            filters: vec![Cow::Owned(filter)],
        };

        socket
            .write(Message::Text(req.as_json().into()))
            .map_err(|e| WatcherError::General(e.to_string()))?;

        socket
            .flush()
            .map_err(|e| WatcherError::General(e.to_string()))?;

        log::info!(
            "Subscribed to fidelity announcements on {} (kind={})",
            relay_url,
            COINSWAP_KIND
        );

        Self::read_event_loop(registry, socket, shutdown, bitcoin_rpc, relay_url)
    }

    // ## TODO: improve this, we don't really need FileRegistry inside Arc<Mutex
    //          and come up with a better way to use rpc, instead of Arc<Mutex
    fn read_event_loop(
        registry: Arc<Mutex<FileRegistry>>,
        mut socket: tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>,
        shutdown: Arc<AtomicBool>,
        bitcoin_rpc: Arc<Mutex<BitcoinRpc>>,
        relay_url: &str,
    ) -> Result<(), WatcherError> {
        while !shutdown.load(Ordering::SeqCst) {
            let msg = socket
                .read()
                .map_err(|e| WatcherError::General(e.to_string()))?;

            let text = match msg {
                Message::Text(t) => t,
                Message::Binary(b) => String::from_utf8(b.to_vec())
                    .map_err(|e| WatcherError::General(e.to_string()))?
                    .into(),
                _ => continue,
            };

            let relay_msg =
                RelayMessage::from_json(&text).map_err(|e| WatcherError::General(e.to_string()))?;

            Self::handle_relay_message(
                registry.clone(),
                relay_msg,
                bitcoin_rpc.clone(),
                relay_url,
            )?;
        }

        Ok(())
    }

    fn handle_relay_message(
        registry: Arc<Mutex<FileRegistry>>,
        msg: RelayMessage,
        bitcoin_rpc: Arc<Mutex<BitcoinRpc>>,
        relay_url: &str,
    ) -> Result<(), WatcherError> {
        match msg {
            RelayMessage::Event { event, .. } => {
                if event.kind != Kind::Custom(COINSWAP_KIND) {
                    return Ok(());
                }

                let Some((txid, vout)) = parse_fidelity_event(&event) else {
                    return Ok(());
                };

                // ## TODO: Optimize for this, we are currently doing a lot of RPC calls which
                //    are redundant as nostr relay's share same event multiple time. Come up
                //    with a clever way to reduce these RPC trips.
                let Ok(tx) = bitcoin_rpc.lock().unwrap().get_raw_tx(&txid) else {
                    log::debug!("Received invalid txid: {txid:?}");
                    return Ok(());
                };

                match process_fidelity(&tx) {
                    Some(fidelity) => {
                        if registry.lock().unwrap().insert_fidelity(txid, fidelity) {
                            log::info!("Stored verified fidelity via {relay_url}: {txid}:{vout}");
                        }
                    }
                    None => {
                        log::debug!("Invalid fidelity {txid}:{vout} via {relay_url}");
                    }
                }
            }

            RelayMessage::EndOfStoredEvents(sub_id) => {
                log::info!("EOSE received for subscription {sub_id} via {relay_url}");
            }

            _ => {}
        }

        Ok(())
    }
}

impl From<Client> for BitcoinRpc {
    fn from(value: Client) -> Self {
        BitcoinRpc { client: value }
    }
}
