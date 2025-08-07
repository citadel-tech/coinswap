use crate::protocol::messages2::{AckResponse, Preimage};
use crate::taker::config::TakerConfig;
use crate::taker::offers::{MakerAddress, OfferAndAddress, OfferBook};
use crate::utill::get_taker_dir;
use crate::wallet::{ RPCConfig, Wallet, WalletError};
use bitcoin::hashes::Hash;
use bitcoin::{Amount, ScriptBuf, Transaction};
use socks::Socks5Stream;
use std::{io::BufWriter, net::TcpStream, path::PathBuf, time::Duration};
use secp256k1::musig;

use super::error::TakerError;

use super::offers::fetch_addresses_from_dns;
use crate::protocol::contract2::calculate_coinswap_fee;
use crate::utill::{check_tor_status, read_message, send_message, ConnectionType};
use std::collections::HashSet;

use crate::protocol::messages2::SwapDetails;
use crate::protocol::messages2::{GetOffer, MakerToTakerMessage, TakerToMakerMessage};
use bitcoind::bitcoincore_rpc::RpcApi;
use chrono::Utc;

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

#[derive(Debug, Default)]
enum TakerPosition {
    #[default]
    FirstPeer,
}

#[derive(Default)]
struct OngoingSwapState {
    pub swap_params: SwapParams,
    pub active_preimage: Preimage,
    pub id: String,
    pub suitable_makers: Vec<OfferAndAddress>,
    pub chosen_makers: Vec<OfferAndAddress>,
    pub outgoing_contract_my_privkey: Option<bitcoin::secp256k1::SecretKey>, // Store taker's private key used for first contract
    pub outgoing_contract_my_pubkey: Option<bitcoin::PublicKey>, // Store taker's public key used for first contract
    pub outgoing_contract_my_x_only: Option<bitcoin::secp256k1::XOnlyPublicKey>, // Store taker's x-only public key used for first contract
    pub outgoing_contract_hashlock_script: Option<ScriptBuf>, // Store taker's hashlock script used for first contract
    pub outgoing_contract_timelock_script: Option<ScriptBuf>, // Store taker's timelock script used for first contract
    pub outgoing_contract_internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>, // Store for first contract (taker + first maker)
    pub outgoing_contract_tap_tweak: Option<bitcoin::secp256k1::Scalar>, // Store for first contract (taker + first maker)
    pub incoming_contract_txid: Option<bitcoin::Txid>, // Contract from final maker back to taker
    pub incoming_contract_internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>, // Store for final contract (taker + last maker)
    pub incoming_contract_tap_tweak: Option<bitcoin::secp256k1::Scalar>, // Store for final contract (taker + last maker)
    pub incoming_contract_hashlock_script: Option<ScriptBuf>, // Store for final contract (taker + last maker)
    pub incoming_contract_timelock_script: Option<ScriptBuf>, // Store for final contract (taker + last maker)
    pub my_spending_tx: Option<Transaction>, // Store taker's original spending transaction for consistent sighash
    pub incoming_contract_my_privkey: Option<bitcoin::secp256k1::SecretKey>, // Store taker's private key used for final contract
    pub incoming_contract_my_pubkey: Option<bitcoin::PublicKey>, // Store taker's public key used for final contract
    pub incoming_contract_my_x_only: Option<bitcoin::secp256k1::XOnlyPublicKey>, // Store taker's x-only public key used for final contract
    pub incoming_contract_other_pubkey: Option<bitcoin::PublicKey>, // Store other final contract pubkey
    // Maker sweeping data: store spending transactions and nonces for each maker (indexed by maker position)
    pub maker_spending_txs: Vec<Option<Transaction>>, // Spending transactions for each maker's incoming contract
    pub maker_receiver_nonces: Vec<Option<crate::protocol::messages2::SerializablePublicNonce>>, // Receiver nonces for each maker
    
    // Store last maker's partial signatures and sender nonce for sweep coordination
    pub last_maker_partial_sigs: Option<Vec<crate::protocol::messages2::SerializablePartialSignature>>,
    pub last_maker_sender_nonce: Option<crate::protocol::messages2::SerializablePublicNonce>,
}

pub(crate) const TCP_TIMEOUT_SECONDS: u64 = 300;
pub(crate) const REFUND_LOCKTIME: u16 = 20;

/// Fetch offers from taproot makers using taproot protocol messages
fn fetch_taproot_offers(
    maker_addresses: Vec<MakerAddress>,
    config: &TakerConfig,
) -> Result<Vec<OfferAndAddress>, TakerError> {
    let mut offers = Vec::new();

    for address in maker_addresses {
        match download_taproot_offer(&address, config) {
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

    // Try connecting to the maker
    let mut socket = match config.connection_type {
        ConnectionType::CLEARNET => {
            match TcpStream::connect(&maker_addr) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("Failed to connect to taproot maker {}: {:?}", maker_addr, e);
                    return None;
                }
            }
        }
        ConnectionType::TOR => {
            match Socks5Stream::connect(
                format!("127.0.0.1:{}", config.socks_port).as_str(),
                maker_addr.as_ref(),
            ) {
                Ok(s) => s.into_inner(),
                Err(e) => {
                    log::error!("Failed to connect to taproot maker {} via Tor: {:?}", maker_addr, e);
                    return None;
                }
            }
        }
    };

    // Set timeouts
    if let Err(e) = socket.set_read_timeout(Some(Duration::from_secs(30))) {
        log::error!("Failed to set read timeout: {:?}", e);
        return None;
    }
    if let Err(e) = socket.set_write_timeout(Some(Duration::from_secs(30))) {
        log::error!("Failed to set write timeout: {:?}", e);
        return None;
    }

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
            log::error!("Unexpected response from {}: expected RespOffer", maker_addr);
            return None;
        }
    };

    // Convert taproot offer to regular offer format
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

    log::info!("Successfully downloaded offer from taproot maker: {}", maker_addr);

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
}

