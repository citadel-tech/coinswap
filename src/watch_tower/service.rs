//! Public watchtower service for sending commands to and receiving events from the watcher.

use bitcoin::OutPoint;
use crossbeam_channel::{unbounded, Receiver as CbReceiver};
use std::{
    path::Path,
    sync::mpsc::{self, Sender as StdSender},
    thread,
    time::Duration,
};

use crate::{
    maker::Maker,
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::FileRegistry,
        rpc_backend::BitcoinRpc,
        watcher::{Watcher, WatcherCommand, WatcherEvent},
        watcher_error::WatcherError,
        zmq_backend::ZmqBackend,
    },
};

#[cfg(not(feature = "integration-test"))]
pub(crate) const DEFAULT_WATCHER_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(feature = "integration-test")]
pub(crate) const DEFAULT_WATCHER_TIMEOUT: Duration = Duration::from_millis(50);

#[cfg(not(feature = "integration-test"))]
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(feature = "integration-test")]
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(50);

/// Client-facing service for sending watcher commands and receiving events.
#[derive(Clone)]
pub struct WatchService {
    tx: StdSender<WatcherCommand>,
    rx: CbReceiver<WatcherEvent>,
}

impl WatchService {
    /// Creates a new service from the given command sender and event receiver.
    pub fn new(tx: StdSender<WatcherCommand>, rx: CbReceiver<WatcherEvent>) -> Self {
        Self { tx, rx }
    }

    /// Registers an outpoint to be monitored for future spends.
    pub fn register_watch_request(&self, outpoint: OutPoint) {
        let _ = self
            .tx
            .send(WatcherCommand::RegisterWatchRequest { outpoint });
    }

    /// Queries whether a previously registered outpoint has been spent.
    pub fn watch_request(&self, outpoint: OutPoint) {
        let _ = self.tx.send(WatcherCommand::WatchRequest { outpoint });
    }

    /// Stops monitoring an outpoint by removing its watch entry from the registry.
    pub fn unwatch(&self, outpoint: OutPoint) {
        let _ = self.tx.send(WatcherCommand::Unwatch { outpoint });
    }

    /// Attempts a non-blocking receive; returns `None` if no event is pending.
    pub fn poll_event(&self) -> Option<WatcherEvent> {
        self.rx.try_recv().ok()
    }

    /// Waits up to the default watcher timeout for the next watcher event.
    pub fn wait_for_event(&self) -> Option<WatcherEvent> {
        self.wait_for_event_timeout(DEFAULT_WATCHER_TIMEOUT)
    }

    /// Waits up to `timeout` for the next watcher event.
    pub fn wait_for_event_timeout(&self, timeout: Duration) -> Option<WatcherEvent> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Requests the list of maker addresses over a dedicated one-shot response channel.
    pub fn request_maker_address(&self) -> Option<Vec<String>> {
        let (response_tx, response_rx) = mpsc::channel();
        self.tx
            .send(WatcherCommand::MakerAddress { response_tx })
            .ok()?;

        match response_rx.recv_timeout(DEFAULT_REQUEST_TIMEOUT) {
            Ok(maker_addresses) => Some(maker_addresses),
            Err(e) => {
                log::warn!("Timed out waiting for maker addresses from watcher: {}", e);
                None
            }
        }
    }

    /// Signals the watcher to shut down gracefully.
    pub fn shutdown(&self) {
        let _ = self.tx.send(WatcherCommand::Shutdown);
    }
}

/// Starts the Maker Watch Service
pub fn start_maker_watch_service(
    zmq_addr: &str,
    rpc_config: &RPCConfig,
    data_dir: &Path,
    network_port: u16,
) -> Result<WatchService, WatcherError> {
    // Backends
    let backend = ZmqBackend::new(zmq_addr);
    let rpc_backend = BitcoinRpc::new(rpc_config.clone())?;
    let blockchain_info = rpc_backend.get_blockchain_info()?;

    // Registry
    let file_registry = data_dir
        .join(format!(".maker_{}_watcher", network_port))
        .join(blockchain_info.chain.to_string());
    let registry = FileRegistry::load(file_registry);

    // Channels
    let (tx_requests, rx_requests) = mpsc::channel();
    let (tx_events, rx_responses) = unbounded();

    // Watcher
    let rpc_config_watcher = rpc_config.clone();
    let mut watcher = Watcher::<Maker>::new(backend, registry, rx_requests, tx_events);

    thread::Builder::new()
        .name("Watcher thread".to_string())
        .spawn(move || watcher.run(rpc_config_watcher))
        .expect("failed to spawn watcher thread");

    Ok(WatchService::new(tx_requests, rx_responses))
}
