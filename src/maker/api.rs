//! The Maker API.
//!
//! Defines the core functionality of the Maker in a swap protocol implementation.
//! It includes structures for managing maker behavior, connection states, and recovery from swap events.
//! The module provides methods for initializing a Maker, verifying swap messages, and monitoring
//! contract broadcasts and handle idle Taker connections. Additionally, it handles recovery by broadcasting
//! contract transactions and claiming funds after an unsuccessful swap event.

use crate::{
    protocol::{
        contract::{check_hashvalues_are_equal, read_hashvalue_from_contract},
        messages::{FidelityProof, ReqContractSigsForSender, PREIMAGE_LEN},
        Hash160,
    },
    utill::{
        check_tor_status, get_maker_dir, redeemscript_to_scriptpubkey, BLOCK_DELAY,
        HEART_BEAT_INTERVAL, REQUIRED_CONFIRMS,
    },
    wallet::{RPCConfig, SwapCoin, WalletSwapCoin},
    watch_tower::{
        registry_storage::FileRegistry,
        rpc_backend::BitcoinRpc,
        service::WatchService,
        watcher::{Role, Watcher, WatcherEvent},
        zmq_backend::ZmqBackend,
    },
};
use bitcoin::{
    ecdsa::Signature,
    hashes::Hash,
    secp256k1::{self, Secp256k1},
    OutPoint, PublicKey, ScriptBuf, Transaction,
};
use bitcoind::bitcoincore_rpc::RpcApi;
use std::{
    collections::{HashMap, HashSet},
    convert::TryInto,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc, Arc, Mutex, RwLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::rpc::server::MakerRpc;

use crate::{
    protocol::{
        contract::{
            check_hashlock_has_pubkey, check_multisig_has_pubkey, check_reedemscript_is_multisig,
            find_funding_output_index, read_contract_locktime,
        },
        messages::ProofOfFunding,
    },
    wallet::{IncomingSwapCoin, OutgoingSwapCoin, Wallet, WalletError},
};

use super::{config::MakerConfig, error::MakerError};

/// Interval for health checks on a stable RPC connection with bitcoind.
pub const RPC_PING_INTERVAL: u32 = 9;

/// Maker triggers the recovery mechanism, if Taker is idle for more than 15 mins during a swap.
#[cfg(feature = "integration-test")]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(not(feature = "integration-test"))]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60 * 15);

/// The minimum difference in locktime (in blocks) between the incoming and outgoing swaps.
///
/// This value specifies the reaction time, in blocks, available to a Maker
/// to claim the refund transaction in case of recovery.
///
/// According to [BOLT #2](https://github.com/lightning/bolts/blob/aa5207aeaa32d841353dd2df3ce725a4046d528d/02-peer-protocol.md?plain=1#L1798),
/// the estimated minimum `cltv_expiry_delta` is 18 blocks.
/// To enhance safety, the default value is set to 20 blocks.
pub const MIN_CONTRACT_REACTION_TIME: u16 = 20;

/// spent with witnesses:
//  hashlock case:
//  <hashlock_signature> <preimage len 32>
pub const MIN_WITNESS_ITEM_FOR_HASHLOCK: usize = 2;
/// # Fee Parameters for Coinswap
///
/// These parameters define the fees charged by Makers in a coinswap transaction.
///
/// - `base_fee`: A fixed base fee charged by the Maker for providing its services (configurable in maker config, default: 100 sats in production, 1000 sats in tests)
/// - `amount_relative_fee_pct`: A percentage fee based on the swap amount (configurable in maker config, default: 0.1% in production, 2.5% in tests)
/// - `TIME_RELATIVE_FEE_PCT`: A percentage fee based on the refund locktime (duration the Maker must wait for a refund).
///
/// The coinswap fee increases with both the swap amount and the refund locktime.
/// Refer to `REFUND_LOCKTIME` and `REFUND_LOCKTIME_STEP` in `taker::api.rs` for related parameters.
///
/// ### Fee Calculation
/// The total fee for a swap is calculated as:
/// `total_fee = base_fee + (swap_amount * amount_relative_fee_pct) / 100 + (swap_amount * refund_locktime * time_relative_fee_pct) / 100`
///
/// ### Example (Default Values)
/// For a swap amount of 100,000 sats and a refund locktime of 20 blocks:
/// - `base_fee` = 100 sats (default production value)
/// - `amount_relative_fee` = (100,000 * 0.1) / 100 = 100 sats (default production value)
/// - `time_relative_fee` = (100,000 * 20 * 0.005) / 100 = 100 sats (default production value)
/// - `total_fee` = 300 sats (0.3%)
///
/// The default fee rate values are designed to asymptotically approach 5% of the swap amount as the swap amount increases.
///
/// Minimum Coinswap amount; makers will not accept amounts below this.
#[cfg(feature = "integration-test")]
pub const TIME_RELATIVE_FEE_PCT: f64 = 0.10;
#[cfg(not(feature = "integration-test"))]
pub const TIME_RELATIVE_FEE_PCT: f64 = 0.005;

pub const MIN_SWAP_AMOUNT: u64 = 10_000;

/// Interval for redeeming expired bonds, creating new ones if needed,
#[cfg(feature = "integration-test")]
pub(crate) const FIDELITY_BOND_UPDATE_INTERVAL: u32 = 30;
#[cfg(not(feature = "integration-test"))]
pub(crate) const FIDELITY_BOND_UPDATE_INTERVAL: u32 = 600; // 1 Block Interval
/// Interval to check if there is enough liquidity for swaps.
/// If the available balance is below the minimum, maker server won't listen for any swap requests until funds are added.
#[cfg(feature = "integration-test")]
pub(crate) const SWAP_LIQUIDITY_CHECK_INTERVAL: u32 = 30;
#[cfg(not(feature = "integration-test"))]
pub(crate) const SWAP_LIQUIDITY_CHECK_INTERVAL: u32 = 600; // Equals to FIDELITY_BOND_UPDATE_INTERVAL

/// Used to configure the maker for testing purposes.
///
/// This enum defines various behaviors that can be assigned to the maker during testing
/// to simulate different scenarios or states. These behaviors can help in verifying
/// the robustness and correctness of the system under different conditions.
#[derive(Debug, Clone, Copy)]
pub enum MakerBehavior {
    /// Represents the normal behavior of the maker.
    Normal,
    /// Simulates closure at the "Request Contract Signatures for Sender" step.
    CloseAtReqContractSigsForSender,
    /// Simulates closure at the "Proof of Funding" step.
    CloseAtProofOfFunding,
    /// Simulates closure at the "Contract Signatures for Receiver and Sender" step.
    CloseAtContractSigsForRecvrAndSender,
    /// Simulates closure at the "Contract Signatures for Receiver" step.
    CloseAtContractSigsForRecvr,
    /// Simulates closure at the "Hash Preimage" step.
    CloseAtHashPreimage,
    /// Simulates broadcasting the contract immediately after setup.
    BroadcastContractAfterSetup,
}

/// Expected messages for the taker in the context of [`ConnectionState`] structure.
///
/// If the received message doesn't match the expected message,
/// a protocol error will be returned.
#[derive(Debug, Default, PartialEq, Clone)]
pub(crate) enum ExpectedMessage {
    #[default]
    TakerHello,
    NewlyConnectedTaker,
    ReqContractSigsForSender,
    ProofOfFunding,
    ProofOfFundingORContractSigsForRecvrAndSender,
    ReqContractSigsForRecvr,
    HashPreimage,
    PrivateKeyHandover,
}

/// Maintains the state of a connection, including the list of swapcoins and the next expected message.
#[derive(Debug, Default, Clone)]
pub(crate) struct ConnectionState {
    pub(crate) allowed_message: ExpectedMessage,
    pub(crate) incoming_swapcoins: Vec<IncomingSwapCoin>,
    pub(crate) outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    pub(crate) pending_funding_txes: Vec<Transaction>,
}

pub(crate) struct ThreadPool {
    pub(crate) threads: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) port: u16,
}

