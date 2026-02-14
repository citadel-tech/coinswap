//! Unified Taker API for both Legacy (ECDSA) and Taproot (MuSig2) protocols.

use std::{
    net::TcpStream,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc, Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(crate) use super::swap_tracker::SwapPhase;
use super::swap_tracker::{
    now_secs, ExchangeProgress, FinalizationProgress, LegacyExchangeProgress, MakerProgress,
    RecoveryPhase, RecoveryState, SerializableSecretKey, SwapRecord, SwapTracker,
    TaprootExchangeProgress,
};

use bitcoin::{
    hashes::{hash160::Hash as Hash160, Hash},
    hex::DisplayHex,
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        SecretKey,
    },
    Amount, OutPoint, PublicKey, Txid,
};
use bitcoind::bitcoincore_rpc::RpcApi;
use socks::Socks5Stream;

use crate::{
    protocol::{
        common_messages::{
            PrivateKeyHandover, ProtocolVersion, SwapDetails, SwapPrivkey, TakerHello,
        },
        contract::calculate_pubkey_from_nonce,
        legacy_messages::LegacyHashPreimage,
        router::{MakerToTakerMessage, TakerToMakerMessage},
        taproot_messages::TaprootHashPreimage,
    },
    utill::{
        check_tor_status, generate_maker_keys, get_taker_dir, read_message, send_message,
        HEART_BEAT_INTERVAL,
    },
    wallet::{
        unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin, WatchOnlySwapCoin},
        RPCConfig, Wallet,
    },
    watch_tower::{
        registry_storage::FileRegistry,
        rpc_backend::BitcoinRpc,
        service::WatchService,
        watcher::{Role, Watcher, WatcherEvent},
        zmq_backend::ZmqBackend,
    },
};

use super::{
    config::TakerConfig,
    error::TakerError,
    offers::{MakerProtocol, OfferAndAddress, OfferBookHandle, OfferSyncHandle, OfferSyncService},
};

/// Connection type for the taker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    /// Direct TCP connection.
    Clearnet,
    /// Connection through Tor SOCKS proxy.
    Tor,
}

/// Timeout for connecting to makers.
pub const CONNECT_TIMEOUT_SECS: u64 = 30;

/// Base refund locktime (in blocks) for the taker's first hop.
pub(crate) const REFUND_LOCKTIME_BASE: u16 = 20;

/// Locktime increment per hop in the swap route.
pub(crate) const REFUND_LOCKTIME_STEP: u16 = 20;

/// Maximum number of finalization retry attempts before triggering recovery.
#[cfg(not(feature = "integration-test"))]
const MAX_FINALIZE_RETRIES: u32 = 3;
#[cfg(feature = "integration-test")]
const MAX_FINALIZE_RETRIES: u32 = 2;

/// Delay between finalization retry attempts.
#[cfg(not(feature = "integration-test"))]
const FINALIZE_RETRY_DELAY: Duration = Duration::from_secs(15);
#[cfg(feature = "integration-test")]
const FINALIZE_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Unified Taker configuration.
#[derive(Debug, Clone)]
pub struct UnifiedTakerConfig {
    /// Data directory path.
    pub data_dir: Option<PathBuf>,
    /// Wallet file name.
    pub wallet_file_name: Option<String>,
    /// RPC configuration for Bitcoin Core.
    pub rpc_config: Option<RPCConfig>,
    /// Tor control port (optional).
    pub control_port: Option<u16>,
    /// Tor authentication password (optional).
    pub tor_auth_password: Option<String>,
    /// SOCKS port for Tor.
    pub socks_port: u16,
    /// ZMQ address for transaction monitoring.
    pub zmq_addr: String,
    /// Wallet password (optional).
    pub password: Option<String>,
    /// Connection type (Tor or Clearnet).
    pub connection_type: ConnectionType,
}

impl Default for UnifiedTakerConfig {
    fn default() -> Self {
        UnifiedTakerConfig {
            data_dir: None,
            wallet_file_name: None,
            rpc_config: None,
            control_port: None,
            tor_auth_password: None,
            socks_port: 19050,
            zmq_addr: "tcp://127.0.0.1:28332".to_string(),
            password: None,
            connection_type: ConnectionType::Tor,
        }
    }
}

impl UnifiedTakerConfig {
    /// Set the data directory.
    pub fn with_data_dir(mut self, path: PathBuf) -> Self {
        self.data_dir = Some(path);
        self
    }

    /// Set the wallet file name.
    pub fn with_wallet_name(mut self, name: String) -> Self {
        self.wallet_file_name = Some(name);
        self
    }

    /// Set the RPC configuration.
    pub fn with_rpc_config(mut self, rpc_config: RPCConfig) -> Self {
        self.rpc_config = Some(rpc_config);
        self
    }

    /// Set the ZMQ address.
    pub fn with_zmq_addr(mut self, addr: String) -> Self {
        self.zmq_addr = addr;
        self
    }
}

/// Unified swap parameters.
#[derive(Debug, Clone, Default)]
pub struct UnifiedSwapParams {
    /// Protocol version to use for this swap.
    pub protocol: ProtocolVersion,
    /// Total amount to swap.
    pub send_amount: Amount,
    /// Number of makers (hops) to use.
    pub maker_count: usize,
    /// Number of transaction splits (Taproot only, defaults to 1 for Legacy).
    pub tx_count: u32,
    /// Required confirmations for funding transactions.
    pub required_confirms: u32,
    /// User-selected UTXOs (optional).
    pub manually_selected_outpoints: Option<Vec<OutPoint>>,
}

impl UnifiedSwapParams {
    /// Create new swap parameters.
    pub fn new(protocol: ProtocolVersion, send_amount: Amount, maker_count: usize) -> Self {
        UnifiedSwapParams {
            protocol,
            send_amount,
            maker_count,
            tx_count: 1,
            required_confirms: 1,
            manually_selected_outpoints: None,
        }
    }

    /// Set the number of transaction splits.
    pub fn with_tx_count(mut self, tx_count: u32) -> Self {
        self.tx_count = tx_count;
        self
    }

    /// Set the required confirmations.
    pub fn with_required_confirms(mut self, confirms: u32) -> Self {
        self.required_confirms = confirms;
        self
    }

    /// Set manual UTXO selection.
    pub fn with_utxos(mut self, outpoints: Vec<OutPoint>) -> Self {
        self.manually_selected_outpoints = Some(outpoints);
        self
    }
}

/// Unified swap report.
#[derive(Debug, Clone)]
pub struct UnifiedSwapReport {
    /// Unique swap ID.
    pub swap_id: String,
    /// Protocol version used.
    pub protocol_version: ProtocolVersion,
    /// Amount sent (total input amount).
    pub amount_sent: Amount,
    /// Amount received (total output amount).
    pub amount_received: Amount,
    /// Total fees paid.
    pub total_fees: Amount,
    /// Number of makers used.
    pub maker_count: usize,
    /// Swap duration in seconds.
    pub duration_seconds: f64,
    /// Fee percentage relative to target amount.
    pub fee_percentage: f64,
}

/// State for an ongoing swap.
#[derive(Debug, Clone, Default)]
pub(crate) struct OngoingSwapState {
    /// Unique swap ID.
    pub(crate) id: String,
    /// The hash preimage for this swap.
    pub(crate) preimage: [u8; 32],
    /// Swap parameters.
    pub(crate) params: UnifiedSwapParams,
    /// Selected makers for this swap.
    pub(crate) makers: Vec<MakerConnection>,
    /// Outgoing swapcoins (our side of the swap).
    pub(crate) outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    /// Incoming swapcoins (receiving side of the swap).
    pub(crate) incoming_swapcoins: Vec<IncomingSwapCoin>,
    /// Watch-only swapcoins for intermediate hops (between makers).
    pub(crate) watchonly_swapcoins: Vec<WatchOnlySwapCoin>,
    /// Multisig nonces for each outgoing swapcoin (Legacy only, used in ProofOfFunding).
    /// Empty for Taproot swaps.
    pub(crate) multisig_nonces: Vec<SecretKey>,
    /// Hashlock nonces for each outgoing swapcoin (used in ProofOfFunding).
    pub(crate) hashlock_nonces: Vec<SecretKey>,
    /// Current phase of the swap lifecycle.
    pub(crate) phase: SwapPhase,
}

/// Connection state for a maker in the swap route.
#[derive(Debug, Clone)]
pub(crate) struct MakerConnection {
    /// Maker's offer and address from offerbook.
    pub(crate) offer_and_address: OfferAndAddress,
    /// Protocol version negotiated with this maker.
    pub(crate) protocol: ProtocolVersion,
    /// Tweakable point for this swap.
    pub(crate) tweakable_point: Option<PublicKey>,
    /// Protocol-specific exchange progress milestones.
    pub(crate) exchange: ExchangeProgress,
    /// Shared finalization milestones (preimage, privkey exchange).
    pub(crate) finalization: FinalizationProgress,
}

impl MakerConnection {
    /// Get mutable reference to Legacy exchange progress.
    /// Panics if protocol is Taproot.
    pub(crate) fn legacy_exchange_mut(&mut self) -> &mut LegacyExchangeProgress {
        match &mut self.exchange {
            ExchangeProgress::Legacy(ref mut l) => l,
            _ => panic!("Expected Legacy exchange progress"),
        }
    }

    /// Get mutable reference to Taproot exchange progress.
    /// Panics if protocol is Legacy.
    pub(crate) fn taproot_exchange_mut(&mut self) -> &mut TaprootExchangeProgress {
        match &mut self.exchange {
            ExchangeProgress::Taproot(ref mut t) => t,
            _ => panic!("Expected Taproot exchange progress"),
        }
    }
}

