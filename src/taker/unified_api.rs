//! Unified Taker API for both Legacy (ECDSA) and Taproot (MuSig2) protocols.

use std::{
    net::TcpStream,
    path::PathBuf,
    sync::{mpsc, Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    thread,
    time::{Duration, Instant},
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
        legacy_messages::LegacyHashPreimage,
        router::{MakerToTakerMessage, TakerToMakerMessage},
        taproot_messages::TaprootHashPreimage,
    },
    utill::{check_tor_status, generate_maker_keys, get_taker_dir, read_message, send_message},
    wallet::{
        unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
        RPCConfig, Wallet,
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
    /// Multisig nonces for each outgoing swapcoin (used in ProofOfFunding).
    pub(crate) multisig_nonces: Vec<SecretKey>,
    /// Hashlock nonces for each outgoing swapcoin (used in ProofOfFunding).
    pub(crate) hashlock_nonces: Vec<SecretKey>,
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
    /// Test behavior.
    #[cfg(feature = "integration-test")]
    pub behavior: UnifiedTakerBehavior,
}

impl Drop for UnifiedTaker {
    fn drop(&mut self) {
        log::info!("Shutting down unified taker.");
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

        Ok(UnifiedTaker {
            config,
            wallet: Arc::new(RwLock::new(wallet)),
            offerbook,
            watch_service,
            offer_sync_handle,
            ongoing_swap: None,
            #[cfg(feature = "integration-test")]
            behavior: UnifiedTakerBehavior::Normal,
        })
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
            multisig_nonces: Vec::new(),
            hashlock_nonces: Vec::new(),
        });

        self.discover_and_select_makers()?;

        #[cfg(feature = "integration-test")]
        if self.behavior == UnifiedTakerBehavior::CloseEarly {
            log::warn!("Test behavior: closing early after maker selection");
            return Err(TakerError::General(
                "Test: Closing early after maker selection".to_string(),
            ));
        }

        self.negotiate_swap_details()?;

        self.initialize_swap_funding()?;

        if self.swap_state()?.params.protocol == ProtocolVersion::Legacy {
            log::info!("Using multi-hop Legacy flow with ProofOfFunding");
            self.exchange_legacy_contract_data()?;
        } else {
            log::info!("Using simplified Taproot flow");
            self.exchange_contract_data()?;
            self.broadcast_contract_txs()?;
        }

        self.finalize_swap()?;

        {
            let mut wallet = self.write_wallet()?;
            let swept = wallet.sweep_unified_incoming_swapcoins(2.0)?;
            log::info!("Swept {} incoming swapcoins", swept.len());
            wallet.sync_and_save()?;
        }

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
    fn discover_and_select_makers(&mut self) -> Result<(), TakerError> {
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
            .map(|offer_and_address| MakerConnection {
                offer_and_address,
                protocol,
                tweakable_point: None,
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

            let mut stream = self.connect_to_maker(&maker_address)?;

            let negotiated_protocol = self.handshake_maker(&mut stream)?;
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
    fn initialize_swap_funding(&mut self) -> Result<(), TakerError> {
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

        let swap = self.swap_state_mut()?;
        swap.multisig_nonces = multisig_nonces;
        swap.hashlock_nonces = hashlock_nonces;

        let mut wallet = self.write_wallet()?;

        let network = wallet.store.network;

        let swapcoins = match protocol {
            ProtocolVersion::Legacy => Self::create_legacy_funding_static(
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
            ProtocolVersion::Taproot => {
                // TODO: Use nonces for taproot as well
                Self::create_taproot_contracts_static(
                    &mut wallet,
                    &[tweakable_point],
                    &hashlock_pubkeys,
                    preimage,
                    refund_locktime,
                    send_amount,
                    &swap_id,
                    network,
                    manually_selected_outpoints,
                )?
            }
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
    pub(crate) fn handshake_maker(
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
    fn effective_connection_type(&self) -> ConnectionType {
        if cfg!(feature = "integration-test") {
            ConnectionType::Clearnet
        } else {
            self.config.connection_type
        }
    }

    /// Connect to a maker using either direct connection or Tor proxy.
    pub(crate) fn connect_to_maker(&self, address: &str) -> Result<TcpStream, TakerError> {
        log::debug!("Connecting to maker at {}", address);
        let timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);

        let socket = match self.effective_connection_type() {
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
    pub(crate) fn wait_for_txids_confirmation(&self, txids: &[Txid]) -> Result<(), TakerError> {
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

            if start.elapsed() > timeout {
                return Err(TakerError::FundingTxWaitTimeOut);
            }

            thread::sleep(Duration::from_secs(5));
        }
    }

    /// Finalize the swap by revealing preimage and exchanging private keys.
    fn finalize_swap(&mut self) -> Result<(), TakerError> {
        log::info!("Finalizing swap...");

        let maker_privkeys = self.reveal_preimage_and_collect_privkeys()?;
        self.set_incoming_swapcoin_privkey(&maker_privkeys)?;
        self.forward_privkeys_between_makers(&maker_privkeys)?;
        self.persist_incoming_swapcoins()?;

        log::info!("Swap finalized successfully");
        Ok(())
    }

    /// Phase 1: Reveal preimage to each maker (in reverse order) and collect their private keys.
    fn reveal_preimage_and_collect_privkeys(
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
            let mut stream = self.connect_to_maker(&maker_address)?;

            self.handshake_maker(&mut stream)?;

            let preimage_msg = Self::make_preimage_message(protocol, swap_id.clone(), preimage);
            send_message(&mut stream, &preimage_msg)?;

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

            if i == 0 {
                let swap = self.swap_state()?;
                if let Some(outgoing) = swap.outgoing_swapcoins.first() {
                    if let Some(privkey) = outgoing.my_privkey {
                        let msg = Self::make_handover_message(protocol, swap_id.clone(), privkey);
                        send_message(&mut stream, &msg)?;
                    }
                }
            }
        }

        Ok(maker_privkeys)
    }

    fn set_incoming_swapcoin_privkey(
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

    fn forward_privkeys_between_makers(
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
                let mut stream = self.connect_to_maker(&maker_address)?;

                self.handshake_maker(&mut stream)?;

                log::info!(
                    "Forwarding maker {}'s outgoing privkey to maker {}",
                    i - 1,
                    i
                );

                let msg =
                    Self::make_handover_message(protocol, swap_id.clone(), prev_maker_privkey);
                send_message(&mut stream, &msg)?;
            } else {
                log::warn!("No privkey from maker {} to forward to maker {}", i - 1, i);
            }
        }

        Ok(())
    }

    /// Persist the taker's incoming swapcoins with their preimage to the wallet.
    fn persist_incoming_swapcoins(&mut self) -> Result<(), TakerError> {
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
    fn make_preimage_message(
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
    fn make_handover_message(
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

    /// Recover from a failed swap by spending contract outputs back to wallet.
    pub fn recover_from_swap(&mut self) -> Result<(), TakerError> {
        log::warn!("Starting swap recovery...");

        // Phase 1: Set preimage on incoming swapcoins (requires mutable swap access).
        let preimage = self.swap_state()?.preimage;
        let swap = self.swap_state_mut()?;
        for incoming in &mut swap.incoming_swapcoins {
            if incoming.hash_preimage.is_none() {
                incoming.set_preimage(preimage);
            }
        }

        // Phase 2: Persist swapcoins to wallet (requires wallet write lock + shared swap access).
        {
            let mut wallet = self.write_wallet()?;
            let swap = self.swap_state()?;

            for outgoing in &swap.outgoing_swapcoins {
                wallet.add_unified_outgoing_swapcoin(outgoing);
            }

            for incoming in &swap.incoming_swapcoins {
                wallet.add_unified_incoming_swapcoin(incoming);
            }

            wallet.save_to_disk()?;

            let incoming_count = swap.incoming_swapcoins.len();
            if incoming_count > 0 {
                log::info!(
                    "Attempting recovery for {} incoming swapcoins",
                    incoming_count
                );
                match wallet.sweep_unified_incoming_swapcoins(2.0) {
                    Ok(swept) => {
                        if !swept.is_empty() {
                            log::info!("Successfully swept {} incoming swapcoins", swept.len());
                        }
                    }
                    Err(e) => {
                        log::warn!("Could not sweep incoming swapcoins now: {:?}", e);
                        log::info!("Will need to wait for timelock expiry or manual recovery");
                    }
                }
            }
        }

        // Phase 3: Register outgoing contracts for monitoring (no wallet lock needed).
        let swap = self.swap_state()?;
        let outgoing_count = swap.outgoing_swapcoins.len();
        if outgoing_count > 0 {
            log::info!(
                "Registered {} outgoing swapcoins for timelock recovery",
                outgoing_count
            );

            for outgoing in &swap.outgoing_swapcoins {
                if !outgoing.contract_tx.input.is_empty() {
                    let txid = outgoing.contract_tx.compute_txid();
                    let outpoint = OutPoint { txid, vout: 0 };
                    self.watch_service.register_watch_request(outpoint);
                    log::info!("Registered outgoing contract {} for monitoring", outpoint);
                }
            }
        }

        self.ongoing_swap = None;

        log::info!("Recovery complete. Swapcoins registered for monitoring.");
        log::info!("Use wallet recovery functions to spend timelocked outputs when ready.");

        Ok(())
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
}
