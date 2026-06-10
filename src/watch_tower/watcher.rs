//! Watchtower watcher module.
//!
//! Runs the core event loop, processes watcher commands, reacts to ZMQ backend events,
//! spawns optional RPC-based discovery and updates the on-disk registry of watches and fidelity records.

use std::{
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver as StdReceiver, TryRecvError},
        Arc,
    },
    time::Duration,
};

use bitcoin::{
    consensus::deserialize, hashes::Hash, Block, BlockHash, Network, OutPoint, ScriptBuf,
    Transaction,
};
use crossbeam_channel::Sender as CbSender;

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        nostr_discovery,
        registry_storage::{Checkpoint, FileRegistry, WatchRequest},
        rest_backend::{electrum_chain_name, electrum_get_raw_tx, BitcoinRest},
        utils::{process_block, process_transaction},
        watcher_error::WatcherError,
        zmq_backend::{BackendEvent, NotificationBackend},
    },
};

/// Describes watcher behavior.
pub trait Role {
    /// Enables or disables discovery.
    const RUN_DISCOVERY: bool;
}

/// Drives the watchtower event loop, coordinating backend events and client commands.
pub struct Watcher<R: Role> {
    backend: NotificationBackend,
    registry: FileRegistry,
    rx_requests: StdReceiver<WatcherCommand>,
    tx_events: CbSender<WatcherEvent>,
    nostr_relays: Vec<String>,
    nostr_tor_config: Option<(u16, String)>,
    /// Electrum server URL. When `Some`, the watcher uses Electrum APIs instead of
    /// Bitcoin Core REST.
    electrum_url: Option<String>,
    _role: PhantomData<R>,
}

/// Events emitted by the watcher to its clients.
#[derive(Debug, Clone)]
pub enum WatcherEvent {
    /// Indicates that a watched outpoint was spent.
    UtxoSpent {
        /// Monitored outpoint.
        outpoint: OutPoint,
        /// Transaction that spent the outpoint, if known.
        spending_tx: Option<Transaction>,
    },
    /// Returned when a queried outpoint is not being watched.
    NoOutpoint,
}

/// Commands accepted by the watcher from clients.
///
/// `RegisterWatchRequest` and `Unwatch` carry the outpoint's `script_pubkey`
/// alongside the outpoint itself so the watcher never needs to fetch the
/// funding tx over the network in its hot loop. Callers always have the
/// scriptPubKey locally (they just built or received the parent tx), so this
/// is zero-cost on their side and keeps spend detection / shutdown handling
/// responsive even when the Electrum server is slow or wedged.
#[derive(Debug, Clone)]
pub enum WatcherCommand {
    /// Store a new watch request.
    RegisterWatchRequest {
        /// Outpoint to begin tracking.
        outpoint: OutPoint,
        /// `scriptPubKey` of the outpoint, used to arm the Electrum
        /// per-script subscription. Ignored on the ZMQ backend.
        script_pubkey: ScriptBuf,
    },
    /// Query whether an outpoint has been spent.
    WatchRequest {
        /// Outpoint being queried.
        outpoint: OutPoint,
    },
    /// Remove an existing watch.
    Unwatch {
        /// Outpoint to stop tracking.
        outpoint: OutPoint,
        /// `scriptPubKey` of the outpoint, used to drop the Electrum
        /// subscription. Ignored on the ZMQ backend.
        script_pubkey: ScriptBuf,
    },
    /// Terminate the watcher loop.
    Shutdown,
}

