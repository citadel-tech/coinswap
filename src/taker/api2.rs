use crate::{
    protocol::{
        contract2::{
            calculate_coinswap_fee, create_hashlock_script, create_taproot_script,
            create_timelock_script,
        },
        error::ProtocolError,
        messages2::{
            AckResponse, GetOffer, MakerToTakerMessage, Preimage, PrivateKeyHandover,
            SenderContractFromMaker, SendersContract, SwapDetails, TakerToMakerMessage,
        },
        musig_interface::{
            aggregate_partial_signatures_compat, generate_new_nonce_pair_compat,
            generate_partial_signature_compat, get_aggregated_nonce_compat,
        },
    },
    taker::{
        config::TakerConfig,
        offers::{MakerAddress, OfferAndAddress, OfferBook, OfferBookUpdater},
    },
    utill::{check_tor_status, get_taker_dir, read_message, send_message},
    wallet::{
        ffi::{MakerFeeInfo, SwapReport},
        IncomingSwapCoinV2, OutgoingSwapCoinV2, RPCConfig, Wallet, WalletError,
    },
    watch_tower::{
        registry_storage::FileRegistry,
        rpc_backend::BitcoinRpc,
        service::WatchService,
        watcher::{Role, Watcher, WatcherEvent},
        zmq_backend::ZmqBackend,
    },
};
use bitcoin::{
    hashes::{sha256, Hash},
    hex::DisplayHex,
    locktime::absolute::LockTime,
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        Keypair, Secp256k1,
    },
    sighash::SighashCache,
    transaction::Version,
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
};
use bitcoind::bitcoincore_rpc::{bitcoincore_rpc_json::ListUnspentResultEntry, RawTx, RpcApi};
use chrono::Utc;
use socks::Socks5Stream;
use std::{
    collections::HashSet,
    convert::TryFrom,
    net::TcpStream,
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::Duration,
};

use super::error::TakerError;

/// Represents how a taproot contract output was spent
#[derive(Debug, Clone)]
pub enum TaprootSpendingPath {
    /// Cooperative key-path spend (MuSig2)
    KeyPath,
    /// Script-path spend using hashlock
    Hashlock {
        /// The 32-byte hash preimage revealed in the spending transaction
        preimage: [u8; 32],
    },
    /// Script-path spend using timelock
    Timelock,
}

#[derive(Debug, Default, Clone)]
/// Parameters for initiating a coinswap
pub struct SwapParams {
    /// Amount to send in the swap
    pub send_amount: Amount,
    /// Number of makers to use in the swap
    pub maker_count: usize,
    /// Number of transaction splits
    pub tx_count: u32,
    /// Required confirmations for funding transactions
    pub required_confirms: u32,
    /// User selected UTXOs (optional, for manual UTXO selection)
    pub manually_selected_outpoints: Option<Vec<OutPoint>>,
}

#[derive(Clone)]
struct OngoingSwapState {
    pub swap_params: SwapParams,
    pub active_preimage: Preimage,
    pub id: String,
    pub suitable_makers: Vec<OfferAndAddress>,
    pub chosen_makers: Vec<OfferAndAddress>,
    pub outgoing_contract: OutgoingSwapCoinV2,
    pub incoming_contract: IncomingSwapCoinV2,
    // Private key handover: store maker outgoing contract private keys (indexed by maker position)
    // Each maker hands over their outgoing contract private key after sweeping their incoming contract
    pub maker_outgoing_privkeys: Vec<Option<bitcoin::secp256k1::SecretKey>>,
}

impl Default for OngoingSwapState {
    fn default() -> Self {
        use bitcoin::{secp256k1::SecretKey, ScriptBuf, Transaction};

        let dummy_key = SecretKey::from_slice(&[1u8; 32]).expect("valid key");

        Self {
            swap_params: SwapParams::default(),
            active_preimage: Preimage::default(),
            id: String::new(),
            suitable_makers: Vec::new(),
            chosen_makers: Vec::new(),
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
            maker_outgoing_privkeys: Vec::new(),
        }
    }
}

pub(crate) const TCP_TIMEOUT_SECONDS: u64 = 300;
pub(crate) const REFUND_LOCKTIME: u16 = 20;
pub(crate) const REFUND_LOCKTIME_STEP: u16 = 20;

/// Helper function to establish a connection to a maker
fn connect_to_maker(
    maker_addr: &str,
    config: &TakerConfig,
    timeout_secs: u64,
) -> Result<TcpStream, TakerError> {
    let socket = if cfg!(feature = "integration-test") {
        TcpStream::connect(maker_addr).map_err(|e| {
            TakerError::General(format!(
                "Failed to connect to maker {}: {:?}",
                maker_addr, e
            ))
        })?
    } else {
        Socks5Stream::connect(
            format!("127.0.0.1:{}", config.socks_port).as_str(),
            maker_addr,
        )
        .map_err(|e| {
            TakerError::General(format!(
                "Failed to connect to maker {} via Tor: {:?}",
                maker_addr, e
            ))
        })?
        .into_inner()
    };

    let timeout = Duration::from_secs(timeout_secs);
    socket
        .set_read_timeout(Some(timeout))
        .and_then(|_| socket.set_write_timeout(Some(timeout)))
        .map_err(|e| TakerError::General(format!("Failed to set write timeout: {:?}", e)))?;

    Ok(socket)
}

/// Fetch offers from taproot makers using taproot protocol messages
fn fetch_taproot_offers(
    maker_addresses: &[MakerAddress],
    config: &TakerConfig,
) -> Result<Vec<OfferAndAddress>, TakerError> {
    use std::sync::mpsc;

    let (offers_writer, offers_reader) = mpsc::channel::<Option<OfferAndAddress>>();
    let maker_addresses_len = maker_addresses.len();

    thread::scope(|s| {
        for address in maker_addresses {
            let offers_writer = offers_writer.clone();
            s.spawn(move || {
                let offer = download_taproot_offer(address, config);
                let _ = offers_writer.send(offer);
            });
        }
    });

    let mut result = Vec::new();
    for _ in 0..maker_addresses_len {
        if let Some(offer_addr) = offers_reader.recv()? {
            result.push(offer_addr);
        }
    }

    Ok(result)
}

/// Download a single offer from a taproot maker
fn download_taproot_offer(address: &MakerAddress, config: &TakerConfig) -> Option<OfferAndAddress> {
    let maker_addr = address.to_string();
    log::info!("Downloading offer from taproot maker: {}", maker_addr);

    // Try connecting to the maker using the helper function
    let mut socket = match connect_to_maker(&maker_addr, config, 30) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to connect to taproot maker {}: {:?}", maker_addr, e);
            return None;
        }
    };

    // Send GetOffer message (taproot protocol)
    let get_offer_msg = GetOffer {
        id: String::new(),
        protocol_version_min: 1,
        protocol_version_max: 1,
        number_of_transactions: 1,
    };

    let msg = TakerToMakerMessage::GetOffer(get_offer_msg);
    if let Err(e) = send_message(&mut socket, &msg) {
        log::error!("Failed to send GetOffer message to {}: {:?}", maker_addr, e);
        return None;
    }

    // Read the response
    let response_bytes = match read_message(&mut socket) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::error!("Failed to read response from {}: {:?}", maker_addr, e);
            return None;
        }
    };

    // Parse the response
    let response: MakerToTakerMessage = match serde_cbor::from_slice(&response_bytes) {
        Ok(msg) => msg,
        Err(e) => {
            log::error!("Failed to parse response from {}: {:?}", maker_addr, e);
            return None;
        }
    };

    // Extract the offer
    let taproot_offer = match response {
        MakerToTakerMessage::RespOffer(offer) => *offer,
        _ => {
            log::error!(
                "Unexpected response from {}: expected RespOffer",
                maker_addr
            );
            return None;
        }
    };

    let offer = crate::protocol::messages::Offer {
        base_fee: taproot_offer.base_fee,
        amount_relative_fee_pct: taproot_offer.amount_relative_fee,
        time_relative_fee_pct: taproot_offer.time_relative_fee,
        required_confirms: 1, // Default value for taproot
        minimum_locktime: taproot_offer.minimum_locktime,
        max_size: taproot_offer.max_size,
        min_size: taproot_offer.min_size,
        tweakable_point: taproot_offer.tweakable_point,
        fidelity: crate::protocol::messages::FidelityProof {
            bond: taproot_offer.fidelity.bond,
            cert_hash: bitcoin::hashes::sha256d::Hash::from_slice(
                taproot_offer.fidelity.cert_hash.as_ref(),
            )
            .ok()?,
            cert_sig: taproot_offer.fidelity.cert_sig,
        },
    };

    log::info!(
        "Successfully downloaded offer from taproot maker: {}",
        maker_addr
    );

    Some(OfferAndAddress {
        offer,
        address: address.clone(),
        timestamp: Utc::now(),
    })
}
/// Taker implementation for coinswap protocol
pub struct Taker {
    wallet: Wallet,
    config: TakerConfig,
    offerbook: Arc<Mutex<OfferBook>>,
    offerbook_updater: Option<OfferBookUpdater>,
    ongoing_swap_state: OngoingSwapState,
    data_dir: PathBuf,
    watch_service: WatchService,
}

