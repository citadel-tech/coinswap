//! The Maker API.
//!
//! Defines the core functionality of the Maker in a swap protocol implementation.
//! It includes structures for managing maker behavior, connection states, and recovery from swap events.
//! The module provides methods for initializing a Maker, verifying swap messages, and monitoring
//! contract broadcasts and handle idle Taker connections. Additionally, it handles recovery by broadcasting
//! contract transactions and claiming funds after an unsuccessful swap event.

use crate::{
    protocol::{
        contract::check_hashvalues_are_equal,
        messages::{FidelityProof, ReqContractSigsForSender},
        Hash160,
    },
    utill::{
        check_tor_status, get_maker_dir, redeemscript_to_scriptpubkey, ConnectionType,
        DEFAULT_TX_FEE_RATE, HEART_BEAT_INTERVAL, REQUIRED_CONFIRMS,
    },
    wallet::{RPCConfig, SwapCoin, WalletSwapCoin},
};
use bitcoin::{
    ecdsa::Signature,
    secp256k1::{self, Secp256k1},
    OutPoint, PublicKey, ScriptBuf, Transaction,
};
use bitcoind::bitcoincore_rpc::RpcApi;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc, Mutex, RwLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

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

/// # Fee Parameters for Coinswap
///
/// These parameters define the fees charged by Makers in a coinswap transaction.
///
/// TODO: These parameters are currently hardcoded. Consider making them configurable for Makers in the future.
///p
/// - `BASE_FEE`: A fixed base fee charged by the Maker for providing its services
/// - `AMOUNT_RELATIVE_FEE_PCT`: A percentage fee based on the swap amount.
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
/// - `base_fee` = 1,000 sats
/// - `amount_relative_fee` = (100,000 * 2.5) / 100 = 2,500 sats
/// - `time_relative_fee` = (100,000 * 20 * 0.1) / 100 = 2,000 sats
/// - `total_fee` = 5,500 sats (5.5%)
///
/// Fee rates are designed to asymptotically approach 5% of the swap amount as the swap amount increases..
#[cfg(feature = "integration-test")]
pub const BASE_FEE: u64 = 1000;
#[cfg(feature = "integration-test")]
pub const AMOUNT_RELATIVE_FEE_PCT: f64 = 2.50;
#[cfg(feature = "integration-test")]
pub const TIME_RELATIVE_FEE_PCT: f64 = 0.10;

#[cfg(not(feature = "integration-test"))]
pub const BASE_FEE: u64 = 100;
#[cfg(not(feature = "integration-test"))]
pub const AMOUNT_RELATIVE_FEE_PCT: f64 = 0.1;
#[cfg(not(feature = "integration-test"))]
pub const TIME_RELATIVE_FEE_PCT: f64 = 0.005;

/// Minimum Coinswap amount; makers will not#[cfg(feature = "integration-test")] accept amounts below this.
pub const MIN_SWAP_AMOUNT: u64 = 10_000;

/// Interval for redeeming expired bonds, creating new ones if needed,  
/// and updating the DNS server with the latest bond proof and maker address.
#[cfg(feature = "integration-test")]
pub(crate) const FIDELITY_BOND_DNS_UPDATE_INTERVAL: u32 = 30;
#[cfg(not(feature = "integration-test"))]
pub(crate) const FIDELITY_BOND_DNS_UPDATE_INTERVAL: u32 = 600; // 1 Block Interval

/// Interval to check if there is enough liquidity for swaps.
/// If the available balance is below the minimum, maker server won't listen for any swap requests until funds are added.
#[cfg(feature = "integration-test")]
pub(crate) const SWAP_LIQUIDITY_CHECK_INTERVAL: u32 = 30;
#[cfg(not(feature = "integration-test"))]
pub(crate) const SWAP_LIQUIDITY_CHECK_INTERVAL: u32 = 600; // Equals to FIDELITY_BOND_DNS_UPDATE_INTERVAL

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

