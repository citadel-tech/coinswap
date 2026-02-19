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

use bitcoin::{OutPoint, Txid};
use bitcoind::bitcoincore_rpc::RpcApi;

use crate::{
    utill::HEART_BEAT_INTERVAL,
    wallet::Wallet,
    watch_tower::{service::WatchService, watcher::WatcherEvent},
};

use super::swap_tracker::{ContractOutcome, ContractResolution, RecoveryPhase, SwapTracker};

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
    ///
    /// The `swap_tracker` is used to update per-contract resolution outcomes
    /// as contracts are resolved in the background.
    pub(crate) fn start(
        wallet: Arc<RwLock<Wallet>>,
        swap_tracker: Arc<Mutex<SwapTracker>>,
    ) -> Self {
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
                    let incoming_result = if let Ok(mut w) = wallet.write() {
                        match w.sweep_unified_incoming_swapcoins(2.0) {
                            Ok(ref swept) if !swept.is_empty() => {
                                log::info!(
                                    "Recovery loop: swept {} incoming swapcoins",
                                    swept.resolved.len()
                                );
                                Some(swept.clone())
                            }
                            Err(e) => {
                                log::debug!("Recovery loop: incoming sweep: {:?}", e);
                                None
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Try timelock recovery (outgoing)
                    let outgoing_result = if let Ok(mut w) = wallet.write() {
                        match w.recover_unified_timelocked_swapcoins(2.0) {
                            Ok(ref recovered) if !recovered.is_empty() => {
                                log::info!(
                                    "Recovery loop: recovered {} timelocked swapcoins",
                                    recovered.len()
                                );
                                Some(recovered.clone())
                            }
                            Err(e) => {
                                log::debug!("Recovery loop: timelock recovery: {:?}", e);
                                None
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Update tracker outcomes from recovery results
                    if incoming_result.is_some() || outgoing_result.is_some() {
                        if let Ok(mut tracker) = swap_tracker.lock() {
                            Self::update_tracker_outcomes(
                                &mut tracker,
                                incoming_result.as_ref(),
                                outgoing_result.as_ref(),
                            );
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
                        // Clean up wallet entries and update tracker
                        let swap_ids: Vec<String> = swap_tracker
                            .lock()
                            .ok()
                            .map(|t| {
                                t.incomplete_swaps()
                                    .iter()
                                    .map(|r| r.swap_id.clone())
                                    .collect()
                            })
                            .unwrap_or_default();

                        if let Ok(mut w) = wallet.write() {
                            for swap_id in &swap_ids {
                                let keys = w.unified_outgoing_keys_for_swap(swap_id);
                                for key in &keys {
                                    w.remove_unified_outgoing_swapcoin(key);
                                }
                                w.remove_unified_watchonly_swapcoins(swap_id);
                            }
                            let _ = w.save_to_disk();
                        }

                        if let Ok(mut tracker) = swap_tracker.lock() {
                            for swap_id in &swap_ids {
                                let _ = tracker.update_and_save(swap_id, |r| {
                                    r.recovery.phase = RecoveryPhase::CleanedUp;
                                });
                            }
                        }
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

    /// Match resolved contract txids against tracker records and update outcomes.
    fn update_tracker_outcomes(
        tracker: &mut SwapTracker,
        incoming: Option<&crate::wallet::RecoveryOutcome>,
        outgoing: Option<&crate::wallet::RecoveryOutcome>,
    ) {
        let swap_ids: Vec<String> = tracker
            .incomplete_swaps()
            .iter()
            .map(|r| r.swap_id.clone())
            .collect();

        for swap_id in swap_ids {
            let mut changed = false;

            let _ = tracker.update_and_save(&swap_id, |record| {
                // Update incoming outcomes from sweep results
                if let Some(swept) = incoming {
                    for (contract_txid, spending_txid) in &swept.resolved {
                        if record.incoming_contract_txids.contains(contract_txid) {
                            // Find existing outcome or add new one
                            if let Some(outcome) = record
                                .recovery
                                .incoming
                                .iter_mut()
                                .find(|o| o.contract_txid == *contract_txid)
                            {
                                if outcome.resolution == ContractResolution::Unresolved {
                                    outcome.resolution = ContractResolution::Hashlock;
                                    outcome.spending_txid = Some(*spending_txid);
                                    changed = true;
                                }
                            } else {
                                record.recovery.incoming.push(ContractOutcome {
                                    contract_txid: *contract_txid,
                                    resolution: ContractResolution::Hashlock,
                                    spending_txid: Some(*spending_txid),
                                });
                                changed = true;
                            }
                        }
                    }
                }

                // Update outgoing outcomes from timelock recovery results
                if let Some(recovered) = outgoing {
                    for (contract_txid, spending_txid) in &recovered.resolved {
                        if record.outgoing_contract_txids.contains(contract_txid) {
                            if let Some(outcome) = record
                                .recovery
                                .outgoing
                                .iter_mut()
                                .find(|o| o.contract_txid == *contract_txid)
                            {
                                if outcome.resolution == ContractResolution::Unresolved {
                                    outcome.resolution = ContractResolution::Timelock;
                                    outcome.spending_txid = Some(*spending_txid);
                                    changed = true;
                                }
                            } else {
                                record.recovery.outgoing.push(ContractOutcome {
                                    contract_txid: *contract_txid,
                                    resolution: ContractResolution::Timelock,
                                    spending_txid: Some(*spending_txid),
                                });
                                changed = true;
                            }
                        }
                    }
                    for contract_txid in &recovered.discarded {
                        if record.outgoing_contract_txids.contains(contract_txid) {
                            if let Some(outcome) = record
                                .recovery
                                .outgoing
                                .iter_mut()
                                .find(|o| o.contract_txid == *contract_txid)
                            {
                                if outcome.resolution == ContractResolution::Unresolved {
                                    outcome.resolution = ContractResolution::Discarded;
                                    changed = true;
                                }
                            } else {
                                record.recovery.outgoing.push(ContractOutcome {
                                    contract_txid: *contract_txid,
                                    resolution: ContractResolution::Discarded,
                                    spending_txid: None,
                                });
                                changed = true;
                            }
                        }
                    }
                }

                // Advance recovery phase based on what was resolved
                if changed {
                    let all_incoming_done = record
                        .recovery
                        .incoming
                        .iter()
                        .all(|o| o.resolution != ContractResolution::Unresolved);
                    let all_outgoing_done = record
                        .recovery
                        .outgoing
                        .iter()
                        .all(|o| o.resolution != ContractResolution::Unresolved);

                    if all_outgoing_done && record.recovery.phase < RecoveryPhase::OutgoingRecovered
                    {
                        record.recovery.phase = RecoveryPhase::OutgoingRecovered;
                    } else if all_incoming_done
                        && record.recovery.phase < RecoveryPhase::IncomingRecovered
                    {
                        record.recovery.phase = RecoveryPhase::IncomingRecovered;
                    }
                }
            });
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
    /// Mapping of funding outpoint → expected contract txid.
    /// Only a spend whose txid matches the expected contract txid is adversarial.
    /// Cooperative spends (after finalization) produce a different txid.
    sentinels: Arc<Mutex<Vec<(OutPoint, Txid)>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BreachDetector {
    /// Spawn a background thread that polls the WatchService for sentinel spends.
    pub(crate) fn start(watch_service: WatchService) -> Self {
        let breached = Arc::new(AtomicBool::new(false));
        let sentinels: Arc<Mutex<Vec<(OutPoint, Txid)>>> = Arc::new(Mutex::new(Vec::new()));
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

                    for (outpoint, expected_contract_txid) in &current_sentinels {
                        watch_service.watch_request(*outpoint);
                        if let Some(WatcherEvent::UtxoSpent {
                            spending_tx: Some(ref tx),
                            ..
                        }) = watch_service.wait_for_event()
                        {
                            let actual_txid = tx.compute_txid();
                            if actual_txid == *expected_contract_txid {
                                // The funding outpoint was spent by the pre-signed contract tx.
                                // This is an adversarial broadcast.
                                log::warn!(
                                    "Breach detector: contract tx {} broadcast on sentinel {}",
                                    actual_txid, outpoint
                                );
                                breached_clone.store(true, Relaxed);
                                return;
                            }
                            // Spent by a different tx — cooperative sweep after finalization.
                            log::info!(
                                "Breach detector: cooperative spend on sentinel {} (tx {})",
                                outpoint, actual_txid
                            );
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

    /// Register funding outpoints as sentinels with the WatchService.
    ///
    /// Each sentinel is a `(funding_outpoint, expected_contract_txid)` pair.
    /// Only a spend matching the contract txid is considered adversarial;
    /// cooperative spends (after finalization) produce a different txid and are ignored.
    pub(crate) fn add_sentinels(
        &self,
        watch_service: &WatchService,
        sentinels: &[(OutPoint, Txid)],
    ) {
        for (outpoint, _) in sentinels {
            watch_service.register_watch_request(*outpoint);
        }
        if let Ok(mut guard) = self.sentinels.lock() {
            guard.extend_from_slice(sentinels);
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