impl<R: Role> Watcher<R> {
    /// Creates a watcher with its backend, registry, and communication channels.
    #[hotpath::measure]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend: NotificationBackend,
        registry: FileRegistry,
        rx_requests: StdReceiver<WatcherCommand>,
        tx_events: CbSender<WatcherEvent>,
        nostr_relays: Vec<String>,
        nostr_tor_config: Option<(u16, String)>,
        electrum_url: Option<String>,
    ) -> Self {
        Self {
            backend,
            registry,
            rx_requests,
            tx_events,
            nostr_relays,
            nostr_tor_config,
            electrum_url,
            _role: PhantomData,
        }
    }

    /// Runs the watcher loop: handles ZMQ events and commands, optionally spawning discovery.
    #[hotpath::measure]
    pub fn run(
        &mut self,
        rpc_config: Option<RPCConfig>,
        initial_sync_complete: Arc<AtomicBool>,
    ) -> Result<(), WatcherError> {
        log::info!("Watcher initiated");

        // Detect network and scan mempool — path depends on backend.
        let (network, rest_for_discovery) = if let Some(url) = self.electrum_url.as_deref() {
            let chain = electrum_chain_name(url)?;
            let net = match chain.as_str() {
                "main" => Network::Bitcoin,
                "test" => Network::Testnet,
                "testnet4" => Network::Testnet4,
                "signet" => Network::Signet,
                "regtest" => Network::Regtest,
                unknown => {
                    return Err(WatcherError::General(format!(
                        "Unsupported Bitcoin network from electrum: {unknown}"
                    )))
                }
            };
            log::debug!("Watcher: electrum mode — skipping mempool scan");
            // Re-subscribe each persisted watch's scriptPubKey so a maker that
            // crashed mid-swap regains spend-detection after restart. Without
            // this, `ElectrumNotifier.subscriptions` (in-memory only) starts
            // empty on restart even though the registry has live watches.
            let watches = self.registry.list_watches();
            for watch in watches {
                if watch.spent_tx.is_some() {
                    continue;
                }
                match Self::lookup_outpoint_spk(url, watch.outpoint) {
                    Ok(spk) => {
                        if let Err(e) = self.backend.subscribe_script(&spk) {
                            log::warn!(
                                "re-subscribe failed for persisted watch {}: {e:?}",
                                watch.outpoint
                            );
                        }
                    }
                    Err(e) => log::warn!(
                        "could not resolve SPK for persisted watch {}: {e:?}",
                        watch.outpoint
                    ),
                }
            }
            (net, None::<BitcoinRest>)
        } else {
            let rpc_config = rpc_config.ok_or_else(|| {
                WatcherError::General("Watcher requires RPCConfig when not in electrum mode".into())
            })?;
            let rest = BitcoinRest::new(rpc_config)?;
            let net = match rest.get_blockchain_info()?.chain.to_string().as_str() {
                "main" => Network::Bitcoin,
                "test" => Network::Testnet,
                "testnet4" => Network::Testnet4,
                "signet" => Network::Signet,
                "regtest" => Network::Regtest,
                unknown => {
                    return Err(WatcherError::General(format!(
                        "Unsupported Bitcoin network from node: {unknown}"
                    )))
                }
            };
            if let Err(e) = rest.process_mempool(&mut self.registry) {
                log::warn!("Failed to process mempool on startup: {}", e);
            }
            (net, Some(rest))
        };

        let electrum_url = self.electrum_url.clone();
        let discovery_shutdown = Arc::new(AtomicBool::new(false));
        let registry = self.registry.clone();
        let nostr_relays = self.nostr_relays.clone();
        let nostr_tor_config = self.nostr_tor_config.clone();
        std::thread::scope(move |s| {
            let discovery_clone = discovery_shutdown.clone();
            if R::RUN_DISCOVERY {
                if let Some(nostr_tor_config) = nostr_tor_config {
                    s.spawn(move || {
                        if let Err(e) = nostr_discovery::run_discovery(
                            rest_for_discovery,
                            electrum_url,
                            network,
                            registry,
                            discovery_shutdown.clone(),
                            initial_sync_complete,
                            &nostr_relays,
                            nostr_tor_config,
                        ) {
                            log::error!("Discovery thread failed: {:?}", e);
                        }
                    });
                }
            }
            loop {
                match self.rx_requests.try_recv() {
                    Ok(cmd) => {
                        if !self.handle_command(cmd) {
                            discovery_clone.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                    Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }

                if let Some(event) = self.backend.poll() {
                    self.handle_event(event);
                } else {
                    // Avoid busy-looping when there are no commands and no ZMQ events.
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Ok(())
    }

    #[hotpath::measure]
    fn handle_command(&mut self, cmd: WatcherCommand) -> bool {
        match cmd {
            WatcherCommand::RegisterWatchRequest {
                outpoint,
                script_pubkey,
            } => {
                log::info!("Intercepted register watch request: {outpoint}");
                let req = WatchRequest {
                    outpoint,
                    in_block: false,
                    spent_tx: None,
                };
                self.registry.upsert_watch(&req);
                // Caller-supplied SPK: no network call on the hot path. A
                // failed subscribe just degrades to "no spend notifications
                // until next-block poll" — funds are still safe via timelock
                // recovery.
                if let Err(e) = self.backend.subscribe_script(&script_pubkey) {
                    log::warn!("electrum script-subscribe failed for {outpoint}: {e}");
                }
            }
            WatcherCommand::WatchRequest { outpoint } => {
                log::info!("Intercepted watch request: {outpoint}");
                let watches = self.registry.list_watches();
                let mut spent = false;
                for watch in watches {
                    if watch.outpoint == outpoint {
                        spent = true;
                        _ = self.tx_events.send(WatcherEvent::UtxoSpent {
                            outpoint: watch.outpoint,
                            spending_tx: watch.spent_tx,
                        });
                    }
                }
                if !spent {
                    _ = self.tx_events.send(WatcherEvent::NoOutpoint);
                }
            }
            WatcherCommand::Unwatch {
                outpoint,
                script_pubkey,
            } => {
                log::info!("Intercepted unwatch request : {outpoint}");
                self.registry.remove_watch(outpoint);
                // Drop the Electrum subscription. Local-only work: removes
                // from the in-memory map and best-effort tells the server.
                if let Err(e) = self.backend.unsubscribe_script(&script_pubkey) {
                    log::warn!("electrum script-unsubscribe failed for {outpoint}: {e}");
                }
            }
            WatcherCommand::Shutdown => return false,
        }
        true
    }

    /// Fetch the funding tx for `op` via the Electrum pool and return its
    /// `vout`'s scriptPubKey. Used to register per-script subscriptions so
    /// future spends of the outpoint surface as `TxSeen` events.
    fn lookup_outpoint_spk(url: &str, op: OutPoint) -> Result<ScriptBuf, WatcherError> {
        let tx = electrum_get_raw_tx(url, &op.txid)?;
        tx.output
            .get(op.vout as usize)
            .map(|o| o.script_pubkey.clone())
            .ok_or_else(|| {
                WatcherError::General(format!(
                    "lookup_outpoint_spk: vout {} out of bounds for {}",
                    op.vout, op.txid
                ))
            })
    }

    /// Handles a backend event, updating registry state and checkpoints.
    #[hotpath::measure]
    pub fn handle_event(&mut self, ev: BackendEvent) {
        match ev {
            BackendEvent::TxSeen { raw_tx } => {
                if let Ok(tx) = deserialize::<Transaction>(&raw_tx) {
                    process_transaction(&tx, &mut self.registry, false);
                }
            }
            BackendEvent::BlockConnected(b) => {
                // ZMQ delivers the full serialized block (~MB); Electrum's
                // `headers.subscribe` delivers only the 32-byte block hash.
                // Discriminate on payload length so the checkpoint advances
                // on either backend and per-tx `process_block` runs whenever
                // we actually have the block bytes.
                if b.hash.len() == 32 {
                    let mut bytes = [0u8; 32];
                    bytes.copy_from_slice(&b.hash);
                    self.registry.save_checkpoint(Checkpoint {
                        height: b.height,
                        hash: BlockHash::from_byte_array(bytes),
                    });
                } else if let Ok(block) = deserialize::<Block>(&b.hash) {
                    if let Ok(height) = block.bip34_block_height() {
                        self.registry.save_checkpoint(Checkpoint {
                            height,
                            hash: block.block_hash(),
                        });
                    }
                    process_block::<R>(block, &mut self.registry);
                }
            }
        }
    }
}
