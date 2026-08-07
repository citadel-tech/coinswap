//! Public watchtower service for sending commands to and receiving events from the watcher.

use bitcoin::{OutPoint, ScriptBuf};
use crossbeam_channel::unbounded;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SendError, Sender as StdSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

use crate::{
    wallet::{AnyBlockchain, BackendConfig},
    watch_tower::{
        registry_storage::FileRegistry,
        watcher::{Role, Watcher, WatcherCommand, WatcherEvent},
        watcher_error::WatcherError,
    },
};

/// Watcher thread handle, shared because [`WatchService`] is `Clone`.
/// The `Option` is taken by whichever clone shuts down first.
type WatcherHandle = Arc<Mutex<Option<JoinHandle<Result<(), WatcherError>>>>>;

/// Marker type for the Maker role in the watchtower.
pub struct MakerRole;

impl Role for MakerRole {
    const RUN_DISCOVERY: bool = false;
}

/// Client-facing service for sending watcher commands and receiving events.
#[derive(Clone)]
pub struct WatchService {
    tx: StdSender<WatcherCommand>,
    /// Watcher thread handle, joined on shutdown so its exit result is not lost.
    handle: WatcherHandle,
    /// Role flag checked by the watcher and every Electrum connection it owns.
    watcher_shutdown: Arc<AtomicBool>,
}

impl WatchService {
    /// Creates a service whose watcher and backend share the role shutdown flag.
    pub fn new(
        tx: StdSender<WatcherCommand>,
        handle: JoinHandle<Result<(), WatcherError>>,
        watcher_shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tx,
            handle: Arc::new(Mutex::new(Some(handle))),
            watcher_shutdown,
        }
    }

    /// Registers an outpoint to be monitored for future spends.
    /// Errs if the watcher thread is gone.
    pub fn register_watch_request(
        &self,
        outpoint: OutPoint,
        script_pubkey: ScriptBuf,
    ) -> Result<(), SendError<WatcherCommand>> {
        self.tx.send(WatcherCommand::RegisterWatchRequest {
            outpoint,
            script_pubkey,
        })
    }

    /// Re-arms the watches for every live contract in the wallet. The registry
    /// is memory-only, so a restart begins with nothing watched.
    /// Errs if the watcher thread is gone.
    pub fn rebuild_watches(
        &self,
        watches: Vec<(OutPoint, ScriptBuf)>,
    ) -> Result<(), SendError<WatcherCommand>> {
        self.tx.send(WatcherCommand::RebuildWatches { watches })
    }

    /// Queries whether a previously registered outpoint has been spent and
    /// waits for this query's own reply. The private channel is what stops
    /// concurrent callers from consuming each other's answers.
    /// `None` means the watcher did not answer within a heartbeat, so callers
    /// keep re-checking their shutdown flags when it is wedged.
    /// Errs if the watcher thread is gone.
    pub fn watch_request(
        &self,
        outpoint: OutPoint,
    ) -> Result<Option<WatcherEvent>, SendError<WatcherCommand>> {
        let (reply_tx, reply_rx) = unbounded();
        self.tx.send(WatcherCommand::WatchRequest {
            outpoint,
            reply: reply_tx,
        })?;
        Ok(reply_rx
            .recv_timeout(crate::utill::HEART_BEAT_INTERVAL)
            .ok())
    }

    /// Stops monitoring an outpoint by removing its watch entry from the
    /// registry. The `scriptPubKey` lets the watcher drop the Electrum
    /// subscription too without re-resolving it from the network.
    /// Errs if the watcher thread is gone.
    pub fn unwatch(
        &self,
        outpoint: OutPoint,
        script_pubkey: ScriptBuf,
    ) -> Result<(), SendError<WatcherCommand>> {
        self.tx.send(WatcherCommand::Unwatch {
            outpoint,
            script_pubkey,
        })
    }

    /// Signals the watcher to shut down gracefully and joins its thread,
    /// aborting any Electrum retry backoff it may be stuck in.
    pub fn shutdown(&self) {
        let _ = self.tx.send(WatcherCommand::Shutdown);
        self.watcher_shutdown.store(true, Ordering::Relaxed);
        let handle = self.handle.lock().ok().and_then(|mut h| h.take());
        if let Some(handle) = handle {
            let thread = handle.thread().clone();
            crate::utill::log_shutdown_join_start("watch_service", &thread);
            let result = handle.join();
            let outcome = match &result {
                Ok(Ok(())) => "ok",
                Ok(Err(_)) => "error",
                Err(_) => "panic",
            };
            crate::utill::log_shutdown_join_done("watch_service", &thread, outcome);
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log::error!("watcher thread exited with error: {e:?}"),
                Err(_) => log::error!("watcher thread panicked"),
            }
        }
    }
}

/// Shares the Maker's stop flag with its watcher and backend.
/// This keeps teardown from waiting through backend retries.
pub fn start_maker_watch_service(
    backend: &BackendConfig,
    shutdown: Arc<AtomicBool>,
) -> Result<WatchService, WatcherError> {
    let blockchain =
        AnyBlockchain::from_config_with_shutdown(&backend.for_watcher(), shutdown.clone())?;
    let registry = FileRegistry::new();

    // Channels
    let (tx_requests, rx_requests) = mpsc::channel();
    let mut watcher = Watcher::<MakerRole>::new(
        blockchain,
        registry,
        rx_requests,
        Vec::new(),
        None,
        shutdown.clone(),
    );

    // Makers don't run discovery, so pass an already-complete flag.
    let handle = thread::Builder::new()
        .name("Watcher thread".to_string())
        .spawn(move || watcher.run(Arc::new(AtomicBool::new(true))))?;

    Ok(WatchService::new(tx_requests, handle, shutdown))
}
