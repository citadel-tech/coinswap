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
    nostr,
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
            for watch in self.registry.list_watches() {
                if watch.spent_tx.is_some() {
                    continue;
                }
                let spk = match &watch.script_pubkey {
                    Some(spk) => spk.clone(),
                    None => match self
                        .blockchain
                        .get_raw_transaction(&watch.outpoint.txid, None)
                    {
                        Ok(tx) => match tx.output.get(watch.outpoint.vout as usize) {
                            Some(txout) => txout.script_pubkey.clone(),
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
                            log::warn!(
                                "could not resolve SPK for persisted watch {}: {e:?}",
                                watch.outpoint
                            );
                            continue;
                        }
                    },
                };
                if let Err(e) = self.blockchain.subscribe_script(&spk) {
                    log::warn!(
                        "re-subscribe failed for persisted watch {}: {e:?}",
                        watch.outpoint
                    );
                }
            }
        }
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
                    // Discovery requires it's own dedicated backend to not overlap with regular watch requests.
                    match self.blockchain.new_connection() {
                        Ok(chain) => {
                            s.spawn(move || {
                                if let Err(e) = nostr::run_discovery(
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
                        Err(e) => log::error!("Discovery backend build failed: {e:?}"),
                    }
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

                if let Some(event) = self.blockchain.poll_event() {
                    self.handle_event(event);
                } else {
                    // Avoid busy-looping when there are no commands and no events.
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
                if let Err(e) = self.blockchain.subscribe_script(&script_pubkey) {
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
                if let Err(e) = self.blockchain.unsubscribe_script(&script_pubkey) {
                    log::warn!("electrum script-unsubscribe failed for {outpoint}: {e}");
                }
            }
            WatcherCommand::Shutdown => return false,
        }
        true
    }

    /// Scan the node mempool into the registry.
    fn process_mempool(&mut self) -> Result<(), WatcherError> {
        let txids = self
            .blockchain
            .get_raw_mempool()
            .map_err(WatcherError::from)?;
        for txid in &txids {
            let tx = self
                .blockchain
                .get_raw_transaction(txid, None)
                .map_err(WatcherError::from)?;
            process_transaction(&tx, &mut self.registry, false);
        }
        Ok(())
    }

    /// Handles a backend event, updating registry state and checkpoints.
    pub fn handle_event(&mut self, ev: WatchEvent) {
        match ev {
            WatchEvent::TxSeen { raw_tx } => {
                if let Ok(tx) = deserialize::<Transaction>(&raw_tx) {
                    process_transaction(&tx, &mut self.registry, false);
                }
            }
            WatchEvent::BlockConnected(b) => {
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
