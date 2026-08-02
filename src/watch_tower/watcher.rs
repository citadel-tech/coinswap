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
};

use bitcoin::{
    consensus::deserialize, hashes::Hash, Block, BlockHash, Network, OutPoint, ScriptBuf,
    Transaction,
};
use crossbeam_channel::Sender as CbSender;

use crate::{
    nostr,
    utill::HEART_BEAT_INTERVAL,
    wallet::{blockchain::WatchEvent, AnyBlockchain, Blockchain},
    watch_tower::{
        registry_storage::{Checkpoint, FileRegistry, WatchRequest},
        utils::{process_block, process_transaction},
        watcher_error::WatcherError,
    },
};

/// Describes watcher behavior.
pub trait Role {
    /// Enables or disables discovery.
    const RUN_DISCOVERY: bool;
}

/// Drives the watchtower event loop, coordinating backend events and client commands.
pub struct Watcher<R: Role> {
    blockchain: AnyBlockchain,
    registry: FileRegistry,
    rx_requests: StdReceiver<WatcherCommand>,
    tx_events: CbSender<WatcherEvent>,
    nostr_relays: Vec<String>,
    nostr_tor_config: Option<(u16, String)>,
    /// Watches whose script-subscribe failed; without a retry they stay
    /// undetected until restart. Retried on the heartbeat tick.
    pending_subscribes: Vec<(OutPoint, ScriptBuf)>,
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
        blockchain: AnyBlockchain,
        registry: FileRegistry,
        rx_requests: StdReceiver<WatcherCommand>,
        tx_events: CbSender<WatcherEvent>,
        nostr_relays: Vec<String>,
        nostr_tor_config: Option<(u16, String)>,
    ) -> Self {
        Self {
            blockchain,
            registry,
            rx_requests,
            tx_events,
            nostr_relays,
            nostr_tor_config,
            pending_subscribes: Vec::new(),
            _role: PhantomData,
        }
    }

    /// Runs the watcher loop: handles ZMQ events and commands, optionally spawning discovery.
    pub fn run(&mut self, initial_sync_complete: Arc<AtomicBool>) -> Result<(), WatcherError> {
        log::info!("Watcher initiated");

        // Detect network from the chain name.
        let network = match self.blockchain.chain_name()?.as_str() {
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

        // Establish the Core ZMQ Connection.
        if let AnyBlockchain::CoreRPC(core) = &self.blockchain {
            if let Err(e) = core.prime_subscription() {
                log::warn!("Failed to prime ZMQ subscription on startup: {e}");
            }
        }

        // Startup catch-up
        // Core: Process the node's mempool.
        if let Err(e) = self.process_mempool() {
            log::warn!("Failed to process mempool on startup: {e}");
        }
        // Electrum: re-subscribe to all the watched scripts.
        if self.blockchain.is_electrum() {
            for mut watch in self.registry.list_watches()? {
                if watch.spent_tx.is_some() {
                    continue;
                }
                let spk = match watch.script_pubkey.clone() {
                    Some(spk) => spk,
                    None => match self
                        .blockchain
                        .get_raw_transaction(&watch.outpoint.txid, None)
                    {
                        Ok(tx) => match tx.output.get(watch.outpoint.vout as usize) {
                            Some(txout) => {
                                let spk = txout.script_pubkey.clone();
                                // rewrite old watch request for restarts. Critical for the electrum path.
                                watch.script_pubkey = Some(spk.clone());
                                self.registry.upsert_watch(&watch)?;
                                spk
                            }
                            None => {
                                log::warn!(
                                    "vout {} out of bounds for {}",
                                    watch.outpoint.vout,
                                    watch.outpoint.txid
                                );
                                continue;
                            }
                        },
                        Err(e) => {
                            log::error!(
                                "could not resolve SPK for persisted watch {}: {e:?}",
                                watch.outpoint
                            );
                            continue;
                        }
                    },
                };
                if let Err(e) = self.blockchain.subscribe_script(&spk) {
                    log::error!(
                        "re-subscribe failed for persisted watch {}: {e:?}",
                        watch.outpoint
                    );
                    self.pending_subscribes.push((watch.outpoint, spk));
                }
            }
        }
        #[cfg(debug_assertions)]
        log::debug!(
            "[WATCH_STATE] Source: watch_tower::watcher::run | Action: watcher_ready | Network: {} | ActiveWatches: {} | Checkpoint: {:?} | Discovery: {}",
            network,
            self.registry.list_watches()?.len(),
            self.registry.load_checkpoint()?.map(|cp| cp.height),
            R::RUN_DISCOVERY
        );

        let discovery_shutdown = Arc::new(AtomicBool::new(false));
        let registry = self.registry.clone();
        let nostr_relays = self.nostr_relays.clone();
        let nostr_tor_config = self.nostr_tor_config.clone();
        std::thread::scope(move |s| -> Result<(), WatcherError> {
            let shutdown_clone = discovery_shutdown.clone();
            let mut discovery_handle = None;
            if R::RUN_DISCOVERY {
                if let Some(nostr_tor_config) = nostr_tor_config {
                    // Discovery requires it's own dedicated backend to not overlap with regular watch requests.
                    let chain = self.blockchain.new_connection()?;
                    // The thread runs until shutdown, so its outcome is only
                    // known at the join below.
                    discovery_handle = Some(s.spawn(move || {
                        let result = nostr::run_discovery(
                            chain,
                            network,
                            registry,
                            discovery_shutdown.clone(),
                            initial_sync_complete,
                            &nostr_relays,
                            nostr_tor_config,
                        );
                        if let Err(e) = &result {
                            log::error!("Discovery thread failed: {e:?}");
                        }
                        result
                    }));
                }
            }
            loop {
                match self.rx_requests.try_recv() {
                    Ok(cmd) => {
                        if !self.handle_command(cmd) {
                            break;
                        }
                    }
                    Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }

                if let Some(event) = self.blockchain.poll_event() {
                    self.handle_event(event);
                } else {
                    // Avoid busy-looping when there are no commands and no events.
                    // Loop at system's heartbeat.
                    std::thread::sleep(HEART_BEAT_INTERVAL);
                }
                // A failed script-subscribe retries on every pass, not just
                // idle ticks; with an empty queue this is a no-op.
                self.retry_subscribes();
            }

            // Stop and join the discovery thread on exit path.
            shutdown_clone.store(true, Ordering::SeqCst);
            if let Some(handle) = discovery_handle {
                // join() nests two results: the panic payload, then discovery's own error.
                // The first `?` surfaces a panic, the second discovery's WatcherError.
                handle.join().map_err(|payload| {
                    // pull out the panic message by downcast.
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown payload".to_string());
                    WatcherError::General(format!("Discovery thread panicked: {msg}"))
                })??;
            }
            Ok(())
        })
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
                if let Err(e) = self.registry.upsert_watch(&req) {
                    log::error!("registry lock poisoned, watch not stored: {e:?}");
                }
                if let Err(e) = self.blockchain.subscribe_script(&script_pubkey) {
                    log::error!("electrum script-subscribe failed for {outpoint}: {e}");
                    self.pending_subscribes.push((outpoint, script_pubkey));
                }
            }
            WatcherCommand::WatchRequest { outpoint } => {
                log::info!("Intercepted watch request: {outpoint}");
                let watches = match self.registry.list_watches() {
                    Ok(watches) => watches,
                    Err(e) => {
                        log::error!("registry lock poisoned, cannot answer watch request: {e:?}");
                        return true;
                    }
                };
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
                if let Err(e) = self.registry.remove_watch(outpoint) {
                    log::error!("registry lock poisoned, watch not removed: {e:?}");
                }
                self.pending_subscribes.retain(|(o, _)| *o != outpoint);
                // Drop the Electrum subscription.
                if let Err(e) = self.blockchain.unsubscribe_script(&script_pubkey) {
                    log::warn!("electrum script-unsubscribe failed for {outpoint}: {e}");
                }
            }
            WatcherCommand::Shutdown => return false,
        }
        true
    }

    /// Retry script-subscribes that failed earlier. The backend records a
    /// subscription only on success, so retrying an armed script is a no-op.
    fn retry_subscribes(&mut self) {
        // Destructured so the closure can call the backend while `retain`
        // holds the vec; borrowing `self` twice would not compile.
        let Self {
            blockchain,
            pending_subscribes,
            ..
        } = self;
        pending_subscribes.retain(|(outpoint, spk)| match blockchain.subscribe_script(spk) {
            Ok(()) => {
                log::info!("re-subscribe succeeded for {outpoint}");
                false
            }
            Err(e) => {
                log::warn!("re-subscribe still failing for {outpoint}: {e}");
                true
            }
        });
    }

    /// Scan the node mempool into the registry.
    fn process_mempool(&mut self) -> Result<(), WatcherError> {
        let txids = self
            .blockchain
            .get_raw_mempool()
            .map_err(WatcherError::from)?;
        for txid in &txids {
            let tx = match self.blockchain.get_raw_transaction(txid, None) {
                Ok(tx) => tx,
                Err(e) => {
                    // There can be lot of txs in mempool.
                    // failing one fetch should not abort the scan.
                    log::error!("could not fetch mempool tx {txid}: {e:?}");
                    continue;
                }
            };
            process_transaction(&tx, &mut self.registry, false)?;
        }
        Ok(())
    }

    /// Handles a backend event, updating registry state and checkpoints.
    pub fn handle_event(&mut self, ev: WatchEvent) {
        match ev {
            WatchEvent::TxSeen { raw_tx } => {
                if let Ok(tx) = deserialize::<Transaction>(&raw_tx) {
                    if let Err(e) = process_transaction(&tx, &mut self.registry, false) {
                        log::error!("registry update failed for mempool tx: {e:?}");
                    }
                }
            }
            WatchEvent::BlockConnected(b) => {
                // ZMQ ships full block bytes; Electrum ships just the 32-byte hash.
                if b.hash.len() == 32 {
                    let mut bytes = [0u8; 32];
                    bytes.copy_from_slice(&b.hash);
                    if let Err(e) = self.registry.save_checkpoint(Checkpoint {
                        height: b.height,
                        hash: BlockHash::from_byte_array(bytes),
                    }) {
                        log::error!("registry lock poisoned, checkpoint not saved: {e:?}");
                    }
                } else if let Ok(block) = deserialize::<Block>(&b.hash) {
                    if let Ok(height) = block.bip34_block_height() {
                        if let Err(e) = self.registry.save_checkpoint(Checkpoint {
                            height,
                            hash: block.block_hash(),
                        }) {
                            log::error!("registry lock poisoned, checkpoint not saved: {e:?}");
                        }
                    }
                    if let Err(e) = process_block::<R>(block, &mut self.registry) {
                        log::error!("registry update failed for connected block: {e:?}");
                    }
                }
            }
        }
    }
}