impl Taker {
    /// Initialize a new Taker instance
    pub fn init(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        control_port: Option<u16>,
        tor_auth_password: Option<String>,
        zmq_addr: String,
        password: Option<String>,
    ) -> Result<Taker, TakerError> {
        let data_dir = data_dir.unwrap_or_else(get_taker_dir);

        // Ensure the data directory exists
        std::fs::create_dir_all(&data_dir)?;

        let wallets_dir = data_dir.join("wallets");

        let wallet_file_name = wallet_file_name.unwrap_or("taker-wallet".to_string());
        let wallet_path = wallets_dir.join(&wallet_file_name);

        let mut rpc_config = rpc_config.unwrap_or_default();
        rpc_config.wallet_name = wallet_file_name;

        let backend = ZmqBackend::new(&zmq_addr);
        let rpc_backend = BitcoinRpc::new(rpc_config.clone())?;
        let blockchain_info = rpc_backend.get_blockchain_info()?;
        let file_registry = data_dir
            .join(".taker_watcher")
            .join(blockchain_info.chain.to_string());
        let registry = FileRegistry::load(file_registry);
        let (tx_requests, rx_requests) = mpsc::channel();
        let (tx_events, rx_responses) = mpsc::channel();

        let mut watcher = Watcher::<Taker>::new(backend, registry, rx_requests, tx_events);
        _ = thread::Builder::new()
            .name("Watcher thread".to_string())
            .spawn(move || watcher.run(rpc_backend));

        let watch_service = WatchService::new(tx_requests, rx_responses);

        let mut wallet = if wallet_path.exists() {
            let wallet = Wallet::load(&wallet_path, &rpc_config, password)?;
            log::info!("Loaded wallet from {}", wallet_path.display());
            wallet
        } else {
            let wallet = Wallet::init(&wallet_path, &rpc_config, None)?;
            log::info!("New Wallet created at {:?}", wallet_path);
            wallet
        };

        let mut config = TakerConfig::new(Some(&data_dir.join("config.toml")))?;

        config.control_port = control_port.unwrap_or(config.control_port);
        if let Some(tor_auth_password) = tor_auth_password {
            config.tor_auth_password = tor_auth_password;
        }
        if !cfg!(feature = "integration-test") {
            check_tor_status(config.control_port, config.tor_auth_password.as_str())?;
        }

        config.write_to_file(&data_dir.join("config.toml"))?;

        let offerbook_path = data_dir.join("offerbook.json");
        let offerbook = if offerbook_path.exists() {
            match OfferBook::read_from_disk(&offerbook_path) {
                Ok(offerbook) => {
                    log::info!("Successfully loaded offerbook at : {:?}", offerbook_path);
                    offerbook
                }
                Err(e) => {
                    log::error!("Offerbook data corrupted. Recreating. {:?}", e);
                    let empty_book = OfferBook::default();
                    empty_book.write_to_disk(&offerbook_path)?;
                    empty_book
                }
            }
        } else {
            let empty_book = OfferBook::default();
            std::fs::File::create(&offerbook_path)?;
            empty_book.write_to_disk(&offerbook_path)?;
            empty_book
        };

        log::info!("Initializing wallet sync...");
        wallet.sync_and_save()?;
        log::info!("Completed wallet sync");

        Ok(Self {
            wallet,
            config,
            offerbook: Arc::new(Mutex::new(offerbook)),
            offerbook_updater: None,
            ongoing_swap_state: OngoingSwapState::default(),
            data_dir,
            watch_service,
        })
    }

    /// Get a reference to the wallet
    pub fn get_wallet(&self) -> &Wallet {
        &self.wallet
    }

    /// Get a mutable reference to the wallet
    pub fn get_wallet_mut(&mut self) -> &mut Wallet {
        &mut self.wallet
    }

    /// Initiate a coinswap with the given parameters
    pub fn do_coinswap(
        &mut self,
        swap_params: SwapParams,
    ) -> Result<Option<SwapReport>, TakerError> {
        let swap_start_time = std::time::Instant::now();
        let initial_utxoset = self.wallet.list_all_utxo();

        let available = self.wallet.get_balances()?.spendable;

        // assuming the fees for the swap is 1000 sats
        let required = swap_params.send_amount + Amount::from_sat(1000);
        if available < required {
            let err = WalletError::InsufficientFund {
                available: available.to_sat(),
                required: required.to_sat(),
            };
            return Err(err.into());
        }

        self.sync_offerbook()?;

        // Generate preimage for the swap
        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        let unique_id = preimage[0..8].to_hex_string(bitcoin::hex::Case::Lower);
        log::info!("Initiating coinswap with id : {}", unique_id);

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.id = unique_id;

        self.choose_makers_for_swap(swap_params)?;
        self.setup_contract_keys_and_scripts()?;

        let outgoing_signed_contract_transactions = self.create_outgoing_contract_transactions()?;
        let tx = match outgoing_signed_contract_transactions.first() {
            Some(tx) => tx,
            None => {
                return Err(TakerError::General(
                    "There is no outgoing contract transactions".to_string(),
                ))
            }
        };
        self.wallet.send_tx(tx)?;

        // Register outgoing contract with watcher for monitoring
        let outgoing_contract_txid = tx.compute_txid();
        let outgoing_outpoint = bitcoin::OutPoint {
            txid: outgoing_contract_txid,
            vout: 0,
        };
        self.watch_service.register_watch_request(outgoing_outpoint);
        log::info!(
            "Registered watcher for taker's outgoing contract: {}",
            outgoing_outpoint
        );

        // Persist outgoing swapcoin to wallet store for recovery
        self.wallet
            .add_outgoing_swapcoin_v2(&self.ongoing_swap_state.outgoing_contract);
        self.wallet.save_to_disk()?;
        log::info!("Persisted outgoing swapcoin to wallet store and saved to disk");

        for tx in &outgoing_signed_contract_transactions {
            self.wallet.wait_for_tx_confirmation(tx.compute_txid())?;
        }

        match self
            .negotiate_with_makers_and_coordinate_sweep(&outgoing_signed_contract_transactions)
        {
            Ok(_) => {
                log::info!(
                    "Swaps settled successfully. Sweeping the coins and resetting everything."
                );

                // Store swap state before reset for report generation
                let prereset_swapstate = self.ongoing_swap_state.clone();

                // Sync wallet and generate report
                self.wallet.sync_and_save()?;
                let swap_report = self.generate_swap_report(
                    &prereset_swapstate,
                    swap_start_time,
                    initial_utxoset,
                )?;

                log::info!("Successfully Completed Taproot Coinswap.");
                Ok(Some(swap_report))
            }
            Err(e) => {
                log::error!("Swap Settlement Failed: {:?}", e);
                log::warn!("Starting recovery from existing swap");
                self.recover_from_swap()?;
                Ok(None)
            }
        }
    }

