//! Unified Maker API for both Legacy (ECDSA) and Taproot (MuSig2) protocols.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bitcoin::{Amount, Network, OutPoint, PublicKey, Transaction};
use bitcoind::bitcoincore_rpc::RpcApi;

use crate::{
    protocol::common_messages::{FidelityProof, ProtocolVersion, SwapDetails},
    utill::MIN_FEE_RATE,
    wallet::{
        unified_swapcoin::{
            IncomingSwapCoin as UnifiedIncomingSwapCoin,
            OutgoingSwapCoin as UnifiedOutgoingSwapCoin,
        },
        AddressType, RPCConfig, Wallet, WalletError,
    },
    watch_tower::service::WatchService,
};

#[cfg(feature = "integration-test")]
pub use super::unified_handlers::UnifiedMakerBehavior;

use super::{
    error::MakerError,
    swap_tracker::MakerSwapTracker,
    unified_handlers::{MakerConfig, UnifiedConnectionState, UnifiedMaker as UnifiedMakerTrait},
};

/// Swap state tracked per swap_id (persisted across connections).
#[derive(Debug, Clone)]
struct SwapState {
    /// Swap amount.
    swap_amount: Amount,
    /// Timelock value.
    timelock: u16,
    /// Protocol version for this swap.
    protocol: ProtocolVersion,
    /// Incoming swapcoins (we receive).
    incoming_swapcoins: Vec<UnifiedIncomingSwapCoin>,
    /// Outgoing swapcoins (we send).
    outgoing_swapcoins: Vec<UnifiedOutgoingSwapCoin>,
    /// Pending funding transactions (for Legacy protocol).
    /// Stored until signature exchange completes, then broadcast.
    pending_funding_txes: Vec<Transaction>,
    /// Last activity timestamp.
    last_activity: Instant,
}

impl Default for SwapState {
    fn default() -> Self {
        SwapState {
            swap_amount: Amount::ZERO,
            timelock: 0,
            protocol: ProtocolVersion::Legacy,
            incoming_swapcoins: Vec::new(),
            outgoing_swapcoins: Vec::new(),
            pending_funding_txes: Vec::new(),
            last_activity: Instant::now(),
        }
    }
}

/// Unified Maker Server configuration for the trait-based approach.
#[derive(Debug, Clone)]
pub struct UnifiedMakerServerConfig {
    /// Data directory for the maker.
    pub data_dir: PathBuf,
    /// Network port for incoming connections.
    pub network_port: u16,
    /// RPC port for maker-cli commands.
    pub rpc_port: u16,
    /// Base fee in satoshis per swap.
    pub base_fee: u64,
    /// Amount-relative fee percentage.
    pub amount_relative_fee_pct: f64,
    /// Time-relative fee percentage.
    pub time_relative_fee_pct: f64,
    /// Minimum swap amount in satoshis.
    pub min_swap_amount: u64,
    /// Required confirmations for funding transactions.
    pub required_confirms: u32,
    /// Supported protocol versions.
    pub supported_protocols: Vec<ProtocolVersion>,
    /// ZMQ address for transaction monitoring.
    pub zmq_addr: String,
    /// Fidelity bond amount in satoshis.
    pub fidelity_amount: u64,
    /// Fidelity bond timelock in blocks.
    pub fidelity_timelock: u32,
    /// Bitcoin network.
    pub network: Network,
    /// Wallet name.
    pub wallet_name: String,
    /// RPC configuration.
    pub rpc_config: RPCConfig,
}

impl Default for UnifiedMakerServerConfig {
    fn default() -> Self {
        UnifiedMakerServerConfig {
            data_dir: PathBuf::from("./data"),
            network_port: 6102,
            rpc_port: 6103,
            base_fee: 1000,
            amount_relative_fee_pct: 0.025,
            time_relative_fee_pct: 0.001,
            min_swap_amount: 10_000,
            required_confirms: 1,
            supported_protocols: vec![ProtocolVersion::Legacy, ProtocolVersion::Taproot],
            zmq_addr: "tcp://127.0.0.1:28332".to_string(),
            fidelity_amount: 5_000_000, // 0.05 BTC
            fidelity_timelock: 26_000,  // ~6 months
            network: Network::Regtest,
            wallet_name: "unified_maker".to_string(),
            rpc_config: RPCConfig::default(),
        }
    }
}

/// Thread pool for managing background threads.
pub struct UnifiedThreadPool {
    threads: Mutex<Vec<JoinHandle<()>>>,
    port: u16,
}

