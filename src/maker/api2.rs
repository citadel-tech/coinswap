//! The Maker API for Taproot Swaps.
//!
//! Defines the core functionality of the Maker in a taproot-based swap protocol implementation.
//! This is a simplified version that adapts the original maker API to work with the new taproot protocol.

use super::rpc::server::MakerRpc;
use crate::{
    protocol::{
        contract2::{
            calculate_coinswap_fee, calculate_contract_sighash, create_taproot_script,
            create_timelock_script,
        },
        error::ProtocolError,
        messages2::{
            Offer, PrivateKeyHandover, SenderContractFromMaker, SendersContract, SwapDetails,
        },
        musig_interface::{
            aggregate_partial_signatures_compat, generate_new_nonce_pair_compat,
            generate_partial_signature_compat, get_aggregated_nonce_compat,
        },
    },
    utill::{check_tor_status, get_maker_dir, HEART_BEAT_INTERVAL, MIN_FEE_RATE},
    wallet::{Destination, IncomingSwapCoinV2, OutgoingSwapCoinV2, RPCConfig, Wallet, WalletError},
    watch_tower::{
        registry_storage::FileRegistry, rpc_backend::BitcoinRpc, service::WatchService,
        watcher::Watcher, zmq_backend::ZmqBackend,
    },
};
use bitcoin::{
    locktime::absolute::LockTime, sighash::SighashCache, Amount, OutPoint, Sequence, Transaction,
    TxIn, TxOut, Witness,
};
use bitcoind::bitcoincore_rpc::{RawTx, RpcApi};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc, Arc, Mutex, RwLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{config::MakerConfig, error::MakerError};

/// Represents different behaviors the maker can have during the swap.
/// Used for testing various failure scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "integration-test")]
pub enum MakerBehavior {
    /// Normal, honest behavior
    Normal,
    /// Close connection before sending PrivateKeyHandover message
    CloseAtPrivateKeyHandover,
    /// Close connection after receiving SendersContract (before creating outgoing contract)
    /// This forces both parties to use timelock recovery
    CloseAtContractSigsExchange,
    /// Close connection after sweeping incoming contract but before completing handover
    /// This allows maker to recover their coins but forces taker to recover via hashlock/timelock
    CloseAfterSweep,
}

/// Interval for health checks on a stable RPC connection with bitcoind.
pub const RPC_PING_INTERVAL: u32 = 9;

/// Maker triggers the recovery mechanism, if Taker is idle for more than 15 mins during a swap.
#[cfg(not(feature = "integration-test"))]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60 * 15);

/// The minimum difference in locktime (in blocks) between the incoming and outgoing swaps.
pub const MIN_CONTRACT_REACTION_TIME: u16 = 20;

/// Fee Parameters for Coinswap
#[cfg(not(feature = "integration-test"))]
pub const BASE_FEE: u64 = 100;
#[cfg(not(feature = "integration-test"))]
pub const AMOUNT_RELATIVE_FEE_PCT: f64 = 0.1;
#[cfg(not(feature = "integration-test"))]
pub const TIME_RELATIVE_FEE_PCT: f64 = 0.005;

/// Minimum Coinswap amount; makers will not accept amounts below this.
pub const MIN_SWAP_AMOUNT: u64 = 10_000;

/// Test Parameters for Coinswap
#[cfg(feature = "integration-test")]
pub const BASE_FEE: u64 = 1000;
#[cfg(feature = "integration-test")]
pub const AMOUNT_RELATIVE_FEE_PCT: f64 = 2.50;
#[cfg(feature = "integration-test")]
pub const TIME_RELATIVE_FEE_PCT: f64 = 0.10;
#[cfg(feature = "integration-test")]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);

/// Maintains the state of a connection, including the list of swapcoins.
#[derive(Debug)]
pub struct ConnectionState {
    pub(crate) swap_amount: Amount,
    pub(crate) timelock: u16,
    pub(crate) incoming_contract: IncomingSwapCoinV2,
    pub(crate) outgoing_contract: OutgoingSwapCoinV2,
}

impl Default for ConnectionState {
    fn default() -> Self {
        use bitcoin::{secp256k1::SecretKey, ScriptBuf, Transaction};

        let dummy_key = SecretKey::from_slice(&[1u8; 32]).expect("valid key");

        Self {
            swap_amount: Amount::ZERO,
            timelock: 0,
            incoming_contract: IncomingSwapCoinV2 {
                my_privkey: None,
                my_pubkey: None,
                other_pubkey: None,
                hashlock_script: ScriptBuf::new(),
                timelock_script: ScriptBuf::new(),
                contract_tx: Transaction {
                    version: bitcoin::transaction::Version::TWO,
                    lock_time: bitcoin::locktime::absolute::LockTime::ZERO,
                    input: vec![],
                    output: vec![],
                },
                contract_txid: None,
                tap_tweak: None,
                internal_key: None,
                spending_tx: None,
                hash_preimage: None,
                other_privkey: None,
                hashlock_privkey: dummy_key,
                funding_amount: Amount::ZERO,
                swap_id: None,
            },
            outgoing_contract: OutgoingSwapCoinV2 {
                my_privkey: None,
                my_pubkey: None,
                other_pubkey: None,
                tap_tweak: None,
                internal_key: None,
                hashlock_script: ScriptBuf::new(),
                timelock_script: ScriptBuf::new(),
                contract_tx: Transaction {
                    version: bitcoin::transaction::Version::TWO,
                    lock_time: bitcoin::locktime::absolute::LockTime::ZERO,
                    input: vec![],
                    output: vec![],
                },
                hash_preimage: None,
                other_privkey: None,
                timelock_privkey: dummy_key,
                funding_amount: Amount::ZERO,
                swap_id: None,
            },
        }
    }
}