/// Unified Taker client.
pub struct UnifiedTaker {
    /// Configuration.
    pub(crate) config: UnifiedTakerConfig,
    /// Wallet for managing funds.
    pub(crate) wallet: Arc<RwLock<Wallet>>,
    /// Offer book for managing maker offers.
    pub(crate) offerbook: OfferBookHandle,
    /// Watch service for transaction monitoring.
    pub(crate) watch_service: WatchService,
    /// Handle for offer sync background service.
    offer_sync_handle: OfferSyncHandle,
    /// Ongoing swap state (`None` when no swap is active).
    pub(crate) ongoing_swap: Option<OngoingSwapState>,
    /// Persistent swap tracker for crash-resilient recovery.
    pub(crate) swap_tracker: SwapTracker,
    /// Test behavior.
    #[cfg(feature = "integration-test")]
    pub behavior: UnifiedTakerBehavior,
}

impl Drop for UnifiedTaker {
    fn drop(&mut self) {
        log::info!("Shutting down unified taker.");
        // Flush any pending swap state before shutdown
        if let Some(swap) = &self.ongoing_swap {
            if let Ok(record) = self.persist_build_record(swap) {
                if let Err(e) = self.swap_tracker.save_record(&record) {
                    log::error!("Failed to flush swap tracker on shutdown: {:?}", e);
                }
            }
        }
        if let Err(e) = self.offerbook.persist() {
            log::error!("Failed to persist offerbook: {:?}", e);
        }
        log::info!("Shutting down offer sync background job");
        self.offer_sync_handle.shutdown();
        log::info!("Shutting down watch service background job");
        self.watch_service.shutdown();
        log::info!("Offerbook data saved to disk.");
        if let Ok(wallet) = self.wallet.write() {
            if let Err(e) = wallet.save_to_disk() {
                log::error!("Failed to save wallet: {:?}", e);
            }
        }
        log::info!("Wallet data saved to disk.");
    }
}

impl Role for UnifiedTaker {
    const RUN_DISCOVERY: bool = true;
}