impl ThreadPool {
    pub(crate) fn new(port: u16) -> Self {
        Self {
            threads: Mutex::new(Vec::new()),
            port,
        }
    }

    pub(crate) fn add_thread(&self, handle: JoinHandle<()>) {
        let mut threads = self.threads.lock().unwrap();
        threads.push(handle);
    }
    #[inline]
    pub(crate) fn join_all_threads(&self) -> Result<(), MakerError> {
        let mut threads = self
            .threads
            .lock()
            .map_err(|_| MakerError::General("Failed to lock threads"))?;

        log::info!("Joining {} threads", threads.len());

        let mut joined_count = 0;
        while let Some(thread) = threads.pop() {
            let thread_name = thread.thread().name().unwrap().to_string();
            println!("joining thread: {thread_name}");

            match thread.join() {
                Ok(_) => {
                    log::info!("[{}] Thread {} joined", self.port, thread_name);
                    joined_count += 1;
                }
                Err(e) => {
                    log::error!(
                        "[{}] Error {:?} while joining thread {}",
                        self.port,
                        e,
                        thread_name
                    );
                }
            }
        }

        log::info!("Successfully joined {joined_count} threads",);
        Ok(())
    }
}

/// Represents the maker in the swap protocol.
pub struct Maker {
    /// Defines special maker behavior, only applicable for testing
    pub(crate) behavior: MakerBehavior,
    /// Maker configurations
    pub(crate) config: MakerConfig,
    /// Maker's underlying wallet
    pub wallet: RwLock<Wallet>,
    /// A flag to trigger shutdown event
    pub shutdown: AtomicBool,
    /// Map of IP address to Connection State + last Connected instant
    pub(crate) ongoing_swap_state: Mutex<HashMap<String, (ConnectionState, Instant)>>,
    /// Highest Value Fidelity Proof
    pub(crate) highest_fidelity_proof: RwLock<Option<FidelityProof>>,
    /// Is setup complete
    pub is_setup_complete: AtomicBool,
    /// Path for the data directory.
    data_dir: PathBuf,
    /// Thread pool for managing all spawned threads
    pub(crate) thread_pool: Arc<ThreadPool>,
    /// Watcher service
    pub watch_service: WatchService,
}

