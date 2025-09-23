//! The Maker API for Taproot Swaps.
//!
//! Defines the core functionality of the Maker in a taproot-based swap protocol implementation.
//! This is a simplified version that adapts the original maker API to work with the new taproot protocol.

use super::rpc::server::MakerRpc;
use crate::{
    protocol::{
        contract2::{calculate_coinswap_fee, calculate_contract_sighash},
        messages2::{Offer, SenderContractFromMaker, SendersContract, SwapDetails},
    },
    utill::{check_tor_status, get_maker_dir, ConnectionType, HEART_BEAT_INTERVAL},
    wallet::{RPCConfig, Wallet},
};
use bitcoin::{hashes::Hash, Amount, ScriptBuf, Transaction};
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

use super::{config::MakerConfig, error::MakerError};

/// Interval for health checks on a stable RPC connection with bitcoind.
pub const RPC_PING_INTERVAL: u32 = 9;

/// Maker triggers the recovery mechanism, if Taker is idle for more than 15 mins during a swap.
#[cfg(feature = "integration-test")]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(not(feature = "integration-test"))]
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60 * 15);

/// The minimum difference in locktime (in blocks) between the incoming and outgoing swaps.
pub const MIN_CONTRACT_REACTION_TIME: u16 = 20;

/// Fee Parameters for Coinswap
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

/// Minimum Coinswap amount; makers will not accept amounts below this.
pub const MIN_SWAP_AMOUNT: u64 = 10_000;

/// Used to configure the maker for testing purposes.
#[derive(Debug, Clone, Copy)]
pub enum MakerBehavior {
    /// Represents the normal behavior of the maker.
    Normal,
    /// Simulates closure at the "SendersContract" step.
    CloseAtSendersContract,
    /// Simulates closure at the "ReceiversContract" step.
    CloseAtReceiversContract,
    /// Simulates closure at the "PartialSignatures" step.
    CloseAtPartialSignatures,
    /// Simulates broadcasting the contract immediately after setup.
    BroadcastContractAfterSetup,
}

/// Maintains the state of a connection, including the list of swapcoins.
#[derive(Debug, Default)]
pub struct ConnectionState {
    pub(crate) swap_amount: Amount,
    pub(crate) timelock: u16,
    pub(crate) incoming_contract_my_privkey: Option<bitcoin::secp256k1::SecretKey>,
    pub(crate) incoming_contract_my_pubkey: Option<bitcoin::PublicKey>,
    pub(crate) incoming_contract_other_pubkey: Option<bitcoin::PublicKey>,
    pub(crate) incoming_contract_hashlock_script: Option<ScriptBuf>,
    pub(crate) incoming_contract_timelock_script: Option<ScriptBuf>,
    // Additional fields for MuSig2 signing
    pub(crate) incoming_contract_txid: Option<bitcoin::Txid>, // Taker's contract transaction
    pub(crate) incoming_contract_my_pub_nonce: Option<secp256k1::musig::PublicNonce>, // Our public nonce for the incoming contract
    // outgoing_contract_txid is defined below in the new fields section
    pub(crate) outgoing_aggregated_nonce: Option<secp256k1::musig::AggregatedNonce>, // For outgoing contract
    pub(crate) incoming_aggregated_nonce: Option<secp256k1::musig::AggregatedNonce>, // For incoming contract
    pub(crate) incoming_contract_tap_tweak: Option<bitcoin::secp256k1::Scalar>,
    pub(crate) incoming_contract_internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>,
    pub(crate) incoming_contract_spending_tx: Option<Transaction>, // Spending transaction for incoming contract
    pub(crate) outgoing_contract_my_privkey: Option<bitcoin::secp256k1::SecretKey>, // Our outgoing contract private key
    pub(crate) outgoing_contract_my_pubkey: Option<bitcoin::PublicKey>, // Our outgoing contract public key
    pub(crate) outgoing_contract_other_pubkey: Option<bitcoin::PublicKey>, // Taker's tweakable pubkey
    pub(crate) outgoing_contract_txid: Option<bitcoin::Txid>, // Our outgoing contract transaction
    pub(crate) outgoing_contract_tap_tweak: Option<bitcoin::secp256k1::Scalar>, // Tap tweak for our outgoing contract
    pub(crate) outgoing_contract_internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>, // Internal key for our outgoing contract
    pub(crate) outgoing_contract_hashlock_script: Option<ScriptBuf>, // Hashlock script for our outgoing contract
    pub(crate) outgoing_contract_timelock_script: Option<ScriptBuf>, // Timelock script for our outgoing contract
    pub(crate) outgoing_contract_my_sec_nonce: Option<secp256k1::musig::SecretNonce>, // Our secret nonce for the outgoing contract
    pub(crate) outgoing_contract_my_pub_nonce: Option<secp256k1::musig::PublicNonce>, // Our public nonce for the outgoing contract
    // Store our partial signature for the incoming contract (to avoid SecretNonce cloning issues)
    pub(crate) incoming_contract_my_partial_sig: Option<secp256k1::musig::PartialSignature>,
    // Store secret nonce bytes for incoming contract (since SecretNonce can't be cloned)
    pub(crate) incoming_contract_my_sec_nonce_bytes: Option<[u8; 132]>, // MUSIG_SECNONCE_SIZE
    pub(crate) outgoing_contract_spending_tx: Option<Transaction>, // Spending transaction for our outgoing contract
    pub(crate) outgoing_contract_my_partial_sig: Option<secp256k1::musig::PartialSignature>, // Our partial signature for outgoing contract
}

