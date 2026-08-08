//! Public watchtower service for sending commands to and receiving events from the watcher.

use bitcoin::{OutPoint, ScriptBuf};
use crossbeam_channel::{unbounded, RecvTimeoutError};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SendError, Sender as StdSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    lock_debug,
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

/// Attempts per watch query before an unanswered watcher is treated as fatal.
const WATCH_REQUEST_ATTEMPTS: u32 = 3;

/// Reply wait per attempt. Short in tests so the retry test stays fast.
#[cfg(not(test))]
const WATCH_REPLY_TIMEOUT: Duration = crate::utill::HEART_BEAT_INTERVAL;
#[cfg(test)]
const WATCH_REPLY_TIMEOUT: Duration = Duration::from_millis(50);

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
    /// is memory-only, so a restart begins with nothing watched. Blocks until
    /// the rebuild (Core rescan included) replies, so startup never runs unwatched.
    pub fn rebuild_watches(&self, watches: Vec<(OutPoint, ScriptBuf)>) -> Result<(), WatcherError> {
        let (reply_tx, reply_rx) = unbounded();
        self.tx
            .send(WatcherCommand::RebuildWatches {
                watches,
                reply: reply_tx,
            })
            .map_err(|_| WatcherError::SendError)?;
        // No total cap: a deep Core rescan legitimately takes minutes. The loop
        // only exists so teardown can still cut the wait short.
        loop {
            match reply_rx.recv_timeout(WATCH_REPLY_TIMEOUT) {
                Ok(result) => return result.map_err(WatcherError::General),
                Err(RecvTimeoutError::Disconnected) => return Err(WatcherError::SendError),
                Err(RecvTimeoutError::Timeout) => {
                    if self.watcher_shutdown.load(Ordering::Relaxed) {
                        return Err(WatcherError::Shutdown);
                    }
                }
            }
        }
    }

    /// Queries whether a previously registered outpoint has been spent and
    /// waits for this query's own reply. The private channel is what stops
    /// concurrent callers from consuming each other's answers.
    /// The watcher answers every request, so silence means wedged or dead:
    /// retried, then fatal — never readable as "not spent".
    pub fn watch_request(&self, outpoint: OutPoint) -> Result<WatcherEvent, WatcherError> {
        for attempt in 1..=WATCH_REQUEST_ATTEMPTS {
            let (reply_tx, reply_rx) = unbounded();
            self.tx
                .send(WatcherCommand::WatchRequest {
                    outpoint,
                    reply: reply_tx,
                })
                .map_err(|_| WatcherError::SendError)?;
            match reply_rx.recv_timeout(WATCH_REPLY_TIMEOUT) {
                Ok(WatcherEvent::Error(error)) => {
                    // The watcher answered with an error; callers must not read
                    // it as "not spent".
                    return Err(WatcherError::General(format!(
                        "watcher could not answer request for {outpoint}: {error}"
                    )));
                }
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Disconnected) => return Err(WatcherError::SendError),
                Err(RecvTimeoutError::Timeout) => {
                    if self.watcher_shutdown.load(Ordering::Relaxed) {
                        // Teardown must not sit through the remaining attempts.
                        return Err(WatcherError::Shutdown);
                    }
                    log::warn!(
                        "watch request for {outpoint} unanswered (attempt {attempt}/{WATCH_REQUEST_ATTEMPTS})"
                    );
                }
            }
        }
        Err(WatcherError::General(format!(
            "watcher silent after {WATCH_REQUEST_ATTEMPTS} attempts"
        )))
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
        let handle = lock_debug!(self.handle.lock())
            .ok()
            .and_then(|mut h| h.take());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_handle() -> JoinHandle<Result<(), WatcherError>> {
        thread::spawn(|| Ok(()))
    }

    #[test]
    fn silent_watcher_gets_three_attempts_then_error() {
        let (tx, rx) = mpsc::channel();
        let service = WatchService::new(tx, dummy_handle(), Arc::new(AtomicBool::new(false)));
        let err = service.watch_request(OutPoint::null()).unwrap_err();
        assert!(matches!(err, WatcherError::General(_)));
        // The receiver never answered, so every attempt landed in the queue.
        assert_eq!(rx.try_iter().count(), WATCH_REQUEST_ATTEMPTS as usize);
    }

    #[test]
    fn shutdown_flag_cuts_retries_short() {
        let (tx, rx) = mpsc::channel();
        let service = WatchService::new(tx, dummy_handle(), Arc::new(AtomicBool::new(true)));
        let err = service.watch_request(OutPoint::null()).unwrap_err();
        assert!(matches!(err, WatcherError::Shutdown));
        assert!(rx.try_iter().count() < WATCH_REQUEST_ATTEMPTS as usize);
    }

    #[test]
    fn dead_watcher_errors_without_retry() {
        let (tx, rx) = mpsc::channel::<WatcherCommand>();
        drop(rx);
        let service = WatchService::new(tx, dummy_handle(), Arc::new(AtomicBool::new(false)));
        let err = service.watch_request(OutPoint::null()).unwrap_err();
        assert!(matches!(err, WatcherError::SendError));
    }

    #[test]
    fn answered_request_returns_event_without_retry() {
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            if let Ok(WatcherCommand::WatchRequest { reply, .. }) = rx.recv() {
                _ = reply.send(WatcherEvent::NoOutpoint);
            }
            Ok(())
        });
        let service = WatchService::new(tx, handle, Arc::new(AtomicBool::new(false)));
        let event = service.watch_request(OutPoint::null()).unwrap();
        assert!(matches!(event, WatcherEvent::NoOutpoint));
    }

    #[test]
    fn error_event_is_returned_as_error() {
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            if let Ok(WatcherCommand::WatchRequest { reply, .. }) = rx.recv() {
                _ = reply.send(WatcherEvent::Error("registry lock poisoned".to_string()));
            }
            Ok(())
        });
        let service = WatchService::new(tx, handle, Arc::new(AtomicBool::new(false)));
        let err = service.watch_request(OutPoint::null()).unwrap_err();
        assert!(matches!(err, WatcherError::General(_)));
    }

    #[test]
    fn rebuild_watches_blocks_until_reply() {
        let (tx, rx) = mpsc::channel();
        let (reply_hold_tx, reply_hold_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            if let Ok(WatcherCommand::RebuildWatches { reply, .. }) = rx.recv() {
                _ = reply_hold_tx.send(reply);
            }
            Ok(())
        });
        let service = WatchService::new(tx, handle, Arc::new(AtomicBool::new(false)));
        let (done_tx, done_rx) = mpsc::channel();
        let svc = service.clone();
        let waiter = thread::spawn(move || _ = done_tx.send(svc.rebuild_watches(Vec::new())));
        let reply = reply_hold_rx.recv().unwrap();
        // The watcher has the command but has not replied, so the caller must
        // still be blocked.
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        _ = reply.send(Ok(()));
        assert!(done_rx.recv().unwrap().is_ok());
        waiter.join().unwrap();
    }

    #[test]
    fn rebuild_watches_returns_shutdown_without_reply() {
        let (tx, _rx) = mpsc::channel();
        let service = WatchService::new(tx, dummy_handle(), Arc::new(AtomicBool::new(true)));
        let err = service.rebuild_watches(Vec::new()).unwrap_err();
        assert!(matches!(err, WatcherError::Shutdown));
    }
}