impl UnifiedThreadPool {
    /// Create a new thread pool.
    pub fn new(port: u16) -> Self {
        Self {
            threads: Mutex::new(Vec::new()),
            port,
        }
    }

    /// Add a thread to the pool.
    pub fn add_thread(&self, handle: JoinHandle<()>) {
        let mut threads = self.threads.lock().unwrap();
        threads.push(handle);
    }

    /// Join all threads in the pool.
    pub fn join_all_threads(&self) -> Result<(), MakerError> {
        let mut threads = self
            .threads
            .lock()
            .map_err(|_| MakerError::General("Failed to lock threads"))?;

        log::info!("Joining {} threads", threads.len());

        while let Some(thread) = threads.pop() {
            let thread_name = thread.thread().name().unwrap_or("unknown").to_string();

            match thread.join() {
                Ok(_) => {
                    log::info!("[{}] Thread {} joined", self.port, thread_name);
                }
                Err(_) => {
                    log::error!("[{}] Thread {} panicked", self.port, thread_name);
                }
            }
        }

        Ok(())
    }
}

/// Unified Maker server
///
/// This implements the `UnifiedMaker` trait with actual swap logic.
pub struct UnifiedMakerServer {
    /// Configuration.
    pub config: UnifiedMakerServerConfig,
    /// Wallet.
    pub wallet: Arc<RwLock<Wallet>>,
    /// Shutdown flag.
    pub shutdown: AtomicBool,
    /// Is setup complete flag.
    pub is_setup_complete: AtomicBool,
    /// Highest fidelity proof.
    pub highest_fidelity_proof: RwLock<Option<FidelityProof>>,
    /// Ongoing swap states by swap_id.
    ongoing_swaps: Mutex<HashMap<String, SwapState>>,
    /// Watch service for contract monitoring.
    pub watch_service: WatchService,
    /// Thread pool for background threads.
    pub thread_pool: Arc<UnifiedThreadPool>,
    /// Data directory.
    pub data_dir: PathBuf,
    /// Persistent swap tracker for recovery progress.
    pub swap_tracker: Mutex<MakerSwapTracker>,
    /// Test-only behavior override.
    #[cfg(feature = "integration-test")]
    pub behavior: UnifiedMakerBehavior,
}

/// Idle swap data returned by [`UnifiedMakerServer::drain_idle_swaps`].
pub struct IdleSwapData {
    /// Unique swap identifier.
    pub swap_id: String,
    /// Protocol version used for this swap.
    pub protocol: crate::protocol::common_messages::ProtocolVersion,
    /// Swap amount in satoshis.
    pub swap_amount_sat: u64,
    /// Incoming swapcoins (maker receives).
    pub incoming_swapcoins: Vec<UnifiedIncomingSwapCoin>,
    /// Outgoing swapcoins (maker sends).
    pub outgoing_swapcoins: Vec<UnifiedOutgoingSwapCoin>,
}

impl UnifiedMakerServer {
    /// Initialize a new unified maker server with full setup.
    pub fn init(config: UnifiedMakerServerConfig) -> Result<Self, MakerError> {
        let data_dir = config.data_dir.clone();
        std::fs::create_dir_all(&data_dir).map_err(MakerError::IO)?;

        let wallets_dir = data_dir.join("wallets");
        let wallet_path = wallets_dir.join(&config.wallet_name);

        // Initialize or load wallet
        let mut rpc_config = config.rpc_config.clone();
        rpc_config.wallet_name = config.wallet_name.clone();

        let wallet = Wallet::load_or_init_wallet(&wallet_path, &rpc_config, None)?;

        // Initial wallet sync
        let mut wallet = wallet;
        log::info!("Sync at:----UnifiedMakerServer init----");
        wallet.sync_and_save()?;

        // Initialize watch service
        let watch_service = crate::watch_tower::service::start_maker_watch_service(
            &config.zmq_addr,
            &rpc_config,
            &data_dir,
            config.network_port,
        )
        .map_err(MakerError::Watcher)?;

        let swap_tracker = MakerSwapTracker::load_or_create(&data_dir)?;
        let incomplete = swap_tracker.incomplete_swaps();
        if !incomplete.is_empty() {
            log::info!(
                "[{}] Loaded {} incomplete swap records from previous run",
                config.network_port,
                incomplete.len()
            );
            swap_tracker.log_state();
        }

        Ok(UnifiedMakerServer {
            config: config.clone(),
            wallet: Arc::new(RwLock::new(wallet)),
            shutdown: AtomicBool::new(false),
            is_setup_complete: AtomicBool::new(false),
            highest_fidelity_proof: RwLock::new(None),
            ongoing_swaps: Mutex::new(HashMap::new()),
            watch_service,
            thread_pool: Arc::new(UnifiedThreadPool::new(config.network_port)),
            data_dir,
            swap_tracker: Mutex::new(swap_tracker),
            #[cfg(feature = "integration-test")]
            behavior: UnifiedMakerBehavior::default(),
        })
    }