impl Clone for ConnectionState {
    fn clone(&self) -> Self {
        ConnectionState {
            swap_amount: self.swap_amount,
            timelock: self.timelock,
            incoming_contract: self.incoming_contract.clone(),
            outgoing_contract: self.outgoing_contract.clone(),
        }
    }
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
            let thread_name = thread.thread().name().unwrap_or("unknown").to_string();

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

/// Represents the maker in the taproot swap protocol.
pub struct Maker {
    /// Maker configurations
    pub(crate) config: MakerConfig,
    /// Maker's underlying wallet
    pub wallet: RwLock<Wallet>,
    /// A flag to trigger shutdown event
    pub shutdown: AtomicBool,
    /// Map of IP address to Connection State + last Connected instant
    pub(crate) ongoing_swap_state: Mutex<HashMap<String, (ConnectionState, Instant)>>,
    /// Highest Value Fidelity Proof
    pub(crate) highest_fidelity_proof: RwLock<Option<crate::protocol::messages2::FidelityProof>>,
    /// Is setup complete
    pub is_setup_complete: AtomicBool,
    /// Path for the data directory.
    data_dir: PathBuf,
    /// Thread pool for managing all spawned threads
    pub(crate) thread_pool: Arc<ThreadPool>,
    /// Watcher Service
    pub watch_service: WatchService,
    /// Behavior mode (for testing)
    #[cfg(feature = "integration-test")]
    pub(crate) behavior: MakerBehavior,
}

impl Maker {
    #[allow(clippy::too_many_arguments)]
    /// Initializes a Maker structure for taproot swaps.
    pub fn init(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        network_port: Option<u16>,
        rpc_port: Option<u16>,
        control_port: Option<u16>,
        tor_auth_password: Option<String>,
        socks_port: Option<u16>,
        zmq_addr: String,
        password: Option<String>,
        #[cfg(feature = "integration-test")] behavior: Option<MakerBehavior>,
    ) -> Result<Self, MakerError> {
        let data_dir = data_dir.unwrap_or(get_maker_dir());
        let wallets_dir = data_dir.join("wallets");

        let wallet_file_name = wallet_file_name.unwrap_or_else(|| "maker-wallet".to_string());
        let wallet_path = wallets_dir.join(&wallet_file_name);

        let mut rpc_config = rpc_config.unwrap_or_default();
        rpc_config.wallet_name = wallet_file_name;

        let mut config = MakerConfig::new(Some(&data_dir.join("config.toml")))?;

        if let Some(port) = network_port {
            config.network_port = port;
        }

        let backend = ZmqBackend::new(&zmq_addr);
        let rpc_backend = BitcoinRpc::new(rpc_config.clone())?;
        let blockchain_info = rpc_backend.get_blockchain_info()?;
        let file_registry = data_dir
            .join(format!(".maker_{}_watcher", config.network_port))
            .join(blockchain_info.chain.to_string());
        let registry = FileRegistry::load(file_registry);
        let (tx_requests, rx_requests) = mpsc::channel();
        let (tx_events, rx_responses) = mpsc::channel();

        let mut watcher = Watcher::new(backend, rpc_backend, registry, rx_requests, tx_events);
        _ = thread::Builder::new()
            .name("Watcher thread".to_string())
            .spawn(move || watcher.run());

        let watch_service = WatchService::new(tx_requests, rx_responses);

        let mut wallet = if wallet_path.exists() {
            let wallet = Wallet::load(&wallet_path, &rpc_config, password)?;
            log::info!("Wallet file at {wallet_path:?} successfully loaded.");
            wallet
        } else {
            let wallet = Wallet::init(&wallet_path, &rpc_config, None)?;
            log::info!("New Wallet created at : {wallet_path:?}");
            wallet
        };

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

        // Check Tor status if not in integration test mode
        if !cfg!(feature = "integration-test") {
            check_tor_status(config.control_port, config.tor_auth_password.as_str())?;
        }

        config.write_to_file(&data_dir.join("config.toml"))?;

        log::info!("Initializing wallet sync");
        wallet.sync()?;
        log::info!("Completed wallet sync");

        let network_port = config.network_port;

        Ok(Self {
            config,
            wallet: RwLock::new(wallet),
            shutdown: AtomicBool::new(false),
            ongoing_swap_state: Mutex::new(HashMap::new()),
            highest_fidelity_proof: RwLock::new(None),
            is_setup_complete: AtomicBool::new(false),
            data_dir,
            thread_pool: Arc::new(ThreadPool::new(network_port)),
            watch_service,
            #[cfg(feature = "integration-test")]
            behavior: behavior.unwrap_or(MakerBehavior::Normal),
        })
    }

    /// Returns data directory of Maker
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Returns a reference to the Maker's wallet.
    pub fn wallet(&self) -> &RwLock<Wallet> {
        &self.wallet
    }

    /// Creates an offer for the taker
    pub fn create_offer(
        &self,
        connection_state: &mut ConnectionState,
    ) -> Result<Offer, MakerError> {
        let wallet = self.wallet.read()?;
        let (incoming_contract_my_privkey, incoming_contract_my_pubkey) =
            wallet.get_tweakable_keypair()?;
        connection_state.incoming_contract.my_privkey = Some(incoming_contract_my_privkey);
        connection_state.incoming_contract.my_pubkey = Some(incoming_contract_my_pubkey);
        log::info!(
            "[{}] create_offer: Set my_privkey for incoming contract, is_some={}",
            self.config.network_port,
            connection_state.incoming_contract.my_privkey.is_some()
        );
        // Get wallet balances to determine max size
        let balances = wallet.get_balances()?;
        let max_size = balances.spendable;

        // Read the cached fidelity proof
        let fidelity_proof = self.highest_fidelity_proof.read()?;
        let fidelity_proof = fidelity_proof
            .as_ref()
            .ok_or_else(|| MakerError::General("No fidelity proof available"))?
            .clone();

        // Calculate minimum swap amount
        let min_size = MIN_SWAP_AMOUNT;
        let max_size = max_size.to_sat();

        Ok(Offer {
            tweakable_point: incoming_contract_my_pubkey,
            base_fee: BASE_FEE,
            amount_relative_fee: AMOUNT_RELATIVE_FEE_PCT,
            time_relative_fee: TIME_RELATIVE_FEE_PCT,
            minimum_locktime: MIN_CONTRACT_REACTION_TIME,
            fidelity: fidelity_proof,
            min_size,
            max_size,
        })
    }

    /// Validates incoming swap parameters
    pub(crate) fn validate_swap_parameters(
        &self,
        swap_details: &SwapDetails,
    ) -> Result<(), MakerError> {
        // Check minimum amount
        if swap_details.amount.to_sat() < MIN_SWAP_AMOUNT {
            return Err(MakerError::General("Swap amount below minimum"));
        }

        // Check if we have enough liquidity
        let wallet = self.wallet.read()?;
        let balances = wallet.get_balances()?;
        if balances.spendable < swap_details.amount {
            return Err(MakerError::General("Insufficient liquidity"));
        }

        // Check timelock requirements
        if swap_details.timelock < MIN_CONTRACT_REACTION_TIME {
            return Err(MakerError::General("Timelock too short"));
        }

        // Check transaction count is reasonable
        if swap_details.no_of_tx == 0 || swap_details.no_of_tx > 10 {
            return Err(MakerError::General("Invalid transaction count"));
        }

        Ok(())
    }

