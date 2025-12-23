//! Public watchtower service for sending commands to and receiving events from the watcher.

use bitcoin::OutPoint;
use std::sync::{
    mpsc::{Receiver, Sender},
    Arc, Mutex,
};

use crate::watch_tower::watcher::{WatcherCommand, WatcherEvent};

/// Client-facing service for sending watcher commands and receiving events.
pub struct WatchService {
    tx: Sender<WatcherCommand>,
    rx: Arc<Mutex<Receiver<WatcherEvent>>>,
}

impl WatchService {
    /// Creates a new service from the given command sender and event receiver.
    pub fn new(tx: Sender<WatcherCommand>, rx: Receiver<WatcherEvent>) -> Self {
        Self {
            tx,
            rx: Arc::new(Mutex::new(rx)),
        }
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
        self.rx.lock().ok()?.try_recv().ok()
    }

    /// Blocks until the next watcher event arrives.
    pub fn wait_for_event(&self) -> Option<WatcherEvent> {
        self.rx.lock().ok()?.recv().ok()
    }

    /// Requests the list of maker addresses.
    pub fn request_maker_address(&self) {
        _ = self.tx.send(WatcherCommand::MakerAddress);
    }

    /// Signals the watcher to shut down gracefully.
    pub fn shutdown(&self) {
        let _ = self.tx.send(WatcherCommand::Shutdown);
    }
}