    /// Check if shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    /// Setup fidelity bond for this maker.
    pub fn setup_fidelity_bond(&self, maker_address: &str) -> Result<FidelityProof, MakerError> {
        use bitcoin::absolute::LockTime;
        use bitcoind::bitcoincore_rpc::RpcApi;

        let highest_index = self
            .wallet
            .read()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?
            .get_highest_fidelity_index()
            .map_err(MakerError::Wallet)?;

        let mut proof = self
            .highest_fidelity_proof
            .write()
            .map_err(|_| MakerError::General("Failed to lock fidelity proof"))?;

        if let Some(i) = highest_index {
            // Existing fidelity bond found
            let wallet_read = self
                .wallet
                .read()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?;
            let bond = wallet_read.store.fidelity_bond.get(&i).unwrap().clone();
            let current_height = wallet_read
                .rpc
                .get_block_count()
                .map_err(WalletError::Rpc)? as u32;
            let bond_value = wallet_read
                .calculate_bond_value(&bond)
                .map_err(MakerError::Wallet)?
                .to_sat();
            drop(wallet_read);

            let highest_proof = self
                .wallet
                .read()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .generate_fidelity_proof(i, maker_address)
                .map_err(MakerError::Wallet)?;

            log::info!(
                "Highest bond at outpoint {} | index {} | Amount {:?} sats | Remaining Timelock: {:?} Blocks | Bond Value: {:?} sats",
                highest_proof.bond.outpoint,
                i,
                bond.amount.to_sat(),
                bond.lock_time.to_consensus_u32() - current_height,
                bond_value
            );

            *proof = Some(highest_proof);
        } else {
            // Need to create new fidelity bond
            log::info!("No active Fidelity Bonds found. Creating one.");

            let amount = Amount::from_sat(self.config.fidelity_amount);
            log::info!("Fidelity value chosen = {:?} sats", amount.to_sat());

            let current_height = self
                .wallet
                .read()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .rpc
                .get_block_count()
                .map_err(WalletError::Rpc)? as u32;

            // Set locktime for test (950 blocks) or production
            let locktime = if cfg!(feature = "integration-test") {
                LockTime::from_height(current_height + 950).map_err(WalletError::Locktime)?
            } else {
                LockTime::from_height(self.config.fidelity_timelock + current_height)
                    .map_err(WalletError::Locktime)?
            };

            log::info!(
                "Fidelity timelock {:?} blocks",
                locktime.to_consensus_u32() - current_height
            );

            // Wait for funds and create fidelity bond
            let sleep_increment = 10;
            let mut sleep_multiplier = 0;

            while !self.shutdown.load(Ordering::Relaxed) {
                sleep_multiplier += 1;

                log::info!("Sync at:----setup_fidelity_bond----");
                self.wallet
                    .write()
                    .map_err(|_| MakerError::General("Failed to lock wallet"))?
                    .sync_and_save()
                    .map_err(MakerError::Wallet)?;

                let fidelity_result = self
                    .wallet
                    .write()
                    .map_err(|_| MakerError::General("Failed to lock wallet"))?
                    .create_fidelity(
                        amount,
                        locktime,
                        Some(maker_address),
                        MIN_FEE_RATE,
                        AddressType::P2WPKH,
                    );

                match fidelity_result {
                    Err(e) => {
                        if let WalletError::InsufficientFund {
                            available,
                            required,
                        } = e
                        {
                            log::warn!("Insufficient funds to create fidelity bond.");
                            let needed = required - available;
                            let addr = self
                                .wallet
                                .write()
                                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                                .get_next_external_address(AddressType::P2WPKH)
                                .map_err(MakerError::Wallet)?;

                            log::info!(
                                "Send at least {:.8} BTC to {:?}",
                                Amount::from_sat(needed).to_btc(),
                                addr
                            );

                            let total_sleep = sleep_increment * sleep_multiplier.min(60);
                            log::info!("Next sync in {total_sleep:?} secs");
                            thread::sleep(std::time::Duration::from_secs(total_sleep));
                        } else {
                            log::error!(
                                "[{}] Fidelity Bond Creation failed: {:?}",
                                self.config.network_port,
                                e
                            );
                            return Err(MakerError::Wallet(e));
                        }
                    }
                    Ok(i) => {
                        log::info!(
                            "[{}] Successfully created fidelity bond",
                            self.config.network_port
                        );
                        let highest_proof = self
                            .wallet
                            .read()
                            .map_err(|_| MakerError::General("Failed to lock wallet"))?
                            .generate_fidelity_proof(i, maker_address)
                            .map_err(MakerError::Wallet)?;

                        *proof = Some(highest_proof);

                        log::info!("Sync at end:----setup_fidelity_bond----");
                        self.wallet
                            .write()
                            .map_err(|_| MakerError::General("Failed to lock wallet"))?
                            .sync_and_save()
                            .map_err(MakerError::Wallet)?;
                        break;
                    }
                }
            }
        }

        proof
            .clone()
            .ok_or(MakerError::General("No fidelity proof after setup"))
    }