    /// Calculates the fee for a given swap
    pub(crate) fn calculate_swap_fee(&self, amount: Amount, timelock: u16) -> Amount {
        let fee_sats = calculate_coinswap_fee(
            amount.to_sat(),
            timelock,
            BASE_FEE,
            AMOUNT_RELATIVE_FEE_PCT,
            TIME_RELATIVE_FEE_PCT,
        );
        Amount::from_sat(fee_sats)
    }

    /// Verifies and processes a SendersContract message
    pub(crate) fn verify_and_process_senders_contract(
        &self,
        message: &SendersContract,
        connection_state: &mut ConnectionState,
    ) -> Result<SenderContractFromMaker, MakerError> {
        // Store relevant data from the message

        connection_state.incoming_contract.hashlock_script = message.hashlock_scripts[0].clone();
        connection_state.incoming_contract.timelock_script = message.timelock_scripts[0].clone();
        connection_state.incoming_contract.contract_txid = Some(message.contract_txs[0]);

        // Store the internal key and tap tweak from the message for cooperative spending
        // If not provided, we'll calculate our own when creating the outgoing contract
        connection_state.incoming_contract.internal_key = message.internal_key;
        connection_state.incoming_contract.tap_tweak =
            message.tap_tweak.as_ref().map(|t| t.clone().into());

        // Store taker's pubkey
        connection_state.incoming_contract.other_pubkey = Some(message.pubkeys_a[0]);

        // Fetch and store the incoming contract transaction
        let incoming_contract_tx = {
            let wallet = self.wallet.read()?;
            wallet
                .rpc
                .get_raw_transaction(&message.contract_txs[0], None)
                .map_err(|_| MakerError::General("Failed to get incoming contract tx"))?
        };
        connection_state.incoming_contract.contract_tx = incoming_contract_tx.clone();
        connection_state.incoming_contract.funding_amount = incoming_contract_tx.output[0].value;

        // Store next party's tweakable pubkey for outgoing contract
        connection_state.outgoing_contract.other_pubkey = Some(message.next_party_tweakable_point);

        // Verify we have sufficient funds and get necessary data
        let (outgoing_privkey, selected_utxos) = {
            let mut wallet = self.wallet.write()?;

            // Sync wallet to get latest UTXO state
            wallet.sync()?;

            let balance = wallet.get_balances()?;
            if balance.spendable < connection_state.swap_amount {
                return Err(MakerError::General("Insufficient funds for swap"));
            }

            // Get our tweakable keypair
            let (outgoing_privkey, outgoing_pubkey) = wallet.get_tweakable_keypair()?;
            connection_state.outgoing_contract.my_privkey = Some(outgoing_privkey);
            connection_state.outgoing_contract.my_pubkey = Some(outgoing_pubkey);

            // Prepare for UTXO selection: unlock all, then lock unspendable UTXOs
            wallet.rpc.unlock_unspent_all().map_err(WalletError::Rpc)?;
            wallet.lock_unspendable_utxos()?;

            // Use coin_select to get UTXOs that sum to the required amount
            let selected_utxos = wallet
                .coin_select(connection_state.swap_amount, MIN_FEE_RATE, None)
                .map_err(|e| {
                    MakerError::General(format!("Coin selection failed: {:?}", e).leak())
                })?;

            // Lock the selected UTXOs to prevent double-spending in concurrent swaps
            let funding_outpoints: Vec<OutPoint> = selected_utxos
                .iter()
                .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
                .collect();
            wallet
                .rpc
                .lock_unspent(&funding_outpoints)
                .map_err(WalletError::Rpc)?;
            log::info!(
                "[{}] Locked {} funding UTXOs for swap",
                self.config.network_port,
                funding_outpoints.len()
            );

            (outgoing_privkey, selected_utxos)
        };

        // We expect only one contract for now
        if message.contract_txs.len() != 1 {
            return Err(MakerError::General(
                "Expected exactly one contract transaction",
            ));
        }

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (outgoing_x_only, _) =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &outgoing_privkey)
                .x_only_public_key();

        let other_pubkey = connection_state.outgoing_contract.other_pubkey()?;
        let other_x_only = bitcoin::key::XOnlyPublicKey::from(other_pubkey.inner);

        // Get the hash from incoming contract to use same preimage
        let incoming_hashlock_script = connection_state.incoming_contract.hashlock_script();
        let hash =
            crate::protocol::contract2::extract_hash_from_hashlock(incoming_hashlock_script)?;
        let hashlock_script =
            crate::protocol::contract2::create_hashlock_script(&hash, &other_x_only);

        let timelock = LockTime::from_height(connection_state.timelock as u32)
            .map_err(WalletError::Locktime)?;
        let timelock_script = create_timelock_script(timelock, &outgoing_x_only);

        connection_state.outgoing_contract.hashlock_script = hashlock_script.clone();
        connection_state.outgoing_contract.timelock_script = timelock_script.clone();

        // Create internal key for cooperative spending between taker and maker
        // Order pubkeys lexicographically to match signing order
        let mut pubkeys_for_internal_key = [
            connection_state.outgoing_contract.pubkey()?,
            connection_state.outgoing_contract.other_pubkey()?,
        ];
        pubkeys_for_internal_key.sort_by(|a, b| a.inner.serialize().cmp(&b.inner.serialize()));
        let internal_key = crate::protocol::musig_interface::get_aggregated_pubkey_compat(
            pubkeys_for_internal_key[0].inner,
            pubkeys_for_internal_key[1].inner,
        )?;

        // Create taproot script (P2TR output)
        let (taproot_script, taproot_spendinfo) = create_taproot_script(
            hashlock_script.clone(),
            timelock_script.clone(),
            internal_key,
        )?;

        // Store the values for later use in the return statement
        connection_state.outgoing_contract.internal_key = Some(internal_key);
        connection_state.outgoing_contract.tap_tweak =
            Some(taproot_spendinfo.tap_tweak().to_scalar());

        // Get the actual amount we received from the incoming contract
        let incoming_contract_txid = connection_state.incoming_contract.contract_txid()?;

        let received_amount = {
            let wallet = self.wallet.read()?;
            let incoming_contract_tx = wallet
                .rpc
                .get_raw_transaction(&incoming_contract_txid, None)
                .map_err(|_| MakerError::General("Failed to get incoming contract transaction"))?;
            incoming_contract_tx.output[0].value
        };

        // Calculate our fee for this swap based on received amount
        let our_fee = self.calculate_swap_fee(received_amount, connection_state.timelock);
        log::info!(
            "Calculated coinswap fee: {} sats for received amount: {} sats",
            our_fee.to_sat(),
            received_amount.to_sat()
        );

