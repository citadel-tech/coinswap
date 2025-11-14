use bitcoin::OutPoint;
use std::sync::{
    mpsc::{Receiver, Sender},
    Arc, Mutex,
};

use crate::watch_tower::watcher::{WatcherCommand, WatcherEvent};

pub struct WatchService {
    tx: Sender<WatcherCommand>,
    rx: Arc<Mutex<Receiver<WatcherEvent>>>,
}

impl WatchService {
    pub fn new(tx: Sender<WatcherCommand>, rx: Receiver<WatcherEvent>) -> Self {
        Self {
            tx,
            rx: Arc::new(Mutex::new(rx)),
        }
    }

    pub fn register_watch_request(&self, outpoint: OutPoint) {
        let _ = self
            .tx
            .send(WatcherCommand::RegisterWatchRequest { outpoint });
    }

    pub fn watch_request(&self, outpoint: OutPoint) {
        let _ = self.tx.send(WatcherCommand::WatchRequest { outpoint });
    }

    pub fn unwatch(&self, outpoint: OutPoint) {
        let _ = self.tx.send(WatcherCommand::Unwatch { outpoint });
    }

    pub fn poll_event(&self) -> Option<WatcherEvent> {
        self.rx.lock().ok()?.try_recv().ok()
    }

    pub fn wait_for_event(&self) -> Option<WatcherEvent> {
        self.rx.lock().ok()?.recv().ok()
    }

    pub fn request_maker_address(&self) {
        _ = self.tx.send(WatcherCommand::MakerAddress);
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(WatcherCommand::Shutdown);
    }
}