impl UnifiedTaker {
    /// Acquire a read lock on the wallet.
    pub(crate) fn read_wallet(&self) -> Result<RwLockReadGuard<'_, Wallet>, TakerError> {
        self.wallet
            .read()
            .map_err(|_| TakerError::General("Failed to lock wallet".to_string()))
    }

    /// Acquire a write lock on the wallet.
    pub(crate) fn write_wallet(&self) -> Result<RwLockWriteGuard<'_, Wallet>, TakerError> {
        self.wallet
            .write()
            .map_err(|_| TakerError::General("Failed to lock wallet".to_string()))
    }

    /// Get a shared reference to the ongoing swap state.
    pub(crate) fn swap_state(&self) -> Result<&OngoingSwapState, TakerError> {
        self.ongoing_swap
            .as_ref()
            .ok_or_else(|| TakerError::General("No active swap".to_string()))
    }

    /// Get a mutable reference to the ongoing swap state.
    pub(crate) fn swap_state_mut(&mut self) -> Result<&mut OngoingSwapState, TakerError> {
        self.ongoing_swap
            .as_mut()
            .ok_or_else(|| TakerError::General("No active swap".to_string()))
    }

    /// Initialize a new unified taker.
    pub fn init(config: UnifiedTakerConfig) -> Result<Self, TakerError> {
        let data_dir = config.data_dir.clone().unwrap_or_else(get_taker_dir);
        std::fs::create_dir_all(&data_dir)?;

        let (wallet, rpc_config) = Self::init_wallet(&config, &data_dir)?;
        let watch_service = Self::init_watch_service(&config, &rpc_config, &data_dir)?;
        Self::init_taker_config(&config, &data_dir)?;
        let offerbook = OfferBookHandle::load_or_create(&data_dir)?;
        let offer_sync_handle =
            Self::init_offer_sync(&offerbook, &watch_service, config.socks_port, rpc_config)?;
        let swap_tracker = SwapTracker::load_or_create(&data_dir)?;

        let mut taker = UnifiedTaker {
            config,
            wallet: Arc::new(RwLock::new(wallet)),
            offerbook,
            watch_service,
            offer_sync_handle,
            ongoing_swap: None,
            swap_tracker,
            #[cfg(feature = "integration-test")]
            behavior: UnifiedTakerBehavior::Normal,
        };

        taker.init_recover_incomplete();
        Ok(taker)
    }

    /// Called on startup to handle taker crash/restart scenarios.
    ///
    /// Uses the persistent swap tracker to identify incomplete swaps and dispatch
    /// to the appropriate recovery strategy based on the recorded phase.
    fn init_recover_incomplete(&mut self) {
        log::info!("Checking for incomplete swaps from previous session...");

        // Collect incomplete swap IDs to avoid borrow issues
        let incomplete_ids: Vec<String> = self
            .swap_tracker
            .incomplete_swaps()
            .iter()
            .map(|r| r.swap_id.clone())
            .collect();

        if incomplete_ids.is_empty() {
            log::info!("No incomplete swaps found in tracker.");
        }

        for swap_id in &incomplete_ids {
            // Re-borrow for each iteration to satisfy borrow checker
            let phase = match self.swap_tracker.get_record(swap_id) {
                Some(r) => r.phase,
                None => continue,
            };

            match phase {
                SwapPhase::Negotiating | SwapPhase::FundingCreated => {
                    log::info!(
                        "Swap {} was in phase {} — no funds at risk, cleaning up",
                        swap_id,
                        phase
                    );
                    if let Err(e) = self.swap_tracker.remove_record(swap_id) {
                        log::error!("Failed to remove tracker record for {}: {:?}", swap_id, e);
                    }
                }
                SwapPhase::FundsBroadcast
                | SwapPhase::ContractsExchanged
                | SwapPhase::Finalizing
                | SwapPhase::PrivkeysCollected
                | SwapPhase::PrivkeysForwarded => {
                    log::warn!(
                        "Swap {} was in phase {} — funds at risk, running recovery",
                        swap_id,
                        phase
                    );
                    if let Err(e) = self.recover_from_record(swap_id) {
                        log::error!("Recovery failed for swap {}: {:?}", swap_id, e);
                    }
                }
                SwapPhase::Failed => {
                    let recovery_phase = self
                        .swap_tracker
                        .get_record(swap_id)
                        .map(|r| r.recovery.phase)
                        .unwrap_or(RecoveryPhase::CleanedUp);

                    if recovery_phase < RecoveryPhase::CleanedUp {
                        log::warn!(
                            "Swap {} failed with incomplete recovery (phase {:?}), resuming",
                            swap_id,
                            recovery_phase
                        );
                        if let Err(e) = self.recover_from_record(swap_id) {
                            log::error!("Recovery resume failed for swap {}: {:?}", swap_id, e);
                        }
                    }
                }
                SwapPhase::Completed => {
                    // Should not appear in incomplete_swaps(), skip
                }
            }
        }

        // Also do the legacy blind sweep for any swapcoins that predate the tracker
        match self.write_wallet() {
            Ok(mut wallet) => {
                match wallet.sweep_unified_incoming_swapcoins(2.0) {
                    Ok(swept) if !swept.is_empty() => {
                        log::info!("Startup recovery: swept {} incoming swapcoins", swept.len());
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("Startup incoming sweep failed: {:?}", e),
                }

                match wallet.recover_unified_timelocked_swapcoins(2.0) {
                    Ok(recovered) if !recovered.is_empty() => {
                        log::info!(
                            "Startup recovery: recovered {} timelocked outgoing swapcoins",
                            recovered.len()
                        );
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("Startup timelock recovery failed: {:?}", e),
                }

                let mut remaining = wallet.unified_outgoing_contract_outpoints();
                remaining.extend(wallet.unified_incoming_contract_outpoints());
                drop(wallet);

                for outpoint in &remaining {
                    self.watch_service.register_watch_request(*outpoint);
                }

                if !remaining.is_empty() {
                    log::info!(
                        "Startup recovery: registered {} contract outputs for monitoring",
                        remaining.len()
                    );
                }
            }
            Err(e) => log::warn!("Startup recovery: failed to lock wallet: {:?}", e),
        }
    }

    /// Set up the wallet from config and return it with the resolved RPC config.
    fn init_wallet(
        config: &UnifiedTakerConfig,
        data_dir: &std::path::Path,
    ) -> Result<(Wallet, RPCConfig), TakerError> {
        let wallet_file_name = config
            .wallet_file_name
            .clone()
            .unwrap_or_else(|| "taker-wallet".to_string());

        let mut rpc_config = config
            .rpc_config
            .clone()
            .ok_or_else(|| TakerError::General("RPC configuration is required".to_string()))?;
        rpc_config.wallet_name = wallet_file_name.clone();

        let wallet_path = data_dir.join("wallets").join(&wallet_file_name);
        let wallet =
            Wallet::load_or_init_wallet(&wallet_path, &rpc_config, config.password.clone())?;

        Ok((wallet, rpc_config))
    }

    /// Initialize the ZMQ-backed watch service and spawn the watcher thread.
    fn init_watch_service(
        config: &UnifiedTakerConfig,
        rpc_config: &RPCConfig,
        data_dir: &std::path::Path,
    ) -> Result<WatchService, TakerError> {
        let backend = ZmqBackend::new(&config.zmq_addr);
        let rpc_backend = BitcoinRpc::new(rpc_config.clone())?;
        let blockchain_info = rpc_backend.get_blockchain_info()?;
        let file_registry = data_dir
            .join(".taker_watcher")
            .join(blockchain_info.chain.to_string());
        let registry = FileRegistry::load(file_registry);

        let (tx_requests, rx_requests) = mpsc::channel();
        let (tx_events, rx_responses) = mpsc::channel();
        let rpc_config_watcher = rpc_config.clone();

        let mut watcher = Watcher::<UnifiedTaker>::new(backend, registry, rx_requests, tx_events);
        let _ = thread::Builder::new()
            .name("Unified Watcher thread".to_string())
            .spawn(move || watcher.run(rpc_config_watcher));

        Ok(WatchService::new(tx_requests, rx_responses))
    }

    /// Load/merge taker config and check Tor status.
    fn init_taker_config(
        config: &UnifiedTakerConfig,
        data_dir: &std::path::Path,
    ) -> Result<(), TakerError> {
        let mut taker_config = TakerConfig::new(Some(&data_dir.join("config.toml")))?;

        if let Some(control_port) = config.control_port {
            taker_config.control_port = control_port;
        }

        if let Some(ref tor_auth_password) = config.tor_auth_password {
            taker_config.tor_auth_password = tor_auth_password.clone();
        }

        if !cfg!(feature = "integration-test") && config.connection_type == ConnectionType::Tor {
            check_tor_status(
                taker_config.control_port,
                taker_config.tor_auth_password.as_str(),
            )?;
        }

        taker_config.write_to_file(&data_dir.join("config.toml"))?;
        Ok(())
    }

    /// Start the background offer sync service.
    fn init_offer_sync(
        offerbook: &OfferBookHandle,
        watch_service: &WatchService,
        socks_port: u16,
        rpc_config: RPCConfig,
    ) -> Result<OfferSyncHandle, TakerError> {
        let rpc_backend_sync = BitcoinRpc::new(rpc_config)?;
        Ok(OfferSyncService::new(
            offerbook.clone(),
            watch_service.clone(),
            socks_port,
            rpc_backend_sync,
        )
        .start())
    }

    /// Get reference to the wallet.
    pub fn get_wallet(&self) -> &Arc<RwLock<Wallet>> {
        &self.wallet
    }

    /// Log the current swap tracker state at INFO level.
    pub fn log_tracker_state(&self) {
        self.swap_tracker.log_state();
    }

    /// Recover timelocked outgoing swapcoins and update the swap tracker.
    ///
    /// Wraps `Wallet::recover_unified_timelocked_swapcoins` — after recovery,
    /// moves resolved contracts from `outgoing_watching` to `outgoing_recovered`
    /// and advances the recovery phase when all outgoing are resolved.
    pub fn recover_timelocked(&mut self, fee_rate: f64) -> Result<Vec<Txid>, TakerError> {
        // Snapshot which contract txids are being watched per swap
        let incomplete: Vec<(String, Vec<Txid>)> = self
            .swap_tracker
            .incomplete_swaps()
            .iter()
            .map(|r| (r.swap_id.clone(), r.recovery.outgoing_watching.clone()))
            .collect();

        // Call the wallet-level recovery
        let recovered_txids = {
            let mut wallet = self.write_wallet()?;
            wallet.recover_unified_timelocked_swapcoins(fee_rate)?
        };

        if recovered_txids.is_empty() && incomplete.is_empty() {
            return Ok(recovered_txids);
        }

        // Check on-chain status of each watched contract output.
        // If the contract output is spent (recovered by us or claimed), move it
        // from outgoing_watching to outgoing_recovered.
        for (swap_id, watching) in &incomplete {
            let mut resolved_txids = Vec::new();
            for contract_txid in watching {
                let is_resolved = {
                    let wallet = self.read_wallet()?;
                    // Contract outputs use vout 0 (consistent with
                    // unified_outgoing_contract_outpoints).
                    match wallet.rpc.get_tx_out(contract_txid, 0, None) {
                        Ok(Some(_)) => false, // still unspent on-chain
                        _ => true,            // spent or not found → resolved
                    }
                };

                if is_resolved {
                    resolved_txids.push(*contract_txid);
                }
            }

            if !resolved_txids.is_empty() {
                self.swap_tracker.update_and_save(swap_id, |record| {
                    for txid in &resolved_txids {
                        record.recovery.outgoing_watching.retain(|t| t != txid);
                        if !record.recovery.outgoing_recovered.contains(txid) {
                            record.recovery.outgoing_recovered.push(*txid);
                        }
                    }
                })?;

                // Check if we can advance to OutgoingRecovered and CleanedUp
                if self.recover_is_outgoing_done(swap_id) {
                    self.recover_update_phase(swap_id, RecoveryPhase::OutgoingRecovered)?;
                    // Clean up recovered swapcoins from wallet store
                    {
                        let mut wallet = self.write_wallet()?;
                        wallet.remove_unified_outgoing_swapcoins(swap_id);
                        wallet.remove_unified_watchonly_swapcoins(swap_id);
                        wallet.save_to_disk()?;
                    }
                    self.recover_update_phase(swap_id, RecoveryPhase::CleanedUp)?;

                    self.swap_tracker.update_and_save(swap_id, |record| {
                        record.phase = SwapPhase::Failed;
                    })?;
                }
            }
        }

        Ok(recovered_txids)
    }

    /// Sweep incoming swapcoins via hashlock and update the swap tracker.
    ///
    /// Wraps `Wallet::sweep_unified_incoming_swapcoins` — after sweep,
    /// adds swept txids to tracker and advances the recovery phase.
    pub fn recover_sweep_incoming(&mut self, fee_rate: f64) -> Result<Vec<Txid>, TakerError> {
        let incomplete: Vec<(String, Vec<Txid>)> = self
            .swap_tracker
            .incomplete_swaps()
            .iter()
            .map(|r| (r.swap_id.clone(), r.recovery.incoming_swept.clone()))
            .collect();

        let swept_txids = {
            let mut wallet = self.write_wallet()?;
            wallet.sweep_unified_incoming_swapcoins(fee_rate)?
        };

        if swept_txids.is_empty() {
            return Ok(swept_txids);
        }

        for (swap_id, already_swept) in &incomplete {
            let new_txids: Vec<Txid> = swept_txids
                .iter()
                .filter(|txid| !already_swept.contains(txid))
                .copied()
                .collect();

            if !new_txids.is_empty() {
                self.swap_tracker.update_and_save(swap_id, |record| {
                    record.recovery.incoming_swept.extend(&new_txids);
                })?;

                if self.recover_is_incoming_done(swap_id) {
                    self.recover_update_phase(swap_id, RecoveryPhase::IncomingRecovered)?;
                }
            }
        }

        Ok(swept_txids)
    }

    /// Perform a coinswap with the given parameters.
    pub fn do_coinswap(
        &mut self,
        params: UnifiedSwapParams,
    ) -> Result<UnifiedSwapReport, TakerError> {
        let swap_start_time = Instant::now();

        log::info!(
            "Starting unified coinswap: amount={}, makers={}, protocol={:?}",
            params.send_amount,
            params.maker_count,
            params.protocol
        );

        let available = self.read_wallet()?.get_balances()?.spendable;

        let required = params.send_amount + Amount::from_sat(10000); // Buffer for fees
        if available < required {
            return Err(TakerError::General(format!(
                "Insufficient balance: available={}, required={}",
                available, required
            )));
        }

        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        let swap_id = preimage[0..8].to_lower_hex_string();
        log::info!("Initiating coinswap with id: {}", swap_id);

        // Extract values needed for the report before moving params into swap state
        let amount_sent = params.send_amount;
        let maker_count = params.maker_count;

        // Initialize swap state
        self.ongoing_swap = Some(OngoingSwapState {
            id: swap_id.clone(),
            preimage,
            params,
            makers: Vec::new(),
            outgoing_swapcoins: Vec::new(),
            incoming_swapcoins: Vec::new(),
            watchonly_swapcoins: Vec::new(),
            multisig_nonces: Vec::new(),
            hashlock_nonces: Vec::new(),
            phase: SwapPhase::Negotiating,
        });

        // Pre-commitment phases: no funds on-chain yet, nothing to recover.
        self.discover_makers()?;

        // SP1: Persist initial swap record with preimage and maker list.
        self.persist_phase(SwapPhase::Negotiating)?;

        #[cfg(feature = "integration-test")]
        if self.behavior == UnifiedTakerBehavior::CloseEarly {
            log::warn!("Test behavior: closing early after maker selection");
            return Err(TakerError::General(
                "Test: Closing early after maker selection".to_string(),
            ));
        }

        self.negotiate_swap_details()?;

        // SP2: Persist after negotiation (tweakable_points updated).
        self.persist_phase(SwapPhase::Negotiating)?;

        self.funding_initialize()?;

        // SP3: Persist after funding initialization (outgoing txids created).
        self.persist_phase(SwapPhase::FundingCreated)?;

        // Protocol-specific execution with phase-aware recovery triggers.
        let protocol = self.swap_state()?.params.protocol;

        match protocol {
            ProtocolVersion::Legacy => {
                // Phase 1: Exchange contract data (includes funding tx broadcast).
                // Phase transitions to FundsBroadcast inside exchange_legacy()
                // after funding txs are sent.
                match self.exchange_legacy() {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("Legacy contract exchange failed: {:?}", e);
                        let phase = self
                            .swap_state()
                            .map(|s| s.phase)
                            .unwrap_or(SwapPhase::Negotiating);
                        if phase >= SwapPhase::FundsBroadcast {
                            log::warn!("Funding txs were broadcast, triggering recovery");
                            self.persist_failure(phase, &e);
                            if let Err(re) = self.recover_active_swap() {
                                log::error!("Recovery failed: {:?}", re);
                            }
                        } else {
                            log::info!("No funds on-chain — safe to abort");
                            let _ = self.swap_tracker.remove_record(
                                &self.swap_state().map(|s| s.id.clone()).unwrap_or_default(),
                            );
                            self.ongoing_swap = None;
                        }
                        return Err(e);
                    }
                }

                #[cfg(feature = "integration-test")]
                if self.behavior == UnifiedTakerBehavior::DropAfterFundsBroadcast {
                    log::warn!("Test behavior: dropping after funds broadcast (Legacy)");
                    let phase = self
                        .swap_state()
                        .map(|s| s.phase)
                        .unwrap_or(SwapPhase::FundsBroadcast);
                    let err =
                        TakerError::General("Test: dropped after funds broadcast".to_string());
                    self.persist_failure(phase, &err);
                    if let Err(re) = self.recover_active_swap() {
                        log::error!("Recovery failed: {:?}", re);
                    }
                    return Err(err);
                }

                // SP7: Finalization starts.
                self.persist_phase(SwapPhase::Finalizing)?;

                // Phase 2: Finalize with retry (exchange succeeded → phase is FundsBroadcast).
                match self.finalize_with_retry() {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("Legacy finalization failed after retries: {:?}", e);
                        self.persist_failure(SwapPhase::Finalizing, &e);
                        if let Err(re) = self.recover_active_swap() {
                            log::error!("Recovery failed: {:?}", re);
                        }
                        return Err(e);
                    }
                }
            }
            ProtocolVersion::Taproot => {
                // Phase 1: Broadcast our outgoing contract txs.
                // Makers verify that contract txs are on-chain before
                // creating their own outgoing, so we must broadcast first.
                self.swap_state_mut()?.phase = SwapPhase::FundsBroadcast;
                // SP4-T: Persist before broadcast (point of no return).
                self.persist_phase(SwapPhase::FundsBroadcast)?;
                match self.funding_broadcast() {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("Taproot broadcast failed: {:?}", e);
                        self.persist_failure(SwapPhase::FundsBroadcast, &e);
                        if let Err(re) = self.recover_active_swap() {
                            log::error!("Recovery failed: {:?}", re);
                        }
                        return Err(e);
                    }
                }

                // Phase 2: Exchange contract data with makers.
                // Our outgoing is on-chain, so makers can verify it.
                match self.exchange_taproot() {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("Taproot contract exchange failed: {:?}", e);
                        self.persist_failure(SwapPhase::FundsBroadcast, &e);
                        if let Err(re) = self.recover_active_swap() {
                            log::error!("Recovery failed: {:?}", re);
                        }
                        return Err(e);
                    }
                }

                #[cfg(feature = "integration-test")]
                if self.behavior == UnifiedTakerBehavior::DropAfterFundsBroadcast {
                    log::warn!("Test behavior: dropping after contract broadcast (Taproot)");
                    let phase = self
                        .swap_state()
                        .map(|s| s.phase)
                        .unwrap_or(SwapPhase::FundsBroadcast);
                    let err =
                        TakerError::General("Test: dropped after contract broadcast".to_string());
                    self.persist_failure(phase, &err);
                    if let Err(re) = self.recover_active_swap() {
                        log::error!("Recovery failed: {:?}", re);
                    }
                    return Err(err);
                }

                // SP7: Finalization starts.
                self.persist_phase(SwapPhase::Finalizing)?;

                // Phase 3: Finalize with retry + watch tower polling.
                match self.finalize_with_retry() {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("Taproot finalization failed after retries: {:?}", e);
                        self.persist_failure(SwapPhase::Finalizing, &e);
                        if let Err(re) = self.recover_active_swap() {
                            log::error!("Recovery failed: {:?}", re);
                        }
                        return Err(e);
                    }
                }
            }
        }

        // Success path: sweep + report (shared by both protocols)
        {
            let mut wallet = self.write_wallet()?;
            let swept = wallet.sweep_unified_incoming_swapcoins(2.0)?;
            log::info!("Swept {} incoming swapcoins", swept.len());
            wallet.sync_and_save()?;
        }

        // Clean up watch-only entries on success
        {
            let swap_id_for_cleanup = self.swap_state()?.id.clone();
            let mut wallet = self.write_wallet()?;
            wallet.remove_unified_watchonly_swapcoins(&swap_id_for_cleanup);
            wallet.save_to_disk()?;
        }

        // SP10: Mark swap as completed in the tracker.
        self.persist_phase(SwapPhase::Completed)?;

        // Generate report
        let duration = swap_start_time.elapsed();
        let amount_received = Amount::from_sat(
            self.swap_state()?
                .incoming_swapcoins
                .iter()
                .map(|sc| sc.funding_amount.to_sat())
                .sum::<u64>(),
        );
        let total_fees = amount_sent
            .checked_sub(amount_received)
            .unwrap_or(Amount::ZERO);

        let report = UnifiedSwapReport {
            swap_id,
            protocol_version: self.swap_state()?.params.protocol,
            amount_sent,
            amount_received,
            total_fees,
            maker_count,
            duration_seconds: duration.as_secs_f64(),
            fee_percentage: if amount_sent.to_sat() > 0 {
                (total_fees.to_sat() as f64 / amount_sent.to_sat() as f64) * 100.0
            } else {
                0.0
            },
        };

        log::info!("Coinswap completed successfully: {:?}", report);
        Ok(report)
    }

    /// Discover and select makers for the swap.
    fn discover_makers(&mut self) -> Result<(), TakerError> {
        let swap = self.swap_state()?;
        let maker_count = swap.params.maker_count;
        let send_amount = swap.params.send_amount;
        let protocol = swap.params.protocol;

        log::info!("Discovering makers for {} hops...", maker_count);

        let maker_protocol = match protocol {
            ProtocolVersion::Legacy => MakerProtocol::Legacy,
            ProtocolVersion::Taproot => MakerProtocol::Taproot,
        };

        let mut available_makers = self.offerbook.active_makers(&maker_protocol);

        // Polling loop: wait for offer sync to complete if no makers are available yet.
        if available_makers.is_empty() {
            log::warn!("No makers found in offerbook. Waiting for offer sync...");

            let start = Instant::now();
            let timeout = Duration::from_secs(60);

            while self.offer_sync_handle.is_syncing() && start.elapsed() < timeout {
                thread::sleep(Duration::from_millis(500));
            }

            available_makers = self.offerbook.active_makers(&maker_protocol);
            if available_makers.is_empty() {
                return Err(TakerError::NotEnoughMakersInOfferBook);
            }
        }

        let suitable_makers: Vec<OfferAndAddress> = available_makers
            .into_iter()
            .filter(|maker| {
                let min_ok = send_amount.to_sat() >= maker.offer.min_size;
                let max_ok = send_amount.to_sat() <= maker.offer.max_size;
                min_ok && max_ok
            })
            .collect();

        if suitable_makers.len() < maker_count {
            log::error!(
                "Not enough suitable makers. Required: {}, Available: {}",
                maker_count,
                suitable_makers.len()
            );
            return Err(TakerError::NotEnoughMakersInOfferBook);
        }

        let selected_makers: Vec<MakerConnection> = suitable_makers
            .into_iter()
            .take(maker_count)
            .map(|offer_and_address| {
                let exchange = match protocol {
                    ProtocolVersion::Legacy => {
                        ExchangeProgress::Legacy(LegacyExchangeProgress::default())
                    }
                    ProtocolVersion::Taproot => {
                        ExchangeProgress::Taproot(TaprootExchangeProgress::default())
                    }
                };
                MakerConnection {
                    offer_and_address,
                    protocol,
                    tweakable_point: None,
                    exchange,
                    finalization: FinalizationProgress::default(),
                }
            })
            .collect();

        log::info!(
            "Selected {} makers: {}",
            selected_makers.len(),
            selected_makers
                .iter()
                .enumerate()
                .map(|(i, m)| format!(
                    "#{} {} (fee: {})",
                    i + 1,
                    m.offer_and_address.address,
                    m.offer_and_address.offer.base_fee
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );

        self.swap_state_mut()?.makers = selected_makers;
        Ok(())
    }

    /// Negotiate swap details with each maker.
    /// TODO: Look for another maker if a maker rejects
    fn negotiate_swap_details(&mut self) -> Result<(), TakerError> {
        log::info!("Negotiating swap details with makers...");

        let swap = self.swap_state()?;
        let num_makers = swap.makers.len();
        let maker_count = swap.params.maker_count;
        let swap_id = swap.id.clone();
        let send_amount = swap.params.send_amount;
        let tx_count = swap.params.tx_count;

        for i in 0..num_makers {
            let maker_address = self.swap_state()?.makers[i]
                .offer_and_address
                .address
                .to_string();
            log::info!("Connecting to maker at {}", maker_address);

            let mut stream = self.net_connect(&maker_address)?;

            let negotiated_protocol = self.net_handshake(&mut stream)?;
            log::info!("Handshake complete, protocol: {:?}", negotiated_protocol);

            let refund_locktime =
                REFUND_LOCKTIME_BASE + REFUND_LOCKTIME_STEP * (maker_count - i - 1) as u16;

            let swap_details = SwapDetails {
                id: swap_id.clone(),
                protocol_version: negotiated_protocol,
                amount: send_amount,
                tx_count,
                timelock: refund_locktime,
            };

            send_message(&mut stream, &TakerToMakerMessage::SwapDetails(swap_details))?;

            let msg_bytes = read_message(&mut stream)?;
            let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

            match msg {
                MakerToTakerMessage::AckSwapDetails(ack) => {
                    if let Some(tweakable_point) = ack.tweakable_point {
                        let swap = self.swap_state_mut()?;
                        swap.makers[i].tweakable_point = Some(tweakable_point);
                        swap.makers[i].protocol = negotiated_protocol;
                        log::info!("Maker {} accepted swap with tweakable point", i);
                    } else {
                        return Err(TakerError::General(format!("Maker {} rejected swap", i)));
                    }
                }
                _ => {
                    return Err(TakerError::General(format!(
                        "Unexpected message from maker {}: expected AckSwapDetails",
                        i
                    )));
                }
            }
        }

        Ok(())
    }

    /// Initialize swap funding by creating outgoing swapcoins.
    fn funding_initialize(&mut self) -> Result<(), TakerError> {
        log::info!("Initializing swap funding...");

        let swap = self.swap_state()?;

        let first_maker = swap
            .makers
            .first()
            .ok_or_else(|| TakerError::General("No makers in swap route".to_string()))?;

        let tweakable_point = first_maker.tweakable_point.ok_or_else(|| {
            TakerError::General("First maker missing tweakable point".to_string())
        })?;

        let protocol = first_maker.protocol;

        let maker_count = swap.params.maker_count;
        let refund_locktime = REFUND_LOCKTIME_BASE + REFUND_LOCKTIME_STEP * maker_count as u16;

        let hashvalue = Hash160::hash(&swap.preimage);
        let preimage = swap.preimage;
        let send_amount = swap.params.send_amount;
        let swap_id = swap.id.clone();
        let manually_selected_outpoints = swap.params.manually_selected_outpoints.clone();

        let (multisig_pubkeys, multisig_nonces, hashlock_pubkeys, hashlock_nonces) =
            generate_maker_keys(&tweakable_point, 1)?;

        // For Taproot, generate hashlock nonces for ALL hops (one per maker)
        // and derive the tweaked hashlock pubkey for the first hop.
        let (taproot_hashlock_nonces, taproot_hashlock_pubkey) =
            if protocol == ProtocolVersion::Taproot {
                let nonces: Vec<SecretKey> = (0..maker_count)
                    .map(|_| SecretKey::new(&mut OsRng))
                    .collect();
                let pubkey = calculate_pubkey_from_nonce(&tweakable_point, &nonces[0])?;
                (Some(nonces), Some(pubkey))
            } else {
                (None, None)
            };

        {
            let swap = self.swap_state_mut()?;
            // Multisig nonces are only used by Legacy for ProofOfFunding recovery.
            // Taproot uses a single aggregated key, so these are not needed.
            if protocol == ProtocolVersion::Legacy {
                swap.multisig_nonces = multisig_nonces;
            }
            if let Some(ref nonces) = taproot_hashlock_nonces {
                swap.hashlock_nonces = nonces.clone();
            } else {
                swap.hashlock_nonces = hashlock_nonces;
            }
        }

        let mut wallet = self.write_wallet()?;

        let network = wallet.store.network;

        let swapcoins = match protocol {
            ProtocolVersion::Legacy => Self::funding_create_legacy(
                &mut wallet,
                &multisig_pubkeys,
                &hashlock_pubkeys,
                hashvalue,
                refund_locktime,
                send_amount,
                &swap_id,
                network,
                manually_selected_outpoints,
            )?,
            ProtocolVersion::Taproot => Self::funding_create_taproot(
                &mut wallet,
                &[tweakable_point],
                &[taproot_hashlock_pubkey.expect("taproot hashlock pubkey must be set")],
                preimage,
                refund_locktime,
                send_amount,
                &swap_id,
                network,
                manually_selected_outpoints,
            )?,
        };

        for swapcoin in &swapcoins {
            wallet.add_unified_outgoing_swapcoin(swapcoin);
        }

        wallet.save_to_disk()?;
        drop(wallet);

        let swap = self.swap_state_mut()?;
        let num_swapcoins = swapcoins.len();
        swap.outgoing_swapcoins = swapcoins;

        log::info!("Created {} outgoing swapcoins for funding", num_swapcoins);
        Ok(())
    }

    /// Perform handshake with a maker and verify protocol support.
    pub(crate) fn net_handshake(
        &self,
        stream: &mut TcpStream,
    ) -> Result<ProtocolVersion, TakerError> {
        // Send TakerHello
        send_message(stream, &TakerToMakerMessage::TakerHello(TakerHello))?;

        let msg_bytes = read_message(stream)?;
        let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

        match msg {
            MakerToTakerMessage::MakerHello(maker_hello) => {
                let desired = self.swap_state()?.params.protocol;
                if maker_hello.supported_protocols.contains(&desired) {
                    Ok(desired)
                } else {
                    Err(TakerError::General(format!(
                        "Maker does not support {:?}. Supported: {:?}",
                        desired, maker_hello.supported_protocols
                    )))
                }
            }
            _ => Err(TakerError::General(
                "Expected MakerHello response".to_string(),
            )),
        }
    }

    /// Returns the effective connection type, accounting for integration-test mode.
    fn net_effective_type(&self) -> ConnectionType {
        if cfg!(feature = "integration-test") {
            ConnectionType::Clearnet
        } else {
            self.config.connection_type
        }
    }

    /// Connect to a maker using either direct connection or Tor proxy.
    pub(crate) fn net_connect(&self, address: &str) -> Result<TcpStream, TakerError> {
        log::debug!("Connecting to maker at {}", address);
        let timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);

        let socket = match self.net_effective_type() {
            ConnectionType::Clearnet => TcpStream::connect(address).map_err(|e| {
                TakerError::General(format!("Failed to connect to {}: {}", address, e))
            })?,
            ConnectionType::Tor => {
                let socks_addr = format!("127.0.0.1:{}", self.config.socks_port);
                Socks5Stream::connect(socks_addr.as_str(), address)
                    .map_err(|e| {
                        TakerError::General(format!(
                            "Failed to connect to {} via Tor: {}",
                            address, e
                        ))
                    })?
                    .into_inner()
            }
        };

        socket
            .set_read_timeout(Some(timeout))
            .and_then(|_| socket.set_write_timeout(Some(timeout)))
            .map_err(|e| TakerError::General(format!("Failed to set socket timeout: {}", e)))?;

        Ok(socket)
    }

    /// Wait for specific transaction IDs to be confirmed.
    ///
    /// If a `BreachDetector` is provided, checks its atomic flag each poll cycle.
    /// Returns `ContractsBroadcasted` immediately if the detector signals a breach.
    pub(crate) fn net_wait_for_confirmation(
        &self,
        txids: &[Txid],
        breach_detector: Option<&BreachDetector>,
    ) -> Result<(), TakerError> {
        let required_confirms = self.swap_state()?.params.required_confirms;
        if required_confirms == 0 || txids.is_empty() {
            return Ok(());
        }

        log::info!(
            "Waiting for {} confirmation(s) on {} transaction(s)...",
            required_confirms,
            txids.len()
        );

        let start = Instant::now();
        let timeout = if cfg!(feature = "integration-test") {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(600)
        };

        loop {
            let mut all_confirmed = true;

            {
                let wallet = self.read_wallet()?;
                for txid in txids {
                    match wallet.rpc.get_raw_transaction_info(txid, None) {
                        Ok(tx_info) => {
                            let confirms = tx_info.confirmations.unwrap_or(0);
                            if confirms < required_confirms {
                                log::debug!(
                                    "Tx {} has {} confirmations (need {})",
                                    txid,
                                    confirms,
                                    required_confirms
                                );
                                all_confirmed = false;
                            }
                        }
                        Err(e) => {
                            log::debug!("Error getting tx info for {}: {:?}", txid, e);
                            all_confirmed = false;
                        }
                    }
                }
            }

            if all_confirmed {
                log::info!("All transactions confirmed");
                return Ok(());
            }

            // Check for adversarial contract activity via the background detector thread.
            if let Some(detector) = breach_detector {
                if detector.is_breached() {
                    log::warn!("Breach detected by background detector — aborting wait");
                    return Err(TakerError::ContractsBroadcasted(vec![]));
                }
            }

            if start.elapsed() > timeout {
                return Err(TakerError::FundingTxWaitTimeOut);
            }

            thread::sleep(Duration::from_secs(5));
        }
    }

    /// Finalize the swap by revealing preimage and exchanging private keys.
    fn finalize_swap(&mut self) -> Result<(), TakerError> {
        log::info!("Finalizing swap...");

        let maker_privkeys = self.finalize_collect_privkeys()?;
        self.finalize_set_incoming_privkey(&maker_privkeys)?;

        // SP8: All maker privkeys received.
        self.persist_phase(SwapPhase::PrivkeysCollected)?;

        self.finalize_forward_privkeys(&maker_privkeys)?;

        // SP9: Inter-maker privkey forwarding done.
        self.persist_phase(SwapPhase::PrivkeysForwarded)?;

        self.finalize_persist_incoming()?;

        log::info!("Swap finalized successfully");
        Ok(())
    }

    /// Attempt finalization with retries and watch tower polling between attempts.
    /// `finalize_swap()` is idempotent (re-sending preimage returns the same privkey),
    /// so retrying is safe.
    fn finalize_with_retry(&mut self) -> Result<(), TakerError> {
        for attempt in 1..=MAX_FINALIZE_RETRIES {
            match self.finalize_swap() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    log::warn!(
                        "Finalization attempt {}/{} failed: {:?}",
                        attempt,
                        MAX_FINALIZE_RETRIES,
                        e
                    );

                    if self.monitor_adversarial_spend() {
                        log::error!("Adversarial contract spend detected — aborting retries");
                        return Err(TakerError::General(
                            "Adversarial contract spend detected during finalization".to_string(),
                        ));
                    }

                    if attempt < MAX_FINALIZE_RETRIES {
                        log::info!("Retrying in {:?}...", FINALIZE_RETRY_DELAY);
                        thread::sleep(FINALIZE_RETRY_DELAY);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }

    /// Poll the watch service for spends on our swap's contract outpoints.
    /// Returns true if an adversarial spend was detected.
    fn monitor_adversarial_spend(&self) -> bool {
        let contract_outpoints: Vec<OutPoint> = match self.swap_state() {
            Ok(swap) => swap
                .outgoing_swapcoins
                .iter()
                .map(|sc| OutPoint {
                    txid: sc.contract_tx.compute_txid(),
                    vout: 0,
                })
                .collect(),
            Err(_) => return false,
        };

        while let Some(event) = self.watch_service.poll_event() {
            if let WatcherEvent::UtxoSpent { outpoint, .. } = event {
                if contract_outpoints.contains(&outpoint) {
                    log::warn!(
                        "Watch service detected spend of contract output: {}",
                        outpoint
                    );
                    return true;
                }
            }
        }
        false
    }

    /// Phase 1: Reveal preimage to each maker (in reverse order) and collect their private keys.
    fn finalize_collect_privkeys(
        &mut self,
    ) -> Result<Vec<Option<bitcoin::secp256k1::SecretKey>>, TakerError> {
        let swap = self.swap_state()?;
        let num_makers = swap.makers.len();
        let protocol = swap.params.protocol;
        let swap_id = swap.id.clone();
        let preimage = swap.preimage;

        let mut maker_privkeys: Vec<Option<bitcoin::secp256k1::SecretKey>> = vec![None; num_makers];

        for i in (0..num_makers).rev() {
            let maker_address = self.swap_state()?.makers[i]
                .offer_and_address
                .address
                .to_string();
            let mut stream = self.net_connect(&maker_address)?;

            self.net_handshake(&mut stream)?;

            let preimage_msg = Self::msg_build_preimage(protocol, swap_id.clone(), preimage);
            send_message(&mut stream, &preimage_msg)?;
            self.swap_state_mut()?.makers[i]
                .finalization
                .preimage_revealed = true;

            let msg_bytes = read_message(&mut stream)?;
            let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

            let received_privkey = match msg {
                MakerToTakerMessage::LegacyPrivateKeyHandover(handover)
                | MakerToTakerMessage::TaprootPrivateKeyHandover(handover) => {
                    log::info!("Received private key from maker {}", i);
                    handover.privkeys.first().map(|p| p.key)
                }
                _ => {
                    return Err(TakerError::General(format!(
                        "Unexpected message from maker {}: expected PrivateKeyHandover",
                        i
                    )));
                }
            };

            maker_privkeys[i] = received_privkey;
            self.swap_state_mut()?.makers[i]
                .finalization
                .privkey_received = true;

            if i == 0 {
                let swap = self.swap_state()?;
                if let Some(outgoing) = swap.outgoing_swapcoins.first() {
                    if let Some(privkey) = outgoing.my_privkey {
                        let msg = Self::msg_build_handover(protocol, swap_id.clone(), privkey);
                        send_message(&mut stream, &msg)?;
                    }
                }
            }
        }

        Ok(maker_privkeys)
    }

    fn finalize_set_incoming_privkey(
        &mut self,
        maker_privkeys: &[Option<bitcoin::secp256k1::SecretKey>],
    ) -> Result<(), TakerError> {
        let num_makers = maker_privkeys.len();
        if let Some(last_maker_privkey) = maker_privkeys.get(num_makers - 1).and_then(|p| *p) {
            if let Some(incoming) = self.swap_state_mut()?.incoming_swapcoins.last_mut() {
                incoming.set_other_privkey(last_maker_privkey);
                log::info!(
                    "Set taker's incoming swapcoin other_privkey from last maker ({})",
                    num_makers - 1
                );
            }
        }
        Ok(())
    }

    fn finalize_forward_privkeys(
        &mut self,
        maker_privkeys: &[Option<bitcoin::secp256k1::SecretKey>],
    ) -> Result<(), TakerError> {
        let num_makers = maker_privkeys.len();
        let protocol = self.swap_state()?.params.protocol;
        let swap_id = self.swap_state()?.id.clone();

        for i in 1..num_makers {
            if let Some(prev_maker_privkey) = maker_privkeys.get(i - 1).and_then(|p| *p) {
                let maker_address = self.swap_state()?.makers[i]
                    .offer_and_address
                    .address
                    .to_string();
                let mut stream = self.net_connect(&maker_address)?;

                self.net_handshake(&mut stream)?;

                log::info!(
                    "Forwarding maker {}'s outgoing privkey to maker {}",
                    i - 1,
                    i
                );

                let msg = Self::msg_build_handover(protocol, swap_id.clone(), prev_maker_privkey);
                send_message(&mut stream, &msg)?;
                self.swap_state_mut()?.makers[i]
                    .finalization
                    .privkey_forwarded = true;
            } else {
                log::warn!("No privkey from maker {} to forward to maker {}", i - 1, i);
            }
        }

        Ok(())
    }

    /// Persist the taker's incoming swapcoins with their preimage to the wallet.
    fn finalize_persist_incoming(&mut self) -> Result<(), TakerError> {
        // Set preimage on the incoming swapcoin before acquiring the wallet lock,
        // since swap_state_mut borrows self which conflicts with write_wallet.
        let swap = self.swap_state_mut()?;
        if let Some(incoming) = swap.incoming_swapcoins.last_mut() {
            incoming.set_preimage(swap.preimage);
        }

        let mut wallet = self.write_wallet()?;
        if let Some(incoming) = self.swap_state()?.incoming_swapcoins.last() {
            wallet.add_unified_incoming_swapcoin(incoming);
        }

        wallet.save_to_disk()?;
        Ok(())
    }

    /// Create a protocol-appropriate preimage reveal message.
    fn msg_build_preimage(
        protocol: ProtocolVersion,
        swap_id: String,
        preimage: [u8; 32],
    ) -> TakerToMakerMessage {
        match protocol {
            ProtocolVersion::Legacy => {
                let msg = LegacyHashPreimage::new(swap_id, preimage, vec![], vec![]);
                TakerToMakerMessage::LegacyHashPreimage(msg)
            }
            ProtocolVersion::Taproot => {
                let msg = TaprootHashPreimage::new(swap_id, preimage);
                TakerToMakerMessage::TaprootHashPreimage(msg)
            }
        }
    }

    /// Create a protocol-appropriate private key handover message.
    fn msg_build_handover(
        protocol: ProtocolVersion,
        swap_id: String,
        privkey: SecretKey,
    ) -> TakerToMakerMessage {
        let handover = PrivateKeyHandover {
            id: swap_id,
            privkeys: vec![SwapPrivkey {
                identifier: bitcoin::ScriptBuf::new(),
                key: privkey,
            }],
        };
        match protocol {
            ProtocolVersion::Legacy => TakerToMakerMessage::LegacyPrivateKeyHandover(handover),
            ProtocolVersion::Taproot => TakerToMakerMessage::TaprootPrivateKeyHandover(handover),
        }
    }

    /// Check if any contract transactions have been broadcast on-chain.
    #[allow(dead_code)]
    pub(crate) fn monitor_contract_broadcasts(&self) -> Result<Vec<Txid>, TakerError> {
        let swap = self.swap_state()?;
        let contract_txids: Vec<Txid> = swap
            .incoming_swapcoins
            .iter()
            .map(|sc| sc.contract_tx.compute_txid())
            .chain(
                swap.outgoing_swapcoins
                    .iter()
                    .map(|sc| sc.contract_tx.compute_txid()),
            )
            .chain(
                swap.watchonly_swapcoins
                    .iter()
                    .map(|sc| sc.contract_tx.compute_txid()),
            )
            .collect();

        let wallet = self.read_wallet()?;
        let seen: Vec<Txid> = contract_txids
            .iter()
            .filter(|txid| wallet.rpc.get_raw_transaction_info(txid, None).is_ok())
            .cloned()
            .collect();

        if !seen.is_empty() {
            log::warn!("Detected {} broadcasted contract transactions!", seen.len());
        }
        Ok(seen)
    }

    /// Build a `SwapRecord` from the current `OngoingSwapState`.
    fn persist_build_record(&self, swap: &OngoingSwapState) -> Result<SwapRecord, TakerError> {
        let now = now_secs();
        Ok(SwapRecord {
            swap_id: swap.id.clone(),
            preimage: swap.preimage,
            protocol: swap.params.protocol,
            send_amount_sat: swap.params.send_amount.to_sat(),
            maker_count: swap.params.maker_count,
            phase: swap.phase,
            failed_at_phase: None,
            failure_reason: None,
            makers: swap
                .makers
                .iter()
                .map(|m| MakerProgress {
                    address: m.offer_and_address.address.to_string(),
                    negotiated: m.tweakable_point.is_some(),
                    exchange: m.exchange.clone(),
                    finalization: m.finalization.clone(),
                })
                .collect(),
            outgoing_contract_txids: swap
                .outgoing_swapcoins
                .iter()
                .map(|sc| sc.contract_tx.compute_txid())
                .collect(),
            incoming_contract_txids: swap
                .incoming_swapcoins
                .iter()
                .map(|sc| sc.contract_tx.compute_txid())
                .collect(),
            watchonly_contract_txids: swap
                .watchonly_swapcoins
                .iter()
                .map(|sc| sc.contract_tx.compute_txid())
                .collect(),
            recovery: RecoveryState::default(),
            multisig_nonces: swap
                .multisig_nonces
                .iter()
                .map(|k| SerializableSecretKey::from(*k))
                .collect(),
            hashlock_nonces: swap
                .hashlock_nonces
                .iter()
                .map(|k| SerializableSecretKey::from(*k))
                .collect(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Persist the current swap state at the given phase to the tracker.
    ///
    /// Always rebuilds the full record from `OngoingSwapState` so that maker
    /// progress, txids, and nonces stay up-to-date. Preserves `created_at`
    /// and `recovery` from any existing record.
    pub(crate) fn persist_phase(&mut self, phase: SwapPhase) -> Result<(), TakerError> {
        let swap = self.swap_state_mut()?;
        swap.phase = phase;

        // Snapshot preserved fields from any existing record.
        let swap_id = self.swap_state()?.id.clone();
        let existing = self.swap_tracker.get_record(&swap_id);
        let created_at = existing.map(|r| r.created_at);
        let recovery = existing.map(|r| r.recovery.clone());
        let failed_at = existing.and_then(|r| r.failed_at_phase);
        let failure_reason = existing.and_then(|r| r.failure_reason.clone());

        // Build full record from current live state.
        let swap_ref = self.swap_state()?;
        let mut record = self.persist_build_record(swap_ref)?;

        // Restore preserved fields so we don't lose recovery progress or timestamps.
        if let Some(ts) = created_at {
            record.created_at = ts;
        }
        if let Some(rec) = recovery {
            record.recovery = rec;
        }
        if let Some(fat) = failed_at {
            record.failed_at_phase = Some(fat);
        }
        if let Some(reason) = failure_reason {
            record.failure_reason = Some(reason);
        }

        self.swap_tracker.save_record(&record)
    }

    /// Persist the current maker progress without changing the swap phase.
    ///
    /// Call this after updating progress flags on `MakerConnection` so the
    /// periodic tracker display (which reads from the CBOR file) stays current.
    pub(crate) fn persist_progress(&mut self) -> Result<(), TakerError> {
        let phase = self.swap_state()?.phase;
        self.persist_phase(phase)
    }

    /// Persist a swap failure (SP-ERR) with the phase at which failure occurred.
    fn persist_failure(&mut self, failed_at: SwapPhase, error: &TakerError) {
        if let Ok(swap) = self.swap_state() {
            let swap_id = swap.id.clone();
            if let Ok(mut record) = self.persist_build_record(swap) {
                record.phase = SwapPhase::Failed;
                record.failed_at_phase = Some(failed_at);
                record.failure_reason = Some(format!("{:?}", error));
                // Preserve existing recovery state if resuming
                if let Some(existing) = self.swap_tracker.get_record(&swap_id) {
                    record.recovery = existing.recovery.clone();
                    record.created_at = existing.created_at;
                }
                record.updated_at = now_secs();
                if let Err(e) = self.swap_tracker.save_record(&record) {
                    log::error!("Failed to persist swap failure: {:?}", e);
                }
            }
        }
    }

    /// Recover from a failed swap by spending contract outputs back to wallet.
    ///
    /// Uses the swap tracker for crash-resilient, idempotent recovery.
    /// Each recovery phase is gated on `record.recovery.phase` so that
    /// re-execution after a crash skips already-completed work.
    pub fn recover_active_swap(&mut self) -> Result<(), TakerError> {
        log::warn!("Starting swap recovery...");

        let swap_id = self.swap_state()?.id.clone();

        // Phase 1: Stamp preimage on all incoming swapcoins (idempotent)
        let preimage = self.swap_state()?.preimage;
        let swap = self.swap_state_mut()?;
        for incoming in &mut swap.incoming_swapcoins {
            if incoming.hash_preimage.is_none() {
                incoming.set_preimage(preimage);
            }
        }

        // Update tracker: RSP1
        self.recover_update_phase(&swap_id, RecoveryPhase::PreimageStamped)?;

        // Phase 2: Persist all swapcoins to wallet (idempotent — add is upsert)
        {
            let mut wallet = self.write_wallet()?;
            let swap = self.swap_state()?;
            for outgoing in &swap.outgoing_swapcoins {
                wallet.add_unified_outgoing_swapcoin(outgoing);
            }
            if let Some(incoming) = swap.incoming_swapcoins.last() {
                wallet.add_unified_incoming_swapcoin(incoming);
            }
            wallet.save_to_disk()?;
        }

        // Update tracker: RSP2
        self.recover_update_phase(&swap_id, RecoveryPhase::SwapcoinsPersisted)?;

        // Phase 3: Recover incoming swapcoins (we have hashlock privilege)
        self.recover_incoming(&swap_id)?;

        // Only mark IncomingRecovered if incoming contracts were actually
        // resolved (swept or none existed). If some are still being watched,
        // stay at SwapcoinsPersisted so retry re-attempts the sweep.
        if self.recover_is_incoming_done(&swap_id) {
            self.recover_update_phase(&swap_id, RecoveryPhase::IncomingRecovered)?;
        }

        // Phase 4: Recover outgoing swapcoins (must wait for timelock)
        self.recover_outgoing(&swap_id)?;

        // Only advance if outgoing recovery is actually resolved:
        // all contracts recovered, or incoming were swept via hashlock
        // (so makers will claim outgoing with the revealed preimage).
        if self.recover_is_outgoing_done(&swap_id) {
            self.recover_update_phase(&swap_id, RecoveryPhase::OutgoingRecovered)?;

            // Phase 5: Clean up watch-only entries + clear swap state
            {
                let swap_id_cleanup = self.swap_state()?.id.clone();
                let mut wallet = self.write_wallet()?;
                wallet.remove_unified_watchonly_swapcoins(&swap_id_cleanup);
                wallet.save_to_disk()?;
            }
            self.recover_update_phase(&swap_id, RecoveryPhase::CleanedUp)?;
        }

        // Mark as terminal Failed in tracker
        self.swap_tracker.update_and_save(&swap_id, |record| {
            record.phase = SwapPhase::Failed;
        })?;

        self.ongoing_swap = None;
        log::info!("Recovery completed.");
        Ok(())
    }

    /// Recover from an incomplete swap using a tracker record (on restart).
    ///
    /// This is called from `init_recover_incomplete()` when a swap record
    /// is found in the tracker with `phase >= FundsBroadcast`.
    fn recover_from_record(&mut self, swap_id: &str) -> Result<(), TakerError> {
        let record = self
            .swap_tracker
            .get_record(swap_id)
            .ok_or_else(|| TakerError::General(format!("No tracker record for {}", swap_id)))?;

        let preimage = record.preimage;
        let recovery_phase = record.recovery.phase;

        log::info!(
            "Recovering swap {} from phase {:?}, recovery at {:?}",
            swap_id,
            record.phase,
            recovery_phase
        );

        // Phase 1: Stamp preimage on all wallet incoming swapcoins (idempotent)
        if recovery_phase < RecoveryPhase::PreimageStamped {
            let mut wallet = self.write_wallet()?;
            // Get txids of all incoming swapcoins, then stamp preimage on each
            let incoming_outpoints = wallet.unified_incoming_contract_outpoints();
            let incoming_txid_strs: Vec<String> = incoming_outpoints
                .iter()
                .map(|op| op.txid.to_string())
                .collect();
            for txid_str in &incoming_txid_strs {
                if let Some(incoming) = wallet.find_unified_incoming_swapcoin_mut(txid_str) {
                    if incoming.hash_preimage.is_none() {
                        incoming.set_preimage(preimage);
                    }
                }
            }
            wallet.save_to_disk()?;
            drop(wallet);
            self.recover_update_phase(swap_id, RecoveryPhase::PreimageStamped)?;
        }

        // Phase 2: Swapcoins should already be in wallet from the active swap.
        // If they aren't (crash before wallet save), we can't recover them from
        // the record alone. Mark as persisted and proceed.
        if recovery_phase < RecoveryPhase::SwapcoinsPersisted {
            self.recover_update_phase(swap_id, RecoveryPhase::SwapcoinsPersisted)?;
        }

        // Phase 3: Recover incoming
        if recovery_phase < RecoveryPhase::IncomingRecovered {
            self.recover_incoming(swap_id)?;
            if self.recover_is_incoming_done(swap_id) {
                self.recover_update_phase(swap_id, RecoveryPhase::IncomingRecovered)?;
            }
        }

        // Phase 4: Recover outgoing
        if recovery_phase < RecoveryPhase::OutgoingRecovered {
            self.recover_outgoing(swap_id)?;
            if self.recover_is_outgoing_done(swap_id) {
                self.recover_update_phase(swap_id, RecoveryPhase::OutgoingRecovered)?;
            }
        }

        // Phase 5: Cleanup
        if recovery_phase < RecoveryPhase::CleanedUp && self.recover_is_outgoing_done(swap_id) {
            {
                let mut wallet = self.write_wallet()?;
                wallet.remove_unified_watchonly_swapcoins(swap_id);
                wallet.save_to_disk()?;
            }
            self.recover_update_phase(swap_id, RecoveryPhase::CleanedUp)?;
        }

        // Mark as terminal Failed only if cleanup is done, otherwise
        // leave the phase so incomplete_swaps() picks it up for retry.
        let is_cleaned_up = self
            .swap_tracker
            .get_record(swap_id)
            .map(|r| r.recovery.phase >= RecoveryPhase::CleanedUp)
            .unwrap_or(true);
        if is_cleaned_up {
            self.swap_tracker.update_and_save(swap_id, |record| {
                record.phase = SwapPhase::Failed;
            })?;
        }

        log::info!("Recovery from record completed for swap {}", swap_id);
        Ok(())
    }

    /// Update the recovery phase for a swap in the tracker.
    fn recover_update_phase(
        &mut self,
        swap_id: &str,
        phase: RecoveryPhase,
    ) -> Result<(), TakerError> {
        self.swap_tracker.update_and_save(swap_id, |r| {
            r.recovery.phase = phase;
        })?;
        Ok(())
    }

    /// Check whether incoming contracts were actually recovered.
    ///
    /// Returns `true` only if at least one incoming contract was swept.
    /// "No incoming contracts" is not the same as "incoming recovered" —
    /// the phase should reflect what actually happened.
    fn recover_is_incoming_done(&self, swap_id: &str) -> bool {
        let Some(record) = self.swap_tracker.get_record(swap_id) else {
            return false;
        };
        !record.recovery.incoming_swept.is_empty()
    }

    /// Check whether outgoing recovery is complete enough to proceed to cleanup.
    ///
    /// Returns `true` if we can advance past `OutgoingRecovered`:
    /// - All outgoing contracts were recovered (none still watching), OR
    /// - Incoming contracts were swept via hashlock, meaning the preimage is
    ///   public and makers will claim the outgoing contracts themselves.
    fn recover_is_outgoing_done(&self, swap_id: &str) -> bool {
        let Some(record) = self.swap_tracker.get_record(swap_id) else {
            return true;
        };
        let has_unresolved_outgoing = !record.recovery.outgoing_watching.is_empty();
        let incoming_swept = !record.recovery.incoming_swept.is_empty();

        if has_unresolved_outgoing && !incoming_swept {
            log::info!(
                "Outgoing contracts still being watched and no incoming swept — \
                 deferring OutgoingRecovered for swap {}",
                swap_id
            );
            return false;
        }
        true
    }

    /// Wait for incoming contract transactions to appear on-chain.
    ///
    /// The maker's contract tx may still be in the mempool when recovery runs
    /// immediately after a drop. Without this wait, `sweep_unified_incoming_swapcoins`
    /// skips the UTXO ("not found on chain") and falls through to timelock-only recovery.
    fn recover_wait_for_contracts(&self, txids: &[Txid]) -> Result<(), TakerError> {
        if txids.is_empty() {
            return Ok(());
        }

        let timeout = if cfg!(feature = "integration-test") {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(300)
        };
        let start = Instant::now();

        log::info!(
            "Waiting for {} incoming contract tx(es) to confirm...",
            txids.len()
        );

        loop {
            let all_confirmed = {
                let wallet = self.read_wallet()?;
                txids.iter().all(|txid| {
                    wallet
                        .rpc
                        .get_raw_transaction_info(txid, None)
                        .ok()
                        .and_then(|info| info.confirmations)
                        .unwrap_or(0)
                        >= 1
                })
            };

            if all_confirmed {
                log::info!("All incoming contract transactions confirmed");
                return Ok(());
            }

            if start.elapsed() > timeout {
                log::warn!(
                    "Timed out waiting for incoming contract confirmations, proceeding anyway"
                );
                return Ok(());
            }

            thread::sleep(Duration::from_secs(3));
        }
    }

    /// Recover incoming swapcoins using protocol-dispatched strategy.
    ///
    /// Tracks per-swapcoin results in the swap record for crash resilience.
    fn recover_incoming(&mut self, swap_id: &str) -> Result<(), TakerError> {
        // Get existing swept/watching txids to skip
        let (already_swept, already_watching) = self
            .swap_tracker
            .get_record(swap_id)
            .map(|r| {
                (
                    r.recovery.incoming_swept.clone(),
                    r.recovery.incoming_watching.clone(),
                )
            })
            .unwrap_or_default();

        // Wait for incoming contract UTXOs to be confirmed before attempting sweep.
        // The maker broadcasts its contract tx during exchange_taproot(), but
        // it may not be in a block yet when recovery runs immediately after a drop.
        let incoming_txids: Vec<Txid> = {
            let wallet = self.read_wallet()?;
            wallet
                .unified_incoming_contract_outpoints()
                .iter()
                .map(|op| op.txid)
                .filter(|txid| !already_swept.contains(txid) && !already_watching.contains(txid))
                .collect()
        };
        self.recover_wait_for_contracts(&incoming_txids)?;

        // Sync wallet after confirmation so the UTXO set is up-to-date
        if !incoming_txids.is_empty() {
            let mut wallet = self.write_wallet()?;
            wallet.sync_and_save().map_err(TakerError::Wallet)?;
        }

        // Try sweeping incoming swapcoins — collect results, then drop wallet lock
        let swept_txids = {
            let mut wallet = self.write_wallet()?;
            match wallet.sweep_unified_incoming_swapcoins(2.0) {
                Ok(swept) if !swept.is_empty() => {
                    log::info!("Swept {} incoming contracts", swept.len());
                    swept
                }
                Ok(_) => {
                    log::info!("No incoming contracts swept");
                    vec![]
                }
                Err(e) => {
                    log::warn!("Incoming sweep failed: {:?}", e);
                    vec![]
                }
            }
        };

        // Collect newly swept txids
        let new_swept: Vec<Txid> = swept_txids
            .iter()
            .filter(|txid| !already_swept.contains(txid))
            .copied()
            .collect();

        // Get remaining incoming outpoints — collect, then drop wallet lock
        let remaining_outpoints = {
            match self.read_wallet() {
                Ok(wallet) => wallet.unified_incoming_contract_outpoints(),
                Err(_) => vec![],
            }
        };

        // Register remaining for monitoring
        let mut new_watching = Vec::new();
        for outpoint in &remaining_outpoints {
            let txid = outpoint.txid;
            if !already_swept.contains(&txid)
                && !swept_txids.contains(&txid)
                && !already_watching.contains(&txid)
            {
                self.watch_service.register_watch_request(*outpoint);
                new_watching.push(txid);
            }
        }

        // Single update_and_save for all mutations
        if !new_swept.is_empty() || !new_watching.is_empty() {
            self.swap_tracker.update_and_save(swap_id, |record| {
                record.recovery.incoming_swept.extend(&new_swept);
                record.recovery.incoming_watching.extend(&new_watching);
            })?;
        }

        Ok(())
    }

    /// Recover outgoing swapcoins using protocol-dispatched strategy.
    ///
    /// Tracks per-swapcoin results in the swap record for crash resilience.
    fn recover_outgoing(&mut self, swap_id: &str) -> Result<(), TakerError> {
        // Get existing recovered/watching txids to skip
        let (already_recovered, already_watching) = self
            .swap_tracker
            .get_record(swap_id)
            .map(|r| {
                (
                    r.recovery.outgoing_recovered.clone(),
                    r.recovery.outgoing_watching.clone(),
                )
            })
            .unwrap_or_default();

        // Try immediate timelock recovery — collect results, then drop wallet lock
        let recovered_txids = {
            let mut wallet = self.write_wallet()?;
            match wallet.recover_unified_timelocked_swapcoins(2.0) {
                Ok(recovered) if !recovered.is_empty() => {
                    log::info!(
                        "Recovered {} timelocked outgoing contracts",
                        recovered.len()
                    );
                    recovered
                }
                Ok(_) => {
                    log::info!("No outgoing contracts ready for timelock recovery yet");
                    vec![]
                }
                Err(e) => {
                    log::warn!("Timelock recovery attempt failed: {:?}", e);
                    vec![]
                }
            }
        };

        // Collect newly recovered txids
        let new_recovered: Vec<Txid> = recovered_txids
            .iter()
            .filter(|txid| !already_recovered.contains(txid))
            .copied()
            .collect();

        // Get remaining outgoing outpoints — collect, then drop wallet lock
        let remaining_outpoints = {
            match self.read_wallet() {
                Ok(wallet) => wallet.unified_outgoing_contract_outpoints(),
                Err(_) => vec![],
            }
        };

        // Register remaining for monitoring
        let mut new_watching = Vec::new();
        for outpoint in &remaining_outpoints {
            let txid = outpoint.txid;
            if !already_recovered.contains(&txid)
                && !recovered_txids.contains(&txid)
                && !already_watching.contains(&txid)
            {
                self.watch_service.register_watch_request(*outpoint);
                new_watching.push(txid);
                log::info!("Registered outgoing contract {} for monitoring", outpoint);
            }
        }

        // Single update_and_save for all mutations
        if !new_recovered.is_empty() || !new_watching.is_empty() {
            self.swap_tracker.update_and_save(swap_id, |record| {
                record.recovery.outgoing_recovered.extend(&new_recovered);
                record.recovery.outgoing_watching.extend(&new_watching);
            })?;
        }

        Ok(())
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

/// Unified taker behavior for testing.
#[cfg(feature = "integration-test")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnifiedTakerBehavior {
    /// Normal behavior.
    #[default]
    Normal,
    /// Close connection early (after maker selection).
    CloseEarly,
    /// Drop after funds/contracts are broadcast but before finalization.
    /// Simulates a taker crash after funds are on-chain.
    DropAfterFundsBroadcast,
}
