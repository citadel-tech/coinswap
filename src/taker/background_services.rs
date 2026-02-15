//! Background service threads for the Unified Taker.
//!
//! Contains `RecoveryLoop` (periodic recovery retry) and `BreachDetector`
//! (adversarial spend monitoring) — standalone structs with their own
//! background threads, `Arc` state, and `Drop` impls.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc, Mutex, RwLock,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use bitcoin::OutPoint;
use bitcoind::bitcoincore_rpc::RpcApi;

use crate::{
    utill::HEART_BEAT_INTERVAL,
    wallet::Wallet,
    watch_tower::{service::WatchService, watcher::WatcherEvent},
};

/// Interval between recovery retry attempts.
#[cfg(not(feature = "integration-test"))]
const RECOVERY_LOOP_INTERVAL: Duration = Duration::from_secs(60);
#[cfg(feature = "integration-test")]
const RECOVERY_LOOP_INTERVAL: Duration = Duration::from_secs(10);

/// Background thread that periodically retries wallet-level recovery
/// (hashlock sweep + timelock recovery) until all contract UTXOs are resolved.
///
/// Spawned at the end of `recover_active_swap()` or `init_recover_incomplete()`
/// when some contracts remain unresolved (e.g. timelocks not yet mature).
pub(crate) struct RecoveryLoop {
    shutdown: Arc<AtomicBool>,
    complete: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RecoveryLoop {
    /// Spawn the background recovery thread.
    pub(crate) fn start(wallet: Arc<RwLock<Wallet>>) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let complete = Arc::new(AtomicBool::new(false));

        let shutdown_clone = shutdown.clone();
        let complete_clone = complete.clone();

        let handle = thread::Builder::new()
            .name("Recovery loop".to_string())
            .spawn(move || {
                log::info!("Recovery loop started");
                while !shutdown_clone.load(Relaxed) {
                    // Sync wallet to refresh chain state
                    if let Ok(mut w) = wallet.write() {
                        if let Err(e) = w.sync_and_save() {
                            log::warn!("Recovery loop: sync failed: {:?}", e);
                        }
                    }

                    // Try hashlock sweep (incoming)
                    if let Ok(mut w) = wallet.write() {
                        match w.sweep_unified_incoming_swapcoins(2.0) {
                            Ok(swept) if !swept.is_empty() => {
                                log::info!(
                                    "Recovery loop: swept {} incoming swapcoins",
                                    swept.len()
                                );
                            }
                            Err(e) => {
                                log::debug!("Recovery loop: incoming sweep: {:?}", e);
                            }
                            _ => {}
                        }
                    }

                    // Try timelock recovery (outgoing)
                    if let Ok(mut w) = wallet.write() {
                        match w.recover_unified_timelocked_swapcoins(2.0) {
                            Ok(recovered) if !recovered.is_empty() => {
                                log::info!(
                                    "Recovery loop: recovered {} timelocked swapcoins",
                                    recovered.len()
                                );
                            }
                            Err(e) => {
                                log::debug!("Recovery loop: timelock recovery: {:?}", e);
                            }
                            _ => {}
                        }
                    }

                    // Check if all contract outpoints are resolved
                    let all_resolved = match wallet.read() {
                        Ok(w) => {
                            let outgoing = w.unified_outgoing_contract_outpoints();
                            let incoming = w.unified_incoming_contract_outpoints();
                            if outgoing.is_empty() && incoming.is_empty() {
                                true
                            } else {
                                outgoing.iter().chain(incoming.iter()).all(|op| {
                                    !matches!(
                                        w.rpc.get_tx_out(&op.txid, op.vout, None),
                                        Ok(Some(_))
                                    )
                                })
                            }
                        }
                        Err(_) => false,
                    };

                    if all_resolved {
                        log::info!("Recovery loop: all contracts resolved");
                        complete_clone.store(true, Relaxed);
                        return;
                    }

                    thread::sleep(RECOVERY_LOOP_INTERVAL);
                }
                log::info!("Recovery loop shut down");
            })
            .expect("failed to spawn recovery loop thread");

        Self {
            shutdown,
            complete,
            handle: Some(handle),
        }
    }

    /// Check whether recovery is complete.
    pub(crate) fn is_complete(&self) -> bool {
        self.complete.load(Relaxed)
    }
}

impl Drop for RecoveryLoop {
    fn drop(&mut self) {
        self.shutdown.store(true, Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Background thread that monitors sentinel outpoints for adversarial spends
/// via the WatchService. Queries the watcher's file registry periodically;
/// the watcher's ZMQ backend captures spending transactions in real-time.
///
/// - **Legacy**: sentinels are funding outpoints — if spent, a contract tx was broadcast.
/// - **Taproot**: sentinels are contract outpoints — if spent during exchange, a script-path
///   spend occurred (adversarial since key-path settlement hasn't happened yet).
pub(crate) struct BreachDetector {
    breached: Arc<AtomicBool>,
    sentinels: Arc<Mutex<Vec<OutPoint>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BreachDetector {
    /// Spawn a background thread that polls the WatchService for sentinel spends.
    pub(crate) fn start(watch_service: WatchService) -> Self {
        let breached = Arc::new(AtomicBool::new(false));
        let sentinels: Arc<Mutex<Vec<OutPoint>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let breached_clone = breached.clone();
        let sentinels_clone = sentinels.clone();
        let shutdown_clone = shutdown.clone();

        let handle = thread::Builder::new()
            .name("Breach detector thread".to_string())
            .spawn(move || {
                while !shutdown_clone.load(Relaxed) {
                    thread::sleep(HEART_BEAT_INTERVAL);

                    let current_sentinels = match sentinels_clone.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => continue,
                    };

                    for sentinel in &current_sentinels {
                        watch_service.watch_request(*sentinel);
                        if let Some(WatcherEvent::UtxoSpent {
                            spending_tx: Some(_),
                            ..
                        }) = watch_service.wait_for_event()
                        {
                            log::warn!(
                                "Breach detector: adversarial spend on sentinel {}",
                                sentinel
                            );
                            breached_clone.store(true, Relaxed);
                            return;
                        }
                    }
                }
            })
            .expect("failed to spawn breach detector thread");

        Self {
            breached,
            sentinels,
            shutdown,
            handle: Some(handle),
        }
    }

    /// Register outpoints as sentinels with the WatchService and add to the monitor list.
    pub(crate) fn add_sentinels(&self, watch_service: &WatchService, outpoints: &[OutPoint]) {
        for outpoint in outpoints {
            watch_service.register_watch_request(*outpoint);
        }
        if let Ok(mut guard) = self.sentinels.lock() {
            guard.extend_from_slice(outpoints);
        }
    }

    /// Check whether an adversarial spend has been detected.
    pub(crate) fn is_breached(&self) -> bool {
        self.breached.load(Relaxed)
    }

    /// Signal the background thread to stop and wait for it to finish.
    pub(crate) fn stop(mut self) {
        self.shutdown.store(true, Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for BreachDetector {
    fn drop(&mut self) {
        self.shutdown.store(true, Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