    /// Check if maker has enough liquidity for swaps.
    pub fn check_swap_liquidity(&self) -> Result<(), MakerError> {
        let sleep_increment = 10u64;
        let mut sleep_duration = 0u64;

        let addr = self
            .wallet
            .write()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?
            .get_next_external_address(AddressType::P2WPKH)
            .map_err(MakerError::Wallet)?;

        while !self.shutdown.load(Ordering::Relaxed) {
            log::info!("Sync at:----check_swap_liquidity----");
            self.wallet
                .write()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .sync_and_save()
                .map_err(MakerError::Wallet)?;

            let offer_max_size = self
                .wallet
                .read()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?
                .store
                .offer_maxsize;

            let min_required = self.config.min_swap_amount;

            if offer_max_size < min_required {
                log::warn!(
                    "Low Swap Liquidity | Min: {min_required} sats | Available: {offer_max_size} sats. Add funds to {addr:?}"
                );

                sleep_duration = (sleep_duration + sleep_increment).min(600);
                log::info!("Next sync in {sleep_duration:?} secs");
                thread::sleep(std::time::Duration::from_secs(sleep_duration));
            } else {
                log::info!(
                    "Swap Liquidity: {offer_max_size} sats | Min: {min_required} sats | Listening for requests."
                );
                break;
            }
        }

        Ok(())
    }