impl Taker {
    /// Initialize a new Taker instance
    pub fn init(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        control_port: Option<u16>,
        tor_auth_password: Option<String>,
        connection_type: Option<ConnectionType>,
    ) -> Result<Taker, TakerError> {
        let data_dir = data_dir.unwrap_or_else(get_taker_dir);

        // Ensure the data directory exists
        std::fs::create_dir_all(&data_dir)?;

        let wallets_dir = data_dir.join("wallets");

        let wallet_file_name = wallet_file_name.unwrap_or("taker-wallet".to_string());
        let wallet_path = wallets_dir.join(&wallet_file_name);

        let mut rpc_config = rpc_config.unwrap_or_default();
        rpc_config.wallet_name = wallet_file_name;

        let mut wallet = if wallet_path.exists() {
            let wallet = Wallet::load(&wallet_path, &rpc_config, &None)?;
            // log::info!("Loaded wallet from {}", wallet_path.display());
            wallet
        } else {
            let wallet = Wallet::init(&wallet_path, &rpc_config, None)?;
            // log::info!("New Wallet created at {:?}", wallet_path);
            wallet
        };

        let mut config = TakerConfig::new(Some(&data_dir.join("config.toml")))?;

        if let Some(connection_type) = connection_type {
            config.connection_type = connection_type;
        }

        config.control_port = control_port.unwrap_or(config.control_port);
        config.tor_auth_password =
            tor_auth_password.unwrap_or_else(|| config.tor_auth_password.clone());

        if matches!(connection_type, Some(ConnectionType::TOR)) {
            check_tor_status(config.control_port, config.tor_auth_password.as_str())?;
        }

        config.write_to_file(&data_dir.join("config.toml"))?;

        let offerbook_path = data_dir.join("offerbook.dat");
        let offerbook = if offerbook_path.exists() {
            match OfferBook::read_from_disk(&offerbook_path) {
                Ok(offerbook) => {
                    // log::info!("Successfully loaded offerbook at : {:?}", offerbook_path);
                    offerbook
                }
                Err(e) => {
                    // log::error!("Offerbook data corrupted. Recreating. {:?}", e);
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

        // log::info!("Initializing wallet sync...");
        wallet.sync()?;
        // log::info!("Completed wallet sync");

        Ok(Self {
            wallet,
            config,
            offerbook,
            ongoing_swap_state: OngoingSwapState::default(),
            data_dir,
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

        let outgoing_signed_contract_transactions = self.create_and_broadcast_contract_transactions()?;

        if let (Some(outgoing_signed_contract_transaction), Some(first_maker)) = (
            outgoing_signed_contract_transactions.first(),
            self.ongoing_swap_state.chosen_makers.first(),
        ) {
            self.wallet.broadcast_transaction(outgoing_signed_contract_transaction)?;
            self.wait_for_contract_confirmations(&outgoing_signed_contract_transactions)?;
            self.negotiate_with_makers_and_coordinate_sweep(&outgoing_signed_contract_transactions)?;
        } else {
            return Err(TakerError::General(
                "No contract transaction or first maker available".to_string(),
            ));
        }

        self.wait_for_final_confirmations(&outgoing_signed_contract_transactions)?;
        
        Ok(())
    }

    /// Choose makers for the swap by negotiating with them
    fn choose_makers_for_swap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
        use crate::protocol::messages2::{MakerToTakerMessage, TakerToMakerMessage};
        use bitcoin::hex::DisplayHex;
        use bitcoin::secp256k1::rand::{rngs::OsRng, RngCore};

        // Find suitable maker asks for an offer from the makers
        let suitable_makers = self.find_suitable_makers_with_params(&swap_params);
        log::info!("Found {} suitable makers for swap (need {})", suitable_makers.len(), swap_params.maker_count);
        if swap_params.maker_count > suitable_makers.len() as usize {
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
        for suitable_maker in suitable_makers {
            // log::info!("Maker: {:?}", suitable_maker.address);
            let swap_details = SwapDetails {
                amount: self.ongoing_swap_state.swap_params.send_amount,
                no_of_tx: self.ongoing_swap_state.swap_params.tx_count as u8,
                timelock: 144 * self.ongoing_swap_state.swap_params.maker_count as u16, // Use 144 blocks (~24 hours)
            };

            let msg = TakerToMakerMessage::SwapDetails(swap_details);
            let response = self.send_to_maker_and_get_response(&suitable_maker.address, msg)?;
            match response {
                MakerToTakerMessage::AckResponse(AckResponse::Ack) => {
                    // log::info!("Received AckResponse from maker: {:?}", suitable_maker);
                    self.ongoing_swap_state
                        .chosen_makers
                        .push(suitable_maker.clone());
                }
                MakerToTakerMessage::AckResponse(AckResponse::Nack) => {
                    // log::warn!("Maker {:?} did not accept the swap request", suitable_maker);
                    continue;
                }
                _ => {
                    // log::warn!("Received unexpected message from maker: {:?}", response);
                    continue;
                }
            }
            if self.ongoing_swap_state.chosen_makers.len() as usize == swap_params.maker_count {
                break; // we have enough makers
            }
        }

        // If we don't get enough ACKs, return an error
        if (self.ongoing_swap_state.chosen_makers.len() as usize) < swap_params.maker_count {
            // log::error!("Not enough makers accepted the swap request. Required {}, got {}", swap_params.maker_count, self.ongoing_swap_state.chosen_makers.len());
            return Err(TakerError::NotEnoughMakersInOfferBook);
        }

        // Initialize maker sweep data storage based on the number of chosen makers
        let chosen_makers_count = self.ongoing_swap_state.chosen_makers.len();
        self.ongoing_swap_state.maker_spending_txs = vec![None; chosen_makers_count];
        self.ongoing_swap_state.maker_receiver_nonces = vec![None; chosen_makers_count];
        self.ongoing_swap_state.last_maker_partial_sigs = None;
        self.ongoing_swap_state.last_maker_sender_nonce = None;

        // Generate preimage for the swap
        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        let unique_id = preimage[0..8].to_hex_string(bitcoin::hex::Case::Lower);
        // log::info!("Initiating coinswap with id : {}", unique_id);

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.id = unique_id;

        Ok(())
    }

    /// Sync the offer book with directory servers
    pub fn sync_offerbook(&mut self) -> Result<(), TakerError> {
        let dns_addr = match self.config.connection_type {
            ConnectionType::CLEARNET => {
                if cfg!(feature = "integration-test") {
                    format!("127.0.0.1:{}", 8080)
                } else {
                    self.config.dns_address.clone()
                }
            }
            ConnectionType::TOR => self.config.dns_address.clone(),
        };

        #[cfg(not(feature = "integration-test"))]
        let socks_port = Some(self.config.socks_port);

        #[cfg(feature = "integration-test")]
        let socks_port = None;

        log::info!("Fetching addresses from DNS: {dns_addr}");

        let addresses_from_dns =
            match fetch_addresses_from_dns(socks_port, dns_addr, self.config.connection_type) {
                Ok(addresses) => {
                    log::info!("Fetched {} addresses from DNS", addresses.len());
                    addresses
                },
                Err(e) => {
                    log::error!("Could not connect to DNS Server: {e:?}");
                    return Err(e);
                }
            };

        let fresh_addrs = self
            .offerbook
            .get_fresh_addrs()
            .iter()
            .map(|oa| &oa.address)
            .collect::<HashSet<_>>();

        let addrs_to_fetch = addresses_from_dns
            .iter()
            .filter(|dns_addr| !fresh_addrs.contains(dns_addr))
            .cloned()
            .collect::<Vec<_>>();

        let new_offers = fetch_taproot_offers(addrs_to_fetch, &self.config)?;

        for new_offer in new_offers {
            if let Err(e) = self
                .wallet
                .verify_fidelity_proof(&new_offer.offer.fidelity, &new_offer.address.to_string())
            {
                // log::error!("Fidelity proof verification failed: {:?}", e);
                self.offerbook.add_bad_maker(&new_offer);
            } else {
                // log::info!("Fidelity proof verified successfully");
                self.offerbook.add_new_offer(&new_offer);
            }
        }

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
        let mut socket = match self.config.connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(address)?,
            ConnectionType::TOR => Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                address.as_str(),
            )?
            .into_inner(),
        };

        let reconnect_timeout = Duration::from_secs(TCP_TIMEOUT_SECONDS);

        socket.set_read_timeout(Some(reconnect_timeout))?;
        socket.set_write_timeout(Some(reconnect_timeout))?;

        send_message(&mut socket, &msg)?;
        // log::info!("===> {msg} | {maker_addr}");

        // Read response
        let response_bytes = read_message(&mut socket)?;
        let response: MakerToTakerMessage = serde_cbor::from_slice(&response_bytes)?;
        // log::info!("<=== {} | {maker_addr}", response);

        Ok(response)
    }

    /// Send a message to a maker without expecting a response
    fn send_message_to_maker(
        &self,
        maker_addr: &MakerAddress,
        msg: TakerToMakerMessage,
    ) -> Result<(), TakerError> {
        let address = maker_addr.to_string();
        let mut socket = match self.config.connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(address)?,
            ConnectionType::TOR => Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                address.as_str(),
            )?
            .into_inner(),
        };

        let reconnect_timeout = Duration::from_secs(TCP_TIMEOUT_SECONDS);

        socket.set_read_timeout(Some(reconnect_timeout))?;
        socket.set_write_timeout(Some(reconnect_timeout))?;

        send_message(&mut socket, &msg)?;
        // log::info!("===> {msg} | {maker_addr} (no response expected)");

        // Close connection immediately, no response expected
        Ok(())
    }

    // Note: recover_from_swap is implemented below as a method

    /// Find suitable makers for the current swap parameters
    pub fn find_suitable_makers(&self) -> Vec<OfferAndAddress> {
        let swap_amount = self.ongoing_swap_state.swap_params.send_amount;
        let max_refund_locktime =
            REFUND_LOCKTIME * (self.ongoing_swap_state.swap_params.maker_count + 1) as u16;
        self.offerbook
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
                swap_amount >= min_size_with_fee
                    && swap_amount <= bitcoin::Amount::from_sat(oa.offer.max_size)
            })
            .cloned()
            .collect()
    }

    /// Find suitable makers for the given swap parameters
    pub fn find_suitable_makers_with_params(
        &self,
        swap_params: &SwapParams,
    ) -> Vec<OfferAndAddress> {
        let swap_amount = swap_params.send_amount;
        let max_refund_locktime = REFUND_LOCKTIME * (swap_params.maker_count + 1) as u16;

        // log::debug!("Finding suitable makers for amount: {} sats, max_refund_locktime: {}",
        //            swap_amount.to_sat(), max_refund_locktime);

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

                // log::debug!("Evaluating maker {}: min_size={}, max_size={}, maker_fee={}, min_size_with_fee={}, swap_amount={}, suitable={}",
                //            oa.address, oa.offer.min_size, oa.offer.max_size, maker_fee, min_size_with_fee.to_sat(), swap_amount.to_sat(), is_suitable);

                is_suitable
            })
            .cloned()
            .collect();

        // log::info!("Found {} suitable makers out of {} total makers",
        //            suitable_makers.len(), self.offerbook.all_good_makers().len());

        suitable_makers
    }

    /// Setup contract keys and scripts for the swap
    fn setup_contract_keys_and_scripts(&mut self) -> Result<(), TakerError> {
        use crate::protocol::contract2::{
            create_hashlock_script, create_taproot_script, create_timelock_script
        };
        use bitcoin::hashes::sha256;
        use bitcoin::locktime::absolute::LockTime;

        let secp = bitcoin::secp256k1::Secp256k1::new();

        let mut preimage = [0u8; 32];
        #[cfg(feature = "integration-test")]
        {
            preimage = [0u8; 32];
        }
        #[cfg(not(feature = "integration-test"))]
        {
            use bitcoin::secp256k1::rand::{rngs::OsRng, RngCore};
            OsRng.fill_bytes(&mut preimage);
        }
        self.ongoing_swap_state.active_preimage = preimage;

        let (outgoing_contract_my_privkey, _) = self.wallet.get_tweakable_keypair()?;
        let outgoing_contract_my_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &outgoing_contract_my_privkey);
        let (outgoing_contract_my_x_only, _) = outgoing_contract_my_keypair.x_only_public_key();
        self.ongoing_swap_state.outgoing_contract_my_privkey= Some(outgoing_contract_my_privkey);
        self.ongoing_swap_state.outgoing_contract_my_pubkey=
            Some(bitcoin::PublicKey::from(outgoing_contract_my_keypair.public_key()));
        self.ongoing_swap_state.outgoing_contract_my_x_only = Some(outgoing_contract_my_x_only);

        let (incoming_contract_my_privkey, incoming_contract_my_pubkey) =
            self.wallet.get_tweakable_keypair()?;
        let incoming_contract_my_keypair =
            bitcoin::secp256k1::Keypair::from_secret_key(&secp, &incoming_contract_my_privkey);
        let (incoming_contract_my_x_only, _) = incoming_contract_my_keypair.x_only_public_key();
        self.ongoing_swap_state.incoming_contract_my_privkey= Some(incoming_contract_my_privkey.clone());
        println!("Taker receiver privkey: {:?}", incoming_contract_my_privkey);
        self.ongoing_swap_state.incoming_contract_my_pubkey = Some(bitcoin::PublicKey::from(
            incoming_contract_my_keypair.public_key(),
        ));
        println!("Taker receiver pubkey: {:?}", incoming_contract_my_pubkey);
        self.ongoing_swap_state.incoming_contract_my_x_only = Some(incoming_contract_my_x_only);
        println!("Taker receiver x only: {:?}", incoming_contract_my_x_only);

        // Create scripts for outgoing contract
        let hash = sha256::Hash::hash(&preimage);
        let hashlock_script =
            create_hashlock_script(&hash.to_byte_array(), &outgoing_contract_my_x_only);
        self.ongoing_swap_state.outgoing_contract_hashlock_script =
            Some(hashlock_script.clone());

        let timelock =
            LockTime::from_height(self.ongoing_swap_state.swap_params.maker_count as u32 * 48)
                .unwrap();
        let timelock_script = create_timelock_script(timelock, &outgoing_contract_my_x_only);
        self.ongoing_swap_state.outgoing_contract_timelock_script =
            Some(timelock_script.clone());

        Ok(())
    }

    /// Create and broadcast contract transactions
    fn create_and_broadcast_contract_transactions(&mut self) -> Result<Vec<Transaction>, TakerError> {
        use crate::protocol::contract2::create_taproot_script;

        let available_utxos = self.wallet.list_all_utxo_spend_info()?;
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

            let hashlock_script = self.ongoing_swap_state.outgoing_contract_hashlock_script
                .clone()
                .ok_or_else(|| TakerError::General("No hashlock script found".to_string()))?;
            let timelock_script = self.ongoing_swap_state.outgoing_contract_timelock_script
                .clone()
                .ok_or_else(|| TakerError::General("No timelock script found".to_string()))?;

            let first_maker_pubkey = first_maker.offer.tweakable_point.inner;
            let outgoing_contract_my_pubkey = self.ongoing_swap_state.outgoing_contract_my_pubkey
                .ok_or_else(|| TakerError::General("No taker pubkey found".to_string()))?;
            let outgoing_contract_internal_key =
                crate::protocol::musig_interface::get_aggregated_pubkey_i(
                    outgoing_contract_my_pubkey.inner,
                    first_maker_pubkey
                );

            // Create taproot script (P2TR output)
            let (outgoing_contract_taproot_script, outgoing_contract_taproot_spendinfo) =
                create_taproot_script(hashlock_script, timelock_script, outgoing_contract_internal_key);

            self.ongoing_swap_state.outgoing_contract_internal_key =
                Some(outgoing_contract_internal_key);
            self.ongoing_swap_state.outgoing_contract_tap_tweak =
                Some(outgoing_contract_taproot_spendinfo.tap_tweak().to_scalar());

            let outgoing_contract_taproot_address =
                bitcoin::Address::from_script(&outgoing_contract_taproot_script, bitcoin::Network::Regtest).map_err(
                    |e| TakerError::General(format!("Failed to create taproot address: {:?}", e)),
                )?;

            let signed_outgoing_contract_tx = {
                use crate::utill::DEFAULT_TX_FEE_RATE;
                use crate::wallet::Destination;

                let funding_utxo_info = self
                    .wallet
                    .get_utxo((funding_utxo.txid, funding_utxo.vout))?
                    .ok_or_else(|| TakerError::General("Funding UTXO not found".to_string()))?;

                let signed_tx = self.wallet.spend_from_wallet(
                    DEFAULT_TX_FEE_RATE,
                    Destination::Sweep(outgoing_contract_taproot_address),
                    &[(funding_utxo.clone(), funding_utxo_info)],
                )?;

                signed_tx
            };

            contract_transactions.push(signed_outgoing_contract_tx);
        } else {
            return Err(TakerError::General("No makers chosen for swap".to_string()));
        }

        Ok(contract_transactions)
    }

    /// Wait for contract transactions to be confirmed  
    fn wait_for_contract_confirmations(&self, outgoing_signed_contract_transactions: &[Transaction]) -> Result<(), TakerError> {
        #[cfg(feature = "integration-test")]
        {
            use std::thread;
            use std::time::Duration;

            for (i, tx) in outgoing_signed_contract_transactions.iter().enumerate() {
                let txid = tx.compute_txid();
                // log::info!("Waiting for contract transaction {} to be confirmed: {}", i, txid);

                // For integration tests, we need to trigger mining and wait for confirmations
                // The test framework should handle the mining externally
                let max_attempts = 30;
                for attempt in 0..max_attempts {
                    // Try to get the transaction and check if it's confirmed
                    match self.wallet.get_transaction_details(&txid) {
                        Ok(Some(_)) => {
                            // log::info!("Contract transaction {} found and confirmed", i);
                            break;
                        }
                        Ok(None) => {
                            if attempt == max_attempts - 1 {
                                return Err(TakerError::General(format!(
                                    "Contract transaction {} not confirmed after {} attempts",
                                    i, max_attempts
                                )));
                            }
                            // log::debug!("Contract transaction {} not yet confirmed (attempt {})", i, attempt + 1);
                            thread::sleep(Duration::from_secs(1));
                        }
                        Err(e) => {
                            // log::warn!("Error checking contract transaction {}: {:?}", i, e);
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
            }
        }

        #[cfg(not(feature = "integration-test"))]
        {
            use std::thread;
            use std::time::Duration;

            for (i, tx) in outgoing_signed_contract_transactions.iter().enumerate() {
                let txid = tx.compute_txid();

                let max_attempts = 60; // Wait up to 60 seconds in production
                for attempt in 0..max_attempts {
                    match self.wallet.get_transaction_details(&txid) {
                        Ok(Some(_)) => {
                            break;
                        }
                        Ok(None) => {
                            if attempt == max_attempts - 1 {
                                return Err(TakerError::General(format!(
                                    "Contract transaction {} not confirmed after {} attempts",
                                    i, max_attempts
                                )));
                            }
                            thread::sleep(Duration::from_secs(1));
                        }
                        Err(e) => {
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Wait for final confirmations at the end of swap
    fn wait_for_final_confirmations(&self, outgoing_signed_contract_transactions: &[Transaction]) -> Result<(), TakerError> {
        #[cfg(feature = "integration-test")]
        {
            use std::thread;
            use std::time::Duration;

            for (i, tx) in outgoing_signed_contract_transactions.iter().enumerate() {
                let txid = tx.compute_txid();
                let max_attempts = 30;
                for attempt in 0..max_attempts {
                    // Try to get the transaction and check if it's confirmed
                    match self.wallet.get_transaction_details(&txid) {
                        Ok(Some(_)) => {
                            // log::info!("Contract transaction {} found and confirmed", i);
                            break;
                        }
                        Ok(None) => {
                            if attempt == max_attempts - 1 {
                                return Err(TakerError::General(format!(
                                    "Contract transaction {} not confirmed after {} attempts",
                                    i, max_attempts
                                )));
                            }
                            // log::debug!("Contract transaction {} not yet confirmed (attempt {})", i, attempt + 1);
                            thread::sleep(Duration::from_secs(1));
                        }
                        Err(e) => {
                            // log::warn!("Error checking contract transaction {}: {:?}", i, e);
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Negotiate with makers and coordinate the sweep process
    fn negotiate_with_makers_and_coordinate_sweep(&mut self, outgoing_signed_contract_transactions: &[Transaction]) -> Result<(), TakerError> {
        use crate::protocol::messages2::{
            MakerToTakerMessage, SendersContract, TakerToMakerMessage,
        };

        let first_maker = self.ongoing_swap_state.chosen_makers.first()
            .ok_or_else(|| TakerError::General("No first maker found".to_string()))?;

        let next_party_tweakable_point =
            if let Some(second_maker) = self.ongoing_swap_state.chosen_makers.get(1) {
                second_maker.offer.tweakable_point
            } else {
                self.ongoing_swap_state
                    .incoming_contract_my_pubkey
                    .clone()
                    .unwrap()
            };

        let senders_contract = SendersContract {
            contract_txs: vec![outgoing_signed_contract_transactions[0].compute_txid()],
            pubkeys_a: vec![self
                .ongoing_swap_state
                .outgoing_contract_my_pubkey
                .clone()
                .unwrap()],
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
                    .clone()
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
    fn forward_contracts_and_coordinate_sweep(&mut self, mut current_contract: crate::protocol::messages2::SenderContractFromMaker) -> Result<(), TakerError> {
        use crate::protocol::messages2::{
            MakerToTakerMessage, SendersContract, TakerToMakerMessage, SpendingTxAndReceiverNonce, PartialSigAndSendersNonce
        };

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
            let maker = &self.ongoing_swap_state.chosen_makers[maker_index];
            
            // Determine the next party in the chain
            let next_party_tweakable_point = if maker_index == maker_count - 1 {
                // Last maker should point back to taker
                self.ongoing_swap_state.incoming_contract_my_pubkey.clone().unwrap()
            } else {
                // Intermediate maker should point to next maker
                self.ongoing_swap_state.chosen_makers[maker_index + 1].offer.tweakable_point
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
            let maker_response = self.send_to_maker_and_get_response(&maker.address, forward_msg)?;

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
                    return Err(TakerError::General(
                        format!("Unexpected response from maker {}", maker_index),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Store final contract data from last maker
    fn store_final_contract_data(&mut self, second_receivers_contract: &crate::protocol::messages2::SenderContractFromMaker) -> Result<(), TakerError> {
        if let Some(incoming_contract_txid) = second_receivers_contract.contract_txs.get(0) {
            self.ongoing_swap_state.incoming_contract_txid = Some(*incoming_contract_txid);
        }

        if let Some(incoming_contract_internal_key) = second_receivers_contract.internal_key {
            self.ongoing_swap_state.incoming_contract_internal_key = Some(incoming_contract_internal_key);
        }
        
        if let Some(incoming_contract_tap_tweak) = &second_receivers_contract.tap_tweak {
            let tap_tweak_scalar: bitcoin::secp256k1::Scalar = incoming_contract_tap_tweak.clone().into();
            self.ongoing_swap_state.incoming_contract_tap_tweak = Some(tap_tweak_scalar);
        }

        if let Some(incoming_contract_hashlock_script) = second_receivers_contract.hashlock_scripts.get(0) {
            self.ongoing_swap_state.incoming_contract_hashlock_script = Some(incoming_contract_hashlock_script.clone());
        }

        if let Some(incoming_contract_timelock_script) = second_receivers_contract.timelock_scripts.get(0) {
            self.ongoing_swap_state.incoming_contract_timelock_script = Some(incoming_contract_timelock_script.clone());
        }

        if let Some(incoming_contract_other_pubkey) = second_receivers_contract.pubkeys_a.get(0) {
            self.ongoing_swap_state.incoming_contract_other_pubkey = Some(incoming_contract_other_pubkey.clone());
        }

        Ok(())
    }

    /// Execute taker's sweep and coordinate with all makers
    fn execute_taker_sweep_and_coordinate_makers(&mut self) -> Result<(), TakerError> {
        use crate::protocol::messages2::{SpendingTxAndReceiverNonce, PartialSigAndSendersNonce};
        use crate::protocol::musig_interface::generate_new_nonce_pair_i;
        use bitcoin::secp256k1::{Message, Secp256k1};
        use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
        use bitcoin::{Amount, OutPoint, Sequence, Transaction, TxIn, TxOut, Witness};

        let secp = Secp256k1::new();
        let last_maker_address = self.ongoing_swap_state.chosen_makers.last()
            .ok_or_else(|| TakerError::General("No last maker found".to_string()))?
            .address.clone();

        let incoming_contract_txid = self.ongoing_swap_state.incoming_contract_txid
            .ok_or_else(|| TakerError::General("No final contract transaction ID found".to_string()))?;

        let incoming_contract_my_privkey = self.ongoing_swap_state.incoming_contract_my_privkey
            .ok_or_else(|| TakerError::General("No stored taker private key for final contract".to_string()))?;

        let final_contract_tx = self.wallet.rpc.get_raw_transaction(&incoming_contract_txid, None)
            .map_err(|e| TakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;
        let incoming_contract_amount = final_contract_tx.output[0].value;

        log::info!("TAKER CREATING SPENDING TX:");
        log::info!("  Spending from contract txid: {:?}", incoming_contract_txid);
        log::info!("  Contract amount: {:?}", incoming_contract_amount);
        log::info!("  Last maker address: {:?}", last_maker_address);
        log::info!("  Internal key: {:?}", self.ongoing_swap_state.incoming_contract_internal_key);
        log::info!("  Tap tweak: {:?}", self.ongoing_swap_state.incoming_contract_tap_tweak);
        
        let mut taker_spending_tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::locktime::absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: incoming_contract_txid,
                    vout: 0,
                },
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: Sequence::ZERO,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: incoming_contract_amount - Amount::from_sat(1000),
                script_pubkey: self.wallet.get_next_internal_addresses(1)
                    .map_err(|e| TakerError::Wallet(e))?[0].script_pubkey(),
            }],
        };

        let incoming_contract_my_keypair = bitcoin::secp256k1::Keypair::from_secret_key(&secp, &incoming_contract_my_privkey);
        let last_maker_pubkey = self.ongoing_swap_state.incoming_contract_other_pubkey
            .ok_or_else(|| TakerError::General("No last maker pubkey found".to_string()))?;

        let tap_tweak = self.ongoing_swap_state.incoming_contract_tap_tweak
            .ok_or_else(|| TakerError::General("No final contract tap tweak found".to_string()))?;

        let internal_key = self.ongoing_swap_state.incoming_contract_internal_key
            .ok_or_else(|| TakerError::General("No final contract internal key found".to_string()))?;

        // Create contract script and compute sighash
        let incoming_contract_hashlock_script = self.ongoing_swap_state.incoming_contract_hashlock_script.clone()
            .ok_or_else(|| TakerError::General("No incoming contract hashlock script found".to_string()))?;
        let incoming_contract_timelock_script = self.ongoing_swap_state.incoming_contract_timelock_script.clone()
            .ok_or_else(|| TakerError::General("No incoming contract timelock script found".to_string()))?;
        
        use crate::protocol::contract2::create_taproot_script;
        let (contract_script, _) = create_taproot_script(
            incoming_contract_hashlock_script,
            incoming_contract_timelock_script,
            internal_key,
        );
        
        let prevout = bitcoin::TxOut {
            value: incoming_contract_amount,
            script_pubkey: contract_script.clone(),
        };
        let prevouts = vec![prevout];
        let prevouts_ref = bitcoin::sighash::Prevouts::All(&prevouts);

        let mut sighasher = bitcoin::sighash::SighashCache::new(&mut taker_spending_tx);
        let sighash = sighasher.taproot_key_spend_signature_hash(0, &prevouts_ref, bitcoin::TapSighashType::Default)
            .map_err(|_| TakerError::General("Failed to compute sighash".to_string()))?;
        let message = bitcoin::secp256k1::Message::from(sighash);

        let pubkey1 = incoming_contract_my_keypair.public_key();
        let pubkey2 = last_maker_pubkey;

        let mut ordered_pubkeys = vec![pubkey1, pubkey2.inner];
        ordered_pubkeys.sort_by(|a, b| a.serialize().cmp(&b.serialize()));

        // Debug logging for taker signing
        log::info!("TAKER SIGNING DEBUG:");
        log::info!("  Signing message: {:?}", message);
        log::info!("  Tap tweak: {:?}", tap_tweak);
        log::info!("  Internal key: {:?}", internal_key);
        log::info!("  Prevout amount: {:?}", incoming_contract_amount);
        log::info!("  Prevout script: {:?}", contract_script);
        log::info!("  Pubkey1 (taker): {:?}", pubkey1);
        log::info!("  Pubkey2 (maker): {:?}", pubkey2.inner);
        log::info!("  Ordered pubkeys: {:?}", ordered_pubkeys);
        log::info!("  Transaction: {:?}", taker_spending_tx);

        let (incoming_contract_my_sec_nonce, incoming_contract_my_pub_nonce) = generate_new_nonce_pair_i(
            tap_tweak,
            ordered_pubkeys[0], // lexicographically first pubkey
            ordered_pubkeys[1], // lexicographically second pubkey
            incoming_contract_my_keypair.public_key(), // Signer is taker
            message,
        );

        self.ongoing_swap_state.my_spending_tx = Some(taker_spending_tx.clone());

        let msg = crate::protocol::messages2::TakerToMakerMessage::SpendingTxAndReceiverNonce(
            SpendingTxAndReceiverNonce {
                spending_transaction: taker_spending_tx.clone(),
                receiver_nonce: incoming_contract_my_pub_nonce.into(),
            },
        );

        let response = self.send_to_maker_and_get_response(&last_maker_address, msg)?;

        // Process last maker's response and complete taker's sweep
        self.complete_taker_sweep(response, incoming_contract_my_sec_nonce, incoming_contract_my_pub_nonce)?;
        
        // Coordinate with all makers for their sweeps
        self.coordinate_maker_sweeps(incoming_contract_my_pub_nonce)?;

        Ok(())
    }

    /// Complete taker's sweep transaction
    fn complete_taker_sweep(&mut self, response: crate::protocol::messages2::MakerToTakerMessage, incoming_contract_my_sec_nonce: musig::SecretNonce, incoming_contract_my_pub_nonce: musig::PublicNonce) -> Result<(), TakerError> {
        use crate::protocol::messages2::MakerToTakerMessage;
        use bitcoin::secp256k1::{Message, Secp256k1};
        use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
        use bitcoin::{TxOut, Witness};

        match response {
            MakerToTakerMessage::NoncesPartialSigsAndSpendingTx(maker_response) => {
                let secp = Secp256k1::new();
                
                let incoming_contract_txid = self.ongoing_swap_state.incoming_contract_txid
                    .ok_or_else(|| TakerError::General("No final contract transaction ID found".to_string()))?;

                let incoming_contract_tx = self.wallet.rpc.get_raw_transaction(&incoming_contract_txid, None)
                    .map_err(|e| TakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;
                let incoming_contract_amount = incoming_contract_tx.output[0].value;

                let incoming_contract_my_privkey = self.ongoing_swap_state.incoming_contract_my_privkey
                    .ok_or_else(|| TakerError::General("No stored taker private key for final contract".to_string()))?;
                let incoming_contract_my_keypair = bitcoin::secp256k1::Keypair::from_secret_key(&secp, &incoming_contract_my_privkey);

                let incoming_contract_other_pubkey = self.ongoing_swap_state.incoming_contract_other_pubkey.clone().unwrap();

                let internal_key = self.ongoing_swap_state.incoming_contract_internal_key
                    .ok_or_else(|| TakerError::General("No final contract internal key found".to_string()))?;
                let tap_tweak = self.ongoing_swap_state.incoming_contract_tap_tweak
                    .ok_or_else(|| TakerError::General("No final contract tap tweak found".to_string()))?;

                // Create the same script construction as maker to ensure consistent prevout script
                let incoming_contract_hashlock_script = self.ongoing_swap_state.incoming_contract_hashlock_script.clone()
                    .ok_or_else(|| TakerError::General("No incoming contract hashlock script found".to_string()))?;
                let incoming_contract_timelock_script = self.ongoing_swap_state.incoming_contract_timelock_script.clone()
                    .ok_or_else(|| TakerError::General("No incoming contract timelock script found".to_string()))?;
                
                use crate::protocol::contract2::create_taproot_script;
                let (contract_script, _) = create_taproot_script(
                    incoming_contract_hashlock_script,
                    incoming_contract_timelock_script,
                    internal_key,
                );
                let prevout = TxOut {
                    value: incoming_contract_amount,
                    script_pubkey: contract_script,
                };

                let original_spending_tx = self.ongoing_swap_state.my_spending_tx.as_ref()
                    .ok_or_else(|| TakerError::General("No stored taker spending transaction found".to_string()))?;

                let prevouts = vec![prevout];
                let prevouts_ref = Prevouts::All(&prevouts);
                let mut final_tx = original_spending_tx.clone();
                
                let mut sighasher = SighashCache::new(&mut final_tx);
                let sighash = sighasher.taproot_key_spend_signature_hash(0, &prevouts_ref, bitcoin::TapSighashType::Default)
                    .map_err(|_| TakerError::General("Failed to compute sighash".to_string()))?;
                let message = Message::from(sighash);
                log::info!("Taker message for signing: {:?}", message);

                let incoming_contract_other_nonce: secp256k1::musig::PublicNonce = maker_response.sender_nonce.clone().into();
                let incoming_contract_other_partial_sig: secp256k1::musig::PartialSignature = maker_response.partial_signatures[0].clone().into();

                let mut pubkeys = vec![incoming_contract_my_keypair.public_key(), incoming_contract_other_pubkey.inner];
                pubkeys.sort_by(|a, b| a.serialize().cmp(&b.serialize()));

                log::info!("Pubkeys: {:?}", pubkeys);

                let nonce_refs = if pubkeys[0].serialize() == incoming_contract_my_keypair.public_key().serialize() {
                    vec![&incoming_contract_my_pub_nonce, &incoming_contract_other_nonce]
                } else {
                    vec![&incoming_contract_other_nonce, &incoming_contract_my_pub_nonce]
                };
                let aggregated_nonce = crate::protocol::musig_interface::get_aggregated_nonce_i(&nonce_refs);
                log::info!("Aggregated nonce: {:?}", aggregated_nonce.serialize());

                let calculated_internal_key = crate::protocol::musig_interface::get_aggregated_pubkey_i(pubkeys[0], pubkeys[1]);

                if internal_key != calculated_internal_key {
                    return Err(TakerError::General("Internal key mismatch during final contract signing".to_string()));
                }

                let incoming_contract_my_partial_sig = crate::protocol::musig_interface::generate_partial_signature_i(
                    message,
                    &aggregated_nonce,
                    incoming_contract_my_sec_nonce,
                    incoming_contract_my_keypair,
                    tap_tweak,
                    pubkeys[0],
                    pubkeys[1]
                );

                let partial_sigs = if pubkeys[0].serialize() == incoming_contract_my_keypair.public_key().serialize() {
                    vec![&incoming_contract_my_partial_sig, &incoming_contract_other_partial_sig]
                } else {
                    vec![&incoming_contract_other_partial_sig, &incoming_contract_my_partial_sig]
                };
                let aggregated_sig = crate::protocol::musig_interface::aggregate_partial_signatures_i(
                    message,
                    aggregated_nonce,
                    tap_tweak,
                    partial_sigs,
                    pubkeys[0],
                    pubkeys[1]
                );

                let final_signature = bitcoin::taproot::Signature::from_slice(aggregated_sig.assume_valid().as_byte_array()).unwrap();

                let mut final_tx = original_spending_tx.clone();
                let mut sighasher = SighashCache::new(&mut final_tx);
                *sighasher.witness_mut(0).unwrap() = Witness::p2tr_key_spend(&final_signature);
                let outgoing_contract_with_witness = sighasher.into_transaction();
                
                use crate::bitcoind::bitcoincore_rpc::RawTx;
                let outgoing_contract_txid = self.wallet.rpc.send_raw_transaction(outgoing_contract_with_witness.raw_hex())
                    .map_err(|e| TakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;
                log::info!("Taker sweeping transaction broadcasted with txid: {:?}", outgoing_contract_txid);

                // Store the maker's spending transaction and receiver nonce for the next sweep
                let maker_count = self.ongoing_swap_state.chosen_makers.len();
                let last_maker_index = maker_count - 1;
                self.ongoing_swap_state.maker_spending_txs[last_maker_index] = Some(maker_response.spending_transaction.clone());
                self.ongoing_swap_state.maker_receiver_nonces[last_maker_index] = Some(maker_response.receiver_nonce.clone());
                
                // Store the last maker's partial signatures and sender nonce
                self.ongoing_swap_state.last_maker_partial_sigs = Some(maker_response.partial_signatures.clone());
                self.ongoing_swap_state.last_maker_sender_nonce = Some(maker_response.sender_nonce.clone());
            }
            _ => {
                return Err(TakerError::General("Expected NoncesPartialSigsAndSpendingTx from last maker".to_string()));
            }
        }

        Ok(())
    }

    /// Coordinate sweeps with all makers in the chain
    fn coordinate_maker_sweeps(&mut self, incoming_contract_my_pub_nonce: musig::PublicNonce) -> Result<(), TakerError> {
        use crate::protocol::messages2::{SpendingTxAndReceiverNonce, PartialSigAndSendersNonce, TakerToMakerMessage, MakerToTakerMessage};

        let maker_count = self.ongoing_swap_state.chosen_makers.len();
        
        // Store partial signatures and sender nonces from each maker
        let mut maker_partial_sigs: Vec<Option<Vec<crate::protocol::messages2::SerializablePartialSignature>>> = vec![None; maker_count];
        let mut maker_sender_nonces: Vec<Option<crate::protocol::messages2::SerializablePublicNonce>> = vec![None; maker_count];

        // Handle single maker case differently - skip the SpendingTxAndReceiverNonce phase
        if maker_count == 1 {
            log::info!("Single maker case: skipping SpendingTxAndReceiverNonce phase");
            // The single maker will construct their own spending transaction
            // We only need to send the taker's partial signature
        } else {
            // Multi-maker case: normal flow
            for maker_index in (0..maker_count).rev() {
            let maker = &self.ongoing_swap_state.chosen_makers[maker_index];

            // Send SpendingTxAndReceiverNonce to ALL makers to collect their partial signatures
            log::info!("Sending SpendingTxAndReceiverNonce to maker {} at {}", maker_index, maker.address);

            // Get the spending transaction and receiver nonce for this maker
            let (spending_tx, receiver_nonce) = if maker_index == maker_count - 1 {
                // Last maker gets the taker's spending transaction (which sweeps the last contract)
                log::info!("Using taker spending transaction for last maker {}", maker_index);
                (
                    self.ongoing_swap_state.my_spending_tx.clone()
                        .ok_or_else(|| TakerError::General("No taker spending transaction stored".to_string()))?,
                    incoming_contract_my_pub_nonce.clone().into()
                )
            } else {
                // Other makers get the spending transaction from the next maker in the chain
                let source_maker_index = maker_index + 1;
                log::info!("Using spending transaction from maker {} for maker {}", source_maker_index, maker_index);
                let spending_tx = self.ongoing_swap_state.maker_spending_txs[source_maker_index].clone()
                    .ok_or_else(|| TakerError::General(format!("No spending transaction stored for maker {}", source_maker_index)))?;
                let receiver_nonce = self.ongoing_swap_state.maker_receiver_nonces[source_maker_index].clone()
                    .ok_or_else(|| TakerError::General(format!("No receiver nonce stored for maker {}", source_maker_index)))?;
                (spending_tx, receiver_nonce)
            };

            let msg = TakerToMakerMessage::SpendingTxAndReceiverNonce(
                SpendingTxAndReceiverNonce {
                    spending_transaction: spending_tx,
                    receiver_nonce: receiver_nonce.into(),
                },
            );

            let response = self.send_to_maker_and_get_response(&maker.address, msg)?;

            match response {
                MakerToTakerMessage::NoncesPartialSigsAndSpendingTx(maker_response) => {
                    // Store this maker's spending transaction and receiver nonce for the next sweep
                    self.ongoing_swap_state.maker_spending_txs[maker_index] = Some(maker_response.spending_transaction.clone());
                    self.ongoing_swap_state.maker_receiver_nonces[maker_index] = Some(maker_response.receiver_nonce.clone());
                    
                    // Store partial signatures and sender nonce for later relay
                    maker_partial_sigs[maker_index] = Some(maker_response.partial_signatures.clone());
                    maker_sender_nonces[maker_index] = Some(maker_response.sender_nonce.clone());

                    // Send partial signature to the next maker in the chain
                    if maker_index < maker_count - 1 {
                        let next_maker_index = maker_index + 1;
                        let next_maker = &self.ongoing_swap_state.chosen_makers[next_maker_index];

                        let partial_sig_msg = TakerToMakerMessage::PartialSigAndSendersNonce(
                            PartialSigAndSendersNonce {
                                partial_signatures: maker_response.partial_signatures.clone(),
                                sender_nonce: maker_response.sender_nonce.clone(),
                            },
                        );

                        self.send_message_to_maker(&next_maker.address, partial_sig_msg)?;
                    }
                }
                _ => {
                    return Err(TakerError::General(format!("Expected NoncesPartialSigsAndSpendingTx from maker {}", maker_index)));
                }
            }
        }
        } // Close the multi-maker else block

        // Send partial signature to last maker (they need first maker's sig)
        if maker_count > 1 {
            let last_maker_index = maker_count - 1;
            let last_maker = &self.ongoing_swap_state.chosen_makers[last_maker_index];
            
            log::info!("Sending partial signature to last maker at {}", last_maker.address);
            
            // Send first maker's partial signature to last maker  
            if let (Some(partial_sigs), Some(sender_nonce)) = (&maker_partial_sigs[0], &maker_sender_nonces[0]) {
                let msg = TakerToMakerMessage::PartialSigAndSendersNonce(
                    PartialSigAndSendersNonce {
                        partial_signatures: partial_sigs.clone(),
                        sender_nonce: sender_nonce.clone(),
                    },
                );
                self.send_message_to_maker(&last_maker.address, msg)?;
                log::info!("Successfully sent first maker's partial signature to last maker");
            } else {
                log::error!("No partial signature or nonce stored for maker 0");
            }
        }
        
        // Send taker's partial signature to first maker (for Taker→Maker0 contract)
        if maker_count > 0 {
            let first_maker = &self.ongoing_swap_state.chosen_makers[0];
            
            log::info!("Sending taker's partial signature to first maker at {}", first_maker.address);
            
            // Generate taker's partial signature for the Taker→Maker0 contract
            let taker_partial_sig = self.generate_taker_partial_signature_for_first_maker()?;
            
            let msg = TakerToMakerMessage::PartialSigAndSendersNonce(taker_partial_sig);
            log::info!("DEBUG: Sending taker's partial signature to first maker");
            self.send_message_to_maker(&first_maker.address, msg)?;
            log::info!("DEBUG: Successfully sent taker's partial signature to first maker");
        }
        
        // Wait for makers to complete their sweeps
        #[cfg(feature = "integration-test")]
        {
            use std::thread;
            use std::time::Duration;
            
            log::info!("Waiting for makers to complete their sweeps...");
            thread::sleep(Duration::from_secs(5));
        }

        Ok(())
    }
    
    /// Generate taker's partial signature for the Taker→Maker0 contract
    /// This is used in step 20 of the protocol where taker sends its partial signature to maker0
    fn generate_taker_partial_signature_for_first_maker(&self) -> Result<crate::protocol::messages2::PartialSigAndSendersNonce, TakerError> {
        use crate::protocol::musig_interface::{generate_new_nonce_pair_i, generate_partial_signature_i};
        use bitcoin::secp256k1::{Message, Secp256k1};
        use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
        use bitcoin::TxOut;

        // Get the first maker's spending transaction that sweeps the Taker→Maker0 contract
        let first_maker_spending_tx = self.ongoing_swap_state.maker_spending_txs[0]
            .as_ref()
            .ok_or_else(|| TakerError::General("No spending transaction stored for first maker".to_string()))?;

        // Get taker's private key for the Taker→Maker0 contract
        let taker_privkey = self.ongoing_swap_state.outgoing_contract_my_privkey
            .ok_or_else(|| TakerError::General("No taker private key for outgoing contract".to_string()))?;
        let secp = Secp256k1::new();
        let taker_keypair = bitcoin::secp256k1::Keypair::from_secret_key(&secp, &taker_privkey);

        // Get first maker's public key from the offers
        let first_maker_pubkey = self.ongoing_swap_state.chosen_makers[0].offer.tweakable_point;

        // Get contract details for the Taker→Maker0 contract
        let internal_key = self.ongoing_swap_state.outgoing_contract_internal_key
            .ok_or_else(|| TakerError::General("No internal key for outgoing contract".to_string()))?;
        let tap_tweak = self.ongoing_swap_state.outgoing_contract_tap_tweak
            .ok_or_else(|| TakerError::General("No tap tweak for outgoing contract".to_string()))?;

        // Get the contract txid that the maker is trying to spend from (Taker→Maker0 contract)
        let contract_txid = first_maker_spending_tx.input[0].previous_output.txid;
        
        // Fetch the contract transaction to get the output value and script
        let contract_tx = self.wallet.rpc.get_raw_transaction(&contract_txid, None)
            .map_err(|e| TakerError::Wallet(crate::wallet::WalletError::Rpc(e)))?;
        let contract_amount = contract_tx.output[0].value;

        // Reconstruct the contract script using the same method as during contract creation
        let hashlock_script = self.ongoing_swap_state.outgoing_contract_hashlock_script
            .clone()
            .ok_or_else(|| TakerError::General("No hashlock script for outgoing contract".to_string()))?;
        let timelock_script = self.ongoing_swap_state.outgoing_contract_timelock_script
            .clone()
            .ok_or_else(|| TakerError::General("No timelock script for outgoing contract".to_string()))?;
        
        use crate::protocol::contract2::create_taproot_script;
        let (contract_script, _) = create_taproot_script(hashlock_script, timelock_script, internal_key);

        // Create prevout for sighash calculation
        let prevout = TxOut {
            value: contract_amount,
            script_pubkey: contract_script,
        };
        let prevouts = vec![prevout];
        let prevouts_ref = Prevouts::All(&prevouts);

        // Calculate sighash for the maker's spending transaction
        let mut sighasher = SighashCache::new(first_maker_spending_tx);
        let sighash = sighasher
            .taproot_key_spend_signature_hash(0, &prevouts_ref, TapSighashType::Default)
            .map_err(|_| TakerError::General("Failed to compute sighash for taker partial signature".to_string()))?;
        let message = Message::from(sighash);

        // Use lexicographic ordering for consistency
        let mut ordered_pubkeys = vec![taker_keypair.public_key(), first_maker_pubkey.inner];
        ordered_pubkeys.sort_by(|a, b| a.serialize().cmp(&b.serialize()));

        // Generate taker's nonce for this signature
        let (taker_sec_nonce, taker_pub_nonce) = generate_new_nonce_pair_i(
            tap_tweak,
            ordered_pubkeys[0], // lexicographically first pubkey
            ordered_pubkeys[1], // lexicographically second pubkey
            taker_keypair.public_key(), // Signer is taker
            message,
        );

        // Get the maker's receiver nonce from their earlier response (step 17)
        let maker_receiver_nonce = self.ongoing_swap_state.maker_receiver_nonces[0]
            .as_ref()
            .ok_or_else(|| TakerError::General("No receiver nonce stored for first maker".to_string()))?;
        let maker_pub_nonce: secp256k1::musig::PublicNonce = maker_receiver_nonce.clone().into();
        
        // DETAILED LOGGING FOR TAKER SIGNATURE GENERATION
        log::info!("=== TAKER GENERATING PARTIAL SIG FOR FIRST MAKER ===");
        log::info!("Contract txid being spent: {:?}", contract_txid);
        log::info!("Message (sighash): {:?}", message);
        log::info!("Taker pubkey: {:?}", taker_keypair.public_key().serialize());
        log::info!("First maker pubkey: {:?}", first_maker_pubkey.inner.serialize()); 
        log::info!("Ordered pubkeys: {:?}", ordered_pubkeys.iter().map(|p| p.serialize()).collect::<Vec<_>>());
        log::info!("Tap tweak: {:?}", tap_tweak);
        log::info!("Internal key: {:?}", internal_key);
        log::info!("Taker NEW nonce: {:?}", taker_pub_nonce.serialize());
        log::info!("Maker receiver nonce (from storage): {:?}", maker_pub_nonce.serialize());
        // Use the same pubkey-based nonce ordering as the maker
        log::info!("PUBKEY COMPARISON DEBUG:");
        log::info!("  ordered_pubkeys[0]: {:?}", ordered_pubkeys[0].serialize());
        log::info!("  taker_keypair.public_key(): {:?}", taker_keypair.public_key().serialize());
        log::info!("  Are they equal (==)? {}", ordered_pubkeys[0] == taker_keypair.public_key());
        log::info!("  Are they equal (serialize)? {}", ordered_pubkeys[0].serialize() == taker_keypair.public_key().serialize());
        
        let nonces = if ordered_pubkeys[0].serialize() == taker_keypair.public_key().serialize() {
            log::info!("Nonce ordering: [taker_nonce, maker_nonce] (taker is first in pubkey order)");
            vec![&taker_pub_nonce, &maker_pub_nonce]
        } else {
            log::info!("Nonce ordering: [maker_nonce, taker_nonce] (maker is first in pubkey order)");
            vec![&maker_pub_nonce, &taker_pub_nonce]
        };
        let aggregated_nonce = crate::protocol::musig_interface::get_aggregated_nonce_i(&nonces);
        log::info!("Aggregated nonce: {:?}", aggregated_nonce.serialize());

        // Generate taker's partial signature for the Taker→Maker0 contract
        let taker_partial_sig = generate_partial_signature_i(
            message,
            &aggregated_nonce,
            taker_sec_nonce,
            taker_keypair,
            tap_tweak,
            ordered_pubkeys[0], // lexicographically first pubkey
            ordered_pubkeys[1], // lexicographically second pubkey
        );
        
        log::info!("Generated taker partial signature: {:?}", taker_partial_sig.serialize());
        log::info!("Sending to maker: partial_sig={:?}, sender_nonce={:?}", 
                   taker_partial_sig.serialize(), taker_pub_nonce.serialize());
        log::info!("=== END TAKER SIGNATURE GENERATION ===");

        Ok(crate::protocol::messages2::PartialSigAndSendersNonce {
            partial_signatures: vec![taker_partial_sig.into()],
            sender_nonce: taker_pub_nonce.into(),
        })
    }
}