#[allow(clippy::too_many_arguments)]
impl Maker {
    /// Initializes a Maker structure.
    ///
    /// This function sets up a Maker instance with configurable parameters.
    /// It handles the initialization of data directories, wallet files, and RPC configurations.
    ///
    /// ### Parameters:
    /// - `data_dir`:
    ///   - `Some(value)`: Use the specified directory for storing data.
    ///   - `None`: Use the default data directory (e.g., for Linux: `~/.coinswap/maker`).
    /// - `wallet_file_name`:
    ///   - `Some(value)`: Attempt to load a wallet file named `value`. If it does not exist, a new wallet with the given name will be created.
    ///   - `None`: Create a new wallet file with the default name `maker-wallet`.
    /// - If `rpc_config` = `None`: Use the default [`RPCConfig`]
    pub fn init(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        network_port: Option<u16>,
        rpc_port: Option<u16>,
        control_port: Option<u16>,
        tor_auth_password: Option<String>,
        socks_port: Option<u16>,
        behavior: MakerBehavior,
        zmq_addr: String,
        password: Option<String>,
    ) -> Result<Self, MakerError> {
        // Get the provided data directory or the default data directory.
        let data_dir = data_dir.unwrap_or(get_maker_dir());
        let wallets_dir = data_dir.join("wallets");

        // Use the provided name or default to `maker-wallet` if not specified.
        let wallet_file_name = wallet_file_name.unwrap_or_else(|| "maker-wallet".to_string());
        let wallet_path = wallets_dir.join(&wallet_file_name);

        let mut rpc_config = rpc_config.unwrap_or_default();
        rpc_config.wallet_name = wallet_file_name;

        // If the config file doesn't exist, the default config will be loaded.
        let mut config = MakerConfig::new(Some(&data_dir.join("config.toml")))?;

        if let Some(port) = network_port {
            config.network_port = port;
        }

        // ## TODO: Encapsulate these initialization inside the watcher and
        //     pollute the client declaration.
        let backend = ZmqBackend::new(&zmq_addr);
        let rpc_backend = BitcoinRpc::new(rpc_config.clone())?;
        let blockchain_info = rpc_backend.get_blockchain_info()?;
        let file_registry = data_dir
            .join(format!(".maker_{}_watcher", config.network_port))
            .join(blockchain_info.chain.to_string());
        let registry = FileRegistry::load(file_registry);
        let (tx_requests, rx_requests) = mpsc::channel();
        let (tx_events, rx_responses) = mpsc::channel();
        let rpc_config_watcher = rpc_config.clone();

        let mut watcher = Watcher::<Maker>::new(backend, registry, rx_requests, tx_events);
        _ = thread::Builder::new()
            .name("Watcher thread".to_string())
            .spawn(move || watcher.run(rpc_config_watcher));

        let watch_service = WatchService::new(tx_requests, rx_responses);

        let mut wallet = Wallet::load_or_init_wallet(&wallet_path, &rpc_config, password)?;

        if let Some(rpc_port) = rpc_port {
            config.rpc_port = rpc_port;
        }

        if let Some(socks_port) = socks_port {
            config.socks_port = socks_port;
        }

        if let Some(control_port) = control_port {
            config.control_port = control_port;
        }

        if let Some(tor_auth_password) = tor_auth_password {
            config.tor_auth_password = tor_auth_password;
        }

        if !cfg!(feature = "integration-test") {
            check_tor_status(config.control_port, config.tor_auth_password.as_str())?;
        }
        config.write_to_file(&data_dir.join("config.toml"))?;

        wallet.sync_and_save()?;

        let network_port = config.network_port;

        Ok(Self {
            behavior,
            config,
            wallet: RwLock::new(wallet),
            shutdown: AtomicBool::new(false),
            ongoing_swap_state: Mutex::new(HashMap::new()),
            highest_fidelity_proof: RwLock::new(None),
            is_setup_complete: AtomicBool::new(false),
            data_dir,
            thread_pool: Arc::new(ThreadPool::new(network_port)),
            watch_service,
        })
    }