    /// Atomically find and remove stale entries from `ongoing_swaps`.
    /// Returns swap data for each idle swap.
    /// Only drains entries where `outgoing_swapcoins` is non-empty (otherwise nothing to recover).
    pub fn drain_idle_swaps(&self, timeout: Duration) -> Vec<IdleSwapData> {
        let mut swaps = self.ongoing_swaps.lock().unwrap();
        let mut idle = Vec::new();

        let stale_ids: Vec<String> = swaps
            .iter()
            .filter(|(_, state)| {
                state.last_activity.elapsed() > timeout && !state.outgoing_swapcoins.is_empty()
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in stale_ids {
            if let Some(state) = swaps.remove(&id) {
                idle.push(IdleSwapData {
                    swap_id: id,
                    protocol: state.protocol,
                    swap_amount_sat: state.swap_amount.to_sat(),
                    incoming_swapcoins: state.incoming_swapcoins,
                    outgoing_swapcoins: state.outgoing_swapcoins,
                });
            }
        }

        idle
    }

    /// Remove a completed swap's entry from `ongoing_swaps`.
    pub fn remove_swap_state(&self, swap_id: &str) {
        let mut swaps = self.ongoing_swaps.lock().unwrap();
        swaps.remove(swap_id);
    }
}

impl UnifiedMakerTrait for UnifiedMakerServer {
    fn network_port(&self) -> u16 {
        self.config.network_port
    }

    fn get_tweakable_keypair(
        &self,
    ) -> Result<(bitcoin::secp256k1::SecretKey, PublicKey), MakerError> {
        let wallet = self
            .wallet
            .read()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;
        wallet.get_tweakable_keypair().map_err(MakerError::Wallet)
    }

    fn get_fidelity_proof(&self) -> Result<FidelityProof, MakerError> {
        let proof = self
            .highest_fidelity_proof
            .read()
            .map_err(|_| MakerError::General("Failed to lock fidelity proof"))?;
        proof
            .clone()
            .ok_or(MakerError::General("No fidelity proof available"))
    }

    fn get_config(&self) -> MakerConfig {
        MakerConfig {
            base_fee: self.config.base_fee,
            amount_relative_fee_pct: self.config.amount_relative_fee_pct,
            time_relative_fee_pct: self.config.time_relative_fee_pct,
            min_swap_amount: self.config.min_swap_amount,
            max_swap_amount: self
                .wallet
                .read()
                .map(|w| w.store.offer_maxsize)
                .unwrap_or(u64::MAX),
            required_confirms: self.config.required_confirms,
            supported_protocols: self.config.supported_protocols.clone(),
        }
    }

    fn validate_swap_parameters(&self, details: &SwapDetails) -> Result<(), MakerError> {
        let config = self.get_config();

        // Check amount is within bounds
        let amount_sat = details.amount.to_sat();
        if amount_sat < config.min_swap_amount {
            return Err(MakerError::General("Swap amount below minimum"));
        }
        if amount_sat > config.max_swap_amount {
            return Err(MakerError::General("Swap amount above maximum"));
        }

        // Check protocol is supported
        if !self
            .config
            .supported_protocols
            .contains(&details.protocol_version)
        {
            return Err(MakerError::General("Protocol version not supported"));
        }

        Ok(())
    }

    fn calculate_swap_fee(&self, amount: Amount, timelock: u16) -> Amount {
        let total_fee = self.config.base_fee as f64
            + (amount.to_sat() as f64 * self.config.amount_relative_fee_pct) / 100.00
            + (amount.to_sat() as f64 * timelock as f64 * self.config.time_relative_fee_pct)
                / 100.00;
        Amount::from_sat(total_fee.ceil() as u64)
    }

    fn network(&self) -> Network {
        self.config.network
    }

    fn create_funding_transaction(
        &self,
        amount: Amount,
        address: bitcoin::Address,
    ) -> Result<(Transaction, u32), MakerError> {
        let mut wallet = self
            .wallet
            .write()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;

        let result = wallet
            .create_funding_txes(
                amount,
                &[address],
                crate::utill::MIN_FEE_RATE,
                None, // No manually selected outpoints
            )
            .map_err(MakerError::Wallet)?;

        // Return the first (and only) funding tx and its output position
        let tx = result
            .funding_txes
            .into_iter()
            .next()
            .ok_or(MakerError::General("No funding tx created"))?;
        let output_position = result
            .payment_output_positions
            .first()
            .copied()
            .unwrap_or(0);

        Ok((tx, output_position))
    }

    fn verify_contract_tx_on_chain(&self, txid: &bitcoin::Txid) -> Result<(), MakerError> {
        let wallet = self
            .wallet
            .read()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;

        wallet
            .rpc
            .get_raw_transaction(txid, None)
            .map_err(|_| MakerError::General("Incoming contract tx not found on-chain"))?;

        Ok(())
    }

    fn broadcast_transaction(&self, tx: &Transaction) -> Result<bitcoin::Txid, MakerError> {
        let wallet = self
            .wallet
            .read()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;

        wallet.send_tx(tx).map_err(MakerError::Wallet)
    }

    fn save_incoming_swapcoin(
        &self,
        swapcoin: &crate::wallet::unified_swapcoin::IncomingSwapCoin,
    ) -> Result<(), MakerError> {
        let mut wallet = self
            .wallet
            .write()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;
        wallet.add_unified_incoming_swapcoin(swapcoin);
        wallet.save_to_disk().map_err(MakerError::Wallet)
    }

    fn save_outgoing_swapcoin(
        &self,
        swapcoin: &crate::wallet::unified_swapcoin::OutgoingSwapCoin,
    ) -> Result<(), MakerError> {
        let mut wallet = self
            .wallet
            .write()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;
        wallet.add_unified_outgoing_swapcoin(swapcoin);
        wallet.save_to_disk().map_err(MakerError::Wallet)
    }

    fn register_watch_outpoint(&self, outpoint: OutPoint) {
        self.watch_service.register_watch_request(outpoint);
    }

    fn sweep_incoming_swapcoins(&self) -> Result<(), MakerError> {
        log::info!(
            "[{}] Sweeping coins after successful swap",
            self.config.network_port
        );

        // Sweep all completed unified incoming swapcoins
        let swept_txids = self
            .wallet
            .write()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?
            .sweep_unified_incoming_swapcoins(MIN_FEE_RATE)
            .map_err(MakerError::Wallet)?;

        if !swept_txids.is_empty() {
            log::info!(
                "[{}] ✅ Successfully swept {} unified incoming swap coins: {:?}",
                self.config.network_port,
                swept_txids.len(),
                swept_txids
            );
        }

        // Sync and save wallet state
        log::info!(
            "[{}] Sync at:----sweep_incoming_swapcoins----",
            self.config.network_port
        );
        self.wallet
            .write()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?
            .sync_and_save()
            .map_err(MakerError::Wallet)?;

        // For tests, terminate the maker at this stage
        #[cfg(feature = "integration-test")]
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);

        Ok(())
    }

    fn store_connection_state(&self, swap_id: &str, state: &UnifiedConnectionState) {
        let mut swaps = self.ongoing_swaps.lock().unwrap();
        let swap_state = swaps.entry(swap_id.to_string()).or_default();
        swap_state.swap_amount = state.swap_amount;
        swap_state.timelock = state.timelock;
        swap_state.protocol = state.protocol;
        swap_state.incoming_swapcoins = state.incoming_swapcoins.clone();
        swap_state.outgoing_swapcoins = state.outgoing_swapcoins.clone();
        swap_state.pending_funding_txes = state.pending_funding_txes.clone();
        swap_state.last_activity = Instant::now();
        log::debug!(
            "[{}] Stored connection state for {}: amount={}, timelock={}, protocol={:?}, outgoing_count={}",
            self.config.network_port,
            swap_id,
            state.swap_amount,
            state.timelock,
            state.protocol,
            state.outgoing_swapcoins.len()
        );
    }

    fn get_connection_state(&self, swap_id: &str) -> Option<UnifiedConnectionState> {
        let swaps = self.ongoing_swaps.lock().unwrap();
        swaps.get(swap_id).map(|s| {
            let mut state = UnifiedConnectionState::new(s.protocol);
            state.swap_id = Some(swap_id.to_string());
            state.swap_amount = s.swap_amount;
            state.timelock = s.timelock;
            state.incoming_swapcoins = s.incoming_swapcoins.clone();
            state.outgoing_swapcoins = s.outgoing_swapcoins.clone();
            state.pending_funding_txes = s.pending_funding_txes.clone();
            state
        })
    }

    fn remove_connection_state(&self, swap_id: &str) {
        self.remove_swap_state(swap_id);
    }

    fn verify_and_sign_sender_contract_txs(
        &self,
        txs_info: &[crate::protocol::legacy_messages::ContractTxInfoForSender],
        _hashvalue: &crate::protocol::Hash160,
        _locktime: u16,
    ) -> Result<Vec<bitcoin::ecdsa::Signature>, MakerError> {
        log::info!(
            "[{}] Verifying and signing {} sender contract txs",
            self.config.network_port,
            txs_info.len()
        );

        let mut sigs = Vec::new();

        for txinfo in txs_info {
            // Basic validation
            if txinfo.senders_contract_tx.input.len() != 1
                || txinfo.senders_contract_tx.output.len() != 1
            {
                return Err(MakerError::General(
                    "Invalid number of inputs or outputs in contract transaction",
                ));
            }

            // Get tweakable keypair
            let (tweakable_privkey, _tweakable_pubkey) = self.get_tweakable_keypair()?;

            // Derive multisig privkey using the nonce
            let multisig_privkey = tweakable_privkey
                .add_tweak(&txinfo.multisig_nonce.into())
                .map_err(|_| MakerError::General("Failed to derive multisig privkey"))?;

            // Sign the contract transaction
            let sig = crate::protocol::contract::sign_contract_tx(
                &txinfo.senders_contract_tx,
                &txinfo.multisig_redeemscript,
                txinfo.funding_input_value,
                &multisig_privkey,
            )
            .map_err(|e| {
                log::error!("Failed to sign contract tx: {:?}", e);
                MakerError::General("Failed to sign contract transaction")
            })?;

            log::debug!("[{}] Signed sender contract tx", self.config.network_port);
            sigs.push(sig);
        }

        log::info!(
            "[{}] Generated {} signatures for sender contracts",
            self.config.network_port,
            sigs.len()
        );
        Ok(sigs)
    }

    fn verify_proof_of_funding(
        &self,
        message: &crate::protocol::legacy_messages::ProofOfFunding,
    ) -> Result<crate::protocol::Hash160, MakerError> {
        use crate::{
            protocol::contract::{
                check_hashlock_has_pubkey, check_multisig_has_pubkey,
                check_reedemscript_is_multisig, read_contract_locktime,
                read_hashvalue_from_contract,
            },
            utill::{redeemscript_to_scriptpubkey, REQUIRED_CONFIRMS},
        };
        use bitcoin::{hashes::Hash, OutPoint};
        use bitcoind::bitcoincore_rpc::RpcApi;

        log::info!(
            "[{}] Verifying proof of funding for swap {}",
            self.config.network_port,
            message.id
        );

        if message.confirmed_funding_txes.is_empty() {
            return Err(MakerError::General("No funding txs provided by Taker"));
        }

        let min_reaction_time: u16 = 20; // MIN_CONTRACT_REACTION_TIME
        let mut hashvalue: Option<crate::protocol::Hash160> = None;

        for funding_info in &message.confirmed_funding_txes {
            // Check that the new locktime is sufficiently short enough
            let locktime = read_contract_locktime(&funding_info.contract_redeemscript)?;
            // Use saturating_sub to avoid overflow
            let locktime_diff = locktime.saturating_sub(message.refund_locktime);
            if locktime_diff < min_reaction_time {
                return Err(MakerError::General(
                    "Next hop locktime too close to current hop locktime",
                ));
            }

            // Find the funding output index
            let multisig_spk = redeemscript_to_scriptpubkey(&funding_info.multisig_redeemscript)?;
            let funding_output_index = funding_info
                .funding_tx
                .output
                .iter()
                .position(|o| o.script_pubkey == multisig_spk)
                .ok_or(MakerError::General("Funding output not found"))?
                as u32;

            // Check the funding_tx is confirmed to required depth
            let wallet_read = self
                .wallet
                .read()
                .map_err(|_| MakerError::General("Failed to lock wallet"))?;

            if let Some(txout) = wallet_read
                .rpc
                .get_tx_out(
                    &funding_info.funding_tx.compute_txid(),
                    funding_output_index,
                    None,
                )
                .map_err(WalletError::Rpc)?
            {
                if txout.confirmations < REQUIRED_CONFIRMS {
                    return Err(MakerError::General(
                        "Funding tx not confirmed to required depth",
                    ));
                }
            } else {
                return Err(MakerError::General("Funding tx output doesn't exist"));
            }

            check_reedemscript_is_multisig(&funding_info.multisig_redeemscript)?;

            let (_, tweakable_pubkey) = wallet_read.get_tweakable_keypair()?;

            check_multisig_has_pubkey(
                &funding_info.multisig_redeemscript,
                &tweakable_pubkey,
                &funding_info.multisig_nonce,
            )?;

            check_hashlock_has_pubkey(
                &funding_info.contract_redeemscript,
                &tweakable_pubkey,
                &funding_info.hashlock_nonce,
            )?;

            // Check that the provided contract matches the scriptpubkey from the cache
            let contract_spk = redeemscript_to_scriptpubkey(&funding_info.contract_redeemscript)?;

            if !wallet_read.does_prevout_match_cached_contract(
                &OutPoint {
                    txid: funding_info.funding_tx.compute_txid(),
                    vout: funding_output_index,
                },
                &contract_spk,
            )? {
                return Err(MakerError::General(
                    "Provided contract does not match sender contract tx, rejecting",
                ));
            }

            // Extract and verify hashvalue
            let this_hashvalue = read_hashvalue_from_contract(&funding_info.contract_redeemscript)?;
            if let Some(ref prev_hashvalue) = hashvalue {
                if *prev_hashvalue != this_hashvalue {
                    return Err(MakerError::General("Hash values in contracts do not match"));
                }
            } else {
                hashvalue = Some(this_hashvalue);
            }
        }

        let hashvalue = hashvalue.ok_or(MakerError::General("No hashvalue found in contracts"))?;
        log::info!(
            "[{}] Proof of funding verified successfully, hashvalue={:?}",
            self.config.network_port,
            hashvalue.to_byte_array()
        );
        Ok(hashvalue)
    }

    fn initialize_coinswap(
        &self,
        send_amount: Amount,
        next_multisig_pubkeys: &[PublicKey],
        next_hashlock_pubkeys: &[PublicKey],
        hashvalue: crate::protocol::Hash160,
        locktime: u16,
        contract_feerate: f64,
    ) -> Result<(Vec<Transaction>, Vec<UnifiedOutgoingSwapCoin>, Amount), MakerError> {
        log::info!(
            "[{}] Initializing coinswap: amount={} sats, {} pubkeys",
            self.config.network_port,
            send_amount.to_sat(),
            next_multisig_pubkeys.len()
        );

        let mut wallet = self
            .wallet
            .write()
            .map_err(|_| MakerError::General("Failed to lock wallet"))?;

        let (coinswap_addresses, my_multisig_privkeys): (Vec<_>, Vec<_>) = next_multisig_pubkeys
            .iter()
            .map(|other_key| wallet.create_and_import_coinswap_address(other_key))
            .collect::<Result<Vec<_>, _>>()
            .map_err(MakerError::Wallet)?
            .into_iter()
            .unzip();

        let create_funding_txes_result = wallet
            .create_funding_txes(send_amount, &coinswap_addresses, contract_feerate, None)
            .map_err(MakerError::Wallet)?;

        let mut outgoing_swapcoins = Vec::new();
        for (
            (((my_funding_tx, &utxo_index), &my_multisig_privkey), &other_multisig_pubkey),
            hashlock_pubkey,
        ) in create_funding_txes_result
            .funding_txes
            .iter()
            .zip(create_funding_txes_result.payment_output_positions.iter())
            .zip(my_multisig_privkeys.iter())
            .zip(next_multisig_pubkeys.iter())
            .zip(next_hashlock_pubkeys.iter())
        {
            let (timelock_pubkey, timelock_privkey) = crate::utill::generate_keypair();
            let contract_redeemscript = crate::protocol::contract::create_contract_redeemscript(
                hashlock_pubkey,
                &timelock_pubkey,
                &hashvalue,
                &locktime,
            );
            let funding_amount = my_funding_tx.output[utxo_index as usize].value;
            let my_senders_contract_tx = crate::protocol::contract::create_senders_contract_tx(
                bitcoin::OutPoint {
                    txid: my_funding_tx.compute_txid(),
                    vout: utxo_index,
                },
                funding_amount,
                &contract_redeemscript,
            )?;

            outgoing_swapcoins.push(UnifiedOutgoingSwapCoin::new_legacy(
                my_multisig_privkey,
                other_multisig_pubkey,
                my_senders_contract_tx,
                contract_redeemscript,
                timelock_privkey,
                funding_amount,
            ));
        }

        let mining_fees = Amount::from_sat(create_funding_txes_result.total_miner_fee);

        log::info!(
            "[{}] Created {} funding txs and {} outgoing swapcoins, mining_fees={}",
            self.config.network_port,
            create_funding_txes_result.funding_txes.len(),
            outgoing_swapcoins.len(),
            mining_fees
        );

        Ok((
            create_funding_txes_result.funding_txes,
            outgoing_swapcoins,
            mining_fees,
        ))
    }

    fn find_outgoing_swapcoin(
        &self,
        multisig_redeemscript: &bitcoin::ScriptBuf,
    ) -> Option<UnifiedOutgoingSwapCoin> {
        // Check the ongoing swap states for outgoing swapcoins
        if let Ok(swaps) = self.ongoing_swaps.lock() {
            for state in swaps.values() {
                for outgoing in &state.outgoing_swapcoins {
                    if outgoing.protocol == crate::protocol::ProtocolVersion::Legacy {
                        if let (Some(my_pubkey), Some(other_pubkey)) =
                            (&outgoing.my_pubkey, &outgoing.other_pubkey)
                        {
                            let computed_script =
                                crate::protocol::contract::create_multisig_redeemscript(
                                    my_pubkey,
                                    other_pubkey,
                                );
                            if &computed_script == multisig_redeemscript {
                                log::debug!(
                                    "[{}] Found outgoing swapcoin in ongoing swap state",
                                    self.config.network_port
                                );
                                return Some(outgoing.clone());
                            }
                        }
                    }
                }
            }
        }

        // Check unified outgoing swapcoins in wallet
        if let Ok(wallet) = self.wallet.read() {
            if let Some(unified_swapcoin) =
                wallet.find_unified_outgoing_swapcoin_by_multisig(multisig_redeemscript)
            {
                log::debug!(
                    "[{}] Found unified outgoing swapcoin in wallet store",
                    self.config.network_port
                );
                return Some(unified_swapcoin.clone());
            }
        }

        log::debug!(
            "[{}] No outgoing swapcoin found for multisig script",
            self.config.network_port
        );
        None
    }

    #[cfg(feature = "integration-test")]
    fn behavior(&self) -> UnifiedMakerBehavior {
        self.behavior
    }
}
