use crate::{
    protocol::{
        contract2::{
            calculate_coinswap_fee, create_hashlock_script, create_taproot_script,
            create_timelock_script,
        },
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
        offers::{MakerAddress, OfferAndAddress, OfferBook},
    },
    utill::{check_tor_status, get_taker_dir, read_message, send_message},
    wallet::{RPCConfig, Wallet, WalletError},
    watch_tower::{
        registry_storage::FileRegistry,
        rpc_backend::BitcoinRpc,
        service::WatchService,
        watcher::{Watcher, WatcherEvent},
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
use bitcoind::bitcoincore_rpc::{RawTx, RpcApi};
use chrono::Utc;
use socks::Socks5Stream;
use std::{
    collections::HashSet, convert::TryFrom, io::BufWriter, net::TcpStream, path::PathBuf,
    sync::mpsc, thread, time::Duration,
};

use super::error::TakerError;

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
}

#[derive(Default)]
struct OngoingSwapState {
    pub swap_params: SwapParams,
    pub active_preimage: Preimage,
    pub id: String,
    pub suitable_makers: Vec<OfferAndAddress>,
    pub chosen_makers: Vec<OfferAndAddress>,
    pub outgoing_contract_my_privkey: Option<bitcoin::secp256k1::SecretKey>,
    pub outgoing_contract_my_pubkey: Option<bitcoin::PublicKey>,
    pub outgoing_contract_hashlock_script: Option<ScriptBuf>,
    pub outgoing_contract_timelock_script: Option<ScriptBuf>,
    pub outgoing_contract_internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>,
    pub outgoing_contract_tap_tweak: Option<bitcoin::secp256k1::Scalar>,
    pub incoming_contract_txid: Option<bitcoin::Txid>,
    pub incoming_contract_internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>,
    pub incoming_contract_tap_tweak: Option<bitcoin::secp256k1::Scalar>,
    pub incoming_contract_hashlock_script: Option<ScriptBuf>,
    pub incoming_contract_timelock_script: Option<ScriptBuf>,
    pub incoming_contract_my_privkey: Option<bitcoin::secp256k1::SecretKey>,
    pub incoming_contract_my_pubkey: Option<bitcoin::PublicKey>,
    pub incoming_contract_other_pubkey: Option<bitcoin::PublicKey>,
    // Private key handover: store maker outgoing contract private keys (indexed by maker position)
    // Each maker hands over their outgoing contract private key after sweeping their incoming contract
    pub maker_outgoing_privkeys: Vec<Option<bitcoin::secp256k1::SecretKey>>,
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
    let mut offers = Vec::new();

    for address in maker_addresses {
        match download_taproot_offer(address, config) {
            Some(offer) => offers.push(offer),
            None => {
                log::warn!("Failed to download offer from taproot maker: {}", address);
            }
        }
    }

    Ok(offers)
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
            .unwrap(),
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
    offerbook: OfferBook,
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
        let rpc_backend = BitcoinRpc::new(rpc_config.clone()).unwrap();
        let registry = FileRegistry::load(data_dir.join(".taker-watcher"));
        let (tx_requests, rx_requests) = mpsc::channel();
        let (tx_events, rx_responses) = mpsc::channel();

        let mut watcher = Watcher::new(backend, rpc_backend, registry, rx_requests, tx_events);
        _ = thread::Builder::new()
            .name("Watcher thread".to_string())
            .spawn(move || watcher.run());

        let watch_service = WatchService::new(tx_requests, rx_responses);

        let mut wallet = if wallet_path.exists() {
            let wallet = Wallet::load(&wallet_path, &rpc_config)?;
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

        let offerbook_path = data_dir.join("offerbook.dat");
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
            let file = std::fs::File::create(&offerbook_path)?;
            let writer = BufWriter::new(file);
            serde_cbor::to_writer(writer, &empty_book)?;
            empty_book
        };

        log::info!("Initializing wallet sync...");
        wallet.sync()?;
        log::info!("Completed wallet sync");

        Ok(Self {
            wallet,
            config,
            offerbook,
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
    pub fn do_coinswap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
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
        self.choose_makers_for_swap(swap_params)?;
        self.setup_contract_keys_and_scripts()?;

        let outgoing_signed_contract_transactions = self.create_outgoing_contract_transactions()?;
        self.wallet
            .send_tx(outgoing_signed_contract_transactions.first().unwrap())?;

        for tx in &outgoing_signed_contract_transactions {
            self.wallet.wait_for_tx_confirmation(tx.compute_txid())?;
        }

        self.negotiate_with_makers_and_coordinate_sweep(&outgoing_signed_contract_transactions)?;

        for tx in &outgoing_signed_contract_transactions {
            self.wallet.wait_for_tx_confirmation(tx.compute_txid())?;
        }

        Ok(())
    }

    /// Choose makers for the swap by negotiating with them
    fn choose_makers_for_swap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
        // Find suitable maker asks for an offer from the makers
        let suitable_makers = self.find_suitable_makers(&swap_params);
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
        for (maker_index, suitable_maker) in suitable_makers.iter().enumerate() {
            log::info!("Maker {}: {:?}", maker_index, suitable_maker.address);

            // Calculate staggered timelock for this maker
            // Taker has the longest, each maker gets progressively shorter
            // With 2 makers: Taker=60, Maker0=40, Maker1=20
            let maker_timelock = REFUND_LOCKTIME
                + REFUND_LOCKTIME_STEP
                    * (self.ongoing_swap_state.swap_params.maker_count - maker_index - 1) as u16;

            log::info!(
                "Assigning timelock {} blocks to maker {} (index {}/{})",
                maker_timelock,
                suitable_maker.address,
                maker_index,
                self.ongoing_swap_state.swap_params.maker_count - 1
            );

            let swap_details = SwapDetails {
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

        // Generate preimage for the swap
        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        let unique_id = preimage[0..8].to_hex_string(bitcoin::hex::Case::Lower);
        log::info!("Initiating coinswap with id : {}", unique_id);

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.id = unique_id;

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
            .unwrap();

        // Find out addresses that was last updated 30 mins ago.
        let fresh_addrs = self
            .offerbook
            .get_fresh_addrs()
            .iter()
            .map(|oa| &oa.address)
            .collect::<HashSet<_>>();

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
                self.offerbook.add_bad_maker(&new_offer);
            } else {
                log::info!("Fidelity Bond verification success. Adding offer to our OfferBook");
                self.offerbook.add_new_offer(&new_offer);
            }
        }

        // Save the updated cache back to disk.
        self.offerbook
            .write_to_disk(&self.data_dir.join("offerbook.dat"))?;

        Ok(())
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

        let suitable_makers: Vec<OfferAndAddress> = self
            .offerbook
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
            self.offerbook.all_good_makers().len()
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
        self.ongoing_swap_state.outgoing_contract_my_privkey = Some(outgoing_contract_my_privkey);
        self.ongoing_swap_state.outgoing_contract_my_pubkey = Some(bitcoin::PublicKey::from(
            outgoing_contract_my_keypair.public_key(),
        ));

        let (incoming_contract_my_privkey, _) = self.wallet.get_tweakable_keypair()?;
        let incoming_contract_my_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &incoming_contract_my_privkey);
        self.ongoing_swap_state.incoming_contract_my_privkey = Some(incoming_contract_my_privkey);
        self.ongoing_swap_state.incoming_contract_my_pubkey = Some(bitcoin::PublicKey::from(
            incoming_contract_my_keypair.public_key(),
        ));

        // Create scripts for outgoing contract
        let hash = sha256::Hash::hash(&preimage);
        let hashlock_script =
            create_hashlock_script(&hash.to_byte_array(), &outgoing_contract_my_x_only);
        self.ongoing_swap_state.outgoing_contract_hashlock_script = Some(hashlock_script.clone());

        // Taker gets the longest timelock (higher than all makers)
        let taker_timelock = REFUND_LOCKTIME
            + REFUND_LOCKTIME_STEP * self.ongoing_swap_state.swap_params.maker_count as u16;
        let timelock = LockTime::from_height(taker_timelock as u32).unwrap();
        let timelock_script = create_timelock_script(timelock, &outgoing_contract_my_x_only);
        self.ongoing_swap_state.outgoing_contract_timelock_script = Some(timelock_script.clone());

        Ok(())
    }

    /// Create and broadcast contract transactions
    fn create_outgoing_contract_transactions(&mut self) -> Result<Vec<Transaction>, TakerError> {
        let available_utxos = self.wallet.list_all_utxo_spend_info();
        let mut contract_transactions = Vec::new();

        if let Some(first_maker) = self.ongoing_swap_state.chosen_makers.first() {
            let funding_utxo = available_utxos
                .iter()
                .find(|(utxo, _)| utxo.amount >= self.ongoing_swap_state.swap_params.send_amount)
                .map(|(utxo, _)| utxo.clone())
                .ok_or_else(|| {
                    TakerError::General(
                        "No available UTXO found for contract transaction".to_string(),
                    )
                })?;

            let hashlock_script = self
                .ongoing_swap_state
                .outgoing_contract_hashlock_script
                .clone()
                .ok_or_else(|| TakerError::General("No hashlock script found".to_string()))?;
            let timelock_script = self
                .ongoing_swap_state
                .outgoing_contract_timelock_script
                .clone()
                .ok_or_else(|| TakerError::General("No timelock script found".to_string()))?;

            let first_maker_pubkey = first_maker.offer.tweakable_point.inner;
            let outgoing_contract_my_pubkey =
                self.ongoing_swap_state
                    .outgoing_contract_my_pubkey
                    .ok_or_else(|| TakerError::General("No taker pubkey found".to_string()))?;
            let outgoing_contract_internal_key =
                crate::protocol::musig_interface::get_aggregated_pubkey_compat(
                    outgoing_contract_my_pubkey.inner,
                    first_maker_pubkey,
                );

            // Create taproot script (P2TR output)
            let (outgoing_contract_taproot_script, outgoing_contract_taproot_spendinfo) =
                create_taproot_script(
                    hashlock_script,
                    timelock_script,
                    outgoing_contract_internal_key,
                );

            self.ongoing_swap_state.outgoing_contract_internal_key =
                Some(outgoing_contract_internal_key);
            self.ongoing_swap_state.outgoing_contract_tap_tweak =
                Some(outgoing_contract_taproot_spendinfo.tap_tweak().to_scalar());

            let outgoing_contract_taproot_address = bitcoin::Address::from_script(
                &outgoing_contract_taproot_script,
                bitcoin::Network::Regtest,
            )
            .map_err(|e| {
                TakerError::General(format!("Failed to create taproot address: {:?}", e))
            })?;

            let signed_outgoing_contract_tx = {
                use crate::{utill::MIN_FEE_RATE, wallet::Destination};

                let funding_utxo_info = self
                    .wallet
                    .get_utxo((funding_utxo.txid, funding_utxo.vout))?
                    .ok_or_else(|| TakerError::General("Funding UTXO not found".to_string()))?;

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
                    &[(funding_utxo.clone(), funding_utxo_info)],
                )?
            };

            contract_transactions.push(signed_outgoing_contract_tx);
        } else {
            return Err(TakerError::General("No makers chosen for swap".to_string()));
        }

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
                self.ongoing_swap_state.incoming_contract_my_pubkey.unwrap()
            };

        let senders_contract = SendersContract {
            contract_txs: vec![outgoing_signed_contract_transactions[0].compute_txid()],
            pubkeys_a: vec![self.ongoing_swap_state.outgoing_contract_my_pubkey.unwrap()],
            hashlock_scripts: vec![self
                .ongoing_swap_state
                .outgoing_contract_hashlock_script
                .clone()
                .unwrap()], // Send actual scripts for taproot spending
            timelock_scripts: vec![self
                .ongoing_swap_state
                .outgoing_contract_timelock_script
                .clone()
                .unwrap()], // Send actual scripts for taproot spending
            next_party_tweakable_point,
            // Include the internal key and tap tweak for THIS specific contract (taker + first maker)
            internal_key: Some(
                self.ongoing_swap_state
                    .outgoing_contract_internal_key
                    .unwrap(),
            ),
            tap_tweak: self
                .ongoing_swap_state
                .outgoing_contract_tap_tweak
                .map(|t| t.into()),
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
                .unwrap();

            // Determine the next party in the chain
            let next_party_tweakable_point = if maker_index == maker_count - 1 {
                // Last maker should point back to taker
                self.ongoing_swap_state.incoming_contract_my_pubkey.unwrap()
            } else {
                // Intermediate maker should point to next maker
                self.ongoing_swap_state
                    .chosen_makers
                    .get(maker_index + 1)
                    .unwrap()
                    .offer
                    .tweakable_point
            };

            let forward_contract = SendersContract {
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
            self.ongoing_swap_state.incoming_contract_txid = Some(*incoming_contract_txid);
        }

        if let Some(incoming_contract_internal_key) = final_contract.internal_key {
            self.ongoing_swap_state.incoming_contract_internal_key =
                Some(incoming_contract_internal_key);
        }

        if let Some(incoming_contract_tap_tweak) = &final_contract.tap_tweak {
            let tap_tweak_scalar: bitcoin::secp256k1::Scalar =
                incoming_contract_tap_tweak.clone().into();
            self.ongoing_swap_state.incoming_contract_tap_tweak = Some(tap_tweak_scalar);
        }

        if let Some(incoming_contract_hashlock_script) = final_contract.hashlock_scripts.first() {
            self.ongoing_swap_state.incoming_contract_hashlock_script =
                Some(incoming_contract_hashlock_script.clone());
        }

        if let Some(incoming_contract_timelock_script) = final_contract.timelock_scripts.first() {
            self.ongoing_swap_state.incoming_contract_timelock_script =
                Some(incoming_contract_timelock_script.clone());
        }

        if let Some(incoming_contract_other_pubkey) = final_contract.pubkeys_a.first() {
            self.ongoing_swap_state.incoming_contract_other_pubkey =
                Some(*incoming_contract_other_pubkey);
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
                self.ongoing_swap_state
                    .outgoing_contract_my_privkey
                    .ok_or_else(|| {
                        TakerError::General("Taker outgoing privkey not found".to_string())
                    })?
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
                secret_key: outgoing_privkey,
            });

            // Send to maker and get their outgoing key in response
            let response = self.send_to_maker_and_get_response(&maker_address, privkey_msg)?;

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
        let incoming_contract_txid = self
            .ongoing_swap_state
            .incoming_contract_txid
            .ok_or_else(|| TakerError::General("No incoming contract txid found".to_string()))?;

        let incoming_contract_my_privkey = self
            .ongoing_swap_state
            .incoming_contract_my_privkey
            .ok_or_else(|| TakerError::General("No taker incoming privkey found".to_string()))?;

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
        let internal_key = self
            .ongoing_swap_state
            .incoming_contract_internal_key
            .ok_or_else(|| {
                TakerError::General("No internal key found for incoming contract".to_string())
            })?;

        let tap_tweak = self
            .ongoing_swap_state
            .incoming_contract_tap_tweak
            .ok_or_else(|| {
                TakerError::General("No tap tweak found for incoming contract".to_string())
            })?;

        // Get contract scripts for sighash calculation
        let hashlock_script = self
            .ongoing_swap_state
            .incoming_contract_hashlock_script
            .as_ref()
            .ok_or_else(|| {
                TakerError::General("No hashlock script for incoming contract".to_string())
            })?;

        let timelock_script = self
            .ongoing_swap_state
            .incoming_contract_timelock_script
            .as_ref()
            .ok_or_else(|| {
                TakerError::General("No timelock script for incoming contract".to_string())
            })?;

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
            generate_new_nonce_pair_compat(taker_keypair.public_key());
        let (maker_sec_nonce, maker_pub_nonce) =
            generate_new_nonce_pair_compat(maker_keypair.public_key());

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
        );

        let maker_partial_sig = generate_partial_signature_compat(
            message,
            &aggregated_nonce,
            maker_sec_nonce,
            maker_keypair,
            tap_tweak,
            ordered_pubkeys[0],
            ordered_pubkeys[1],
        );

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
        );

        let final_signature =
            bitcoin::taproot::Signature::from_slice(aggregated_sig.assume_valid().as_byte_array())
                .unwrap();

        log::info!("  Created MuSig2 aggregated signature for key-path spend");

        // Set the witness
        let mut final_tx = sweep_tx;
        let mut sighasher = SighashCache::new(&mut final_tx);
        *sighasher.witness_mut(0).unwrap() = Witness::p2tr_key_spend(&final_signature);
        let completed_tx = sighasher.into_transaction();

        // Broadcast the transaction
        let txid = self
            .wallet
            .rpc
            .send_raw_transaction(completed_tx.raw_hex())
            .map_err(|e| TakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;

        log::info!("  Broadcast taker sweep transaction: {}", txid);

        Ok(())
    }
}
