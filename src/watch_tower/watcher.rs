//! Watchtower watcher module.
//!
//! Runs the core event loop, processes watcher commands, reacts to ZMQ backend events,
//! spawns optional RPC-based discovery and updates the on-disk registry of watches and fidelity records.

use std::{
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, TryRecvError},
        Arc,
    },
};

use bitcoin::{consensus::deserialize, Block, OutPoint, Transaction};

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        nostr_discovery,
        registry_storage::{Checkpoint, FileRegistry, WatchRequest},
        rpc_backend::BitcoinRpc,
        utils::{process_block, process_transaction},
        watcher_error::WatcherError,
        zmq_backend::{BackendEvent, ZmqBackend},
    },
};

/// Describes watcher behavior.
pub trait Role {
    /// Enables or disables discovery.
    const RUN_DISCOVERY: bool;
}

/// Drives the watchtower event loop, coordinating backend events and client commands.
pub struct Watcher<R: Role> {
    backend: ZmqBackend,
    registry: FileRegistry,
    rx_requests: Receiver<WatcherCommand>,
    tx_events: Sender<WatcherEvent>,
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
    /// Maker addresses.
    MakerAddresses {
        /// All maker addresses currently recorded in the registry.
        maker_addresses: Vec<String>,
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
    },
    /// Ask for the current maker address list.
    MakerAddress,
    /// Terminate the watcher loop.
    Shutdown,
}

impl<R: Role> Watcher<R> {
    /// Creates a watcher with its backend, registry, and communication channels.
    pub fn new(
        backend: ZmqBackend,
        registry: FileRegistry,
        rx_requests: Receiver<WatcherCommand>,
        tx_events: Sender<WatcherEvent>,
    ) -> Self {
        Self {
            backend,
            registry,
            rx_requests,
            tx_events,
            _role: PhantomData,
        }
    }

    // #TODO: When watcher starts index the mempool, to check if watch request is present there
    //        or not.
    /// Runs the watcher loop: handles ZMQ events and commands, optionally spawning discovery.
    pub fn run(&mut self, rpc_config: RPCConfig) -> Result<(), WatcherError> {
        log::info!("Watcher initiated");
        let rpc_backend_1 = BitcoinRpc::new(rpc_config.clone())?;
        let rpc_backend_2 = BitcoinRpc::new(rpc_config)?;
        let discovery_shutdown = Arc::new(AtomicBool::new(false));
        let registry = self.registry.clone();
        std::thread::scope(move |s| {
            let discovery_clone = discovery_shutdown.clone();
            if R::RUN_DISCOVERY {
                s.spawn(move || {
                    if let Err(e) = nostr_discovery::run_discovery(
                        rpc_backend_1,
                        registry,
                        discovery_shutdown.clone(),
                    ) {
                        log::error!("Discovery thread failed: {:?}", e);
                    }
                });
            }
            loop {
                match self.rx_requests.try_recv() {
                    Ok(cmd) => {
                        if !self.handle_command(cmd, &rpc_backend_2) {
                            discovery_clone.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                    Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }

                if let Some(event) = self.backend.poll() {
                    self.handle_event(event);
                }
            }
        });
        Ok(())
    }

    fn handle_command(&mut self, cmd: WatcherCommand, rpc_backend: &BitcoinRpc) -> bool {
        match cmd {
            WatcherCommand::RegisterWatchRequest { outpoint } => {
                log::info!("Intercepted register watch request: {outpoint}");
                let req = WatchRequest {
                    outpoint,
                    in_block: false,
                    spent_tx: None,
                };
                self.registry.upsert_watch(&req);
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
            WatcherCommand::Unwatch { outpoint } => {
                log::info!("Intercepted unwatch request : {outpoint}");
                self.registry.remove_watch(outpoint);
            }
            WatcherCommand::MakerAddress => {
                log::debug!("Intercepted maker address request");
                let height = rpc_backend
                    .get_blockchain_info()
                    .ok()
                    .map(|v| v.blocks)
                    .unwrap_or(0);
                let maker_addresses: Vec<String> = self
                    .registry
                    .list_fidelity(height as u32)
                    .into_iter()
                    .map(|fidelity| fidelity.onion_address)
                    .collect();
                _ = self
                    .tx_events
                    .send(WatcherEvent::MakerAddresses { maker_addresses });
            }
            WatcherCommand::Shutdown => return false,
        }
        true
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
                if let Ok(block) = deserialize::<Block>(&b.hash) {
                    self.registry.save_checkpoint(Checkpoint {
                        height: block.bip34_block_height().unwrap(),
                        hash: block.block_hash(),
                    });
                    process_block::<R>(block, &mut self.registry);
                }
            }
        }
    }
}