impl Clone for ConnectionState {
    fn clone(&self) -> Self {
        ConnectionState {
            swap_amount: self.swap_amount,
            timelock: self.timelock,
            incoming_contract_my_privkey: self.incoming_contract_my_privkey,
            incoming_contract_my_pubkey: self.incoming_contract_my_pubkey,
            incoming_contract_other_pubkey: self.incoming_contract_other_pubkey,
            incoming_contract_hashlock_script: self.incoming_contract_hashlock_script.clone(),
            incoming_contract_timelock_script: self.incoming_contract_timelock_script.clone(),
            incoming_contract_txid: self.incoming_contract_txid,
            incoming_contract_my_pub_nonce: self.incoming_contract_my_pub_nonce,
            // outgoing_contract_txid is handled below
            outgoing_aggregated_nonce: self.outgoing_aggregated_nonce,
            incoming_aggregated_nonce: self.incoming_aggregated_nonce,
            incoming_contract_tap_tweak: self.incoming_contract_tap_tweak,
            incoming_contract_internal_key: self.incoming_contract_internal_key,
            incoming_contract_spending_tx: self.incoming_contract_spending_tx.clone(),
            // Ordered pubkeys are computed on-the-fly
            outgoing_contract_my_privkey: self.outgoing_contract_my_privkey,
            outgoing_contract_my_pubkey: self.outgoing_contract_my_pubkey,
            outgoing_contract_other_pubkey: self.outgoing_contract_other_pubkey,
            outgoing_contract_hashlock_script: self.outgoing_contract_hashlock_script.clone(),
            outgoing_contract_timelock_script: self.outgoing_contract_timelock_script.clone(),
            // New fields for backwards sweeping protocol
            outgoing_contract_txid: self.outgoing_contract_txid,
            outgoing_contract_tap_tweak: self.outgoing_contract_tap_tweak,
            outgoing_contract_internal_key: self.outgoing_contract_internal_key,
            outgoing_contract_my_sec_nonce: None,
            outgoing_contract_my_pub_nonce: self.outgoing_contract_my_pub_nonce,
            incoming_contract_my_partial_sig: self.incoming_contract_my_partial_sig,
            incoming_contract_my_sec_nonce_bytes: self.incoming_contract_my_sec_nonce_bytes,
            outgoing_contract_spending_tx: self.outgoing_contract_spending_tx.clone(),
            outgoing_contract_my_partial_sig: self.outgoing_contract_my_partial_sig,
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
    /// Is setup complete
    pub is_setup_complete: AtomicBool,
    /// Path for the data directory.
    data_dir: PathBuf,
    /// Thread pool for managing all spawned threads
    pub(crate) thread_pool: Arc<ThreadPool>,
    /// Tracker address to connect to
    pub(crate) tracker: RwLock<Option<String>>,
}

#[allow(clippy::too_many_arguments)]
impl Maker {
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
        _connection_type: Option<ConnectionType>,
        behavior: MakerBehavior,
    ) -> Result<Self, MakerError> {
        let data_dir = data_dir.unwrap_or(get_maker_dir());
        let wallets_dir = data_dir.join("wallets");

        let wallet_file_name = wallet_file_name.unwrap_or_else(|| "maker-wallet".to_string());
        let wallet_path = wallets_dir.join(&wallet_file_name);

        let mut rpc_config = rpc_config.unwrap_or_default();
        rpc_config.wallet_name = wallet_file_name;

        let mut wallet = if wallet_path.exists() {
            let wallet = Wallet::load(&wallet_path, &rpc_config)?;
            log::info!("Wallet file at {wallet_path:?} successfully loaded.");
            wallet
        } else {
            let wallet = Wallet::init(&wallet_path, &rpc_config, None)?;
            log::info!("New Wallet created at : {wallet_path:?}");
            wallet
        };

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
            behavior,
            config,
            wallet: RwLock::new(wallet),
            shutdown: AtomicBool::new(false),
            ongoing_swap_state: Mutex::new(HashMap::new()),
            is_setup_complete: AtomicBool::new(false),
            data_dir,
            thread_pool: Arc::new(ThreadPool::new(network_port)),
            tracker: RwLock::new(None),
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

    /// Creates an offer for the taker
    pub fn create_offer(
        &self,
        connection_state: &mut ConnectionState,
    ) -> Result<Offer, MakerError> {
        let wallet = self.wallet.read()?;
        let (incoming_contract_my_privkey, incoming_contract_my_pubkey) =
            wallet.get_tweakable_keypair()?;
        connection_state.incoming_contract_my_privkey = Some(incoming_contract_my_privkey);
        connection_state.incoming_contract_my_pubkey = Some(incoming_contract_my_pubkey);
        // Get wallet balances to determine max size
        let balances = wallet.get_balances()?;
        let max_size = balances.spendable;

        // Get the highest value fidelity bond, similar to regular makers
        let highest_index = wallet.get_highest_fidelity_index()?;

        let fidelity_proof = if let Some(i) = highest_index {
            // Convert regular fidelity proof to taproot format
            let regular_proof = wallet
                .generate_fidelity_proof(i, &format!("127.0.0.1:{}", self.config.network_port))?;

            // Convert sha256d::Hash to sha256::Hash (taproot uses single hash)
            use bitcoin::hashes::Hash as HashTrait;
            let sha256_hash = bitcoin::hashes::sha256::Hash::from_byte_array(
                regular_proof.cert_hash.to_byte_array(),
            );

            crate::protocol::messages2::FidelityProof {
                bond: regular_proof.bond,
                cert_hash: sha256_hash,
                cert_sig: regular_proof.cert_sig,
            }
        } else {
            return Err(MakerError::General(
                "No fidelity bond available. Please create one first.",
            ));
        };

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
        connection_state.incoming_contract_hashlock_script =
            Some(message.hashlock_scripts[0].clone());
        connection_state.incoming_contract_timelock_script =
            Some(message.timelock_scripts[0].clone());
        connection_state.incoming_contract_txid = Some(message.contract_txs[0]);

        // Store the internal key and tap tweak from the message for cooperative spending
        // If not provided, we'll calculate our own when creating the outgoing contract
        connection_state.incoming_contract_internal_key = message.internal_key;
        connection_state.incoming_contract_tap_tweak =
            message.tap_tweak.as_ref().map(|t| t.clone().into());

        // Store taker's pubkey
        connection_state.incoming_contract_other_pubkey = Some(message.pubkeys_a[0]);

        // Store taker's tweakable pubkey for backwards sweeping protocol
        connection_state.outgoing_contract_other_pubkey = Some(message.next_party_tweakable_point);

        // Verify we have sufficient funds and get necessary data
        let (outgoing_privkey, funding_utxo) = {
            let wallet = self.wallet.write()?;
            let balance = wallet.get_balances()?;
            if balance.spendable < connection_state.swap_amount {
                return Err(MakerError::General("Insufficient funds for swap"));
            }

            // Get our tweakable keypair
            let (outgoing_privkey, outgoing_pubkey) = wallet.get_tweakable_keypair()?;
            connection_state.outgoing_contract_my_privkey = Some(outgoing_privkey);
            connection_state.outgoing_contract_my_pubkey = Some(outgoing_pubkey);

            // Get funding UTXO from our wallet
            let spendable_utxos = wallet.list_descriptor_utxo_spend_info()?;
            let funding_utxo = spendable_utxos
                .into_iter()
                .find(|(utxo, _)| utxo.amount >= connection_state.swap_amount)
                .map(|(utxo, _)| utxo)
                .ok_or_else(|| {
                    MakerError::General("No single UTXO found with sufficient amount")
                })?;

            (outgoing_privkey, funding_utxo)
        };

        // We expect only one contract for now
        if message.contract_txs.len() != 1 {
            return Err(MakerError::General(
                "Expected exactly one contract transaction",
            ));
        }

        use crate::protocol::contract2::{create_taproot_script, create_timelock_script};
        use bitcoin::locktime::absolute::LockTime;

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (outgoing_x_only, _) =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &outgoing_privkey)
                .x_only_public_key();

        let hashlock_script = connection_state
            .incoming_contract_hashlock_script
            .clone()
            .unwrap();
        connection_state.outgoing_contract_hashlock_script = Some(hashlock_script.clone());

        let timelock = LockTime::from_height(connection_state.timelock as u32).unwrap();
        let timelock_script = create_timelock_script(timelock, &outgoing_x_only);
        connection_state.outgoing_contract_timelock_script = Some(timelock_script.clone());
        // Create internal key for cooperative spending between taker and maker
        // Order pubkeys lexicographically to match signing order
        let mut pubkeys_for_internal_key = [
            connection_state.outgoing_contract_my_pubkey.unwrap(),
            connection_state.outgoing_contract_other_pubkey.unwrap(),
        ];
        pubkeys_for_internal_key.sort_by(|a, b| a.inner.serialize().cmp(&b.inner.serialize()));
        let internal_key = crate::protocol::musig_interface::get_aggregated_pubkey_i(
            pubkeys_for_internal_key[0].inner,
            pubkeys_for_internal_key[1].inner,
        );

        // Create taproot script (P2TR output)
        let (taproot_script, taproot_spendinfo) = create_taproot_script(
            hashlock_script.clone(),
            timelock_script.clone(),
            internal_key,
        );

        // Store the values for later use in the return statement
        connection_state.outgoing_contract_internal_key = Some(internal_key);
        connection_state.outgoing_contract_tap_tweak =
            Some(taproot_spendinfo.tap_tweak().to_scalar());

        // Get the actual amount we received from the incoming contract
        let incoming_contract_txid = connection_state
            .incoming_contract_txid
            .ok_or_else(|| MakerError::General("No taker contract transaction hash found"))?;

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

            // Use wallet's spend_from_wallet method to properly sign the transaction
            use crate::utill::MIN_FEE_RATE;
            use crate::wallet::Destination;

            // Get the funding UTXO spend info
            let funding_utxo_info = wallet
                .get_utxo((funding_utxo.txid, funding_utxo.vout))?
                .ok_or_else(|| MakerError::General("Funding UTXO not found"))?;

            // Use Destination::Multi to send exact amount to contract and keep the fee as change
            let contract_address =
                bitcoin::Address::from_script(&taproot_script, bitcoin::Network::Regtest)
                    .map_err(|_| MakerError::General("Failed to create address"))?;

            // Create a proper signed transaction using the wallet with Multi destination
            // This sends exactly outgoing_contract_amount to the contract and returns change to maker
            let signed_tx = wallet.spend_from_wallet(
                MIN_FEE_RATE,
                Destination::Multi {
                    outputs: vec![(contract_address, outgoing_contract_amount)],
                    op_return_data: None,
                },
                &[(funding_utxo.clone(), funding_utxo_info)],
            )?;

            // Broadcast the signed transaction
            wallet.send_tx(&signed_tx)?;
            signed_tx.compute_txid()
        };
        log::info!("Outgoing contract txid: {:?}", outgoing_contract_txid);

        // Store our own contract transaction hash for later use
        connection_state.outgoing_contract_txid = Some(outgoing_contract_txid);

        // For cooperative spending, we prepare to spend from the incoming contract transaction
        let incoming_contract_txid = connection_state
            .incoming_contract_txid
            .ok_or_else(|| MakerError::General("No taker contract transaction hash found"))?;

        // Get the prevout for sighash calculation
        // Use the internal key and tap tweak from the message (set during contract creation)
        let internal_key = connection_state
            .incoming_contract_internal_key
            .ok_or_else(|| MakerError::General("No internal key found in message"))?;

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

        // Use helper to calculate sighash
        let _message = calculate_contract_sighash(
            &spending_tx,
            incoming_contract_tx_output_value,
            &connection_state
                .incoming_contract_hashlock_script
                .clone()
                .unwrap(),
            &connection_state
                .incoming_contract_timelock_script
                .clone()
                .unwrap(),
            internal_key,
        )
        .map_err(|_| MakerError::General("Failed to calculate sighash"))?;

        // Store taproot contract data directly in connection state instead of using traditional IncomingSwapCoin
        // The taker's contract transaction hash is already stored in connection_state.contract_tx_hash
        // We have all the necessary taproot data: internal_key, tap_tweak, hashlock_script, timelock_script

        // Return our contract transaction details
        Ok(SenderContractFromMaker {
            contract_txs: vec![outgoing_contract_txid], // Our own contract transaction
            pubkeys_a: vec![connection_state.outgoing_contract_my_pubkey.unwrap()], // Next party's pubkey
            hashlock_scripts: vec![hashlock_script],
            timelock_scripts: vec![timelock_script],
            // Include the internal key and tap tweak that were used to create OUR outgoing contract
            internal_key: Some(connection_state.outgoing_contract_internal_key.unwrap()),
            tap_tweak: Some(connection_state.outgoing_contract_tap_tweak.unwrap().into()),
        })
    }