        // Calculate the amount for our outgoing contract (received amount minus our fee)
        // The taker will pay mining fees when sweeping
        let outgoing_contract_amount = received_amount - our_fee;
        log::info!(
            "Creating outgoing contract with amount: {} sats (after deducting {} sats coinswap fee)",
            outgoing_contract_amount.to_sat(),
            our_fee.to_sat()
        );

        // Sign and broadcast our contract transaction using the wallet
        log::info!("Signing and broadcasting maker's contract transaction");
        let outgoing_contract_txid = {
            let mut wallet = self.wallet.write()?;

            // Use Destination::Multi to send exact amount to contract and keep the fee as change
            let contract_address =
                bitcoin::Address::from_script(&taproot_script, wallet.store.network)
                    .map_err(|_| MakerError::General("Failed to create address"))?;

            // Create a proper signed transaction using the wallet with Multi destination
            // This sends exactly outgoing_contract_amount to the contract and returns change to maker
            let signed_tx = wallet.spend_from_wallet(
                MIN_FEE_RATE,
                Destination::Multi {
                    outputs: vec![(contract_address, outgoing_contract_amount)],
                    op_return_data: None,
                },
                &selected_utxos,
            )?;

            // Broadcast the signed transaction
            wallet.send_tx(&signed_tx)?;

            // Store the contract transaction and funding amount
            connection_state.outgoing_contract.contract_tx = signed_tx.clone();
            connection_state.outgoing_contract.funding_amount = outgoing_contract_amount;

            signed_tx.compute_txid()
        };
        log::info!("Outgoing contract txid: {:?}", outgoing_contract_txid);

        // For cooperative spending, we prepare to spend from the incoming contract transaction
        let incoming_contract_txid = connection_state.incoming_contract.contract_txid()?;

        // Get the prevout for sighash calculation
        // Use the internal key and tap tweak from the message (set during contract creation)
        let internal_key = connection_state.incoming_contract.internal_key()?;

        use bitcoin::{OutPoint, Sequence, TxIn, TxOut, Witness};

        let incoming_contract_tx = {
            let wallet = self
                .wallet
                .read()
                .map_err(|_e| MakerError::General("Failed to read wallet"))?;
            wallet
                .rpc
                .get_raw_transaction(&incoming_contract_txid, None)
                .map_err(|_e| MakerError::General("Failed to get incoming contract transaction"))?
        };

        let incoming_contract_tx_output = incoming_contract_tx.output[0].clone();
        let incoming_contract_tx_output_value = incoming_contract_tx_output.value;