    /// Generate a swap report after successful completion of a taproot coinswap
    fn generate_swap_report(
        &self,
        prereset_swapstate: &OngoingSwapState,
        start_time: std::time::Instant,
        initial_utxos: Vec<ListUnspentResultEntry>,
    ) -> Result<SwapReport, TakerError> {
        let swap_state = prereset_swapstate;
        let target_amount = swap_state.swap_params.send_amount.to_sat();
        let swap_duration = start_time.elapsed();

        let all_regular_utxo = self
            .wallet
            .list_descriptor_utxo_spend_info()
            .into_iter()
            .map(|(utxo, _)| utxo)
            .collect::<Vec<_>>();

        let initial_outpoints = initial_utxos
            .iter()
            .map(|utxo| OutPoint {
                txid: utxo.txid,
                vout: utxo.vout,
            })
            .collect::<HashSet<_>>();

        let current_outpoints = all_regular_utxo
            .iter()
            .map(|utxo| OutPoint {
                txid: utxo.txid,
                vout: utxo.vout,
            })
            .collect::<HashSet<_>>();

        // Present in initial set but not in current set (consumed)
        let input_utxos = initial_utxos
            .iter()
            .filter(|utxo| {
                let initial_outpoint = OutPoint {
                    txid: utxo.txid,
                    vout: utxo.vout,
                };
                !current_outpoints.contains(&initial_outpoint)
            })
            .map(|utxo| utxo.amount.to_sat())
            .collect::<Vec<u64>>();

        let output_regular_utxos = all_regular_utxo
            .iter()
            .filter(|utxo| {
                let final_outpoint = OutPoint {
                    txid: utxo.txid,
                    vout: utxo.vout,
                };
                !initial_outpoints.contains(&final_outpoint)
            })
            .collect::<Vec<_>>();

        // Present in current set but not in initial regular set (created as change)
        let output_change_amounts = output_regular_utxos
            .iter()
            .map(|utxo| utxo.amount.to_sat())
            .collect::<Vec<u64>>();

        let network = self.wallet.store.network;

        // Get swept swap UTXOs (V2)
        let output_swap_utxos = self
            .wallet
            .list_swept_incoming_swap_utxos()
            .into_iter()
            .map(|(utxo, _)| {
                let address = utxo
                    .address
                    .as_ref()
                    .and_then(|addr| addr.clone().require_network(network).ok())
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                (utxo.amount.to_sat(), address)
            })
            .collect::<Vec<(u64, String)>>();

        let output_swap_amounts = output_swap_utxos
            .iter()
            .map(|(amount, _)| *amount)
            .collect::<Vec<u64>>();

        let output_change_utxos = output_regular_utxos
            .iter()
            .map(|utxo| {
                let address = utxo
                    .address
                    .as_ref()
                    .and_then(|addr| addr.clone().require_network(network).ok())
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                (utxo.amount.to_sat(), address)
            })
            .collect::<Vec<(u64, String)>>();

        let output_utxos = [output_change_amounts.clone(), output_swap_amounts.clone()].concat();

        let total_input_amount = input_utxos.iter().sum::<u64>();
        let total_output_amount = output_utxos.iter().sum::<u64>();

        println!("\n\x1b[1;36m════════════════════════════════════════════════════════════════════════════════");
        println!("                         🪙 TAPROOT COINSWAP REPORT 🪙");
        println!("════════════════════════════════════════════════════════════════════════════════\x1b[0m\n");

        println!("\x1b[1;37mSwap ID           :\x1b[0m {}", swap_state.id);
        println!("\x1b[1;37mStatus            :\x1b[0m \x1b[1;32m✅ COMPLETED SUCCESSFULLY\x1b[0m");
        println!(
            "\x1b[1;37mDuration          :\x1b[0m {:.2} seconds",
            swap_duration.as_secs_f64()
        );

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                              Swap Parameters");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[1;37mTarget Amount     :\x1b[0m {target_amount} Sats");
        println!("\x1b[1;37mTotal Input       :\x1b[0m {total_input_amount} Sats");
        println!("\x1b[1;37mTotal Output      :\x1b[0m {total_output_amount} Sats");
        println!(
            "\x1b[1;37mMakers Involved   :\x1b[0m {}",
            swap_state.swap_params.maker_count
        );

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                                Makers Used");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");
        for (index, maker) in swap_state.chosen_makers.iter().enumerate() {
            println!("  \x1b[1;33m{}.\x1b[0m {}", index + 1, maker.address);
        }

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                            Transaction Details");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");

        // In V2 taproot, we have one outgoing contract tx
        let outgoing_txid = swap_state.outgoing_contract.contract_tx.compute_txid();
        println!(
            "\x1b[1;37mOutgoing Contract :\x1b[0m \x1b[2m{}\x1b[0m",
            outgoing_txid
        );

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                              Fee Information");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");

        let total_fee = total_input_amount.saturating_sub(total_output_amount);
        println!("\x1b[1;37mTotal Fees        :\x1b[0m \x1b[1;31m{total_fee} sats\x1b[0m");

        // Collect maker fee information for V2
        let mut maker_fee_info = Vec::new();
        let mut total_maker_fees = 0u64;

        for (maker_index, maker) in swap_state.chosen_makers.iter().enumerate() {
            let maker_fee = calculate_coinswap_fee(
                swap_state.swap_params.send_amount.to_sat(),
                0, // timelock not used for fee display in report
                maker.offer.base_fee,
                maker.offer.amount_relative_fee_pct,
                maker.offer.time_relative_fee_pct,
            );

            println!("\n\x1b[1;33mMaker {}:\x1b[0m", maker_index + 1);
            println!("    Address              : {}", maker.address);
            println!("    Base Fee             : {}", maker.offer.base_fee);
            println!(
                "    Amount Relative Fee  : {:.2}%",
                maker.offer.amount_relative_fee_pct
            );
            println!("    Total Fee            : {} sats", maker_fee);

            maker_fee_info.push(MakerFeeInfo {
                maker_index,
                maker_address: maker.address.to_string(),
                base_fee: maker.offer.base_fee as f64,
                amount_relative_fee: (maker.offer.amount_relative_fee_pct
                    * swap_state.swap_params.send_amount.to_sat() as f64)
                    / 100.0,
                time_relative_fee: 0.0, // Simplified for report
                total_fee: maker_fee as f64,
            });

            total_maker_fees += maker_fee;
        }

        let mining_fee = total_fee.saturating_sub(total_maker_fees);
        println!("\n\x1b[1;37mTotal Maker Fees  :\x1b[0m \x1b[36m{total_maker_fees} sats\x1b[0m");
        println!("\x1b[1;37mMining Fees       :\x1b[0m \x1b[36m{mining_fee} sats\x1b[0m");

        let fee_percentage = if target_amount > 0 {
            (total_fee as f64 / target_amount as f64) * 100.0
        } else {
            0.0
        };
        println!("\x1b[1;37mTotal Fee Rate    :\x1b[0m \x1b[1;31m{fee_percentage:.2} %\x1b[0m");

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                              UTXO Information");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[1;37mInput UTXOs:\x1b[0m {input_utxos:?}");
        println!("\x1b[1;37mOutput UTXOs:\x1b[0m");
        println!("  Seed / Regular : {output_change_utxos:?}");
        println!("  Swap Coins     : {output_swap_utxos:?}");

        println!("\n\x1b[1;36m════════════════════════════════════════════════════════════════════════════════");
        println!("                                END REPORT");
        println!("════════════════════════════════════════════════════════════════════════════════\x1b[0m\n");

        // Collect maker addresses
        let maker_addresses = swap_state
            .chosen_makers
            .iter()
            .map(|maker| maker.address.to_string())
            .collect::<Vec<_>>();

        // For V2, we have a single funding tx (the outgoing contract)
        let funding_txids_by_hop = vec![vec![outgoing_txid.to_string()]];

        let report = SwapReport {
            swap_id: swap_state.id.clone(),
            swap_duration_seconds: swap_duration.as_secs_f64(),
            target_amount,
            total_input_amount,
            total_output_amount,
            makers_count: swap_state.swap_params.maker_count,
            maker_addresses,
            total_funding_txs: 1,
            funding_txids_by_hop,
            total_fee,
            total_maker_fees,
            mining_fee,
            fee_percentage,
            maker_fee_info,
            input_utxos,
            output_change_amounts,
            output_swap_amounts,
            output_swap_utxos,
            output_change_utxos,
        };

        Ok(report)
    }

    /// Choose makers for the swap by negotiating with them
    fn choose_makers_for_swap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
        // Find suitable maker asks for an offer from the makers
        let mut suitable_makers = self.find_suitable_makers(&swap_params);
        log::info!(
            "Found {} suitable makers for swap (need {})",
            suitable_makers.len(),
            swap_params.maker_count
        );
        if swap_params.maker_count > suitable_makers.len() {
            log::error!(
                "Not enough suitable makers for the requested amount. Required {}, available {}",
                swap_params.maker_count,
                suitable_makers.len()
            );
            return Err(TakerError::NotEnoughMakersInOfferBook);
        }

        self.ongoing_swap_state.suitable_makers = suitable_makers.clone();

        // Set swap params early so they're available for SwapDetails
        self.ongoing_swap_state.swap_params = swap_params.clone();

        // Send SwapDetails message to all the makers
        // Receive the Ack or Nack message from the maker
        for (maker_index, suitable_maker) in suitable_makers.iter_mut().enumerate() {
            // Skip if this maker is already chosen (prevent same maker in multiple hops)
            let already_chosen = self
                .ongoing_swap_state
                .chosen_makers
                .iter()
                .any(|m| m.address == suitable_maker.address);

            if already_chosen {
                log::warn!(
                    "Skipping maker {} - already chosen for this swap",
                    suitable_maker.address
                );
                continue;
            }

            // Use chosen_makers.len() for position in swap chain (not maker_index which is position in suitable_makers list)
            let chain_position = self.ongoing_swap_state.chosen_makers.len();

            log::info!(
                "Maker {} (chain position {}): {:?}",
                maker_index,
                chain_position,
                suitable_maker.address
            );

            // Always send GetOffer first to ensure maker has fresh keypair state
            // This is required because offers may be cached but maker might have restarted
            let get_offer_msg = GetOffer {
                id: self.ongoing_swap_state.id.clone(),
                protocol_version_min: 1,
                protocol_version_max: 1,
                number_of_transactions: 1,
            };
            let get_offer_response = self.send_to_maker_and_get_response(
                &suitable_maker.address,
                TakerToMakerMessage::GetOffer(get_offer_msg),
            )?;
            match get_offer_response {
                MakerToTakerMessage::RespOffer(fresh_offer) => {
                    log::info!(
                        "Received fresh offer from maker: {:?}, updating tweakable_point",
                        suitable_maker.address
                    );
                    // TODO: Update entire offer instead of just tweakable_point.
                    // This requires OfferAndAddress to use messages2::Offer for taproot swaps.
                    suitable_maker.offer.tweakable_point = fresh_offer.tweakable_point;
                }
                _ => {
                    log::warn!(
                        "Unexpected response to GetOffer from maker: {:?}",
                        suitable_maker.address
                    );
                    continue;
                }
            }

            // Calculate staggered timelock for this maker
            // Taker has the longest, each maker gets progressively shorter
            // With 2 makers: Taker=60, Maker0=40, Maker1=20
            let maker_timelock = REFUND_LOCKTIME
                + REFUND_LOCKTIME_STEP
                    * (self.ongoing_swap_state.swap_params.maker_count - chain_position - 1) as u16;

            log::info!(
                "Assigning timelock {} blocks to maker {} (position {}/{})",
                maker_timelock,
                suitable_maker.address,
                chain_position,
                self.ongoing_swap_state.swap_params.maker_count - 1
            );

            let swap_details = SwapDetails {
                id: self.ongoing_swap_state.id.clone(),
                amount: self.ongoing_swap_state.swap_params.send_amount,
                no_of_tx: self.ongoing_swap_state.swap_params.tx_count as u8,
                timelock: maker_timelock,
            };

            let msg = TakerToMakerMessage::SwapDetails(swap_details);
            let response = self.send_to_maker_and_get_response(&suitable_maker.address, msg)?;
            match response {
                MakerToTakerMessage::AckResponse(AckResponse::Ack) => {
                    log::info!("Received AckResponse from maker: {:?}", suitable_maker);
                    self.ongoing_swap_state
                        .chosen_makers
                        .push(suitable_maker.clone());
                }
                MakerToTakerMessage::AckResponse(AckResponse::Nack) => {
                    log::warn!("Maker {:?} did not accept the swap request", suitable_maker);
                    continue;
                }
                _ => {
                    log::warn!("Received unexpected message from maker: {:?}", response);
                    continue;
                }
            }
            if self.ongoing_swap_state.chosen_makers.len() == swap_params.maker_count {
                break; // we have enough makers
            }
        }

        // If we don't get enough ACKs, return an error
        if self.ongoing_swap_state.chosen_makers.len() < swap_params.maker_count {
            log::error!(
                "Not enough makers accepted the swap request. Required {}, got {}",
                swap_params.maker_count,
                self.ongoing_swap_state.chosen_makers.len()
            );
            return Err(TakerError::NotEnoughMakersInOfferBook);
        }

        // Initialize storage for maker private keys received during handover
        let chosen_makers_count = self.ongoing_swap_state.chosen_makers.len();
        self.ongoing_swap_state.maker_outgoing_privkeys = vec![None; chosen_makers_count];

        Ok(())
    }

    /// Sync the offer book with directory servers
    pub fn sync_offerbook(&mut self) -> Result<(), TakerError> {
        self.watch_service.request_maker_address();
        let watcher_event = self.watch_service.wait_for_event();

        let Some(WatcherEvent::MakerAddresses { maker_addresses }) = watcher_event else {
            return Err(TakerError::General("No response from watcher".to_string()));
        };

        let maker_addresses = maker_addresses
            .into_iter()
            .map(MakerAddress::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                TakerError::General(format!("Failed to parse maker addresses: {:?}", e))
            })?;

        // Find out addresses that was last updated 30 mins ago.
        let fresh_addrs = {
            let book = self
                .offerbook
                .lock()
                .map_err(|e| TakerError::General(format!("Failed to lock offerbook: {}", e)))?;
            book.get_fresh_addrs()
                .iter()
                .map(|oa| oa.address.clone())
                .collect::<HashSet<_>>()
        };

        // Fetch only those addresses which are new, or last updated more than 30 mins ago
        let addrs_to_fetch = maker_addresses
            .iter()
            .filter(|tracker_addr| !fresh_addrs.contains(tracker_addr))
            .cloned()
            .collect::<Vec<_>>();

        let new_offers = fetch_taproot_offers(&addrs_to_fetch, &self.config)?;

        for new_offer in new_offers {
            log::info!(
                "Found offer from {}. Verifying Fidelity Proof",
                new_offer.address
            );
            if let Err(e) = self
                .wallet
                .verify_fidelity_proof(&new_offer.offer.fidelity, &new_offer.address.to_string())
            {
                log::warn!(
                    "Fidelity Proof Verification failed with error: {:?}. Adding this to bad maker list: {}",
                    e,
                    new_offer.address
                );
                let mut book = self
                    .offerbook
                    .lock()
                    .map_err(|e| TakerError::General(format!("Failed to lock offerbook: {}", e)))?;
                book.add_bad_maker(&new_offer);
            } else {
                log::info!("Fidelity Bond verification success. Adding offer to our OfferBook");
                let mut book = self
                    .offerbook
                    .lock()
                    .map_err(|e| TakerError::General(format!("Failed to lock offerbook: {}", e)))?;
                book.add_new_offer(&new_offer);
            }
        }

        // Save the updated cache back to disk.
        {
            let book = self
                .offerbook
                .lock()
                .map_err(|e| TakerError::General(format!("Failed to lock offerbook: {}", e)))?;
            book.write_to_disk(&self.data_dir.join("offerbook.json"))?;
        }

        // Start background updater if not already running
        if self.offerbook_updater.is_none() {
            log::info!("Starting background offer book updater");
            let updater = OfferBookUpdater::new(
                Arc::clone(&self.offerbook),
                maker_addresses,
                self.config.clone(),
            );
            updater.start_background_updates();
            self.offerbook_updater = Some(OfferBookUpdater::new(
                Arc::clone(&self.offerbook),
                Vec::new(),
                self.config.clone(),
            ));
        }

        Ok(())
    }

    /// Fetch offers from available makers
    /// This syncs the offerbook first and then returns a cloned copy of it
    pub fn fetch_offers(&mut self) -> Result<OfferBook, TakerError> {
        self.sync_offerbook()?;
        let book = self
            .offerbook
            .lock()
            .map_err(|e| TakerError::General(format!("Failed to lock offerbook: {}", e)))?;
        Ok((*book).clone())
    }

    /// Send a message to a maker and get response
    fn send_to_maker_and_get_response(
        &self,
        maker_addr: &MakerAddress,
        msg: TakerToMakerMessage,
    ) -> Result<MakerToTakerMessage, TakerError> {
        let address = maker_addr.to_string();
        let mut socket = connect_to_maker(&address, &self.config, TCP_TIMEOUT_SECONDS)?;

        send_message(&mut socket, &msg)?;
        log::info!("===> {msg} | {maker_addr}");

        // Read response
        let response_bytes = read_message(&mut socket)?;
        let response: MakerToTakerMessage = serde_cbor::from_slice(&response_bytes)?;
        log::info!("<=== {} | {maker_addr}", response);

        Ok(response)
    }

    /// Find suitable makers for the given swap parameters
    pub fn find_suitable_makers(&self, swap_params: &SwapParams) -> Vec<OfferAndAddress> {
        let swap_amount = swap_params.send_amount;
        let max_refund_locktime = REFUND_LOCKTIME * (swap_params.maker_count + 1) as u16;

        log::debug!(
            "Finding suitable makers for amount: {} sats, max_refund_locktime: {}",
            swap_amount.to_sat(),
            max_refund_locktime
        );

        let book = match self.offerbook.lock() {
            Ok(book) => book,
            Err(e) => {
                log::error!("Failed to lock offerbook: {}", e);
                return Vec::new();
            }
        };

        let suitable_makers: Vec<OfferAndAddress> = book
            .all_good_makers()
            .into_iter()
            .filter(|oa| {
                let maker_fee = calculate_coinswap_fee(
                    swap_amount.to_sat(),
                    max_refund_locktime,
                    oa.offer.base_fee,
                    oa.offer.amount_relative_fee_pct,
                    oa.offer.time_relative_fee_pct,
                );
                let min_size_with_fee = bitcoin::Amount::from_sat(
                    oa.offer.min_size + maker_fee + 500, /* Estimated mining fee */
                );
                let is_suitable = swap_amount >= min_size_with_fee
                    && swap_amount <= bitcoin::Amount::from_sat(oa.offer.max_size);

                log::debug!("Evaluating maker {}: min_size={}, max_size={}, maker_fee={}, min_size_with_fee={}, swap_amount={}, suitable={}",
                           oa.address, oa.offer.min_size, oa.offer.max_size, maker_fee, min_size_with_fee.to_sat(), swap_amount.to_sat(), is_suitable);

                is_suitable
            })
            .cloned()
            .collect();

        log::info!(
            "Found {} suitable makers out of {} total makers",
            suitable_makers.len(),
            book.all_good_makers().len()
        );

        suitable_makers
    }

    /// Setup contract keys and scripts for the swap
    fn setup_contract_keys_and_scripts(&mut self) -> Result<(), TakerError> {
        let secp = Secp256k1::new();

        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);
        self.ongoing_swap_state.active_preimage = preimage;

        let (outgoing_contract_my_privkey, _) = self.wallet.get_tweakable_keypair()?;
        let outgoing_contract_my_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &outgoing_contract_my_privkey);
        let (outgoing_contract_my_x_only, _) = outgoing_contract_my_keypair.x_only_public_key();
        self.ongoing_swap_state.outgoing_contract.my_privkey = Some(outgoing_contract_my_privkey);
        self.ongoing_swap_state.outgoing_contract.my_pubkey = Some(bitcoin::PublicKey::from(
            outgoing_contract_my_keypair.public_key(),
        ));
        // Store preimage on outgoing contract for recovery
        self.ongoing_swap_state.outgoing_contract.hash_preimage = Some(preimage);

        let (incoming_contract_my_privkey, _) = self.wallet.get_tweakable_keypair()?;
        let incoming_contract_my_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &incoming_contract_my_privkey);
        self.ongoing_swap_state.incoming_contract.my_privkey = Some(incoming_contract_my_privkey);
        self.ongoing_swap_state.incoming_contract.my_pubkey = Some(bitcoin::PublicKey::from(
            incoming_contract_my_keypair.public_key(),
        ));
        // Store preimage on incoming contract for recovery
        self.ongoing_swap_state.incoming_contract.hash_preimage = Some(preimage);

        // For outgoing contract scripts:
        // - Hashlock: Created with MAKER's pubkey (so maker can spend it with preimage)
        // - Timelock: Created with TAKER's pubkey (so taker can recover after timelock)

        let first_maker = self
            .ongoing_swap_state
            .chosen_makers
            .first()
            .ok_or(TakerError::General("No makers chosen".to_string()))?;
        let maker_pubkey = first_maker.offer.tweakable_point.inner;
        let maker_x_only = bitcoin::key::XOnlyPublicKey::from(maker_pubkey);

        // Create scripts for outgoing contract
        let hash = sha256::Hash::hash(&preimage);
        let hashlock_script = create_hashlock_script(&hash.to_byte_array(), &maker_x_only);
        self.ongoing_swap_state.outgoing_contract.hashlock_script = hashlock_script.clone();

        let maker_count = self.ongoing_swap_state.swap_params.maker_count;
        let taker_timelock = REFUND_LOCKTIME + REFUND_LOCKTIME_STEP * maker_count as u16;
        let timelock =
            LockTime::from_height(taker_timelock as u32).map_err(WalletError::Locktime)?;
        let timelock_script = create_timelock_script(timelock, &outgoing_contract_my_x_only);
        self.ongoing_swap_state.outgoing_contract.timelock_script = timelock_script.clone();

        Ok(())
    }

    /// Create and broadcast contract transactions
    fn create_outgoing_contract_transactions(&mut self) -> Result<Vec<Transaction>, TakerError> {
        use crate::utill::MIN_FEE_RATE;

        let mut contract_transactions = Vec::new();
        let first_maker = match self.ongoing_swap_state.chosen_makers.first() {
            Some(maker) => maker,
            None => {
                return Err(TakerError::General("No makers chosen for swap".to_string()));
            }
        };
        let send_amount = self.ongoing_swap_state.swap_params.send_amount;

        // Use coin_select to get UTXOs that sum to the required amount
        let selected_utxos = self
            .wallet
            .coin_select(send_amount, MIN_FEE_RATE, None)
            .map_err(|e| TakerError::General(format!("Coin selection failed: {:?}", e)))?;

        let hashlock_script = self
            .ongoing_swap_state
            .outgoing_contract
            .hashlock_script()
            .clone();
        let timelock_script = self
            .ongoing_swap_state
            .outgoing_contract
            .timelock_script()
            .clone();

        let first_maker_pubkey = first_maker.offer.tweakable_point.inner;
        let outgoing_contract_my_pubkey = self.ongoing_swap_state.outgoing_contract.pubkey()?;
        let outgoing_contract_internal_key =
            crate::protocol::musig_interface::get_aggregated_pubkey_compat(
                outgoing_contract_my_pubkey.inner,
                first_maker_pubkey,
            )?;

        // Create taproot script (P2TR output)
        let (outgoing_contract_taproot_script, outgoing_contract_taproot_spendinfo) =
            create_taproot_script(
                hashlock_script,
                timelock_script,
                outgoing_contract_internal_key,
            )?;
        self.ongoing_swap_state.outgoing_contract.internal_key =
            Some(outgoing_contract_internal_key);
        self.ongoing_swap_state.outgoing_contract.tap_tweak =
            Some(outgoing_contract_taproot_spendinfo.tap_tweak().to_scalar());

        let outgoing_contract_taproot_address = bitcoin::Address::from_script(
            &outgoing_contract_taproot_script,
            self.wallet.store.network,
        )
        .map_err(|e| TakerError::General(format!("Failed to create taproot address: {:?}", e)))?;

        let signed_outgoing_contract_tx = {
            use crate::wallet::Destination;

            // Use Destination::Multi to send exact amount and get change back
            self.wallet.spend_from_wallet(
                MIN_FEE_RATE,
                Destination::Multi {
                    outputs: vec![(
                        outgoing_contract_taproot_address,
                        self.ongoing_swap_state.swap_params.send_amount,
                    )],
                    op_return_data: None,
                },
                &selected_utxos,
            )?
        };

        // Store the contract transaction and funding amount in the outgoing swapcoin
        self.ongoing_swap_state.outgoing_contract.contract_tx = signed_outgoing_contract_tx.clone();
        self.ongoing_swap_state.outgoing_contract.funding_amount =
            self.ongoing_swap_state.swap_params.send_amount;

        contract_transactions.push(signed_outgoing_contract_tx);

        Ok(contract_transactions)
    }

    /// Negotiate with makers and coordinate the sweep process
    fn negotiate_with_makers_and_coordinate_sweep(
        &mut self,
        outgoing_signed_contract_transactions: &[Transaction],
    ) -> Result<(), TakerError> {
        let first_maker = self
            .ongoing_swap_state
            .chosen_makers
            .first()
            .ok_or_else(|| TakerError::General("No first maker found".to_string()))?;

        let next_party_tweakable_point =
            if let Some(second_maker) = self.ongoing_swap_state.chosen_makers.get(1) {
                second_maker.offer.tweakable_point
            } else {
                self.ongoing_swap_state.incoming_contract.pubkey()?
            };

        let senders_contract = SendersContract {
            id: self.ongoing_swap_state.id.clone(),
            contract_txs: vec![outgoing_signed_contract_transactions[0].compute_txid()],
            pubkeys_a: vec![self.ongoing_swap_state.outgoing_contract.pubkey()?],
            hashlock_scripts: vec![self
                .ongoing_swap_state
                .outgoing_contract
                .hashlock_script()
                .clone()], // Send actual scripts for taproot spending
            timelock_scripts: vec![self
                .ongoing_swap_state
                .outgoing_contract
                .timelock_script()
                .clone()], // Send actual scripts for taproot spending
            next_party_tweakable_point,
            // Include the internal key and tap tweak for THIS specific contract (taker + first maker)
            internal_key: Some(self.ongoing_swap_state.outgoing_contract.internal_key()?),
            tap_tweak: Some((self.ongoing_swap_state.outgoing_contract.tap_tweak()?).into()),
        };

        let msg = TakerToMakerMessage::SendersContract(senders_contract.clone());
        let response = self.send_to_maker_and_get_response(&first_maker.address, msg)?;

        match response {
            MakerToTakerMessage::SenderContractFromMaker(incoming_contract) => {
                self.forward_contracts_and_coordinate_sweep(incoming_contract)?;
            }
            _ => {
                return Err(TakerError::General(
                    "Unexpected response from first maker".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Forward contracts through makers and coordinate the sweep process
    fn forward_contracts_and_coordinate_sweep(
        &mut self,
        mut current_contract: SenderContractFromMaker,
    ) -> Result<(), TakerError> {
        let maker_count = self.ongoing_swap_state.chosen_makers.len();

        // Handle single maker case
        if maker_count == 1 {
            // For single maker, the first maker's response is the final contract
            self.store_final_contract_data(&current_contract)?;
            self.execute_taker_sweep_and_coordinate_makers()?;
            return Ok(());
        }

        // Forward contract through all intermediate makers (from index 1 to maker_count-1)
        for maker_index in 1..maker_count {
            let maker = &self
                .ongoing_swap_state
                .chosen_makers
                .get(maker_index)
                .ok_or_else(|| {
                    TakerError::General(format!("No maker found at index {}", maker_index))
                })?;

            // Determine the next party in the chain
            let next_party_tweakable_point = if maker_index == maker_count - 1 {
                // Last maker should point back to taker
                self.ongoing_swap_state.incoming_contract.pubkey()?
            } else {
                // Intermediate maker should point to next maker
                self.ongoing_swap_state
                    .chosen_makers
                    .get(maker_index + 1)
                    .ok_or_else(|| {
                        TakerError::General(format!(
                            "No maker found at next index {}",
                            maker_index + 1
                        ))
                    })?
                    .offer
                    .tweakable_point
            };

            let forward_contract = SendersContract {
                id: self.ongoing_swap_state.id.clone(),
                contract_txs: current_contract.contract_txs.clone(),
                pubkeys_a: current_contract.pubkeys_a.clone(),
                hashlock_scripts: current_contract.hashlock_scripts.clone(),
                timelock_scripts: current_contract.timelock_scripts.clone(),
                next_party_tweakable_point,
                internal_key: current_contract.internal_key,
                tap_tweak: current_contract.tap_tweak,
            };

            let forward_msg = TakerToMakerMessage::SendersContract(forward_contract);
            let maker_response =
                self.send_to_maker_and_get_response(&maker.address, forward_msg)?;

            match maker_response {
                MakerToTakerMessage::SenderContractFromMaker(maker_contract) => {
                    if maker_index == maker_count - 1 {
                        // This is the last maker - store its response as final contract data
                        self.store_final_contract_data(&maker_contract)?;
                        self.execute_taker_sweep_and_coordinate_makers()?;
                        break;
                    } else {
                        // This is an intermediate maker - use its response for next iteration
                        current_contract = maker_contract;
                    }
                }
                _ => {
                    return Err(TakerError::General(format!(
                        "Unexpected response from maker {}",
                        maker_index
                    )));
                }
            }
        }

        Ok(())
    }

    /// Store final contract data from last maker
    fn store_final_contract_data(
        &mut self,
        final_contract: &crate::protocol::messages2::SenderContractFromMaker,
    ) -> Result<(), TakerError> {
        if let Some(incoming_contract_txid) = final_contract.contract_txs.first() {
            self.ongoing_swap_state.incoming_contract.contract_txid = Some(*incoming_contract_txid);
        }

        if let Some(incoming_contract_internal_key) = final_contract.internal_key {
            self.ongoing_swap_state.incoming_contract.internal_key =
                Some(incoming_contract_internal_key);
        }

        if let Some(incoming_contract_tap_tweak) = &final_contract.tap_tweak {
            let tap_tweak_scalar: bitcoin::secp256k1::Scalar =
                incoming_contract_tap_tweak.clone().into();
            self.ongoing_swap_state.incoming_contract.tap_tweak = Some(tap_tweak_scalar);
        }

        if let Some(incoming_contract_hashlock_script) = final_contract.hashlock_scripts.first() {
            self.ongoing_swap_state.incoming_contract.hashlock_script =
                incoming_contract_hashlock_script.clone();
        }

        if let Some(incoming_contract_timelock_script) = final_contract.timelock_scripts.first() {
            self.ongoing_swap_state.incoming_contract.timelock_script =
                incoming_contract_timelock_script.clone();
        }

        if let Some(incoming_contract_other_pubkey) = final_contract.pubkeys_a.first() {
            self.ongoing_swap_state.incoming_contract.other_pubkey =
                Some(*incoming_contract_other_pubkey);
        }

        // Register incoming contract with watcher for monitoring
        if let Some(incoming_contract_txid) = final_contract.contract_txs.first() {
            let incoming_outpoint = bitcoin::OutPoint {
                txid: *incoming_contract_txid,
                vout: 0,
            };
            self.watch_service.register_watch_request(incoming_outpoint);
            log::info!(
                "Registered watcher for taker's incoming contract: {}",
                incoming_outpoint
            );

            // Fetch and store the incoming contract transaction
            let incoming_contract_tx = self
                .wallet
                .rpc
                .get_raw_transaction(incoming_contract_txid, None)
                .map_err(|e| {
                    TakerError::General(format!("Failed to get incoming contract tx: {:?}", e))
                })?;
            self.ongoing_swap_state.incoming_contract.contract_tx = incoming_contract_tx.clone();
            // Set funding amount from the transaction output
            self.ongoing_swap_state.incoming_contract.funding_amount =
                incoming_contract_tx.output[0].value;

            // Persist incoming swapcoin to wallet store for recovery
            self.wallet
                .add_incoming_swapcoin_v2(&self.ongoing_swap_state.incoming_contract);
            self.wallet.save_to_disk()?;
            log::info!("Persisted incoming swapcoin to wallet store and saved to disk");
        }

        Ok(())
    }

    /// Execute private key handover protocol with all makers
    ///
    /// Forward flow: Each party sends their OUTGOING contract private key
    /// 1. Taker → Maker0: Taker's outgoing key
    /// 2. Maker0 sweeps, responds with Maker0's outgoing key
    /// 3. Taker → Maker1: Maker0's outgoing key (relay)
    /// 4. Maker1 sweeps, responds with Maker1's outgoing key
    /// 5. Taker sweeps using Maker1's outgoing key
    fn execute_taker_sweep_and_coordinate_makers(&mut self) -> Result<(), TakerError> {
        let maker_count = self.ongoing_swap_state.chosen_makers.len();

        log::info!(
            "Starting forward-flow private key handover with {} makers",
            maker_count
        );

        // Forward flow: distribute outgoing keys to each maker in order
        for maker_index in 0..maker_count {
            let maker_address = self.ongoing_swap_state.chosen_makers[maker_index]
                .address
                .clone();

            log::info!(
                "  [Maker {}] Sending private key to {}",
                maker_index,
                maker_address
            );

            // Determine which outgoing key to send
            let outgoing_privkey = if maker_index == 0 {
                // To first maker: send taker's outgoing contract key
                self.ongoing_swap_state.outgoing_contract.privkey()?
            } else {
                // To subsequent makers: relay previous maker's outgoing key
                self.ongoing_swap_state.maker_outgoing_privkeys[maker_index - 1].ok_or_else(
                    || {
                        TakerError::General(format!(
                            "Previous maker {} outgoing key not received yet",
                            maker_index - 1
                        ))
                    },
                )?
            };

            // Create private key handover message
            let privkey_msg = TakerToMakerMessage::PrivateKeyHandover(PrivateKeyHandover {
                id: Some(self.ongoing_swap_state.id.clone()),
                secret_key: outgoing_privkey,
            });

            // Send to maker and get their outgoing key in response
            let response = self.send_to_maker_and_get_response(&maker_address, privkey_msg)?;

            // remove taker's outgoing swapcoin since we've handed over the key
            if maker_index == 0 {
                let outgoing_txid = self
                    .ongoing_swap_state
                    .outgoing_contract
                    .contract_tx
                    .compute_txid();
                self.wallet.remove_outgoing_swapcoin_v2(&outgoing_txid);
                log::info!(
                    "Removed taker's outgoing swapcoin {} after sending PrivateKeyHandover",
                    outgoing_txid
                );
                self.wallet.save_to_disk()?;
            }

            // Extract maker's outgoing key from response
            match response {
                MakerToTakerMessage::PrivateKeyHandover(maker_privkey_handover) => {
                    let maker_outgoing_privkey = maker_privkey_handover.secret_key;

                    log::info!("  [Maker {}] Received outgoing private key", maker_index);

                    // Store maker's outgoing key
                    self.ongoing_swap_state.maker_outgoing_privkeys[maker_index] =
                        Some(maker_outgoing_privkey);
                }
                _ => {
                    return Err(TakerError::General(format!(
                        "Unexpected response from maker {}: expected PrivateKeyHandover",
                        maker_index
                    )));
                }
            }
        }

        log::info!("All makers have responded with their outgoing keys");

        // Finally, taker sweeps their incoming contract using the last maker's outgoing key
        let last_maker_outgoing_key = self.ongoing_swap_state.maker_outgoing_privkeys
            [maker_count - 1]
            .ok_or_else(|| TakerError::General("Last maker outgoing key not found".to_string()))?;

        self.sweep_incoming_contract_with_maker_key(last_maker_outgoing_key)?;

        log::info!("Taker sweep completed successfully");

        Ok(())
    }

    /// Sweep taker's incoming contract using the last maker's outgoing private key
    ///
    /// The taker's incoming contract is a 2-of-2 between:
    /// - Taker's incoming key (which taker has)
    /// - Last maker's outgoing key (received via handover)
    ///
    /// Uses proper MuSig2 protocol with nonce pairs and partial signatures
    fn sweep_incoming_contract_with_maker_key(
        &mut self,
        maker_outgoing_privkey: bitcoin::secp256k1::SecretKey,
    ) -> Result<(), TakerError> {
        let secp = Secp256k1::new();

        log::info!("Sweeping taker's incoming contract using maker's outgoing key");

        // Get taker's incoming contract details
        let incoming_contract_txid = self.ongoing_swap_state.incoming_contract.contract_txid()?;

        let incoming_contract_my_privkey = self.ongoing_swap_state.incoming_contract.privkey()?;

        // Fetch the incoming contract transaction to get amount and verify it exists
        let incoming_contract_tx = self
            .wallet
            .rpc
            .get_raw_transaction(&incoming_contract_txid, None)
            .map_err(|e| TakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;

        let incoming_amount = incoming_contract_tx
            .output
            .first()
            .ok_or_else(|| TakerError::General("Incoming contract has no outputs".to_string()))?
            .value;

        log::info!(
            "  Incoming contract: txid={}, amount={}",
            incoming_contract_txid,
            incoming_amount
        );

        // Create sweep transaction
        let fee = Amount::from_sat(1000);
        let output_amount = incoming_amount
            .checked_sub(fee)
            .ok_or_else(|| TakerError::General("Insufficient amount for fee".to_string()))?;

        let destination_address = self
            .wallet
            .get_next_internal_addresses(1)
            .map_err(TakerError::Wallet)?
            .into_iter()
            .next()
            .ok_or_else(|| TakerError::General("Failed to get destination address".to_string()))?;

        let sweep_tx = Transaction {
            version: Version::TWO,
            lock_time: bitcoin::locktime::absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: incoming_contract_txid,
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ZERO,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: output_amount,
                script_pubkey: destination_address.script_pubkey(),
            }],
        };

        log::info!(
            "  Created sweep transaction, output amount: {}",
            output_amount
        );

        // Create keypairs from both private keys
        let taker_keypair = Keypair::from_secret_key(&secp, &incoming_contract_my_privkey);
        let maker_keypair = Keypair::from_secret_key(&secp, &maker_outgoing_privkey);

        // Get the internal key and tweak from connection state
        let internal_key = self.ongoing_swap_state.incoming_contract.internal_key()?;

        let tap_tweak = self.ongoing_swap_state.incoming_contract.tap_tweak()?;

        // Get contract scripts for sighash calculation
        let hashlock_script = self.ongoing_swap_state.incoming_contract.hashlock_script();
        let timelock_script = self.ongoing_swap_state.incoming_contract.timelock_script();

        // Calculate sighash using the helper function
        let message = crate::protocol::contract2::calculate_contract_sighash(
            &sweep_tx,
            incoming_amount,
            hashlock_script,
            timelock_script,
            internal_key,
        )
        .map_err(|e| TakerError::General(format!("Failed to calculate sighash: {:?}", e)))?;

        // Order pubkeys lexicographically
        let mut ordered_pubkeys = [taker_keypair.public_key(), maker_keypair.public_key()];
        ordered_pubkeys.sort_by_key(|a| a.serialize());

        // Generate nonce pairs for both parties
        let (taker_sec_nonce, taker_pub_nonce) =
            generate_new_nonce_pair_compat(taker_keypair.public_key())?;
        let (maker_sec_nonce, maker_pub_nonce) =
            generate_new_nonce_pair_compat(maker_keypair.public_key())?;

        // Aggregate nonces in the correct order
        let nonce_refs = if ordered_pubkeys[0] == taker_keypair.public_key() {
            vec![&taker_pub_nonce, &maker_pub_nonce]
        } else {
            vec![&maker_pub_nonce, &taker_pub_nonce]
        };
        let aggregated_nonce = get_aggregated_nonce_compat(&nonce_refs);

        // Generate partial signatures from both parties
        let taker_partial_sig = generate_partial_signature_compat(
            message,
            &aggregated_nonce,
            taker_sec_nonce,
            taker_keypair,
            tap_tweak,
            ordered_pubkeys[0],
            ordered_pubkeys[1],
        )?;

        let maker_partial_sig = generate_partial_signature_compat(
            message,
            &aggregated_nonce,
            maker_sec_nonce,
            maker_keypair,
            tap_tweak,
            ordered_pubkeys[0],
            ordered_pubkeys[1],
        )?;

        // Aggregate partial signatures in the correct order
        let partial_sigs = if ordered_pubkeys[0] == taker_keypair.public_key() {
            vec![&taker_partial_sig, &maker_partial_sig]
        } else {
            vec![&maker_partial_sig, &taker_partial_sig]
        };

        let aggregated_sig = aggregate_partial_signatures_compat(
            message,
            aggregated_nonce,
            tap_tweak,
            partial_sigs,
            ordered_pubkeys[0],
            ordered_pubkeys[1],
        )?;

        let final_signature =
            bitcoin::taproot::Signature::from_slice(aggregated_sig.assume_valid().as_byte_array())
                .map_err(ProtocolError::TaprootSigSlice)?;

        log::info!("  Created MuSig2 aggregated signature for key-path spend");

        // Set the witness
        let mut final_tx = sweep_tx;
        let mut sighasher = SighashCache::new(&mut final_tx);
        *sighasher
            .witness_mut(0)
            .ok_or(ProtocolError::General("Failed to get witness"))? =
            Witness::p2tr_key_spend(&final_signature);
        let completed_tx = sighasher.into_transaction();

        // Broadcast the transaction
        let txid = self
            .wallet
            .rpc
            .send_raw_transaction(completed_tx.raw_hex())
            .map_err(|e| TakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;

        log::info!("  Broadcast taker sweep transaction: {}", txid);

        // Record the swept coin to track swap balance
        let output_scriptpubkey = destination_address.script_pubkey();
        self.wallet
            .store
            .swept_incoming_swapcoins
            .insert(output_scriptpubkey.clone(), output_scriptpubkey);
        log::info!(
            "Recorded swept incoming swapcoin V2: {}",
            incoming_contract_txid
        );

        // Remove the incoming swapcoin since we've successfully swept it
        self.wallet
            .remove_incoming_swapcoin_v2(&incoming_contract_txid);
        log::info!(
            "Removed taker's incoming swapcoin {} after successful sweep",
            incoming_contract_txid
        );
        self.wallet.save_to_disk()?;

        Ok(())
    }

    /// Recover from a failed taproot swap by attempting to spend unfinished contracts
    ///
    /// Recovery strategy:
    /// 1. For incoming contracts: Try hashlock spend using stored preimage, fallback to timelock
    /// 2. For outgoing contracts: Wait for timelock maturity, then spend via timelock
    /// 3. Skip contracts that were already spent (via keypath, hashlock, or timelock)
    pub fn recover_from_swap(&mut self) -> Result<(), TakerError> {
        log::info!("Starting taproot swap recovery");

        // Find unfinished swapcoins (contracts that weren't cooperatively swept)
        let (incoming_swapcoins, outgoing_swapcoins) = self.wallet.find_unfinished_swapcoins_v2();

        if incoming_swapcoins.is_empty() && outgoing_swapcoins.is_empty() {
            log::info!("No unfinished swapcoins found - nothing to recover");
            return Ok(());
        }

        log::info!(
            "Found {} incoming and {} outgoing unfinished swapcoins",
            incoming_swapcoins.len(),
            outgoing_swapcoins.len()
        );

        // Try to recover incoming swapcoins first (we're the receiver, have hashlock privilege)
        if !incoming_swapcoins.is_empty() {
            log::info!(
                "Attempting to recover {} incoming swapcoins",
                incoming_swapcoins.len()
            );

            for incoming in &incoming_swapcoins {
                let contract_txid = incoming.contract_tx.compute_txid();
                log::info!(
                    "Checking recovery options for incoming contract: {}",
                    contract_txid
                );

                // Check if contract has been spent already
                let outpoint = bitcoin::OutPoint {
                    txid: contract_txid,
                    vout: 0,
                };
                self.watch_service.watch_request(outpoint);

                let spending_result = if let Some(event) = self.watch_service.wait_for_event() {
                    match event {
                        WatcherEvent::UtxoSpent { spending_tx, .. } => spending_tx,
                        _ => None,
                    }
                } else {
                    None
                };

                match spending_result {
                    Some(spending_tx) => {
                        log::info!(
                            "Contract {} already spent, analyzing spending path",
                            contract_txid
                        );

                        // Detect how it was spent by analyzing witness
                        match crate::protocol::contract2::detect_taproot_spending_path(
                            &spending_tx,
                            outpoint,
                        )? {
                            TaprootSpendingPath::KeyPath => {
                                log::info!(
                                    "Contract spent via cooperative key-path - no recovery needed"
                                );
                                continue;
                            }
                            TaprootSpendingPath::Hashlock { preimage: _ } => {
                                log::info!("Contract spent via hashlock - preimage available");
                                continue;
                            }
                            TaprootSpendingPath::Timelock => {
                                log::warn!("Contract spent via timelock by counterparty - this indicates failure");
                                continue;
                            }
                        }
                    }
                    None => {
                        // Contract not spent yet - we can try to spend it
                        log::info!(
                            "Contract {} not spent yet - attempting hashlock recovery",
                            contract_txid
                        );

                        // Try hashlock first (we're the receiver)
                        if let Some(preimage) = &incoming.hash_preimage {
                            match self.wallet.spend_via_hashlock_v2(
                                incoming,
                                preimage,
                                &self.watch_service,
                            ) {
                                Ok(txid) => {
                                    log::info!(
                                        "Successfully recovered incoming contract via hashlock: {}",
                                        txid
                                    );
                                }
                                Err(e) => {
                                    log::warn!("Failed to spend via hashlock: {:?}, will retry with timelock after maturity", e);
                                }
                            }
                        } else {
                            log::warn!("No preimage available for incoming contract - waiting for timelock");
                            // Will try timelock below after checking maturity
                        }
                    }
                }
            }
        }

        // Recover outgoing swapcoins (we're the sender, must wait for timelock)
        if !outgoing_swapcoins.is_empty() {
            log::info!(
                "Attempting to recover {} outgoing swapcoins",
                outgoing_swapcoins.len()
            );

            for outgoing in &outgoing_swapcoins {
                let contract_txid = outgoing.contract_tx.compute_txid();
                log::info!(
                    "Checking recovery options for outgoing contract: {}",
                    contract_txid
                );

                // Check if contract has been spent
                let outpoint = bitcoin::OutPoint {
                    txid: contract_txid,
                    vout: 0,
                };
                self.watch_service.watch_request(outpoint);

                let spending_result = if let Some(event) = self.watch_service.wait_for_event() {
                    match event {
                        WatcherEvent::UtxoSpent { spending_tx, .. } => spending_tx,
                        _ => None,
                    }
                } else {
                    None
                };

                match spending_result {
                    Some(spending_tx) => {
                        log::info!("Outgoing contract {} already spent", contract_txid);

                        match crate::protocol::contract2::detect_taproot_spending_path(
                            &spending_tx,
                            outpoint,
                        )? {
                            TaprootSpendingPath::KeyPath => {
                                log::info!("Contract spent cooperatively - no recovery needed");
                                continue;
                            }
                            TaprootSpendingPath::Hashlock { .. } => {
                                log::info!("Contract spent by receiver via hashlock - swap partially completed");
                                continue;
                            }
                            TaprootSpendingPath::Timelock => {
                                log::warn!("We already recovered this via timelock");
                                continue;
                            }
                        }
                    }
                    None => {
                        // Contract not spent - check if timelock matured
                        log::info!(
                            "Outgoing contract {} not spent - checking timelock maturity",
                            contract_txid
                        );

                        if let Some(timelock) = outgoing.get_timelock() {
                            if crate::protocol::contract2::is_timelock_mature(
                                &self.wallet.rpc,
                                &contract_txid,
                                timelock,
                            )? {
                                log::info!("Timelock matured, attempting recovery");

                                match self
                                    .wallet
                                    .spend_via_timelock_v2(outgoing, &self.watch_service)
                                {
                                    Ok(txid) => {
                                        log::info!("Successfully recovered outgoing contract via timelock: {}", txid);
                                    }
                                    Err(e) => {
                                        log::error!("Failed to spend via timelock: {:?}", e);
                                    }
                                }
                            } else {
                                log::info!(
                                    "Timelock not yet mature for contract {}",
                                    contract_txid
                                );
                            }
                        }
                    }
                }
            }
        }

        log::info!("Taproot swap recovery completed");
        Ok(())
    }
}

impl Role for Taker {
    const RUN_DISCOVERY: bool = true;
}