    /// Process SpendingTxAndReceiverNonce message and return response with nonces, partial signatures and spending tx
    pub(crate) fn process_spending_tx_and_receiver_nonce(
        &self,
        spending_tx_msg: &crate::protocol::messages2::SpendingTxAndReceiverNonce,
        connection_state: &mut ConnectionState,
    ) -> Result<crate::protocol::messages2::NoncesPartialSigsAndSpendingTx, MakerError> {
        use crate::protocol::musig_interface::generate_new_nonce_pair_i;
        use bitcoin::secp256k1::Secp256k1;

        log::info!("Processing SpendingTxAndReceiverNonce message");

        // Extract the spending transaction and receiver nonce
        let outgoing_contract_spending_tx = &spending_tx_msg.spending_transaction;
        let outgoing_contract_other_nonce: secp256k1::musig::PublicNonce =
            spending_tx_msg.receiver_nonce.clone().into();

        let outgoing_contract_my_pubkey = connection_state.outgoing_contract_my_pubkey.unwrap();
        let outgoing_contract_my_privkey = connection_state.outgoing_contract_my_privkey.unwrap();

        // Get taker pubkey from connection state
        let outgoing_contract_other_pubkey =
            connection_state.outgoing_contract_other_pubkey.unwrap();

        // Use outgoing contract tap_tweak (the one we calculated when creating our outgoing contract)
        // This is the contract that the taker is trying to spend from
        let tap_tweak = connection_state
            .outgoing_contract_tap_tweak
            .unwrap_or_else(|| {
                // log::warn!("No outgoing_contract_tap_tweak found in connection state, using default zeros");
                bitcoin::secp256k1::Scalar::from_be_bytes([0u8; 32]).unwrap()
            });

        // Generate sender nonce for this sweep (maker's nonce for taker's spending tx)
        // Use consistent order: taker first, maker second (not lexicographic)
        // This matches the e2e test pattern where each party uses the same pubkey order
        let pubkey1 = outgoing_contract_other_pubkey; // taker pubkey first
        let pubkey2 = outgoing_contract_my_pubkey; // maker pubkey second

        // Calculate sighash for the spending transaction to use in nonce generation
        let contract_txid = outgoing_contract_spending_tx.input[0].previous_output.txid;

        log::info!(
            "  Received spending tx trying to spend: {:?}",
            contract_txid
        );
        log::info!(
            "  My outgoing contract txid: {:?}",
            connection_state.outgoing_contract_txid
        );

        // Validate that the taker is trying to spend from the correct contract
        if let Some(my_contract_txid) = connection_state.outgoing_contract_txid {
            if contract_txid != my_contract_txid {
                log::error!(
                    "Taker is trying to spend from wrong contract. Expected: {:?}, Got: {:?}",
                    my_contract_txid,
                    contract_txid
                );
                return Err(MakerError::General(
                    "Taker is trying to spend from wrong contract",
                ));
            }
        } else {
            return Err(MakerError::General("No outgoing contract txid stored"));
        }

        let contract_tx = self
            .wallet
            .read()?
            .rpc
            .get_raw_transaction(&contract_txid, None)
            .map_err(|e| MakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;
        let contract_amount = contract_tx.output[0].value;

        // Create prevout for sighash calculation
        // Use the same method as taker: construct script from internal key
        // For the final contract, internal key is aggregated from taker and maker pubkeys
        // We need to use lexicographic ordering for internal key calculation to match contract creation
        let mut internal_key_pubkeys = [pubkey1, pubkey2];
        internal_key_pubkeys.sort_by(|a, b| a.inner.serialize().cmp(&b.inner.serialize()));
        let expected_internal_key = crate::protocol::musig_interface::get_aggregated_pubkey_i(
            internal_key_pubkeys[0].inner,
            internal_key_pubkeys[1].inner,
        );
        let outgoing_contract_hashlock_script = connection_state
            .outgoing_contract_hashlock_script
            .clone()
            .unwrap();
        let outgoing_contract_timelock_script = connection_state
            .outgoing_contract_timelock_script
            .clone()
            .unwrap();
        // Use helper to calculate sighash
        let message = calculate_contract_sighash(
            outgoing_contract_spending_tx,
            contract_amount,
            &outgoing_contract_hashlock_script,
            &outgoing_contract_timelock_script,
            expected_internal_key,
        )
        .map_err(|_| MakerError::General("Failed to calculate sighash"))?;

        log::info!(
            "[{}] message (sighash): {:?}",
            self.config.network_port,
            message
        );

        // Use lexicographic ordering to match how the contract was created
        let mut ordered_pubkeys = [pubkey1, pubkey2];
        ordered_pubkeys.sort_by(|a, b| a.inner.serialize().cmp(&b.inner.serialize()));

        let (outgoing_contract_my_sec_nonce, outgoing_contract_my_pub_nonce) =
            generate_new_nonce_pair_i(
                tap_tweak,
                ordered_pubkeys[0].inner, // lexicographically first pubkey
                ordered_pubkeys[1].inner, // lexicographically second pubkey
                outgoing_contract_my_pubkey.inner, // Signer is maker
                message,
            );

        connection_state.outgoing_contract_my_sec_nonce = Some(outgoing_contract_my_sec_nonce);
        connection_state.outgoing_contract_my_pub_nonce = Some(outgoing_contract_my_pub_nonce);

        let nonce_refs = vec![
            &outgoing_contract_my_pub_nonce,
            &outgoing_contract_other_nonce,
        ];
        let aggregated_nonce =
            crate::protocol::musig_interface::get_aggregated_nonce_i(&nonce_refs);
        let secp = Secp256k1::new();

        let outgoing_contract_my_partial_sig =
            crate::protocol::musig_interface::generate_partial_signature_i(
                message,
                &aggregated_nonce,
                connection_state
                    .outgoing_contract_my_sec_nonce
                    .take()
                    .unwrap(),
                bitcoin::secp256k1::Keypair::from_secret_key(&secp, &outgoing_contract_my_privkey),
                tap_tweak,
                internal_key_pubkeys[0].inner,
                internal_key_pubkeys[1].inner,
            );

        // Save the aggregated nonce, partial signature, and spending transaction for later sweep completion
        connection_state.outgoing_aggregated_nonce = Some(aggregated_nonce);
        connection_state.outgoing_contract_my_partial_sig = Some(outgoing_contract_my_partial_sig);
        connection_state.outgoing_contract_spending_tx =
            Some(outgoing_contract_spending_tx.clone());

        // let next_message = self.get_incoming_contract_sighash_message();
        let unsigned_spending_tx = self.create_unsigned_spending_tx(connection_state)?;
        let next_message = bitcoin::secp256k1::Message::from_digest(
            unsigned_spending_tx.compute_txid().to_byte_array(),
        );
        let (incoming_contract_my_sec_nonce, incoming_contract_my_pub_nonce) =
            generate_new_nonce_pair_i(
                tap_tweak,
                pubkey1.inner, // Use same consistent order: taker first, maker second
                pubkey2.inner,
                outgoing_contract_my_pubkey.inner, // Signer is maker
                next_message,
            );

        // Store nonces and spending transaction for later use in sweep completion
        connection_state.incoming_contract_my_pub_nonce = Some(incoming_contract_my_pub_nonce);
        connection_state.incoming_contract_spending_tx = Some(unsigned_spending_tx.clone());

        // Store secret nonce bytes since SecretNonce can't be cloned
        // We'll regenerate the partial signature later when we have the sender nonce
        connection_state.incoming_contract_my_sec_nonce_bytes =
            Some(incoming_contract_my_sec_nonce.dangerous_into_bytes());

        // Create response
        let response = crate::protocol::messages2::NoncesPartialSigsAndSpendingTx {
            sender_nonce: outgoing_contract_my_pub_nonce.into(),
            receiver_nonce: incoming_contract_my_pub_nonce.into(),
            partial_signatures: vec![outgoing_contract_my_partial_sig.into()],
            spending_transaction: unsigned_spending_tx,
        };

        // log::info!("Generated NoncesPartialSigsAndSpendingTx response");
        Ok(response)
    }

    /// Complete sweep with received partial signature
    pub(crate) fn complete_sweep_with_partial_signature(
        &self,
        partial_sig_and_senders_nonce: &crate::protocol::messages2::PartialSigAndSendersNonce,
        connection_state: &mut ConnectionState,
    ) -> Result<(), MakerError> {
        use crate::protocol::musig_interface::{
            aggregate_partial_signatures_i, generate_partial_signature_i, get_aggregated_nonce_i,
        };
        use bitcoin::secp256k1::Secp256k1;
        use bitcoin::sighash::SighashCache;
        use bitcoin::Witness;
        use bitcoind::bitcoincore_rpc::RpcApi;

        log::info!(
            "[{}] Completing maker sweep with received partial signature",
            self.config.network_port
        );

        // Check if we have partial signatures from the other party
        if partial_sig_and_senders_nonce.partial_signatures.is_empty() {
            log::warn!("[{}] No partial signatures received - taker partial signature generation not yet complete", self.config.network_port);
            return Ok(());
        }
        let other_partial_sig: secp256k1::musig::PartialSignature = partial_sig_and_senders_nonce
            .partial_signatures[0]
            .clone()
            .into();

        // Get the sender nonce that we just received
        let sender_nonce: secp256k1::musig::PublicNonce =
            partial_sig_and_senders_nonce.sender_nonce.clone().into();

        // DETAILED LOGGING FOR MAKER SIGNATURE VERIFICATION
        log::info!(
            "[{}] Received sender_nonce from taker",
            self.config.network_port
        );
        log::info!(
            "[{}] Received partial_sig from taker",
            self.config.network_port
        );

        // Get the spending transaction we created for our incoming contract
        let spending_tx = connection_state
            .incoming_contract_spending_tx
            .as_ref()
            .ok_or_else(|| {
                MakerError::General("No spending transaction found for sweep completion")
            })?;

        // Now we can generate our partial signature for the incoming contract sweep
        // Get stored secret nonce bytes and reconstruct the SecretNonce
        let sec_nonce_bytes = connection_state
            .incoming_contract_my_sec_nonce_bytes
            .ok_or_else(|| {
                MakerError::General("No stored secret nonce bytes for incoming contract")
            })?;
        let incoming_contract_my_sec_nonce =
            secp256k1::musig::SecretNonce::dangerous_from_bytes(sec_nonce_bytes);

        // Get our public nonce
        let incoming_contract_my_pub_nonce = connection_state
            .incoming_contract_my_pub_nonce
            .as_ref()
            .ok_or_else(|| MakerError::General("No stored public nonce for incoming contract"))?;

        // Get incoming contract details
        let incoming_contract_txid = connection_state
            .incoming_contract_txid
            .ok_or_else(|| MakerError::General("No incoming contract txid"))?;
        let incoming_tap_tweak = connection_state
            .incoming_contract_tap_tweak
            .ok_or_else(|| MakerError::General("No tap tweak for incoming contract"))?;
        let incoming_internal_key = connection_state
            .incoming_contract_internal_key
            .ok_or_else(|| MakerError::General("No internal key for incoming contract"))?;

        // Get contract transaction and reconstruct script
        let incoming_contract_tx = self
            .wallet
            .read()?
            .rpc
            .get_raw_transaction(&incoming_contract_txid, None)
            .map_err(|_e| MakerError::General("Failed to get incoming contract transaction"))?;
        let incoming_contract_amount = incoming_contract_tx.output[0].value;

        let incoming_hashlock_script =
            connection_state
                .incoming_contract_hashlock_script
                .clone()
                .ok_or_else(|| MakerError::General("No incoming hashlock script"))?;
        let incoming_timelock_script =
            connection_state
                .incoming_contract_timelock_script
                .clone()
                .ok_or_else(|| MakerError::General("No incoming timelock script"))?;

        // Use helper to calculate sighash
        let incoming_message = calculate_contract_sighash(
            spending_tx,
            incoming_contract_amount,
            &incoming_hashlock_script,
            &incoming_timelock_script,
            incoming_internal_key,
        )
        .map_err(|_| MakerError::General("Failed to calculate sighash"))?;

        // Get keypairs and order them for incoming contract
        let incoming_my_privkey = connection_state
            .incoming_contract_my_privkey
            .ok_or_else(|| MakerError::General("No incoming contract private key"))?;
        let secp = Secp256k1::new();
        let incoming_my_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &incoming_my_privkey);
        let incoming_other_pubkey = connection_state
            .incoming_contract_other_pubkey
            .ok_or_else(|| MakerError::General("No incoming contract other pubkey"))?;

        let mut incoming_ordered_pubkeys = [
            incoming_my_keypair.public_key(),
            incoming_other_pubkey.inner,
        ];
        incoming_ordered_pubkeys.sort_by_key(|a| a.serialize());

        // Create aggregated nonce with sender nonce and our public nonce
        let incoming_nonce_refs = if incoming_ordered_pubkeys[0] == incoming_my_keypair.public_key()
        {
            vec![incoming_contract_my_pub_nonce, &sender_nonce]
        } else {
            vec![&sender_nonce, incoming_contract_my_pub_nonce]
        };
        let incoming_aggregated_nonce = get_aggregated_nonce_i(&incoming_nonce_refs);

        // Generate our partial signature for the incoming contract
        let our_incoming_partial_sig = generate_partial_signature_i(
            incoming_message,
            &incoming_aggregated_nonce,
            incoming_contract_my_sec_nonce,
            incoming_my_keypair,
            incoming_tap_tweak,
            incoming_ordered_pubkeys[0],
            incoming_ordered_pubkeys[1],
        );

        // Aggregate both partial signatures
        let partial_sigs = if incoming_ordered_pubkeys[0] == incoming_my_keypair.public_key() {
            vec![&our_incoming_partial_sig, &other_partial_sig]
        } else {
            vec![&other_partial_sig, &our_incoming_partial_sig]
        };
        let aggregated_sig = aggregate_partial_signatures_i(
            incoming_message,
            incoming_aggregated_nonce,
            incoming_tap_tweak,
            partial_sigs,
            incoming_ordered_pubkeys[0],
            incoming_ordered_pubkeys[1],
        );

        // Create final signature and add to transaction witness
        let final_signature =
            bitcoin::taproot::Signature::from_slice(aggregated_sig.assume_valid().as_byte_array())
                .unwrap();

        let mut final_tx = spending_tx.clone();
        let mut final_sighasher = SighashCache::new(&mut final_tx);
        *final_sighasher.witness_mut(0).unwrap() = Witness::p2tr_key_spend(&final_signature);
        let completed_tx = final_sighasher.into_transaction();

        // Broadcast the completed spending transaction
        use crate::bitcoind::bitcoincore_rpc::RawTx;
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

        Ok(())
    }