    /// Returns data directory of Maker
    pub fn get_data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Returns a reference to the Maker's wallet.
    pub fn get_wallet(&self) -> &RwLock<Wallet> {
        &self.wallet
    }

    /// Ensures all unconfirmed fidelity bonds in the maker's wallet are tracked until confirmation.  
    /// Once confirmed, updates their confirmation details in the wallet.
    pub(super) fn track_and_update_unconfirmed_fidelity_bonds(&self) -> Result<(), MakerError> {
        let bond_conf_heights = {
            let wallet_read = self.get_wallet().read()?;

            wallet_read
                .store
                .fidelity_bond
                .iter()
                .filter_map(|(i, bond)| {
                    if bond.conf_height.is_none() && bond.cert_expiry.is_none() {
                        let conf_height = wallet_read
                            .wait_for_tx_confirmation(bond.outpoint.txid)
                            .unwrap();
                        Some((*i, conf_height))
                    } else {
                        None
                    }
                })
                .collect::<HashMap<u32, u32>>()
        };

        bond_conf_heights.into_iter().try_for_each(|(i, ht)| {
            self.get_wallet()
                .write()?
                .update_fidelity_bond_conf_details(i, ht)?;
            Ok::<(), MakerError>(())
        })?;

        Ok(())
    }

    /// Checks consistency of the [`ProofOfFunding`] message and return the Hashvalue
    /// used in hashlock transaction.
    pub(crate) fn verify_proof_of_funding(
        &self,
        message: &ProofOfFunding,
    ) -> Result<Hash160, MakerError> {
        if message.confirmed_funding_txes.is_empty() {
            return Err(MakerError::General("No funding txs provided by Taker"));
        }

        for funding_info in &message.confirmed_funding_txes {
            // check that the new locktime is sufficiently short enough compared to the
            // locktime in the provided funding tx
            let locktime = read_contract_locktime(&funding_info.contract_redeemscript)?;
            if locktime - message.refund_locktime < MIN_CONTRACT_REACTION_TIME {
                return Err(MakerError::General(
                    "Next hop locktime too close to current hop locktime",
                ));
            }

            let funding_output_index = find_funding_output_index(funding_info)?;

            // check the funding_tx is confirmed to required depth
            if let Some(txout) = self
                .wallet
                .read()?
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

            let (_, tweabale_pubkey) = self.wallet.read()?.get_tweakable_keypair()?;

            check_multisig_has_pubkey(
                &funding_info.multisig_redeemscript,
                &tweabale_pubkey,
                &funding_info.multisig_nonce,
            )?;

            check_hashlock_has_pubkey(
                &funding_info.contract_redeemscript,
                &tweabale_pubkey,
                &funding_info.hashlock_nonce,
            )?;

            // check that the provided contract matches the scriptpubkey from the
            // cache which was populated when the ReqContractSigsForSender message arrived
            let contract_spk = redeemscript_to_scriptpubkey(&funding_info.contract_redeemscript)?;

            if !self.wallet.read()?.does_prevout_match_cached_contract(
                &(OutPoint {
                    txid: funding_info.funding_tx.compute_txid(),
                    vout: funding_output_index,
                }),
                &contract_spk,
            )? {
                return Err(MakerError::General(
                    "Provided contract does not match sender contract tx, rejecting",
                ));
            }
        }

        Ok(check_hashvalues_are_equal(message)?)
    }

