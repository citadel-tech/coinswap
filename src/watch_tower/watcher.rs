use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use bitcoin::{consensus::deserialize, Block, OutPoint, Transaction};

use crate::watch_tower::{
    registry_storage::{Checkpoint, FileRegistry, WatchRequest},
    rpc_backend::BitcoinRpc,
    utils::{process_block, process_transaction},
    watcher_error::WatcherError,
    zmq_backend::{BackendEvent, ZmqBackend},
};

pub struct Watcher {
    backend: ZmqBackend,
    rpc_backend: BitcoinRpc,
    registry: FileRegistry,
    rx_requests: Receiver<WatcherCommand>,
    tx_events: Sender<WatcherEvent>,
}

#[derive(Debug, Clone)]
pub enum WatcherEvent {
    UtxoSpent {
        outpoint: OutPoint,
        spending_tx: Option<Transaction>,
    },
    MakerAddresses {
        maker_addresses: Vec<String>,
    },
    NoOutpoint,
}

#[derive(Debug, Clone)]
pub enum WatcherCommand {
    RegisterWatchRequest { outpoint: OutPoint },
    WatchRequest { outpoint: OutPoint },
    Unwatch { outpoint: OutPoint },
    MakerAddress,
    Shutdown,
}

impl Watcher {
    pub fn new(
        backend: ZmqBackend,
        rpc_backend: BitcoinRpc,
        registry: FileRegistry,
        rx_requests: Receiver<WatcherCommand>,
        tx_events: Sender<WatcherEvent>,
    ) -> Self {
        Self {
            backend,
            rpc_backend,
            registry,
            rx_requests,
            tx_events,
        }
    }

    pub fn run(&mut self) -> Result<(), WatcherError> {
        log::info!("Watcher initiated");
        log::info!("Starting with recovery");
        self.rpc_backend.run_recovery(&mut self.registry)?;
        log::info!("recovery completed");
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

            if let Some(event) = self.backend.poll() {
                self.handle_event(event);
            }
        }
        Ok(())
    }

    fn handle_command(&mut self, cmd: WatcherCommand) -> bool {
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
                log::info!("Intercepted unwatch : {outpoint}");
                self.registry.remove_watch(outpoint);
            }
            WatcherCommand::MakerAddress => {
                log::info!("Intercepted maker address");
                let maker_addresses: Vec<String> = self
                    .registry
                    .list_fidelity()
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
                    process_block(block, &mut self.registry);
                }
            }
        }
    }
}