/// Expected messages for the taker in the context of [ConnectionState] structure.
///
/// If the received message doesn't match expected message,
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
            println!("joining thread: {}", thread_name);

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

        log::info!("Successfully joined {} threads", joined_count,);
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
        connection_type: Option<ConnectionType>,
        behavior: MakerBehavior,
    ) -> Result<Self, MakerError> {
        // Get provided data directory or the default data directory.
        let data_dir = data_dir.unwrap_or(get_maker_dir());
        let wallets_dir = data_dir.join("wallets");

        // Use the provided name or default to `maker-wallet` if not specified.
        let wallet_file_name = wallet_file_name.unwrap_or_else(|| "maker-wallet".to_string());
        let wallet_path = wallets_dir.join(&wallet_file_name);

        let mut rpc_config = rpc_config.unwrap_or_default();

        rpc_config.wallet_name = wallet_file_name;

        let mut wallet = if wallet_path.exists() {
            // wallet already exists , load the wallet
            let wallet = Wallet::load(&wallet_path, &rpc_config)?;
            log::info!("Wallet file at {:?} successfully loaded.", wallet_path);
            wallet
        } else {
            // wallet doesn't exists at the given path , create a new one
            let wallet = Wallet::init(&wallet_path, &rpc_config)?;
            log::info!("New Wallet created at : {:?}", wallet_path);
            wallet
        };

        // If config file doesn't exist, default config will be loaded.
        let mut config = MakerConfig::new(Some(&data_dir.join("config.toml")))?;

        if let Some(port) = network_port {
            config.network_port = port;
        }

        if let Some(rpc_port) = rpc_port {
            config.rpc_port = rpc_port;
        }

        if let Some(socks_port) = socks_port {
            config.socks_port = socks_port;
        }

        if let Some(connection_type) = connection_type {
            config.connection_type = connection_type;
        }

        if let Some(control_port) = control_port {
            config.control_port = control_port;
        }

        if let Some(tor_auth_password) = tor_auth_password {
            config.tor_auth_password = tor_auth_password;
        }

        if matches!(connection_type, Some(ConnectionType::TOR)) {
            check_tor_status(config.control_port, config.tor_auth_password.as_str())?;
        }

        config.write_to_file(&data_dir.join("config.toml"))?;

        log::info!("Initializing wallet sync");
        wallet.sync()?;
        log::info!("Completed wallet sync");

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
                .filter_map(|(i, (bond, _))| {
                    if bond.conf_height.is_none() && bond.cert_expiry.is_none() {
                        let conf_height = wallet_read
                            .wait_for_fidelity_tx_confirmation(bond.outpoint.txid)
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

    /// Checks consistency of the [ProofOfFunding] message and return the Hashvalue
    /// used in hashlock transaction.
    pub(crate) fn verify_proof_of_funding(
        &self,
        message: &ProofOfFunding,
    ) -> Result<Hash160, MakerError> {
        if message.confirmed_funding_txes.is_empty() {
            return Err(MakerError::General("No funding txs provided by Taker"));
        }

        for funding_info in &message.confirmed_funding_txes {
            // check that the new locktime is sufficently short enough compared to the
            // locktime in the provided funding tx
            let locktime = read_contract_locktime(&funding_info.contract_redeemscript)?;
            if locktime - message.refund_locktime < MIN_CONTRACT_REACTION_TIME {
                return Err(MakerError::General(
                    "Next hop locktime too close to current hop locktime",
                ));
            }

            let funding_output_index = find_funding_output_index(funding_info)?;

            //check the funding_tx is confirmed to required depth
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
                        "funding tx not confirmed to required depth",
                    ));
                }
            } else {
                return Err(MakerError::General("funding tx output doesnt exist"));
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

            //check that the provided contract matches the scriptpubkey from the
            //cache which was populated when the ReqContractSigsForSender message arrived
            let contract_spk = redeemscript_to_scriptpubkey(&funding_info.contract_redeemscript)?;

            if !self.wallet.read()?.does_prevout_match_cached_contract(
                &(OutPoint {
                    txid: funding_info.funding_tx.compute_txid(),
                    vout: funding_output_index,
                }),
                &contract_spk,
            )? {
                return Err(MakerError::General(
                    "provided contract does not match sender contract tx, rejecting",
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
                    "invalid number of inputs or outputs in contract transaction",
                ));
            }

            if !self.wallet.read()?.does_prevout_match_cached_contract(
                &txinfo.senders_contract_tx.input[0].previous_output,
                &txinfo.senders_contract_tx.output[0].script_pubkey,
            )? {
                return Err(MakerError::General(
                    "taker attempting multiple contract attack, rejecting",
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
}

/// Constantly checks for contract transactions in the bitcoin network for all
/// unsettled swap.
///
/// If any one of the is ever observed, run the recovery routine.
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
                    if maker
                        .wallet
                        .read()?
                        .rpc
                        .get_raw_transaction_info(&txid, None)
                        .is_ok()
                    {
                        let mut outgoings = Vec::new();
                        let mut incomings = Vec::new();
                        // Something is broadcasted. Report, Recover and Abort.
                        log::warn!(
                            "[{}] Contract txs broadcasted!! txid: {} Recovering from ongoing swaps.",
                            maker.config.network_port,
                            txid
                        );
                        // Extract Incoming and Outgoing contracts, and timelock spends of the contract transactions.
                        // fully signed.
                        for (og_sc, ic_sc) in connection_state
                            .outgoing_swapcoins
                            .iter()
                            .zip(connection_state.incoming_swapcoins.iter())
                        {
                            let contract_timelock = og_sc.get_timelock()?;
                            let next_internal_address =
                                &maker.wallet.read()?.get_next_internal_addresses(1)?[0];
                            let time_lock_spend = maker.wallet.read()?.create_timelock_spend(
                                og_sc,
                                next_internal_address,
                                DEFAULT_TX_FEE_RATE,
                            )?;
                            // Sometimes we might not have other's contact signatures.
                            // This means the protocol have been stopped abruptly.
                            // This needs more careful consideration as this should not happen
                            // after funding transactions have been broadcasted for outgoing contracts.
                            // For incomings, its less lethal as thats mostly the other party's burden.
                            if let Ok(tx) = og_sc.get_fully_signed_contract_tx() {
                                outgoings.push((
                                    (og_sc.get_multisig_redeemscript(), tx),
                                    (contract_timelock, time_lock_spend),
                                ));
                            } else {
                                log::warn!(
                                    "[{}] Outgoing contact signature not known. Not Broadcasting",
                                    maker.config.network_port
                                );
                            }
                            if let Ok(tx) = ic_sc.get_fully_signed_contract_tx() {
                                incomings.push((ic_sc.get_multisig_redeemscript(), tx));
                            } else {
                                log::warn!(
                                    "[{}] Incoming contact signature not known. Not Broadcasting",
                                    maker.config.network_port
                                );
                            }
                        }
                        failed_swap_ip.push(ip.clone());

                        // Spawn a separate thread to wait for contract maturity and broadcasting timelocked.
                        let maker_clone = maker.clone();
                        log::info!(
                            "[{}] Spawning recovery thread after seeing contracts in mempool",
                            maker.config.network_port
                        );
                        let handle = std::thread::Builder::new()
                            .name("Swap recovery thread".to_string())
                            .spawn(move || {
                                if let Err(e) = recover_from_swap(maker_clone, outgoings, incomings)
                                {
                                    log::error!("Failed to recover from swap due to: {:?}", e);
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
///
/// If any one of the is ever observed, run the recovery routine.
pub(crate) fn restore_broadcasted_contracts_on_reboot(
    maker: &Arc<Maker>,
) -> Result<(), MakerError> {
    let (inc, out) = maker.wallet.read()?.find_unfinished_swapcoins();
    let mut outgoings = Vec::new();
    let mut incomings = Vec::new();
    // Extract Incoming and Outgoing contracts, and timelock spends of the contract transactions.
    // fully signed.
    for og_sc in out.iter() {
        let contract_timelock = og_sc.get_timelock()?;
        let next_internal_address = &maker.wallet.read()?.get_next_internal_addresses(1)?[0];
        let time_lock_spend = maker.wallet.read()?.create_timelock_spend(
            og_sc,
            next_internal_address,
            DEFAULT_TX_FEE_RATE,
        )?;

        let tx = match og_sc.get_fully_signed_contract_tx() {
            Ok(tx) => tx,
            Err(e) => {
                log::error!(
                    "Error: {:?} \
                    This was not supposed to happen. \
                    Kindly open an issue at https://github.com/citadel-tech/coinswap/issues.",
                    e
                );
                maker
                    .wallet
                    .write()?
                    .remove_outgoing_swapcoin(&og_sc.get_multisig_redeemscript())?;
                continue;
            }
        };
        outgoings.push((
            (og_sc.get_multisig_redeemscript(), tx),
            (contract_timelock, time_lock_spend),
        ));
    }

    for ic_sc in inc.iter() {
        let tx = match ic_sc.get_fully_signed_contract_tx() {
            Ok(tx) => tx,
            Err(e) => {
                log::error!(
                    "Error: {:?} \
                    This was not supposed to happen. \
                    Kindly open an issue at https://github.com/citadel-tech/coinswap/issues.",
                    e
                );
                maker
                    .wallet
                    .write()?
                    .remove_incoming_swapcoin(&ic_sc.get_multisig_redeemscript())?;
                continue;
            }
        };
        incomings.push((ic_sc.get_multisig_redeemscript(), tx));
    }

    // Spawn a separate thread to wait for contract maturity and broadcasting timelocked.
    let maker_clone = maker.clone();
    let handle = std::thread::Builder::new()
        .name("Swap recovery thread".to_string())
        .spawn(move || {
            if let Err(e) = recover_from_swap(maker_clone, outgoings, incomings) {
                log::error!("Failed to recover from swap due to: {:?}", e);
            }
        })?;
    maker.thread_pool.add_thread(handle);

    Ok(())
}

/// Check that if any Taker connection went idle.
///
/// If a connection remains idle for more than idle timeout time, thats a potential DOS attack.
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
                let mut outgoings = Vec::new();
                let mut incomings = Vec::new();

                let no_response_since =
                    current_time.saturating_duration_since(*last_connected_time);

                if no_response_since > IDLE_CONNECTION_TIMEOUT {
                    log::error!(
                        "[{}] Potential Dropped Connection from taker. No response since : {} secs. Recovering from swap",
                        maker.config.network_port,
                        no_response_since.as_secs()
                    );

                    // Extract Incoming and Outgoing contracts, and timelock spends of the contract transactions.
                    // fully signed.
                    for (og_sc, ic_sc) in state
                        .outgoing_swapcoins
                        .iter()
                        .zip(state.incoming_swapcoins.iter())
                    {
                        let contract_timelock = og_sc.get_timelock()?;
                        let contract = og_sc.get_fully_signed_contract_tx()?;
                        let next_internal_address =
                            &maker.wallet.read()?.get_next_internal_addresses(1)?[0];
                        let time_lock_spend = maker.wallet.read()?.create_timelock_spend(
                            og_sc,
                            next_internal_address,
                            DEFAULT_TX_FEE_RATE,
                        )?;
                        outgoings.push((
                            (og_sc.get_multisig_redeemscript(), contract),
                            (contract_timelock, time_lock_spend),
                        ));
                        let incoming_contract = ic_sc.get_fully_signed_contract_tx()?;
                        incomings.push((ic_sc.get_multisig_redeemscript(), incoming_contract));
                    }
                    bad_ip.push(ip.clone());
                    // Spawn a separate thread to wait for contract maturity and broadcasting timelocked.
                    let maker_clone = maker.clone();
                    log::info!(
                        "[{}] Spawning recovery thread after Taker dropped",
                        maker.config.network_port
                    );
                    let handle = std::thread::Builder::new()
                        .name("Swap Recovery Thread".to_string())
                        .spawn(move || {
                            if let Err(e) = recover_from_swap(maker_clone, outgoings, incomings) {
                                log::error!("Failed to recover from swap due to: {:?}", e);
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

/// Broadcast Incoming and Outgoing Contract transactions & timelock transactions after maturity.
/// Remove contract transactions from the wallet.
pub(crate) fn recover_from_swap(
    maker: Arc<Maker>,
    // Tuple of ((Multisig_reedemscript, Contract Tx), (Timelock, Timelock Tx))
    outgoings: Vec<((ScriptBuf, Transaction), (u16, Transaction))>,
    // Tuple of (Multisig Reedemscript, Contract Tx)
    incomings: Vec<(ScriptBuf, Transaction)>,
) -> Result<(), MakerError> {
    // broadcast all the incoming contracts and remove them from the wallet.
    for (incoming_reedemscript, tx) in incomings {
        if maker
            .wallet
            .read()?
            .rpc
            .get_raw_transaction_info(&tx.compute_txid(), None)
            .is_ok()
        {
            log::info!(
                "[{}] Incoming Contract Already Broadcasted",
                maker.config.network_port
            );
        } else if let Err(e) = maker.wallet.read()?.send_tx(&tx) {
            log::info!(
                "Can't send incoming contract: {} | {:?}",
                tx.compute_txid(),
                e
            );
        } else {
            log::info!(
                "[{}] Broadcasted Incoming Contract : {}",
                maker.config.network_port,
                tx.compute_txid()
            );
        }

        let removed_incoming = maker
            .wallet
            .write()?
            .remove_incoming_swapcoin(&incoming_reedemscript)?
            .expect("Incoming swapcoin expected");
        log::info!(
            "[{}] Removed Incoming Swapcoin From Wallet, Contract Txid : {}",
            maker.config.network_port,
            removed_incoming.contract_tx.compute_txid()
        );
    }

    //broadcast all the outgoing contracts
    for ((og_rs, tx), _) in outgoings.iter() {
        let check_tx_result = maker
            .wallet
            .read()?
            .rpc
            .get_raw_transaction_info(&tx.compute_txid(), None);

        match check_tx_result {
            Ok(_) => {
                log::info!(
                    "[{}] Outgoing Contract already broadcasted",
                    maker.config.network_port
                );
            }
            Err(_) => {
                let send_tx_result = maker.wallet.read()?.send_tx(tx);
                match send_tx_result {
                    Ok(_) => {
                        log::info!(
                            "[{}] Broadcasted Outgoing Contract : {}",
                            maker.config.network_port,
                            tx.compute_txid()
                        );
                    }
                    Err(e) => {
                        log::info!(
                            "Can't send ougoing contract: {} | {:?}",
                            tx.compute_txid(),
                            e
                        );
                        if format!("{:?}", e).contains("bad-txns-inputs-missingorspent") {
                            // This means the funding utxo doesn't exist anymore. Just remove this coin.
                            maker
                                .get_wallet()
                                .write()?
                                .remove_outgoing_swapcoin(og_rs)?;
                            log::info!("Removed outgoing swapcoin: {}", tx.compute_txid());
                        }
                    }
                }
            }
        }
    }

    // Save the wallet here before going into the expensive loop.
    maker.get_wallet().write()?.sync_no_fail();
    maker.get_wallet().read()?.save_to_disk()?;
    log::info!("Wallet file synced and saved to disk.");

    // Check for contract confirmations and broadcast timelocked transaction
    let mut timelock_boardcasted = Vec::new();
    let trigger_count = if cfg!(feature = "integration-test") {
        10 / HEART_BEAT_INTERVAL.as_secs() // triggers every 10 secs for tests
    } else {
        60 / HEART_BEAT_INTERVAL.as_secs() // triggers every 60 secs for prod
    };

    let mut i = 0;

    while !maker.shutdown.load(Relaxed) {
        if i >= trigger_count || i == 0 {
            for ((outgoing_reedemscript, contract), (timelock, timelocked_tx)) in outgoings.iter() {
                // We have already broadcasted this tx, so skip
                if timelock_boardcasted.contains(&timelocked_tx) {
                    continue;
                }
                // Check if the contract tx has reached required maturity
                // Failure here means the transaction hasn't been broadcasted yet. So do nothing and try again.
                let tx_from_chain = if let Ok(result) = maker
                    .wallet
                    .read()?
                    .rpc
                    .get_raw_transaction_info(&contract.compute_txid(), None)
                {
                    log::info!(
                        "[{}] Contract Txid : {} reached confirmation : {:?}, Required Confirmation : {}",
                        maker.config.network_port,
                        contract.compute_txid(),
                        result.confirmations,
                        timelock
                    );
                    result
                } else {
                    continue;
                };

                if let Some(confirmation) = tx_from_chain.confirmations {
                    // Now the transaction is confirmed in a block, check for required maturity
                    if confirmation > (*timelock as u32) {
                        log::info!(
                            "[{}] Timelock maturity of {} blocks reached for Contract Txid : {}",
                            maker.config.network_port,
                            timelock,
                            contract.compute_txid()
                        );
                        log::info!(
                            "[{}] Broadcasting timelocked tx: {}",
                            maker.config.network_port,
                            timelocked_tx.compute_txid()
                        );
                        maker
                            .wallet
                            .read()?
                            .rpc
                            .send_raw_transaction(timelocked_tx)
                            .map_err(WalletError::Rpc)?;
                        timelock_boardcasted.push(timelocked_tx);

                        let outgoing_removed = maker
                            .wallet
                            .write()?
                            .remove_outgoing_swapcoin(outgoing_reedemscript)?
                            .expect("outgoing swapcoin expected");

                        log::info!(
                            "[{}] Removed Outgoing Swapcoin from Wallet, Contract Txid: {}",
                            maker.config.network_port,
                            outgoing_removed.contract_tx.compute_txid()
                        );

                        log::info!("initializing Wallet Sync.");
                        {
                            let mut wallet_write = maker.wallet.write()?;
                            wallet_write.sync()?;
                            wallet_write.save_to_disk()?;
                        }
                        log::info!("Completed Wallet Sync.");
                    }
                }
            }

            log::info!(
                "{} outgoing contracts detected | {} timelock txs broadcasted.",
                outgoings.len(),
                timelock_boardcasted.len()
            );

            if timelock_boardcasted.len() == outgoings.len() {
                // For tests, terminate the maker at this stage.
                #[cfg(feature = "integration-test")]
                maker.shutdown.store(true, Relaxed);

                log::info!(
                    "All outgoing transactions claimed back via timelock. Recovery loop exiting."
                );
                break;
            }
            // Reset counter
            i = 0;
        }
        i += 1;
        std::thread::sleep(HEART_BEAT_INTERVAL);
    }
    Ok(())
}