    /// Verify the contract transaction for Sender and return the signatures.
    pub(crate) fn verify_and_sign_contract_tx(
        &self,
        message: &ReqContractSigsForSender,
    ) -> Result<Vec<Signature>, MakerError> {
        let mut sigs = Vec::<Signature>::new();
        for txinfo in &message.txs_info {
            if txinfo.senders_contract_tx.input.len() != 1
                || txinfo.senders_contract_tx.output.len() != 1
            {
                return Err(MakerError::General(
                    "Invalid number of inputs or outputs in contract transaction",
                ));
            }

            if !self.wallet.read()?.does_prevout_match_cached_contract(
                &txinfo.senders_contract_tx.input[0].previous_output,
                &txinfo.senders_contract_tx.output[0].script_pubkey,
            )? {
                return Err(MakerError::General(
                    "Taker attempting multiple contract attacks, rejecting",
                ));
            }

            let (tweakable_privkey, tweakable_pubkey) =
                self.wallet.read()?.get_tweakable_keypair()?;

            check_multisig_has_pubkey(
                &txinfo.multisig_redeemscript,
                &tweakable_pubkey,
                &txinfo.multisig_nonce,
            )?;

            let secp = Secp256k1::new();

            let hashlock_privkey = tweakable_privkey.add_tweak(&txinfo.hashlock_nonce.into())?;

            let hashlock_pubkey = PublicKey {
                compressed: true,
                inner: secp256k1::PublicKey::from_secret_key(&secp, &hashlock_privkey),
            };

            crate::protocol::contract::is_contract_out_valid(
                &txinfo.senders_contract_tx.output[0],
                &hashlock_pubkey,
                &txinfo.timelock_pubkey,
                &message.hashvalue,
                &message.locktime,
                &MIN_CONTRACT_REACTION_TIME,
            )?;

            self.wallet.write()?.cache_prevout_to_contract(
                txinfo.senders_contract_tx.input[0].previous_output,
                txinfo.senders_contract_tx.output[0].script_pubkey.clone(),
            )?;

            let multisig_privkey = tweakable_privkey.add_tweak(&txinfo.multisig_nonce.into())?;

            let sig = crate::protocol::contract::sign_contract_tx(
                &txinfo.senders_contract_tx,
                &txinfo.multisig_redeemscript,
                txinfo.funding_input_value,
                &multisig_privkey,
            )?;
            sigs.push(sig);
        }
        Ok(sigs)
    }

    pub(crate) fn send_watch_request(
        &self,
        swap_coin: &impl SwapCoin,
    ) -> Result<Vec<Transaction>, MakerError> {
        let mut responses = Vec::new();

        let contract_tx = swap_coin.get_contract_tx();
        let contract_txid = contract_tx.compute_txid();

        for (vout, _) in contract_tx.output.iter().enumerate() {
            let outpoint = OutPoint {
                txid: contract_txid,
                vout: vout as u32,
            };
            self.watch_service.watch_request(outpoint);
            let watch_event = self.watch_service.wait_for_event();

            if let Some(WatcherEvent::UtxoSpent {
                spending_tx: Some(spending_tx),
                ..
            }) = watch_event
            {
                responses.push(spending_tx);
            }
        }
        Ok(responses)
    }
}

