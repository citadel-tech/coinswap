//! Public watchtower service for sending commands to and receiving events from the watcher.

use bitcoin::{OutPoint, ScriptBuf};
use crossbeam_channel::{unbounded, Receiver as CbReceiver};
use std::{
    path::Path,
    sync::{
        atomic::AtomicBool,
        mpsc::{self, Sender as StdSender},
        Arc,
    },
    thread,
};

use crate::{
    wallet::RPCConfig,
    watch_tower::{
        registry_storage::FileRegistry,
        rest_backend::{electrum_chain_name, BitcoinRest},
        watcher::{Role, Watcher, WatcherCommand, WatcherEvent},
        watcher_error::WatcherError,
        zmq_backend::{ElectrumNotifier, NotificationBackend, ZmqBackend},
    },
};

/// Marker type for the Maker role in the watchtower.
pub struct MakerRole;

impl Role for MakerRole {
    const RUN_DISCOVERY: bool = false;
}

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

    /// Registers an outpoint to be monitored for future spends. The caller
    /// supplies the outpoint's `scriptPubKey` so the watcher's hot loop never
    /// does a network fetch — callers always have the parent tx in scope when
    /// they decide to start watching one of its outputs.
    pub fn register_watch_request(&self, outpoint: OutPoint, script_pubkey: ScriptBuf) {
        let _ = self.tx.send(WatcherCommand::RegisterWatchRequest {
            outpoint,
            script_pubkey,
        });
    }

    /// Queries whether a previously registered outpoint has been spent.
    pub fn watch_request(&self, outpoint: OutPoint) {
        let _ = self.tx.send(WatcherCommand::WatchRequest { outpoint });
    }

    /// Stops monitoring an outpoint by removing its watch entry from the
    /// registry. The `scriptPubKey` lets the watcher drop the Electrum
    /// subscription too without re-resolving it from the network.
    pub fn unwatch(&self, outpoint: OutPoint, script_pubkey: ScriptBuf) {
        let _ = self.tx.send(WatcherCommand::Unwatch {
            outpoint,
            script_pubkey,
        });
    }

    /// Attempts a non-blocking receive; returns `None` if no event is pending.
    pub fn poll_event(&self) -> Option<WatcherEvent> {
        self.rx.try_recv().ok()
    }

    /// Blocks until the next watcher event arrives.
    pub fn wait_for_event(&self) -> Option<WatcherEvent> {
        self.rx.recv().ok()
    }

    /// Signals the watcher to shut down gracefully.
    pub fn shutdown(&self) {
        let _ = self.tx.send(WatcherCommand::Shutdown);
    }
}

/// Starts the Maker Watch Service.
///
/// Exactly one of `rpc_config` / `electrum_url` should be provided.
/// `zmq_addr` is consulted only in the Bitcoin Core branch.
#[hotpath::measure]
pub fn start_maker_watch_service(
    zmq_addr: &str,
    rpc_config: Option<&RPCConfig>,
    data_dir: &Path,
    network_port: u16,
    electrum_url: Option<&str>,
) -> Result<WatchService, WatcherError> {
    // Notification backend + chain name for registry path.
    let (backend, chain_name) = if let Some(url) = electrum_url {
        let notif = NotificationBackend::Electrum(Box::new(
            ElectrumNotifier::new(url)
                .map_err(|e| WatcherError::General(format!("electrum notifier: {e:?}")))?,
        ));
        (notif, electrum_chain_name(url)?)
    } else {
        let rpc = rpc_config.ok_or_else(|| {
            WatcherError::General(
                "start_maker_watch_service: RPCConfig required when not in electrum mode".into(),
            )
        })?;
        let notif = NotificationBackend::Zmq(ZmqBackend::new(zmq_addr));
        let info = BitcoinRest::new(rpc.clone())?.get_blockchain_info()?;
        (notif, info.chain.to_string())
    };

    // Registry
    let file_registry = data_dir
        .join(format!(".maker_{}_watcher", network_port))
        .join(chain_name);
    let registry = FileRegistry::load(file_registry);

    // Channels
    let (tx_requests, rx_requests) = mpsc::channel();
    let (tx_events, rx_responses) = unbounded();

    // Watcher
    let rpc_config_watcher = rpc_config.cloned();
    let mut watcher = Watcher::<MakerRole>::new(
        backend,
        registry,
        rx_requests,
        tx_events,
        Vec::new(),
        None,
        electrum_url.map(str::to_string),
    );

    // Makers don't run discovery, so pass an already-complete flag.
    thread::Builder::new()
        .name("Watcher thread".to_string())
        .spawn(move || watcher.run(rpc_config_watcher, Arc::new(AtomicBool::new(true))))
        .expect("failed to spawn watcher thread");

    Ok(WatchService::new(tx_requests, rx_responses))
}
