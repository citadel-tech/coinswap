//! Unified Taker API for both Legacy (ECDSA) and Taproot (MuSig2) protocols.

use std::{
    net::TcpStream,
    path::PathBuf,
    sync::{mpsc, Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
    thread,
    time::{Duration, Instant},
};

pub(crate) use super::swap_tracker::SwapPhase;
use super::swap_tracker::{
    now_secs, ContractOutcome, ContractResolution, ExchangeProgress, FinalizationProgress,
    LegacyExchangeProgress, MakerProgress, RecoveryState, SerializableSecretKey,
    SwapRecord, SwapTracker, TaprootExchangeProgress,
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
        router::{MakerToTakerMessage, TakerToMakerMessage},
    },
    utill::{check_tor_status, generate_maker_keys, get_taker_dir, read_message, send_message},
    wallet::{
        unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin, WatchOnlySwapCoin},
        RPCConfig, RecoveryOutcome, Wallet,
    },
    watch_tower::{
        registry_storage::FileRegistry,
        rpc_backend::BitcoinRpc,
        service::WatchService,
        watcher::{Role, Watcher},
        zmq_backend::ZmqBackend,
    },
};

use super::{
    background_services::{BreachDetector, RecoveryLoop},
    config::TakerConfig,
    error::TakerError,
    offers::{
        MakerOfferCandidate, MakerProtocol, OfferAndAddress, OfferBook, OfferBookHandle,
        OfferSyncHandle, OfferSyncService,
    },
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

/// Base refund locktime (in blocks) for the innermost hop.
///
/// In integration tests the idle-connection timeout fires after ~200 blocks
/// (60 s at 10 blocks / 3 s).  The base must exceed that so makers have time
/// to detect the drop and sweep via hashlock before the outer timelocks expire.
#[cfg(not(feature = "integration-test"))]
pub(crate) const REFUND_LOCKTIME_BASE: u16 = 20;
#[cfg(feature = "integration-test")]
pub(crate) const REFUND_LOCKTIME_BASE: u16 = 150;

/// Locktime increment per hop in the swap route.
#[cfg(not(feature = "integration-test"))]
pub(crate) const REFUND_LOCKTIME_STEP: u16 = 20;
#[cfg(feature = "integration-test")]
pub(crate) const REFUND_LOCKTIME_STEP: u16 = 75;

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
            socks_port: 9050,
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
    pub(crate) fn legacy_exchange_mut(&mut self) -> Result<&mut LegacyExchangeProgress, TakerError> {
        match &mut self.exchange {
            ExchangeProgress::Legacy(ref mut l) => Ok(l),
            _ => Err(TakerError::General("Expected Legacy exchange progress".to_string())),
        }
    }

    /// Get mutable reference to Taproot exchange progress.
    pub(crate) fn taproot_exchange_mut(&mut self) -> Result<&mut TaprootExchangeProgress, TakerError> {
        match &mut self.exchange {
            ExchangeProgress::Taproot(ref mut t) => Ok(t),
            _ => Err(TakerError::General("Expected Taproot exchange progress".to_string())),
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
    pub(crate) swap_tracker: Arc<Mutex<SwapTracker>>,
    /// Background recovery loop (active when incomplete swap recovery is in progress).
    recovery_loop: Option<RecoveryLoop>,
    /// Breach detector for legacy swaps (monitors funding outpoints for adversarial contract broadcasts).
    pub(crate) breach_detector: Option<BreachDetector>,
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
                if let Err(e) = self.swap_tracker.lock().unwrap().save_record(&record) {
                    log::error!("Failed to flush swap tracker on shutdown: {:?}", e);
                }
            }
        }
        // Shut down background recovery loop (if running)
        if let Some(recovery) = self.recovery_loop.take() {
            log::info!("Shutting down recovery loop");
            drop(recovery);
        }
        // Shut down breach detector (if running)
        if let Some(detector) = self.breach_detector.take() {
            log::info!("Shutting down breach detector");
            detector.stop();
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
        let swap_tracker = Arc::new(Mutex::new(SwapTracker::load_or_create(&data_dir)?));
        swap_tracker.lock().unwrap().cleanup_incomplete();

        let mut taker = UnifiedTaker {
            config,
            wallet: Arc::new(RwLock::new(wallet)),
            offerbook,
            watch_service,
            offer_sync_handle,
            ongoing_swap: None,
            swap_tracker,
            recovery_loop: None,
            breach_detector: None,
            #[cfg(feature = "integration-test")]
            behavior: UnifiedTakerBehavior::Normal,
        };

        taker.init_recover_wallet();
        Ok(taker)
    }

    /// Called on startup to recover funds from incomplete swaps.
    ///
    /// Sweeps incoming swapcoins (hashlock path), recovers timelocked outgoing
    /// swapcoins, and spawns a background RecoveryLoop for any remaining
    /// unresolved contracts.
    fn init_recover_wallet(&mut self) {
        log::info!("Checking wallet for unresolved swap contracts...");

        // Wallet-driven recovery: sweep incoming + recover timelocked
        let has_remaining = match self.write_wallet() {
            Ok(mut wallet) => {
                match wallet.sweep_unified_incoming_swapcoins(2.0) {
                    Ok(ref swept) if !swept.is_empty() => {
                        log::info!(
                            "Startup recovery: swept {} incoming swapcoins",
                            swept.resolved.len()
                        );
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("Startup incoming sweep failed: {:?}", e),
                }

                match wallet.recover_unified_timelocked_swapcoins(2.0) {
                    Ok(ref recovered) if !recovered.is_empty() => {
                        log::info!(
                            "Startup recovery: recovered {} timelocked outgoing swapcoins",
                            recovered.len()
                        );
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("Startup timelock recovery failed: {:?}", e),
                }

                let has_contracts =
                    !wallet.unified_outgoing_contract_outpoints().is_empty()
                        || !wallet.unified_incoming_contract_outpoints().is_empty();
                drop(wallet);
                has_contracts
            }
            Err(e) => {
                log::warn!("Startup recovery: failed to lock wallet: {:?}", e);
                false
            }
        };

        if has_remaining {
            self.recovery_loop = Some(RecoveryLoop::start(
                self.wallet.clone(),
                self.swap_tracker.clone(),
            ));
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
        self.swap_tracker.lock().unwrap().log_state();
    }

    /// Check whether the background recovery loop has completed.
    /// Returns `true` if no recovery is needed or if all contracts are resolved.
    pub fn is_recovery_complete(&self) -> bool {
        match &self.recovery_loop {
            Some(loop_) => loop_.is_complete(),
            None => true,
        }
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
            phase: SwapPhase::MakersDiscovered,
        });

        // Pre-commitment phases: no funds on-chain yet, nothing to recover.
        self.discover_makers()?;

        // SP1: Persist initial swap record with preimage and maker list.
        self.persist_swap(SwapPhase::MakersDiscovered)?;

        #[cfg(feature = "integration-test")]
        if self.behavior == UnifiedTakerBehavior::CloseEarly {
            log::warn!("Test behavior: closing early after maker selection");
            return Err(TakerError::General(
                "Test: Closing early after maker selection".to_string(),
            ));
        }

        self.negotiate_swap_details()?;

        // SP2: Persist after negotiation (tweakable_points updated).
        self.persist_swap(SwapPhase::Negotiated)?;

        self.funding_initialize()?;

        // SP3: Persist after funding initialization (outgoing txids created).
        self.persist_swap(SwapPhase::FundingCreated)?;

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
                            .unwrap_or(SwapPhase::MakersDiscovered);
                        if phase >= SwapPhase::FundsBroadcast {
                            log::warn!("Funding txs were broadcast, triggering recovery");
                            self.persist_failure(phase, &e);
                            if let Err(re) = self.recover_active_swap() {
                                log::error!("Recovery failed: {:?}", re);
                            }
                        } else {
                            log::info!("No funds on-chain — safe to abort");
                            let _ = self.swap_tracker.lock().unwrap().remove_record(
                                &self.swap_state().map(|s| s.id.clone()).unwrap_or_default(),
                            );
                            self.ongoing_swap = None;
                        }
                        return Err(e);
                    }
                }

            }
            ProtocolVersion::Taproot => {
                match self.exchange_taproot() {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("Taproot exchange failed: {:?}", e);
                        let phase = self
                            .swap_state()
                            .map(|s| s.phase)
                            .unwrap_or(SwapPhase::MakersDiscovered);
                        if phase >= SwapPhase::FundsBroadcast {
                            log::warn!("Funds were broadcast, triggering recovery");
                            self.persist_failure(phase, &e);
                            if let Err(re) = self.recover_active_swap() {
                                log::error!("Recovery failed: {:?}", re);
                            }
                        } else {
                            log::info!("No funds on-chain — safe to abort");
                            let _ = self.swap_tracker.lock().unwrap().remove_record(
                                &self.swap_state().map(|s| s.id.clone()).unwrap_or_default(),
                            );
                            self.ongoing_swap = None;
                        }
                        return Err(e);
                    }
                }
            }
        }

        // Common post-exchange path (both protocols).

        #[cfg(feature = "integration-test")]
        if self.behavior == UnifiedTakerBehavior::DropAfterFundsBroadcast {
            log::warn!("Test behavior: dropping after contract exchange");
            let phase = self
                .swap_state()
                .map(|s| s.phase)
                .unwrap_or(SwapPhase::FundsBroadcast);
            let err = TakerError::General("Test: dropped after contract exchange".to_string());
            self.persist_failure(phase, &err);
            if let Err(re) = self.recover_active_swap() {
                log::error!("Recovery failed: {:?}", re);
            }
            return Err(err);
        }

        // SP7: Finalization starts.
        self.persist_swap(SwapPhase::Finalizing)?;

        match self.finalize_with_retry() {
            Ok(()) => {}
            Err(e) => {
                log::error!("Finalization failed after retries: {:?}", e);
                self.persist_failure(SwapPhase::Finalizing, &e);
                if let Err(re) = self.recover_active_swap() {
                    log::error!("Recovery failed: {:?}", re);
                }
                return Err(e);
            }
        }

        // Finalization succeeded — stop breach detector.
        if let Some(detector) = self.breach_detector.take() {
            detector.stop();
        }

        // Success path: sweep + report (shared by both protocols)
        let swept = {
            let mut wallet = self.write_wallet()?;
            let swept = wallet.sweep_unified_incoming_swapcoins(2.0)?;
            log::info!("Swept {} incoming swapcoins", swept.resolved.len());
            wallet.sync_and_save()?;
            swept
        };

        // Record per-contract outcomes before cleanup removes swap state
        self.populate_success_outcomes(&swap_id, &swept)?;

        // Clean up outgoing and watch-only entries on success
        {
            let swap_id_for_cleanup = self.swap_state()?.id.clone();
            let mut wallet = self.write_wallet()?;
            let outgoing_keys = wallet.unified_outgoing_keys_for_swap(&swap_id_for_cleanup);
            for key in &outgoing_keys {
                wallet.remove_unified_outgoing_swapcoin(key);
            }
            wallet.remove_unified_watchonly_swapcoins(&swap_id_for_cleanup);
            wallet.save_to_disk()?;
        }

        // SP10: Mark swap as completed in the tracker.
        self.persist_swap(SwapPhase::Completed)?;

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

        // Get reference height once for consistent absolute timelocks (Taproot).
        let reference_height =
            {
                let wallet = self.read_wallet()?;
                wallet.rpc.get_block_count().map_err(|e| {
                    TakerError::General(format!("Failed to get block count: {:?}", e))
                })? as u32
            };

        for i in 0..num_makers {
            let maker_address = self.swap_state()?.makers[i]
                .offer_and_address
                .address
                .to_string();
            log::info!("Connecting to maker at {}", maker_address);

            let mut stream = self.net_connect(&maker_address)?;

            let negotiated_protocol = self.net_handshake(&mut stream)?;
            log::info!("Handshake complete, protocol: {:?}", negotiated_protocol);

            let refund_locktime_offset =
                REFUND_LOCKTIME_BASE + REFUND_LOCKTIME_STEP * (maker_count - i - 1) as u16;

            // Legacy: send relative offset (CSV). Taproot: send absolute height (CLTV).
            let timelock = if negotiated_protocol == ProtocolVersion::Taproot {
                reference_height + refund_locktime_offset as u32
            } else {
                refund_locktime_offset as u32
            };

            let swap_details = SwapDetails {
                id: swap_id.clone(),
                protocol_version: negotiated_protocol,
                amount: send_amount,
                tx_count,
                timelock,
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
        let refund_locktime_offset =
            REFUND_LOCKTIME_BASE + REFUND_LOCKTIME_STEP * maker_count as u16;

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
                refund_locktime_offset,
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
                refund_locktime_offset,
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

    /// Connect to a maker using either direct connection or Tor proxy.
    pub(crate) fn net_connect(&self, address: &str) -> Result<TcpStream, TakerError> {
        log::debug!("Connecting to maker at {}", address);
        let timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);

        let connection_type = if cfg!(feature = "integration-test") {
            ConnectionType::Clearnet
        } else {
            self.config.connection_type
        };

        let socket = match connection_type {
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

    /// Finalize the swap by exchanging private keys with all makers.
    fn finalize_swap(&mut self) -> Result<(), TakerError> {
        log::info!("Finalizing swap...");

        self.finalize_exchange_privkeys()?;

        self.persist_swap(SwapPhase::PrivkeysForwarded)?;

        self.finalize_persist_incoming()?;

        log::info!("Swap finalized successfully");
        Ok(())
    }

    /// Attempt finalization with retries between attempts.
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

                    if self
                        .breach_detector
                        .as_ref()
                        .is_some_and(|d| d.is_breached())
                    {
                        log::error!(
                            "Contract broadcast detected during finalization — aborting retries"
                        );
                        return Err(TakerError::General(
                            "Contract broadcast detected during finalization".to_string(),
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

    /// Exchange private keys with all makers in forward order.
    /// Each maker receives the privkey for their incoming contract and
    /// responds with their outgoing privkey.
    fn finalize_exchange_privkeys(&mut self) -> Result<(), TakerError> {
        let swap = self.swap_state()?;
        let num_makers = swap.makers.len();
        let protocol = swap.params.protocol;
        let swap_id = swap.id.clone();

        // Start with the taker's own outgoing privkey (for Maker[0]'s incoming)
        let mut current_privkey = swap
            .outgoing_swapcoins
            .first()
            .and_then(|sc| sc.my_privkey)
            .ok_or_else(|| TakerError::General("No outgoing privkey".to_string()))?;

        for i in 0..num_makers {
            let maker_address = self.swap_state()?.makers[i]
                .offer_and_address
                .address
                .to_string();
            let mut stream = self.net_connect(&maker_address)?;

            self.net_handshake(&mut stream)?;

            log::info!("Sending privkey to maker {} and awaiting response", i);

            let msg = Self::msg_build_handover(protocol, swap_id.clone(), current_privkey);
            send_message(&mut stream, &msg)?;

            let msg_bytes = read_message(&mut stream)?;
            let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

            let received_privkey = match msg {
                MakerToTakerMessage::LegacyPrivateKeyHandover(handover)
                | MakerToTakerMessage::TaprootPrivateKeyHandover(handover) => {
                    log::info!("Received private key from maker {}", i);
                    handover.privkeys.first().map(|p| p.key).ok_or_else(|| {
                        TakerError::General(format!("Empty privkey response from maker {}", i))
                    })?
                }
                _ => {
                    return Err(TakerError::General(format!(
                        "Unexpected response from maker {}: expected PrivateKeyHandover",
                        i
                    )));
                }
            };

            self.swap_state_mut()?.makers[i].finalization.privkey_received = true;
            self.swap_state_mut()?.makers[i].finalization.privkey_forwarded = true;

            // For the last maker: set their privkey on taker's incoming swapcoin
            if i == num_makers - 1 {
                if let Some(incoming) = self.swap_state_mut()?.incoming_swapcoins.last_mut() {
                    incoming.set_other_privkey(received_privkey);
                    log::info!(
                        "Set taker's incoming swapcoin other_privkey from last maker ({})",
                        i
                    );
                }
            }

            current_privkey = received_privkey;
        }

        Ok(())
    }

    /// Persist the taker's incoming swapcoins to the wallet.
    /// Preimage is already stamped at swapcoin creation time.
    fn finalize_persist_incoming(&mut self) -> Result<(), TakerError> {
        let mut wallet = self.write_wallet()?;
        if let Some(incoming) = self.swap_state()?.incoming_swapcoins.last() {
            wallet.add_unified_incoming_swapcoin(incoming);
        }

        wallet.save_to_disk()?;
        Ok(())
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

    /// Flush the current swap state to the tracker on disk.
    ///
    /// Sets the swap phase and rebuilds the full record from `OngoingSwapState`
    /// so that maker progress, txids, and nonces stay up-to-date. Preserves
    /// `created_at` and `recovery` from any existing record.
    pub(crate) fn persist_swap(&mut self, phase: SwapPhase) -> Result<(), TakerError> {
        let swap = self.swap_state_mut()?;
        swap.phase = phase;

        // Snapshot preserved fields from any existing record.
        let swap_id = self.swap_state()?.id.clone();
        let tracker_guard = self.swap_tracker.lock().unwrap();
        let existing = tracker_guard.get_record(&swap_id);
        let created_at = existing.map(|r| r.created_at);
        let recovery = existing.map(|r| r.recovery.clone());
        let failed_at = existing.and_then(|r| r.failed_at_phase);
        let failure_reason = existing.and_then(|r| r.failure_reason.clone());
        drop(tracker_guard);

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

        self.swap_tracker.lock().unwrap().save_record(&record)
    }

    /// Flush the current swap state to disk without changing the phase.
    pub(crate) fn persist_progress(&mut self) -> Result<(), TakerError> {
        let phase = self.swap_state()?.phase;
        self.persist_swap(phase)
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
                if let Some(existing) = self.swap_tracker.lock().unwrap().get_record(&swap_id) {
                    record.recovery = existing.recovery.clone();
                    record.created_at = existing.created_at;
                }
                record.updated_at = now_secs();
                if let Err(e) = self.swap_tracker.lock().unwrap().save_record(&record) {
                    log::error!("Failed to persist swap failure: {:?}", e);
                }
            }
        }
    }

    /// Recover from a failed swap by persisting swapcoins to wallet and
    /// spawning a background `RecoveryLoop` for sweep/timelock recovery.
    ///
    /// All recovery attempts, per-contract outcome tracking, phase transitions,
    /// and wallet cleanup are handled by the `RecoveryLoop`.
    pub fn recover_active_swap(&mut self) -> Result<(), TakerError> {
        log::warn!("Starting swap recovery...");

        let swap_id = if let Some(ref swap) = self.ongoing_swap {
            let id = swap.id.clone();
            let mut wallet = self.write_wallet()?;
            for outgoing in &swap.outgoing_swapcoins {
                wallet.add_unified_outgoing_swapcoin(outgoing);
            }
            if let Some(incoming) = swap.incoming_swapcoins.last() {
                wallet.add_unified_incoming_swapcoin(incoming);
            }
            wallet.save_to_disk()?;
            id
        } else {
            // Cross-session recovery: get swap_id from persisted swapcoins
            let wallet = self.read_wallet()?;
            let (incoming, outgoing) = wallet.find_unfinished_unified_swapcoins();
            drop(wallet);
            outgoing
                .first()
                .and_then(|sc| sc.swap_id.clone())
                .or_else(|| incoming.first().and_then(|sc| sc.swap_id.clone()))
                .ok_or_else(|| {
                    TakerError::General("No persisted swapcoins found for recovery".to_string())
                })?
        };

        self.swap_tracker
            .lock()
            .unwrap()
            .update_and_save(&swap_id, |record| {
                record.phase = SwapPhase::Failed;
            })?;

        self.ongoing_swap = None;

        log::info!("Spawning recovery loop for swap {}", swap_id);
        self.recovery_loop = Some(RecoveryLoop::start(
            self.wallet.clone(),
            self.swap_tracker.clone(),
        ));

        Ok(())
    }

    /// Populate per-contract outcomes for a successful swap.
    ///
    /// On success, all contracts resolve cooperatively:
    /// - Incoming: `KeyPath` (swept via key-path using maker's privkey)
    /// - Outgoing: `KeyPath` (maker claimed via key-path using our privkey)
    /// - Watchonly: `KeyPath` (makers exchanged privkeys and spent cooperatively)
    fn populate_success_outcomes(
        &mut self,
        swap_id: &str,
        swept: &RecoveryOutcome,
    ) -> Result<(), TakerError> {
        let mut incoming_outcomes = Vec::new();
        let mut outgoing_outcomes = Vec::new();
        let mut watchonly_outcomes = Vec::new();

        // Incoming contracts were swept cooperatively (key-path spend)
        for (contract_txid, spending_txid) in &swept.resolved {
            incoming_outcomes.push(ContractOutcome {
                contract_txid: *contract_txid,
                resolution: ContractResolution::KeyPath,
                spending_txid: Some(*spending_txid),
            });
        }

        // Outgoing + watchonly contracts resolved via key-path
        if let Ok(swap) = self.swap_state() {
            for sc in &swap.outgoing_swapcoins {
                outgoing_outcomes.push(ContractOutcome {
                    contract_txid: sc.contract_tx.compute_txid(),
                    resolution: ContractResolution::KeyPath,
                    spending_txid: None, // Maker's spending tx not tracked by us
                });
            }
            for sc in &swap.watchonly_swapcoins {
                watchonly_outcomes.push(ContractOutcome {
                    contract_txid: sc.contract_tx.compute_txid(),
                    resolution: ContractResolution::KeyPath,
                    spending_txid: None,
                });
            }
        }

        self.swap_tracker
            .lock()
            .unwrap()
            .update_and_save(swap_id, |r| {
                r.recovery.incoming = incoming_outcomes;
                r.recovery.outgoing = outgoing_outcomes;
                r.recovery.watchonly = watchonly_outcomes;
            })?;

        Ok(())
    }

    // ── CLI helper methods ──────────────────────────────────────────────

    /// Returns the current offerbook snapshot.
    pub fn fetch_offers(&self) -> Result<OfferBook, TakerError> {
        Ok(self.offerbook.snapshot())
    }

    /// Indicates if offerbook syncing is in progress.
    pub fn is_offerbook_syncing(&self) -> bool {
        self.offer_sync_handle.is_syncing()
    }

    /// Triggers a manual offerbook re-sync without waiting for the next interval.
    pub fn trigger_offerbook_sync(&self) {
        self.offer_sync_handle.run_sync_now();
    }

    /// Displays a maker offer candidate in a human-readable format.
    pub fn display_offer(&self, candidate: &MakerOfferCandidate) -> Result<String, TakerError> {
        let header = format!(
            r#"
    Maker
    ─────
    Address        : {address}
    Protocol       : {protocol}
    State          : {state}
    "#,
            address = candidate.address,
            protocol = candidate
                .protocol
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "Unknown".into()),
            state = super::offers::format_state(&candidate.state),
        );

        let Some(offer) = &candidate.offer else {
            return Ok(header);
        };

        let bond = &offer.fidelity.bond;
        let bond_value = self.read_wallet()?.calculate_bond_value(bond)?;

        Ok(format!(
            r#"{header}

    Offer
    ─────
    Base Fee       : {base_fee}
    Amount Fee %   : {amount_fee:.4}
    Time Fee %     : {time_fee:.4}

    Limits
    ──────
    Min Size       : {min_size}
    Max Size       : {max_size}
    Required Conf. : {confirms}
    Min Locktime   : {locktime}

    Fidelity Bond
    ─────────────
    Outpoint       : {outpoint}
    Value          : {bond_value}
    Expiry         : {expiry}
    "#,
            header = header.trim_end(),
            base_fee = offer.base_fee,
            amount_fee = offer.amount_relative_fee_pct,
            time_fee = offer.time_relative_fee_pct,
            min_size = offer.min_size,
            max_size = offer.max_size,
            confirms = offer.required_confirms,
            locktime = offer.minimum_locktime,
            outpoint = bond.outpoint,
            bond_value = bond_value,
            expiry = bond.lock_time,
        ))
    }

    /// Restore a wallet from a backup file (static — no taker instance needed).
    pub fn restore_wallet(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        backup_file: &String,
    ) {
        let backup_file_path = PathBuf::from(backup_file);
        let restored_wallet_filename = wallet_file_name.unwrap_or_default();

        let restored_wallet_path = data_dir
            .unwrap_or_else(get_taker_dir)
            .join("wallets")
            .join(restored_wallet_filename);

        Wallet::restore_interactive(
            &backup_file_path,
            &rpc_config.unwrap_or_default(),
            &restored_wallet_path,
        );
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