impl MakerRpc for Maker {
    fn wallet(&self) -> &RwLock<Wallet> {
        &self.wallet
    }
    fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    fn config(&self) -> &MakerConfig {
        &self.config
    }
    fn shutdown(&self) -> &AtomicBool {
        &self.shutdown
    }
}

/// Constantly checks for contract transactions in the bitcoin network for all
/// unsettled swap.
///
/// If any one of them is ever observed, run the recovery routine.
pub(crate) fn check_for_broadcasted_contracts(maker: Arc<Maker>) -> Result<(), MakerError> {
    let mut failed_swap_ip = Vec::new();
    loop {
        if maker.shutdown.load(Relaxed) {
            break;
        }
        // An extra scope to release all locks when done.
        {
            let mut lock_onstate = maker.ongoing_swap_state.lock()?;
            for (ip, (connection_state, _)) in lock_onstate.iter_mut() {
                let txids_to_watch = connection_state
                    .incoming_swapcoins
                    .iter()
                    .map(|is| is.contract_tx.compute_txid())
                    .chain(
                        connection_state
                            .outgoing_swapcoins
                            .iter()
                            .map(|oc| oc.contract_tx.compute_txid()),
                    )
                    .collect::<Vec<_>>();

                // No need to check for other contracts in the connection state, if any one of them
                // is ever observed in the mempool/block, run recovery routine.
                for txid in txids_to_watch {
                    let read_lock = maker.wallet.read()?;
                    let transaction_broadcasted =
                        read_lock.rpc.get_raw_transaction_info(&txid, None).is_ok();
                    drop(read_lock);
                    if transaction_broadcasted {
                        // Something is broadcasted. Report, Recover and Abort.
                        log::warn!(
                            "[{}] Contract txs broadcasted!! txid: {} Recovering from ongoing swaps.",
                            maker.config.network_port,
                            txid
                        );
                        failed_swap_ip.push(ip.clone());

                        // Spawn a separate thread to wait for contract maturity and broadcasting timelocked/hashlocked.
                        let maker_clone = maker.clone();
                        log::info!(
                            "[{}] Spawning recovery thread after seeing contracts in mempool",
                            maker.config.network_port
                        );

                        let incomings = connection_state.incoming_swapcoins.clone();
                        let outgoings = connection_state.outgoing_swapcoins.clone();

                        let handle = std::thread::Builder::new()
                            .name("Swap recovery thread".to_string())
                            .spawn(move || {
                                if let Err(e) = recover_from_swap(maker_clone, incomings, outgoings)
                                {
                                    log::error!("Failed to recover from swap due to: {e:?}");
                                }
                            })?;
                        maker.thread_pool.add_thread(handle);
                        // Clear the state value here
                        *connection_state = ConnectionState::default();
                        break;
                    }
                }
            }

            // Clear the state entry here
            for ip in failed_swap_ip.iter() {
                lock_onstate.remove(ip);
            }
        } // All locks are cleared here.

        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Checks for swapcoins present in wallet store on reboot and starts recovery if found on bitcoind network.
/// If any one of them is ever observed, run the recovery routine.
pub(crate) fn restore_broadcasted_contracts_on_reboot(
    maker: &Arc<Maker>,
) -> Result<(), MakerError> {
    let (incomings, outgoings) = maker.wallet.read()?.find_unfinished_swapcoins();
    // Spawn a separate thread to wait for contract maturity and broadcasting timelocked/hashlocked.
    let maker_clone = maker.clone();
    let handle = std::thread::Builder::new()
        .name("Swap recovery thread".to_string())
        .spawn(move || {
            if let Err(e) = recover_from_swap(maker_clone, incomings, outgoings) {
                log::error!("Failed to recover from swap due to: {e:?}");
            }
        })?;
    maker.thread_pool.add_thread(handle);

    Ok(())
}

/// Check that if any Taker connection went idle.
///
/// If a connection remains idle for more than idle timeout time, that's a potential DOS attack.
/// Broadcast the contract transactions and claim funds via timelock.
pub(crate) fn check_for_idle_states(maker: Arc<Maker>) -> Result<(), MakerError> {
    let mut bad_ip = Vec::new();

    loop {
        if maker.shutdown.load(Relaxed) {
            break;
        }
        let current_time = Instant::now();

        // Extra scope to release all locks when done.
        {
            let mut lock_on_state = maker.ongoing_swap_state.lock()?;
            for (ip, (state, last_connected_time)) in lock_on_state.iter_mut() {
                let no_response_since =
                    current_time.saturating_duration_since(*last_connected_time);

                if no_response_since > IDLE_CONNECTION_TIMEOUT {
                    log::error!(
                        "[{}] Potential Dropped Connection from taker. No response since : {} secs. Recovering from swap",
                        maker.config.network_port,
                        no_response_since.as_secs()
                    );
                    bad_ip.push(ip.clone());
                    // Spawn a separate thread to wait for contract maturity and broadcasting timelocked,hashlocked
                    let maker_clone = maker.clone();
                    log::info!(
                        "[{}] Spawning recovery thread after Taker dropped",
                        maker.config.network_port
                    );

                    let incomings = state.incoming_swapcoins.clone();
                    let outgoings = state.outgoing_swapcoins.clone();

                    let handle = std::thread::Builder::new()
                        .name("Swap Recovery Thread".to_string())
                        .spawn(move || {
                            if let Err(e) = recover_from_swap(maker_clone, incomings, outgoings) {
                                log::error!("Failed to recover from swap due to: {e:?}");
                            }
                        })?;
                    maker.thread_pool.add_thread(handle);
                    // Clear the state values here
                    *state = ConnectionState::default();
                    break;
                }
            }

            // Clear the state entry here
            for ip in bad_ip.iter() {
                lock_on_state.remove(ip);
            }
        } // All locks are cleared here

        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

pub(crate) fn recover_from_swap(
    maker: Arc<Maker>,
    incoming_swapcoins: Vec<IncomingSwapCoin>,
    outgoing_swapcoins: Vec<OutgoingSwapCoin>,
) -> Result<(), MakerError> {
    let timelock = outgoing_swapcoins
        .first()
        .and_then(|o| o.get_timelock().ok())
        .ok_or(MakerError::General("missing timelock on outgoing swapcoin"))?
        as u32;

    let start_height = maker
        .wallet
        .read()?
        .rpc
        .get_block_count()
        .map_err(WalletError::Rpc)? as u32;
    let timelock_expiry = start_height.saturating_add(timelock);

    log::info!(
        "[{}] recover_from_swap started | height={} timelock_expiry={}",
        maker.config.network_port,
        start_height,
        timelock_expiry
    );

    while !maker.shutdown.load(Relaxed) {
        let current_height = maker
            .wallet
            .read()?
            .rpc
            .get_block_count()
            .map_err(WalletError::Rpc)? as u32;

        if current_height >= timelock_expiry {
            log::info!(
                "[{}] timelock expired at {} (expiry={}), using timelock path",
                maker.config.network_port,
                current_height,
                timelock_expiry
            );
            return recover_via_timelock(maker, outgoing_swapcoins);
        }

        check_for_watch_response(&maker, &outgoing_swapcoins, &incoming_swapcoins)?;

        let wallet = maker.wallet.read()?;
        let all_preimages_known = incoming_swapcoins.iter().all(|incoming| {
            wallet
                .find_incoming_swapcoin(&incoming.get_multisig_redeemscript())
                .is_some_and(|s| s.is_hash_preimage_known())
        });
        drop(wallet);

        if all_preimages_known {
            log::info!(
                "[{}] all preimages known at height {}, using hashlock path",
                maker.config.network_port,
                current_height
            );
            return recover_via_hashlock(maker, incoming_swapcoins);
        }

        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

fn recover_via_hashlock(
    maker: Arc<Maker>,
    incoming: Vec<IncomingSwapCoin>,
) -> Result<(), MakerError> {
    let infos = maker
        .wallet
        .write()?
        .broadcast_incoming_contracts(incoming.clone())?;

    while !maker.shutdown.load(Relaxed) {
        let broadcasted = maker
            .wallet
            .write()?
            .spend_from_hashlock_contract(&infos, &maker.watch_service)?;
        log::info!(
            "[{}] hashlock recovery: {}/{} txs broadcasted",
            maker.config.network_port,
            broadcasted.len(),
            incoming.len()
        );
        if broadcasted.len() == incoming.len() {
            #[cfg(feature = "integration-test")]
            maker.shutdown.store(true, Relaxed);
            break;
        }
        std::thread::sleep(HEART_BEAT_INTERVAL);
    }
    Ok(())
}

fn recover_via_timelock(
    maker: Arc<Maker>,
    outgoing: Vec<OutgoingSwapCoin>,
) -> Result<(), MakerError> {
    let infos = maker
        .wallet
        .write()?
        .broadcast_outgoing_contracts(outgoing.clone())?;

    while !maker.shutdown.load(Relaxed) {
        let broadcasted = maker
            .wallet
            .write()?
            .spend_from_timelock_contract(&infos, &maker.watch_service)?;
        log::info!(
            "[{}] timelock recovery: {}/{} txs broadcasted",
            maker.config.network_port,
            broadcasted.len(),
            outgoing.len()
        );
        if broadcasted.len() == outgoing.len() {
            #[cfg(feature = "integration-test")]
            maker.shutdown.store(true, Relaxed);
            break;
        }
        std::thread::sleep(BLOCK_DELAY);
    }
    Ok(())
}

/// Checks for `WatchResponse` messages from the tracker, extracts preimages from mempool
/// transactions, and updates maker swapcoins accordingly.
fn check_for_watch_response(
    maker: &Maker,
    outgoings: &[OutgoingSwapCoin],
    incomings: &[IncomingSwapCoin],
) -> Result<(), MakerError> {
    let mut seen_outpoints = HashSet::new();
    let mut preimages = Vec::new();

    // Collect all responses for the outgoing contracts
    let mut responses = Vec::new();
    for outgoing in outgoings {
        responses.extend(maker.send_watch_request(outgoing)?);
    }

    for transaction in responses {
        log::info!(
            "[{}] Received WatchResponse with mempool txs: {:?}",
            maker.config.network_port,
            transaction
        );

        for input in &transaction.input {
            let outpoint = (input.previous_output.txid, input.previous_output.vout);
            if seen_outpoints.insert(outpoint)
                && input.witness.len() >= MIN_WITNESS_ITEM_FOR_HASHLOCK
                && input.witness[1].len() == PREIMAGE_LEN
            {
                if let Ok(preimage) = input.witness[1].try_into() {
                    preimages.push(preimage);
                }
            }
        }
        update_swapcoins_with_preimages(maker, incomings, &preimages, true)?;
        update_swapcoins_with_preimages(maker, outgoings, &preimages, false)?;
    }

    Ok(())
}

fn update_swapcoins_with_preimages<T: SwapCoin>(
    maker: &Maker,
    coins: &[T],
    preimages: &[[u8; 32]],
    is_incoming: bool,
) -> Result<(), MakerError> {
    if preimages.is_empty() {
        return Ok(());
    }

    let mut wallet = maker.wallet.write()?;

    let mut apply_preimage = |redeemscript: &ScriptBuf, preimage: [u8; 32]| {
        if is_incoming {
            if let Some(swap) = wallet.find_incoming_swapcoin_mut(redeemscript) {
                swap.hash_preimage = Some(preimage);
                return true;
            }
        } else if let Some(swap) = wallet.find_outgoing_swapcoin_mut(redeemscript) {
            swap.hash_preimage = Some(preimage);
            return true;
        }
        false
    };

    for coin in coins {
        for preimage in preimages {
            let hash = Hash160::hash(preimage);
            let matches_contract =
                read_hashvalue_from_contract(&coin.get_contract_redeemscript())? == hash;
            if matches_contract {
                let redeemscript = coin.get_multisig_redeemscript();
                if apply_preimage(&redeemscript, *preimage) {
                    log::info!(
                        "[{}] Applied preimage for {} swapcoin {:?}",
                        maker.config.network_port,
                        if is_incoming { "incoming" } else { "outgoing" },
                        redeemscript
                    );
                }
            }
        }
    }

    Ok(())
}

impl Role for Maker {
    const RUN_DISCOVERY: bool = false;
}
