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

use crate::watch_tower::{
    nostr_discovery,
    registry_storage::{Checkpoint, FileRegistry, WatchRequest},
    utils::{process_block, process_transaction},
    watcher_error::WatcherError,
    zmq_backend::{BackendEvent, ChainSource, NotificationBackend},
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
    chain: ChainSource,
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

/// Commands accepted by the watcher from clients. Register/Unwatch carry
/// `script_pubkey` so the Electrum backend can arm/drop its per-script
/// subscription without re-fetching the funding tx from the network.
#[derive(Debug, Clone)]
pub enum WatcherCommand {
    /// Store a new watch request.
    RegisterWatchRequest {
        /// Outpoint to begin tracking.
        outpoint: OutPoint,
        /// `scriptPubKey` of the outpoint, used to arm the Electrum per-script subscription.
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
        /// `scriptPubKey` of the outpoint, used to drop the Electrum subscription.
        script_pubkey: ScriptBuf,
    },
    /// Terminate the watcher loop.
    Shutdown,
}

impl<R: Role> Watcher<R> {
    /// Creates a watcher with its backend, registry, and communication channels.
    pub fn new(
        backend: NotificationBackend,
        registry: FileRegistry,
        rx_requests: StdReceiver<WatcherCommand>,
        tx_events: CbSender<WatcherEvent>,
        nostr_relays: Vec<String>,
        nostr_tor_config: Option<(u16, String)>,
        chain: ChainSource,
    ) -> Self {
        Self {
            backend,
            registry,
            rx_requests,
            tx_events,
            nostr_relays,
            nostr_tor_config,
            chain,
            _role: PhantomData,
        }
    }

    /// Runs the watcher loop: handles ZMQ events and commands, optionally spawning discovery.
    pub fn run(&mut self, initial_sync_complete: Arc<AtomicBool>) -> Result<(), WatcherError> {
        log::info!("Watcher initiated");

        // Detect network from the chain name.
        let network = match self.chain.chain_name()?.as_str() {
            "main" => Network::Bitcoin,
            "test" => Network::Testnet,
            "testnet4" => Network::Testnet4,
            "signet" => Network::Signet,
            "regtest" => Network::Regtest,
            unknown => {
                return Err(WatcherError::General(format!(
                    "Unsupported Bitcoin network: {unknown}"
                )))
            }
        };
        match &self.chain {
            ChainSource::Rest(rest) => {
                if let Err(e) = rest.process_mempool(&mut self.registry) {
                    log::warn!("Failed to process mempool on startup: {}", e);
                }
            }
            ChainSource::Electrum(_) => {
                log::debug!("Watcher: electrum mode, skipping mempool scan");
                // Re-subscribe each persisted watch so a maker that crashed mid-swap regains spend-detection after restart.
                let watches = self.registry.list_watches();
                for watch in watches {
                    if watch.spent_tx.is_some() {
                        continue;
                    }
                    match self.script_pubkey_for_watch(&watch) {
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
            }
        }
        let chain = self.chain.clone();

        #[cfg(debug_assertions)]
        log::debug!(
            "[WATCH_STATE] Source: watch_tower::watcher::run | Action: watcher_ready | Network: {} | ActiveWatches: {} | Checkpoint: {:?} | Discovery: {}",
            network,
            self.registry.list_watches().len(),
            self.registry.load_checkpoint().map(|cp| cp.height),
            R::RUN_DISCOVERY
        );
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
                            chain,
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

    fn handle_command(&mut self, cmd: WatcherCommand) -> bool {
        match cmd {
            WatcherCommand::RegisterWatchRequest {
                outpoint,
                script_pubkey,
            } => {
                log::info!("Intercepted register watch request: {outpoint}");
                let req = WatchRequest {
                    outpoint,
                    script_pubkey: Some(script_pubkey.clone()),
                    in_block: false,
                    spent_tx: None,
                };
                self.registry.upsert_watch(&req);
                // A failed subscribe just degrades to "no spend notifications until next-block poll" — funds are still safe via timelock recovery.
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
                // Drop the Electrum subscription.
                if let Err(e) = self.backend.unsubscribe_script(&script_pubkey) {
                    log::warn!("electrum script-unsubscribe failed for {outpoint}: {e}");
                }
            }
            WatcherCommand::Shutdown => return false,
        }
        true
    }

    fn script_pubkey_for_watch(&self, watch: &WatchRequest) -> Result<ScriptBuf, WatcherError> {
        if let Some(spk) = watch.script_pubkey.as_ref() {
            return Ok(spk.clone());
        }
        self.chain.get_raw_tx(&watch.outpoint.txid).and_then(|tx| {
            tx.output
                .get(watch.outpoint.vout as usize)
                .map(|o| o.script_pubkey.clone())
                .ok_or_else(|| {
                    WatcherError::General(format!(
                        "vout {} out of bounds for {}",
                        watch.outpoint.vout, watch.outpoint.txid
                    ))
                })
        })
    }

    /// Handles a backend event, updating registry state and checkpoints.
    pub fn handle_event(&mut self, ev: BackendEvent) {
        match ev {
            BackendEvent::TxSeen { raw_tx } => {
                if let Ok(tx) = deserialize::<Transaction>(&raw_tx) {
                    process_transaction(&tx, &mut self.registry, false);
                }
            }
            BackendEvent::BlockConnected(b) => {
                // ZMQ ships full block bytes; Electrum ships just the 32-byte hash.
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