    /// Create an unsigned transaction to spend from the incoming contract
    pub(crate) fn create_unsigned_spending_tx(
        &self,
        connection_state: &ConnectionState,
    ) -> Result<Transaction, MakerError> {
        use bitcoin::{Amount, OutPoint, Sequence, TxIn, TxOut, Witness};

        // Get the incoming contract transaction hash
        let incoming_contract_txid = connection_state
            .incoming_contract_txid
            .ok_or_else(|| MakerError::General("No incoming contract transaction hash found"))?;

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
}

impl MakerRpc for Maker {
    fn get_wallet(&self) -> &RwLock<Wallet> {
        &self.wallet
    }
    fn get_data_dir(&self) -> &Path {
        &self.data_dir
    }
    fn get_config(&self) -> &MakerConfig {
        &self.config
    }
    fn get_shutdown(&self) -> &AtomicBool {
        &self.shutdown
    }
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
                    log::warn!(
                        "[{}] Idle connection timeout for IP: {}. Removing connection state.",
                        maker.config.network_port,
                        ip
                    );
                    bad_ip.push(ip.clone());
                    *state = ConnectionState::default();
                    break;
                }
            }

            for ip in bad_ip.iter() {
                lock_on_state.remove(ip);
            }
        }

        std::thread::sleep(HEART_BEAT_INTERVAL);
    }

    Ok(())
}