        let spending_tx = bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::locktime::absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: incoming_contract_txid,
                    vout: 0, // Spend the first output of the taker's contract transaction
                },
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: Sequence::ZERO,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: incoming_contract_tx_output_value - bitcoin::Amount::from_sat(1000), // Use actual swap amount
                script_pubkey: self
                    .wallet
                    .read()?
                    .get_next_internal_addresses(1)
                    .map_err(MakerError::Wallet)?[0]
                    .script_pubkey(),
            }],
        };
        let hashlock_script = connection_state.incoming_contract.hashlock_script();
        let timelock_script = connection_state.incoming_contract.timelock_script();
        // Use helper to calculate sighash
        let _message = calculate_contract_sighash(
            &spending_tx,
            incoming_contract_tx_output_value,
            hashlock_script,
            timelock_script,
            internal_key,
        )
        .map_err(|_| MakerError::General("Failed to calculate sighash"))?;

        // Register both contracts with watcher for monitoring
        let incoming_outpoint = OutPoint {
            txid: incoming_contract_txid,
            vout: 0,
        };
        self.watch_service.register_watch_request(incoming_outpoint);
        log::info!(
            "Registered watcher for incoming contract: {}",
            incoming_outpoint
        );

        // Register outgoing contract
        let outgoing_outpoint = OutPoint {
            txid: outgoing_contract_txid,
            vout: 0,
        };
        self.watch_service.register_watch_request(outgoing_outpoint);
        log::info!(
            "Registered watcher for outgoing contract: {}",
            outgoing_outpoint
        );

        // Store taproot contract data directly in connection state instead of using traditional IncomingSwapCoin
        // The taker's contract transaction hash is already stored in connection_state.contract_tx_hash
        // We have all the necessary taproot data: internal_key, tap_tweak, hashlock_script, timelock_script

        // Return our contract transaction details
        Ok(SenderContractFromMaker {
            contract_txs: vec![outgoing_contract_txid], // Our own contract transaction
            pubkeys_a: vec![connection_state.outgoing_contract.pubkey()?], // Next party's pubkey
            hashlock_scripts: vec![connection_state.outgoing_contract.hashlock_script().clone()],
            timelock_scripts: vec![connection_state.outgoing_contract.timelock_script().clone()],
            // Include the internal key and tap tweak that were used to create OUR outgoing contract
            internal_key: Some(connection_state.outgoing_contract.internal_key()?),
            tap_tweak: Some((connection_state.outgoing_contract.tap_tweak()?).into()),
        })
    }

    pub(crate) fn process_private_key_handover(
        &self,
        privkey_handover_message: &PrivateKeyHandover,
        connection_state: &mut ConnectionState,
    ) -> Result<PrivateKeyHandover, MakerError> {
        // Check for test behavior: close connection before sweeping
        #[cfg(feature = "integration-test")]
        if self.behavior == MakerBehavior::CloseAtPrivateKeyHandover {
            log::warn!(
                "[{}] Maker behavior: CloseAtPrivateKeyHandover - Closing connection before sweep",
                self.config.network_port
            );
            return Err(MakerError::General(
                "Maker closing connection before PrivateKeyHandover (test behavior)",
            ));
        }

        // Create the spending transaction if it doesn't exist
        if connection_state.incoming_contract.spending_tx().is_none() {
            log::info!(
                "[{}] Creating spending transaction for incoming contract",
                self.config.network_port
            );
            let tx = self.create_unsigned_spending_tx(connection_state)?;
            connection_state.incoming_contract.spending_tx = Some(tx);
        }
        let spending_tx = connection_state
            .incoming_contract
            .spending_tx()
            .as_ref()
            .ok_or_else(|| MakerError::General("No spending tx found"))?
            .clone();

        // Generate fresh nonce pairs for both parties (maker and sender)
        // In the private key handover protocol, we generate nonces when we receive the private key

        // Generate maker's nonce pair for incoming contract
        let incoming_my_privkey = connection_state.incoming_contract.privkey()?;
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let incoming_my_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &incoming_my_privkey);

        let incoming_other_keypair = bitcoin::secp256k1::Keypair::from_secret_key(
            &secp,
            &privkey_handover_message.secret_key,
        );

        let (incoming_contract_my_sec_nonce, incoming_contract_my_pub_nonce) =
            generate_new_nonce_pair_compat(incoming_my_keypair.public_key())?;

        // Generate sender's nonce pair using their keypair
        let (incoming_contract_other_sec_nonce, incoming_contract_other_pub_nonce) =
            generate_new_nonce_pair_compat(incoming_other_keypair.public_key())?;

        let sender_nonce = incoming_contract_other_pub_nonce;

        // Get incoming contract details
        let incoming_contract_txid = connection_state.incoming_contract.contract_txid()?;
        let incoming_tap_tweak = connection_state.incoming_contract.tap_tweak()?;
        let incoming_internal_key = connection_state.incoming_contract.internal_key()?;

        // Get contract transaction and reconstruct script
        let incoming_contract_tx = self
            .wallet
            .read()?
            .rpc
            .get_raw_transaction(&incoming_contract_txid, None)
            .map_err(|_e| MakerError::General("Failed to get incoming contract transaction"))?;
        let incoming_contract_amount = incoming_contract_tx.output[0].value;

        let incoming_hashlock_script = connection_state.incoming_contract.hashlock_script().clone();
        let incoming_timelock_script = connection_state.incoming_contract.timelock_script().clone();

        // Use helper to calculate sighash
        let incoming_message = calculate_contract_sighash(
            &spending_tx,
            incoming_contract_amount,
            &incoming_hashlock_script,
            &incoming_timelock_script,
            incoming_internal_key,
        )
        .map_err(|_| MakerError::General("Failed to calculate sighash"))?;

        // Get other pubkey for ordering
        let incoming_other_pubkey = connection_state.incoming_contract.other_pubkey()?;
        let mut incoming_ordered_pubkeys = [
            incoming_my_keypair.public_key(),
            incoming_other_pubkey.inner,
        ];
        incoming_ordered_pubkeys.sort_by_key(|a| a.serialize());

        // Create aggregated nonce with sender nonce and our public nonce
        let incoming_nonce_refs = if incoming_ordered_pubkeys[0] == incoming_my_keypair.public_key()
        {
            vec![&incoming_contract_my_pub_nonce, &sender_nonce]
        } else {
            vec![&sender_nonce, &incoming_contract_my_pub_nonce]
        };
        let incoming_aggregated_nonce = get_aggregated_nonce_compat(&incoming_nonce_refs);

        // Generate our partial signature for the incoming contract
        let our_incoming_partial_sig = generate_partial_signature_compat(
            incoming_message,
            &incoming_aggregated_nonce,
            incoming_contract_my_sec_nonce,
            incoming_my_keypair,
            incoming_tap_tweak,
            incoming_ordered_pubkeys[0],
            incoming_ordered_pubkeys[1],
        )?;

        let other_partial_sig = generate_partial_signature_compat(
            incoming_message,
            &incoming_aggregated_nonce,
            incoming_contract_other_sec_nonce,
            incoming_other_keypair,
            incoming_tap_tweak,
            incoming_ordered_pubkeys[0],
            incoming_ordered_pubkeys[1],
        )?;

        // Aggregate both partial signatures
        let partial_sigs = if incoming_ordered_pubkeys[0] == incoming_my_keypair.public_key() {
            vec![&our_incoming_partial_sig, &other_partial_sig]
        } else {
            vec![&other_partial_sig, &our_incoming_partial_sig]
        };
        let aggregated_sig = aggregate_partial_signatures_compat(
            incoming_message,
            incoming_aggregated_nonce,
            incoming_tap_tweak,
            partial_sigs,
            incoming_ordered_pubkeys[0],
            incoming_ordered_pubkeys[1],
        )?;

        // Create final signature and add to transaction witness
        let final_signature =
            bitcoin::taproot::Signature::from_slice(aggregated_sig.assume_valid().as_byte_array())
                .map_err(ProtocolError::TaprootSigSlice)?;

        let mut final_tx = spending_tx.clone();
        let mut final_sighasher = SighashCache::new(&mut final_tx);
        *final_sighasher
            .witness_mut(0)
            .ok_or(MakerError::General("Failed to access witness for signing"))? =
            Witness::p2tr_key_spend(&final_signature);
        let completed_tx = final_sighasher.into_transaction();

        // Broadcast the completed spending transaction
        let txid = self
            .wallet
            .read()?
            .rpc
            .send_raw_transaction(completed_tx.raw_hex())
            .map_err(|e| MakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;

        log::info!(
            "[{}] Maker sweep transaction broadcasted with txid: {:?}",
            self.config.network_port,
            txid
        );

        // Check for test behavior: close connection after sweeping
        #[cfg(feature = "integration-test")]
        if self.behavior == MakerBehavior::CloseAfterSweep {
            log::warn!(
                "[{}] Maker behavior: CloseAfterSweep - Closing connection after sweep",
                self.config.network_port
            );
            return Err(MakerError::General(
                "Maker closing connection after sweep (test behavior)",
            ));
        }

        // Mark the incoming swapcoin as finished by storing the received private key
        connection_state.incoming_contract.other_privkey =
            Some(privkey_handover_message.secret_key);

        // Update the wallet with the completed incoming swapcoin
        {
            let mut wallet = self.wallet.write()?;
            wallet.add_incoming_swapcoin_v2(&connection_state.incoming_contract);
            wallet.save_to_disk()?;
            log::info!(
                "[{}] Marked incoming swapcoin as finished (other_privkey stored)",
                self.config.network_port
            );
        }

        let privkey_handover_message = PrivateKeyHandover {
            secret_key: connection_state.outgoing_contract.privkey()?,
        };

        Ok(privkey_handover_message)
    }

    /// Create an unsigned transaction to spend from the incoming contract
    pub(crate) fn create_unsigned_spending_tx(
        &self,
        connection_state: &ConnectionState,
    ) -> Result<Transaction, MakerError> {
        // Get the incoming contract transaction hash
        let incoming_contract_txid = connection_state.incoming_contract.contract_txid()?;

        log::info!(
            "[{}] Creating unsigned spending tx for incoming contract: {}",
            self.config.network_port,
            incoming_contract_txid
        );

        // Fetch the contract transaction to get the output value
        let incoming_contract_tx = {
            let wallet = self.wallet.read()?;
            wallet
                .rpc
                .get_raw_transaction(&incoming_contract_txid, None)
                .map_err(|e| MakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?
        };

        // Get the contract output value (assuming it's the first output)
        let contract_value = incoming_contract_tx.output[0].value;

        // Get a fresh internal address for the destination
        let destination_address = {
            let wallet = self.wallet.read()?;
            wallet
                .get_next_internal_addresses(1)
                .map_err(MakerError::Wallet)?[0]
                .clone()
        };

        // Create the unsigned spending transaction
        let spending_tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::locktime::absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: incoming_contract_txid,
                    vout: 0, // Spend the first output
                },
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: Sequence::ZERO,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: contract_value - Amount::from_sat(1000), // Deduct standard fee
                script_pubkey: destination_address.script_pubkey(),
            }],
        };

        log::info!(
            "[{}] Created unsigned spending tx with output value: {}",
            self.config.network_port,
            contract_value - Amount::from_sat(1000)
        );

        Ok(spending_tx)
    }

    /// Ensures all unconfirmed fidelity bonds in the maker's wallet are tracked until confirmation.  
    /// Once confirmed, updates their confirmation details in the wallet.
    pub(super) fn track_and_update_unconfirmed_fidelity_bonds(&self) -> Result<(), MakerError> {
        let bond_conf_heights = {
            let wallet_read = self.wallet().read()?;

            wallet_read
                .store
                .fidelity_bond
                .iter()
                .filter_map(|(i, bond)| {
                    if bond.conf_height.is_none() && bond.cert_expiry.is_none() {
                        let conf_height = wallet_read
                            .wait_for_tx_confirmation(bond.outpoint.txid)
                            .ok()?;
                        Some((*i, conf_height))
                    } else {
                        None
                    }
                })
                .collect::<HashMap<u32, u32>>()
        };

        bond_conf_heights.into_iter().try_for_each(|(i, ht)| {
            self.wallet()
                .write()?
                .update_fidelity_bond_conf_details(i, ht)?;
            Ok::<(), MakerError>(())
        })?;

        Ok(())
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

/// Checks for spent contract outputs and triggers recovery.
/// This detects when contract outputs are spent via hashlock or timelock paths,
/// indicating protocol violations or adjacent maker failures that require recovery.
pub(crate) fn check_for_broadcasted_contracts(maker: Arc<Maker>) -> Result<(), MakerError> {
    let mut failed_swap_ip = Vec::new();
    loop {
        if maker.shutdown.load(Relaxed) {
            break;
        }

        {
            let mut lock_on_state = maker.ongoing_swap_state.lock()?;
            for (ip, (connection_state, _)) in lock_on_state.iter_mut() {
                // Skip if no contracts have been exchanged yet
                let Some(incoming_txid) = connection_state.incoming_contract.contract_txid else {
                    continue;
                };

                let outgoing_txid = connection_state
                    .outgoing_contract
                    .contract_tx
                    .compute_txid();

                // Check if the outgoing contract output has been SPENT (not just broadcasted)
                // If spent, the taker/next-maker used hashlock to claim it, revealing the preimage
                let outgoing_outpoint = OutPoint {
                    txid: outgoing_txid,
                    vout: 0,
                };

                let outgoing_spent = {
                    let read_lock = maker.wallet.read()?;
                    // get_tx_out returns None if the UTXO is spent
                    read_lock
                        .rpc
                        .get_tx_out(&outgoing_outpoint.txid, outgoing_outpoint.vout, Some(true))
                        .map_err(WalletError::Rpc)?
                        .is_none()
                };

                if outgoing_spent {
                    log::warn!(
                        "[{}] Outgoing contract {} has been SPENT! Triggering recovery for swap with {}",
                        maker.config.network_port,
                        outgoing_txid,
                        ip
                    );
                    failed_swap_ip.push(ip.clone());

                    let incoming = connection_state.incoming_contract.clone();
                    let outgoing = connection_state.outgoing_contract.clone();
                    let maker_clone = maker.clone();

                    log::info!(
                        "[{}] Spawning recovery thread after detecting outgoing contract spend",
                        maker.config.network_port
                    );

                    let handle = std::thread::Builder::new()
                        .name("Taproot Contract Recovery Thread".to_string())
                        .spawn(move || {
                            if let Err(e) = recover_from_swap(maker_clone, incoming, outgoing) {
                                log::error!("Failed to recover from taproot swap: {:?}", e);
                            }
                        })?;

                    maker.thread_pool.add_thread(handle);

                    // Clear the state since recovery thread now owns it
                    *connection_state = ConnectionState::default();
                    continue;
                }

                // Also check if incoming contract was spent by someone else (not us)
                // This could indicate the taker recovered via timelock
                let incoming_outpoint = OutPoint {
                    txid: incoming_txid,
                    vout: 0,
                };

                let incoming_spent = {
                    let read_lock = maker.wallet.read()?;
                    read_lock
                        .rpc
                        .get_tx_out(&incoming_outpoint.txid, incoming_outpoint.vout, Some(true))
                        .map_err(WalletError::Rpc)?
                        .is_none()
                };

                if incoming_spent {
                    log::warn!(
                        "[{}] Incoming contract {} has been SPENT! Triggering recovery for swap with {}",
                        maker.config.network_port,
                        incoming_txid,
                        ip
                    );
                    failed_swap_ip.push(ip.clone());

                    let incoming = connection_state.incoming_contract.clone();
                    let outgoing = connection_state.outgoing_contract.clone();
                    let maker_clone = maker.clone();

                    log::info!(
                        "[{}] Spawning recovery thread after detecting incoming contract spend",
                        maker.config.network_port
                    );

                    let handle = std::thread::Builder::new()
                        .name("Taproot Contract Recovery Thread".to_string())
                        .spawn(move || {
                            if let Err(e) = recover_from_swap(maker_clone, incoming, outgoing) {
                                log::error!("Failed to recover from taproot swap: {:?}", e);
                            }
                        })?;

                    maker.thread_pool.add_thread(handle);

                    *connection_state = ConnectionState::default();
                }
            }

            // Remove failed swap entries
            for ip in failed_swap_ip.iter() {
                lock_on_state.remove(ip);
            }
        }

        failed_swap_ip.clear();
        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Checks for idle connection states and removes them after timeout.
pub(crate) fn check_for_idle_states(maker: Arc<Maker>) -> Result<(), MakerError> {
    let mut bad_ip = Vec::new();
    loop {
        if maker.shutdown.load(Relaxed) {
            break;
        }

        {
            let mut lock_on_state = maker.ongoing_swap_state.lock()?;
            for (ip, (state, instant)) in lock_on_state.iter_mut() {
                if instant.elapsed() > IDLE_CONNECTION_TIMEOUT {
                    log::error!(
                        "[{}] Potential dropped connection from taker {}. No response since {} secs. Recovering from swap.",
                        maker.config.network_port,
                        ip,
                        instant.elapsed().as_secs()
                    );

                    // Check if we have contracts to recover (swap was in progress)
                    // Verify that actual contract transactions were created and exchanged
                    let has_contracts = state.incoming_contract.contract_txid.is_some();

                    if has_contracts {
                        let incoming = state.incoming_contract.clone();
                        let outgoing = state.outgoing_contract.clone();
                        let maker_clone = maker.clone();

                        log::info!(
                            "[{}] Spawning recovery thread after taker {} dropped",
                            maker.config.network_port,
                            ip
                        );

                        // Spawn recovery thread
                        let handle = std::thread::Builder::new()
                            .name("Taproot Swap Recovery Thread".to_string())
                            .spawn(move || {
                                if let Err(e) = recover_from_swap(maker_clone, incoming, outgoing) {
                                    log::error!("Failed to recover from taproot swap: {:?}", e);
                                }
                            })?;

                        maker.thread_pool.add_thread(handle);
                    }

                    bad_ip.push(ip.clone());
                    *state = ConnectionState::default();
                    break;
                }
            }

            for ip in bad_ip.iter() {
                lock_on_state.remove(ip);
            }
        }

        bad_ip.clear();
        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Checks for unfinished taproot swapcoins in wallet on reboot and starts recovery if needed.
/// Matches incoming and outgoing swapcoins by swap_id to ensure correct pairing.
pub(crate) fn restore_broadcasted_contracts_on_reboot_v2(
    maker: &Arc<Maker>,
) -> Result<(), MakerError> {
    let (incoming_swapcoins, outgoing_swapcoins) =
        maker.wallet.read()?.find_unfinished_swapcoins_v2();

    log::info!(
        "[{}] Found {} unfinished incoming and {} unfinished outgoing taproot swapcoins on reboot",
        maker.config.network_port,
        incoming_swapcoins.len(),
        outgoing_swapcoins.len()
    );

    // Match incoming and outgoing swapcoins by swap_id
    for incoming in incoming_swapcoins.iter() {
        let Some(ref incoming_swap_id) = incoming.swap_id else {
            log::warn!(
                "[{}] Incoming swapcoin {} has no swap_id, skipping",
                maker.config.network_port,
                incoming.contract_tx.compute_txid()
            );
            continue;
        };

        // Find matching outgoing swapcoin
        let matching_outgoing = outgoing_swapcoins
            .iter()
            .find(|o| o.swap_id.as_ref() == Some(incoming_swap_id));

        let Some(outgoing) = matching_outgoing else {
            log::warn!(
                "[{}] No matching outgoing swapcoin found for swap_id={}, skipping",
                maker.config.network_port,
                incoming_swap_id
            );
            continue;
        };

        log::info!(
            "[{}] Spawning recovery thread for swap_id={} (incoming={}, outgoing={})",
            maker.config.network_port,
            incoming_swap_id,
            incoming.contract_tx.compute_txid(),
            outgoing.contract_tx.compute_txid()
        );

        let maker_clone = maker.clone();
        let incoming_clone = incoming.clone();
        let outgoing_clone = outgoing.clone();

        let handle = std::thread::Builder::new()
            .name("Taproot Reboot Recovery Thread".to_string())
            .spawn(move || {
                if let Err(e) = recover_from_swap(maker_clone, incoming_clone, outgoing_clone) {
                    log::error!("Failed to recover from taproot swap on reboot: {:?}", e);
                }
            })?;

        maker.thread_pool.add_thread(handle);
    }

    Ok(())
}

/// Recover from a failed taproot swap by monitoring contract maturity and attempting recovery.
///
/// This function waits for either:
/// 1. Preimages to become available (via hashlock path)
/// 2. Timelock to mature (via timelock path)
pub(crate) fn recover_from_swap(
    maker: Arc<Maker>,
    mut incoming_swapcoin: IncomingSwapCoinV2,
    outgoing_swapcoin: OutgoingSwapCoinV2,
) -> Result<(), MakerError> {
    // Get timelock value from outgoing contract
    let timelock = outgoing_swapcoin
        .get_timelock()
        .ok_or(MakerError::General("missing timelock on outgoing swapcoin"))?;

    let outgoing_contract_txid = outgoing_swapcoin.contract_tx.compute_txid();

    log::info!(
        "[{}] Taproot recover_from_swap started for outgoing contract {}",
        maker.config.network_port,
        outgoing_contract_txid
    );

    // Create watch request for outgoing contract to detect if taker spends it via hashlock
    let outgoing_outpoint = bitcoin::OutPoint {
        txid: outgoing_contract_txid,
        vout: 0,
    };

    while !maker.shutdown.load(Relaxed) {
        // First, check if incoming contract has already been spent (e.g., via key-path)
        // If so, the maker already recovered their funds and we can exit
        let incoming_contract_txid = incoming_swapcoin.contract_tx.compute_txid();
        let incoming_outpoint = bitcoin::OutPoint {
            txid: incoming_contract_txid,
            vout: 0,
        };
        let incoming_spent = {
            let wallet = maker.wallet.read()?;
            wallet
                .rpc
                .get_tx_out(&incoming_outpoint.txid, incoming_outpoint.vout, Some(true))
                .map_err(WalletError::Rpc)?
                .is_none()
        };

        if incoming_spent {
            // If we have other_privkey, the swap was successful (key exchange happened)
            // and we already claimed the incoming contract via key-path. No recovery needed.
            if incoming_swapcoin.other_privkey.is_some() {
                log::info!(
                    "[{}] Incoming contract {} already spent via key-path (swap succeeded). Recovery not needed.",
                    maker.config.network_port,
                    incoming_contract_txid
                );
                // Stop watching the outgoing contract
                maker.watch_service.unwatch(outgoing_outpoint);
                return Ok(());
            }

            // If we don't have other_privkey, the taker used timelock recovery on the incoming
            // contract. We must recover our funds from the outgoing contract via timelock.
            log::warn!(
                "[{}] Incoming contract {} was spent by taker via timelock (no key exchange). We must recover our outgoing contract.",
                maker.config.network_port,
                incoming_contract_txid
            );
            // Continue to timelock recovery for our outgoing contract below
        }

        // Check if outgoing contract has been spent (taker may have used hashlock)
        if incoming_swapcoin.hash_preimage.is_none() {
            maker.watch_service.watch_request(outgoing_outpoint);
            if let Some(crate::watch_tower::watcher::WatcherEvent::UtxoSpent {
                spending_tx: Some(spending_tx),
                ..
            }) = maker.watch_service.poll_event()
            {
                log::info!(
                    "[{}] Detected spend of outgoing contract, attempting to extract preimage",
                    maker.config.network_port
                );
                // Try to extract preimage from witness
                if let Some(preimage) =
                    crate::protocol::contract2::extract_preimage_from_spending_tx(&spending_tx)
                {
                    log::info!(
                        "[{}] Successfully extracted preimage from outgoing contract spend",
                        maker.config.network_port
                    );
                    incoming_swapcoin.hash_preimage = Some(preimage);
                }
            }
        }

        // Check if we have the preimage for hashlock recovery (prioritize this over timelock)
        if incoming_swapcoin.hash_preimage.is_some() {
            log::info!(
                "[{}] Preimage available, recovering incoming contract via hashlock",
                maker.config.network_port
            );
            // Stop watching the outgoing contract before recovery
            maker.watch_service.unwatch(outgoing_outpoint);
            return recover_via_hashlock(maker, incoming_swapcoin);
        }

        // Check if timelock has matured using the helper function
        let timelock_matured = {
            let wallet = maker.wallet.read()?;
            crate::protocol::contract2::is_timelock_mature(
                &wallet.rpc,
                &outgoing_contract_txid,
                timelock,
            )?
        };

        if timelock_matured {
            // Before attempting timelock recovery, do one final check for outgoing contract spend
            // The taker may have spent it via hashlock, in which case we should extract preimage
            if incoming_swapcoin.hash_preimage.is_none() {
                log::info!(
                    "[{}] Timelock expired, doing final check for outgoing contract spend before timelock recovery",
                    maker.config.network_port
                );

                // Check if outgoing contract is spent by checking if the UTXO exists
                let outgoing_spent = {
                    let wallet = maker.wallet.read()?;
                    wallet
                        .rpc
                        .get_tx_out(&outgoing_outpoint.txid, outgoing_outpoint.vout, Some(true))
                        .map_err(WalletError::Rpc)?
                        .is_none()
                };

                if outgoing_spent {
                    log::info!(
                        "[{}] Outgoing contract already spent, attempting to extract preimage from blockchain",
                        maker.config.network_port
                    );

                    // Try to get spending transaction via watcher
                    maker.watch_service.watch_request(outgoing_outpoint);
                    if let Some(crate::watch_tower::watcher::WatcherEvent::UtxoSpent {
                        spending_tx: Some(spending_tx),
                        ..
                    }) = maker.watch_service.poll_event()
                    {
                        if let Some(preimage) =
                            crate::protocol::contract2::extract_preimage_from_spending_tx(
                                &spending_tx,
                            )
                        {
                            log::info!(
                                "[{}] Successfully extracted preimage from spent outgoing contract",
                                maker.config.network_port
                            );
                            incoming_swapcoin.hash_preimage = Some(preimage);
                            // Stop watching and recover incoming via hashlock
                            maker.watch_service.unwatch(outgoing_outpoint);
                            return recover_via_hashlock(maker, incoming_swapcoin);
                        }
                    }

                    // If we couldn't extract preimage but outgoing is spent, maker already swept
                    // their incoming during the swap, so they have recovered their funds
                    log::warn!(
                        "[{}] Outgoing contract spent but couldn't extract preimage. Maker should have already swept incoming.",
                        maker.config.network_port
                    );
                    maker.watch_service.unwatch(outgoing_outpoint);
                    return Ok(());
                }
            }

            log::info!(
                "[{}] Timelock matured, recovering outgoing contract via timelock",
                maker.config.network_port
            );
            maker.watch_service.unwatch(outgoing_outpoint);
            return recover_via_timelock(maker, outgoing_swapcoin);
        }

        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Recover incoming contract via hashlock script-path spend.
fn recover_via_hashlock(maker: Arc<Maker>, incoming: IncomingSwapCoinV2) -> Result<(), MakerError> {
    log::info!(
        "[{}] Starting hashlock recovery for incoming contract",
        maker.config.network_port
    );

    // Try to spend via hashlock
    loop {
        if maker.shutdown.load(Relaxed) {
            break;
        }

        let result = {
            // Check if we have the preimage
            if incoming.hash_preimage.is_none() {
                log::warn!(
                    "[{}] Preimage not available yet, waiting...",
                    maker.config.network_port
                );
                None
            } else {
                let preimage = incoming.hash_preimage.unwrap();
                let mut wallet = maker.wallet.write()?;

                // Attempt to spend via hashlock
                match wallet.spend_via_hashlock_v2(&incoming, &preimage, &maker.watch_service) {
                    Ok(txid) => {
                        log::info!(
                            "[{}] Successfully recovered incoming contract via hashlock: {}",
                            maker.config.network_port,
                            txid
                        );
                        Some(Ok(()))
                    }
                    Err(e) => {
                        log::error!(
                            "[{}] Failed to recover via hashlock: {:?}",
                            maker.config.network_port,
                            e
                        );
                        Some(Err(MakerError::Wallet(e)))
                    }
                }
            }
        };

        if let Some(result) = result {
            #[cfg(feature = "integration-test")]
            maker.shutdown.store(true, Relaxed);
            return result;
        }

        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}

/// Recover outgoing contract via timelock script-path spend.
fn recover_via_timelock(maker: Arc<Maker>, outgoing: OutgoingSwapCoinV2) -> Result<(), MakerError> {
    log::info!(
        "[{}] Starting timelock recovery for outgoing contract",
        maker.config.network_port
    );

    // Try to spend via timelock
    loop {
        if maker.shutdown.load(Relaxed) {
            break;
        }

        let result = {
            let mut wallet = maker.wallet.write()?;

            // Attempt to spend via timelock
            match wallet.spend_via_timelock_v2(&outgoing, &maker.watch_service) {
                Ok(txid) => {
                    log::info!(
                        "[{}] Successfully recovered outgoing contract via timelock: {}",
                        maker.config.network_port,
                        txid
                    );
                    Some(Ok(()))
                }
                Err(e) => {
                    log::error!(
                        "[{}] Failed to recover via timelock: {:?}",
                        maker.config.network_port,
                        e
                    );
                    Some(Err(MakerError::Wallet(e)))
                }
            }
        };

        if let Some(result) = result {
            #[cfg(feature = "integration-test")]
            maker.shutdown.store(true, Relaxed);
            return result;
        }

        std::thread::sleep(Duration::from_secs(10));
    }

    Ok(())
}
