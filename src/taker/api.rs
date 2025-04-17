//! The Taker API.
//!
//! This module describes the main [Taker] structure and all other associated data sets related to a coinswap round.
//!
//! [TakerConfig]: Set of configuration parameters defining [Taker]'s behavior.
//! [SwapParams]: Set of parameters defining a specific Swap round.
//! [OngoingSwapState]: Represents the State of an ongoing swap round. All swap related data are stored in this state.
//!
//! [Taker::do_coinswap]: The routine running all other protocol subroutines.

use std::{
    collections::{HashMap, HashSet},
    io::BufWriter,
    net::TcpStream,
    path::PathBuf,
    thread::sleep,
    time::{Duration, Instant},
};

use bitcoind::bitcoincore_rpc::RpcApi;

use serde::{Deserialize, Serialize};
use socks::Socks5Stream;

use bitcoin::{
    absolute::LockTime,
    consensus::encode::deserialize,
    hashes::{hash160::Hash as Hash160, Hash},
    hex::{Case, DisplayHex},
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        SecretKey,
    },
    Amount, BlockHash, OutPoint, PublicKey, ScriptBuf, Transaction, Txid,
};

use super::{
    error::TakerError,
    offers::{fetch_addresses_from_dns, fetch_offer_from_makers, MakerAddress, OfferAndAddress},
    routines::*,
};
use crate::{
    protocol::{
        error::ProtocolError,
        messages::{
            ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForRecvrAndSender,
            ContractSigsForSender, FundingTxInfo, MultisigPrivkey, Preimage, PrivKeyHandover,
            TakerToMakerMessage,
        },
    },
    taker::{config::TakerConfig, offers::OfferBook},
    utill::*,
    wallet::{
        IncomingSwapCoin, OutgoingSwapCoin, RPCConfig, SwapCoin, Wallet, WalletError,
        WalletSwapCoin, WatchOnlySwapCoin,
    },
};

// Default values for Taker configurations
pub(crate) const REFUND_LOCKTIME: u16 = 20;
pub(crate) const REFUND_LOCKTIME_STEP: u16 = 20;
pub(crate) const FIRST_CONNECT_ATTEMPTS: u32 = 5;
pub(crate) const FIRST_CONNECT_SLEEP_DELAY_SEC: u64 = 1;
pub(crate) const FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC: u64 = 30;

// Tries reconnection by variable delay.
// First 10 attempts at 1 sec interval.
// Next 5 attempts by 30 sec interval.
// This is useful to cater for random network failure.
pub(crate) const RECONNECT_ATTEMPTS: u32 = 10;
pub(crate) const RECONNECT_SHORT_SLEEP_DELAY: u64 = 1;
pub(crate) const RECONNECT_LONG_SLEEP_DELAY: u64 = 5;
pub(crate) const SHORT_LONG_SLEEP_DELAY_TRANSITION: u32 = 30;
pub(crate) const TCP_TIMEOUT_SECONDS: u64 = 300;
// TODO: Maker should decide this miner fee
// This fee is used for both funding and contract txs.
#[cfg(feature = "integration-test")]
pub(crate) const MINER_FEE: u64 = 1000;

/// This fee is used for both funding and contract txs.
#[cfg(not(feature = "integration-test"))]
pub(crate) const MINER_FEE: u64 = 300; // around 2 sats/vb for funding tx

/// Swap specific parameters. These are user's policy and can differ among swaps.
/// SwapParams govern the criteria to find suitable set of makers from the offerbook.
///
/// If no maker matches with a given SwapParam, that coinswap round will fail.
#[derive(Debug, Default, Clone, Copy)]
pub struct SwapParams {
    /// Total Amount to Swap.
    pub send_amount: Amount,
    /// How many hops.
    pub maker_count: usize,
    /// How many splits
    pub tx_count: u32,
    // TODO: Following two should be moved to TakerConfig as global configuration.
    /// Confirmation count required for funding txs.
    pub required_confirms: u32,
}

// Defines the Taker's position in the current ongoing swap.
#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum TakerPosition {
    #[default]
    /// Taker is the First Peer of the swap (Sender Side)
    FirstPeer,
    /// Swap Happening between Makers, Taker is in WatchOnly mode.
    WatchOnly,
    /// Taker is the last peer of the swap (Receiver Side)
    LastPeer,
}

/// The Swap State defining a current ongoing swap. This structure is managed by the Taker while
/// performing a swap. Various data are appended into the lists and are oly read from the last entry as the
/// swap progresses. This ensures the swap state is always consistent.
///
/// This states can be used to recover from a failed swap round.
#[derive(Default)]
struct OngoingSwapState {
    /// SwapParams used in current swap round.
    pub(crate) swap_params: SwapParams,
    /// SwapCoins going out from the Taker.
    pub(crate) outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    /// SwapCoins between Makers.
    pub(crate) watchonly_swapcoins: Vec<Vec<WatchOnlySwapCoin>>,
    /// SwapCoins received by the Taker.
    pub(crate) incoming_swapcoins: Vec<IncomingSwapCoin>,
    /// Information regarding all the swap participants (Makers).
    /// The last entry at the end of the swap round will be the Taker, as it's the last peer.
    pub(crate) peer_infos: Vec<NextPeerInfo>,
    /// List of funding transactions with optional merkleproofs.
    pub(crate) funding_txs: Vec<(Vec<Transaction>, Vec<String>)>,
    /// The preimage being used for this coinswap round.
    pub(crate) active_preimage: Preimage,
    /// Enum defining the position of the Taker at each steps of a multihop swap.
    pub(crate) taker_position: TakerPosition,
    /// Unique ID for a swap
    pub(crate) id: String,
}

/// Information for the next maker in the hop.
#[derive(Debug, Clone)]
struct NextPeerInfo {
    peer: OfferAndAddress,
    multisig_pubkeys: Vec<PublicKey>,
    multisig_nonces: Vec<SecretKey>,
    hashlock_nonces: Vec<SecretKey>,
    contract_reedemscripts: Vec<ScriptBuf>,
}

/// Enum representing different behaviors of the Taker in a coinswap protocol.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum TakerBehavior {
    /// No special behavior.
    Normal,
    /// This depicts the behavior when the taker drops connections after the full coinswap setup.
    DropConnectionAfterFullSetup,
    /// Behavior to broadcast the contract after the full coinswap setup.
    BroadcastContractAfterFullSetup,
}

/// The Taker structure that performs bulk of the coinswap protocol. Taker connects
/// to multiple Makers and send protocol messages sequentially to them. The communication
///
/// sequence and corresponding SwapCoin infos are stored in `ongoing_swap_state`.
pub struct Taker {
    wallet: Wallet,
    /// Taker configuration with refund, connection, and sleep settings.
    pub config: TakerConfig,
    offerbook: OfferBook,
    ongoing_swap_state: OngoingSwapState,
    behavior: TakerBehavior,
    data_dir: PathBuf,
}

impl Drop for Taker {
    fn drop(&mut self) {
        log::info!("Shutting down taker.");
        self.offerbook
            .write_to_disk(&self.data_dir.join("offerbook.dat"))
            .unwrap();
        log::info!("offerbook data saved to disk.");
        self.wallet.save_to_disk().unwrap();
        log::info!("Wallet data saved to disk.");
    }
}

impl Taker {
    // ######## MAIN PUBLIC INTERFACE ############

    ///  Initializes a Maker structure.
    ///
    /// This function sets up a Maker instance with configurable parameters.
    /// It handles the initialization of data directories, wallet files, and RPC configurations.
    ///
    /// ### Parameters:
    /// - `data_dir`:
    ///   - `Some(value)`: Use the specified directory for storing data.
    ///   - `None`: Use the default data directory (e.g., for Linux: `~/.coinswap/taker`).
    /// - `wallet_file_name`:
    ///   - `Some(value)`: Attempt to load a wallet file named `value`. If it does not exist, a new wallet with the given name will be created.
    ///   - `None`: Create a new wallet file with the default name `taker-wallet`.
    /// - If `rpc_config` = `None`: Use the default [`RPCConfig`]
    pub fn init(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        behavior: TakerBehavior,
        control_port: Option<u16>,
        tor_auth_password: Option<String>,
        connection_type: Option<ConnectionType>,
    ) -> Result<Taker, TakerError> {
        // Get provided data directory or the default data directory.
        let data_dir = data_dir.unwrap_or(get_taker_dir());
        let wallets_dir = data_dir.join("wallets");

        // Use the provided name or default to `taker-wallet` if not specified.
        let wallet_file_name = wallet_file_name.unwrap_or_else(|| "taker-wallet".to_string());
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
        let mut config = TakerConfig::new(Some(&data_dir.join("config.toml")))?;

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

        // Load offerbook. If doesn't exists, creates fresh file.
        let offerbook_path = data_dir.join("offerbook.dat");
        let offerbook = if offerbook_path.exists() {
            // If read fails, recreate a fresh offerbook.
            match OfferBook::read_from_disk(&offerbook_path) {
                Ok(offerbook) => {
                    log::info!("Succesfully loaded offerbook at : {:?}", offerbook_path);
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
            // Crewate a new offer book
            let empty_book = OfferBook::default();
            let file = std::fs::File::create(&offerbook_path)?;
            let writer = BufWriter::new(file);
            serde_cbor::to_writer(writer, &empty_book)?;
            empty_book
        };

        log::info!("Initializing wallet sync");
        wallet.sync()?;
        log::info!("Completed wallet sync");

        Ok(Self {
            wallet,
            config,
            offerbook,
            ongoing_swap_state: OngoingSwapState::default(),
            behavior,
            data_dir,
        })
    }

    /// Get wallet
    pub fn get_wallet(&self) -> &Wallet {
        &self.wallet
    }

    /// Get mutable reference to wallet
    pub fn get_wallet_mut(&mut self) -> &mut Wallet {
        &mut self.wallet
    }

    ///  Does the coinswap process
    pub fn do_coinswap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
        self.send_coinswap(swap_params)
    }

    /// Perform a coinswap round with given [SwapParams]. The Taker will try to perform swap with makers
    /// in it's [OfferBook] sequentially as per the maker_count given in swap params.
    /// If [SwapParams] doesn't fit suitably with any available offers, or not enough makers
    /// respond back, the swap round will fail.
    ///
    /// Depending upon the failure situation, Taker will automatically try to recover from failed swaps
    /// by executing the contract txs. If that fails too for any reason, user should manually call the [Taker::recover_from_swap].
    ///
    /// If that fails too. Open an issue at [our github](https://github.com/citadel-tech/coinswap/issues)
    pub(crate) fn send_coinswap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
        // Check if we have enough balance.
        let available = self.wallet.get_balances()?.spendable;

        // TODO: Make more exact estimate of swap cost and ensure balance.
        // For now ensure at least swap_amount + 1000 sats is available.
        let required = swap_params.send_amount + Amount::from_sat(1000);
        if available < required {
            let err = WalletError::InsufficientFund {
                available: available.to_sat(),
                required: required.to_sat(),
            };
            log::error!("Not enough balance to do swap : {:?}", err);
            return Err(err.into());
        }

        log::info!("Syncing Offerbook");
        self.sync_offerbook()?;

        // Error early if hop_count > available good makers.
        if swap_params.maker_count > self.offerbook.all_good_makers().len() {
            log::error!(
                "Not enough makers in the offerbook. Required {}, avaialable {}",
                swap_params.maker_count,
                self.offerbook.all_good_makers().len()
            );
            return Err(TakerError::NotEnoughMakersInOfferBook);
        }

        // Error early if less than 2 makers.
        if swap_params.maker_count < 2 {
            log::error!("Cannot swap with less than 2 makers");
            return Err(ProtocolError::General("Swap maker count < 2").into());
        }

        // Generate new random preimage and initiate the first hop.
        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        let unique_id = preimage[0..8].to_hex_string(Case::Lower);

        log::info!("Initiating coinswap with id : {}", unique_id);

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.swap_params = swap_params;
        self.ongoing_swap_state.id = unique_id;

        // Try first hop. Abort if error happens.
        if let Err(e) = self.init_first_hop() {
            log::error!("Could not initiate first hop: {:?}", e);
            self.recover_from_swap()?;
            return Err(e);
        }

        // Iterate until `maker_count` numbers of Makers are found and initiate swap between them sequentially.
        for maker_index in 0..self.ongoing_swap_state.swap_params.maker_count {
            if maker_index == 0 {
                self.ongoing_swap_state.taker_position = TakerPosition::FirstPeer;
            } else if maker_index == self.ongoing_swap_state.swap_params.maker_count - 1 {
                self.ongoing_swap_state.taker_position = TakerPosition::LastPeer;
            } else {
                self.ongoing_swap_state.taker_position = TakerPosition::WatchOnly;
            }

            // Refund lock time decreases by `refund_locktime_step` for each hop.
            let maker_refund_locktime = REFUND_LOCKTIME
                + REFUND_LOCKTIME_STEP
                    * (self.ongoing_swap_state.swap_params.maker_count - maker_index - 1) as u16;

            let funding_tx_infos = self.funding_info_for_next_maker();

            // Attempt to initiate the next hop of the swap. If anything goes wrong, abort immediately.
            // If succeeded, collect the funding_outpoints and multisig_reedemscripts of the next hop.
            // If error then aborts from current swap. Ban the Peer.
            let (funding_outpoints, multisig_reedemscripts) =
                match self.send_sigs_init_next_hop(maker_refund_locktime, &funding_tx_infos) {
                    Ok((next_peer_info, contract_sigs)) => {
                        self.ongoing_swap_state.peer_infos.push(next_peer_info);
                        let multisig_reedemscripts = contract_sigs
                            .senders_contract_txs_info
                            .iter()
                            .map(|senders_contract_tx_info| {
                                senders_contract_tx_info.multisig_redeemscript.clone()
                            })
                            .collect::<Vec<_>>();
                        let funding_outpoints = contract_sigs
                            .senders_contract_txs_info
                            .iter()
                            .map(|senders_contract_tx_info| {
                                senders_contract_tx_info.contract_tx.input[0].previous_output
                            })
                            .collect::<Vec<OutPoint>>();

                        (funding_outpoints, multisig_reedemscripts)
                    }
                    Err(e) => {
                        log::error!("Could not initiate next hop. Error : {:?}", e);
                        log::warn!("Starting recovery from existing swap");
                        self.recover_from_swap()?;
                        return Ok(());
                    }
                };

            // Watch for both expected and unexpected transactions.
            // This errors in two cases.
            // TakerError::ContractsBroadcasted and TakerError::FundingTxWaitTimeOut.
            // For all cases, abort from swap immediately.
            // For the timeout case also ban the Peer.
            let txids_to_watch = funding_outpoints.iter().map(|op| op.txid).collect();
            match self.watch_for_txs(&txids_to_watch) {
                Ok(r) => self.ongoing_swap_state.funding_txs.push(r),
                Err(e) => {
                    log::error!("Error: {:?}", e);
                    log::warn!("Starting recovery from existing swap");
                    if let TakerError::FundingTxWaitTimeOut = e {
                        let bad_maker = &self.ongoing_swap_state.peer_infos[maker_index].peer;
                        self.offerbook.add_bad_maker(bad_maker);
                    }
                    self.recover_from_swap()?;
                    return Ok(());
                }
            }

            // For the last hop, initiate the incoming swapcoins, and request the sigs for it.
            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                let incoming_swapcoins =
                    self.create_incoming_swapcoins(multisig_reedemscripts, funding_outpoints)?;
                log::debug!("Incoming Swapcoins: {:?}", incoming_swapcoins);
                self.ongoing_swap_state.incoming_swapcoins = incoming_swapcoins;
                match self.request_sigs_for_incoming_swap() {
                    Ok(_) => (),
                    Err(e) => {
                        log::error!("Incoming SwapCoin Generation failed : {:?}", e);
                        log::warn!("Starting recovery from existing swap");
                        self.recover_from_swap()?;
                        return Ok(());
                    }
                }
            }
        } // Contract establishment completed.

        if self.behavior == TakerBehavior::DropConnectionAfterFullSetup {
            log::error!("Dropping Swap Process after full setup");
            return Ok(());
        }

        if self.behavior == TakerBehavior::BroadcastContractAfterFullSetup {
            log::error!("Special Behavior BroadcastContractAfterFullSetup");
            self.recover_from_swap()?;
            return Ok(());
        }

        match self.settle_all_swaps() {
            Ok(_) => (),
            Err(e) => {
                log::error!("Swap Settlement Failed : {:?}", e);
                log::warn!("Starting recovery from existing swap");
                self.recover_from_swap()?;
                return Ok(());
            }
        }

        log::info!("Initializing Sync and Save.");
        self.save_and_reset_swap_round()?;
        log::info!("Completed Sync and Save.");
        log::info!("Successfully Completed Coinswap.");
        Ok(())
    }

    // ######## PROTOCOL SUBROUTINES ############

    /// Initiate the first coinswap hop. Makers are selected from the [OfferBook], and round will
    /// fail if no suitable makers are found.
    /// Creates and stores the [OutgoingSwapCoin] into [OngoingSwapState], and also saves it into the [Wallet] file.
    fn init_first_hop(&mut self) -> Result<(), TakerError> {
        log::info!("Initializing First Hop.");
        // Set the Taker Position state
        self.ongoing_swap_state.taker_position = TakerPosition::FirstPeer;

        // Locktime to be used for this swap.
        let swap_locktime = REFUND_LOCKTIME
            + REFUND_LOCKTIME_STEP * self.ongoing_swap_state.swap_params.maker_count as u16;

        // Loop until we find a live maker who responded to our signature request.
        let (maker, funding_txs) = loop {
            let maker = self.choose_next_maker()?.clone();
            log::info!("Choosing next maker: {}", maker.address);
            let (multisig_pubkeys, multisig_nonces, hashlock_pubkeys, hashlock_nonces) =
                generate_maker_keys(
                    &maker.offer.tweakable_point,
                    self.ongoing_swap_state.swap_params.tx_count,
                )?;
            let (funding_txs, mut outgoing_swapcoins, funding_fee) =
                self.wallet.initalize_coinswap(
                    self.ongoing_swap_state.swap_params.send_amount,
                    &multisig_pubkeys,
                    &hashlock_pubkeys,
                    self.get_preimage_hash(),
                    swap_locktime,
                    Amount::from_sat(MINER_FEE),
                )?;

            let contract_reedemscripts = outgoing_swapcoins
                .iter()
                .map(|swapcoin| swapcoin.contract_redeemscript.clone())
                .collect();

            // Request for Sender's Signatures
            let contract_sigs = match self.req_sigs_for_sender(
                &maker.address,
                &outgoing_swapcoins,
                &multisig_nonces,
                &hashlock_nonces,
                swap_locktime,
            ) {
                Ok(contract_sigs) => contract_sigs,
                Err(e) => {
                    // Bad maker, mark it, and try next one.
                    self.offerbook.add_bad_maker(&maker);
                    log::error!(
                        "Failed to obtain sender's contract signatures from first_maker {}: {:?}",
                        maker.address,
                        e
                    );
                    continue;
                }
            };

            // // Maker has returned a valid signature, save all the data in memory,
            // // and persist in disk.
            self.ongoing_swap_state.peer_infos.push(NextPeerInfo {
                peer: maker.clone(),
                multisig_pubkeys,
                multisig_nonces,
                hashlock_nonces,
                contract_reedemscripts,
            });

            contract_sigs
                .sigs
                .iter()
                .zip(outgoing_swapcoins.iter_mut())
                .for_each(|(sig, outgoing_swapcoin)| {
                    outgoing_swapcoin.others_contract_sig = Some(*sig);
                });

            for outgoing_swapcoin in &outgoing_swapcoins {
                self.wallet.add_outgoing_swapcoin(outgoing_swapcoin);
            }
            self.wallet.save_to_disk()?;

            self.ongoing_swap_state.outgoing_swapcoins = outgoing_swapcoins;

            log::info!("Total Funding Txs Fees: {}", funding_fee);

            break (maker, funding_txs);
        };

        log::debug!(
            "Outgoing SwapCoins: {:?}",
            self.ongoing_swap_state.outgoing_swapcoins
        );

        // Broadcast and wait for funding txs to confirm
        let funding_txids = funding_txs
            .iter()
            .map(|tx| {
                // Calculate the virtual size in bytes (vbytes)
                let tx_vbytes = tx.weight().to_vbytes_ceil();

                // Convert vbytes to kilovirtual bytes (kvB)
                let tx_kvb = (tx_vbytes as f32) / 1000.0;

                log::info!("Transaction size: {} vB ({:.3} kvB)", tx_vbytes, tx_kvb);

                let txid = self.wallet.send_tx(tx)?;
                log::info!("Broadcasted Funding tx. txid: {}", txid);
                assert_eq!(txid, tx.compute_txid());
                Ok(txid)
            })
            .collect::<Result<_, TakerError>>()?;

        // Watch for the funding transactions to be confirmed.
        // This errors in two cases.
        // TakerError::ContractsBroadcasted and TakerError::FundingTxWaitTimeOut.
        // For all cases, abort from swap immediately.
        // For the contract-broadcasted case also ban the Peer.
        match self.watch_for_txs(&funding_txids) {
            Ok(stuffs) => {
                self.ongoing_swap_state.funding_txs.push(stuffs);
            }
            Err(e) => {
                log::error!("Error: {:?}", e);
                if let TakerError::ContractsBroadcasted(_) = e {
                    self.offerbook.add_bad_maker(&maker);
                }
                return Err(e);
            }
        }

        Ok(())
    }

    /// Return a list of confirmed funding txs with their corresponding merkle proofs.
    /// Errors if any watching contract txs have been broadcasted during the time too.
    /// The error contanis the list of broadcasted contract [Txid]s.
    fn watch_for_txs(
        &self,
        funding_txids: &Vec<Txid>,
    ) -> Result<(Vec<Transaction>, Vec<String>), TakerError> {
        let mut txid_tx_map = HashMap::<Txid, Transaction>::new();
        let mut txid_blockhash_map = HashMap::<Txid, BlockHash>::new();

        // Find next maker's details
        let required_confirmations =
            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                self.ongoing_swap_state.swap_params.required_confirms
            } else {
                self.ongoing_swap_state
                    .peer_infos
                    .last()
                    .map(|npi| npi.peer.offer.required_confirms)
                    .expect("Maker information expected in swap state")
            };

        let maker_addrs = self
            .ongoing_swap_state
            .peer_infos
            .iter()
            .map(|npi| npi.peer.address.clone())
            .collect::<HashSet<_>>();

        log::info!(
            "Waiting for funding transaction confirmation. Txids : {:?}",
            funding_txids
        );

        // Wait for this much time for txs to appear in mempool.
        let mempool_wait_timeout = if cfg!(feature = "integration-test") {
            10u64 // 10 secs for the tests
        } else {
            60 * 5 // 5mins for production
        };

        // Check for funding confirmation at this frequency
        let sleep_interval = if cfg!(feature = "integration-test") {
            1u64 // 1 secs for the tests
        } else {
            30 // 30 secs for production
        };

        let start_time = Instant::now();

        loop {
            // Abort if any of the contract transaction is broadcasted
            // TODO: Find the culprit Maker, and ban it's fidelity bond.
            let contracts_broadcasted = self.check_for_broadcasted_contract_txes();
            if !contracts_broadcasted.is_empty() {
                log::error!(
                    "Fatal! Contract txs broadcasted by makers. Txids : {:?}",
                    contracts_broadcasted
                );
                return Err(TakerError::ContractsBroadcasted(contracts_broadcasted));
            }

            // Check for each funding transactions if they are confirmed
            for txid in funding_txids {
                if txid_tx_map.contains_key(txid) {
                    continue;
                }
                let gettx = match self.wallet.rpc.get_raw_transaction_info(txid, None) {
                    Ok(r) => r,
                    // Transaction haven't arrived in our mempool, keep looping.
                    Err(_e) => {
                        let elapsed = start_time.elapsed().as_secs();
                        log::info!(
                            "Waiting for funding tx to appear in mempool | {} secs",
                            elapsed
                        );
                        if elapsed > mempool_wait_timeout {
                            log::error!("Timed out waiting for funding tx to appear in mempool. | No tx seen in {} secs", elapsed);
                            return Err(TakerError::FundingTxWaitTimeOut);
                        }
                        continue;
                    }
                };

                // log that its waiting for confirmation.
                if gettx.confirmations.is_none() {
                    let elapsed = start_time.elapsed().as_secs();
                    log::info!(
                        "Funding tx Seen in Mempool. Waiting for confirmation for {} secs",
                        elapsed,
                    );

                    // Send wait-notif to all makers
                    for addr in &maker_addrs {
                        // Ignore transient network error and retry in next loop.
                        // It's safe to ignore the error here, because if the maker is actually offline, the swap will fail in the later stages.
                        if let Err(e) = self.send_to_maker(
                            addr,
                            TakerToMakerMessage::WaitingFundingConfirmation(
                                self.ongoing_swap_state.id.clone(),
                            ),
                        ) {
                            log::error!("error sending wait-notif to maker {} | {:?}", addr, e);
                        }
                    }
                }

                // handle confirmations
                //TODO handle confirm<0
                if gettx.confirmations >= Some(required_confirmations) {
                    txid_tx_map.insert(
                        *txid,
                        deserialize::<Transaction>(&gettx.hex).map_err(WalletError::from)?,
                    );
                    txid_blockhash_map.insert(*txid, gettx.blockhash.expect("Blockhash expected"));
                    log::info!("Tx {} | Confirmed at {}", txid, required_confirmations);
                }
            }
            if txid_tx_map.len() == funding_txids.len() {
                let txes = funding_txids
                    .iter()
                    .map(|txid| {
                        txid_tx_map
                            .get(txid)
                            .expect("txid expected in the map")
                            .clone()
                    })
                    .collect::<Vec<Transaction>>();
                let merkleproofs = funding_txids
                    .iter()
                    .map(|&txid| {
                        self.wallet
                            .rpc
                            .get_tx_out_proof(
                                &[txid],
                                Some(
                                    txid_blockhash_map
                                        .get(&txid)
                                        .expect("txid expected in the map"),
                                ),
                            )
                            .map(|gettxoutproof_result| gettxoutproof_result.to_lower_hex_string())
                    })
                    .collect::<Result<Vec<String>, _>>()
                    .map_err(WalletError::from)?;
                return Ok((txes, merkleproofs));
            }
            sleep(Duration::from_secs(sleep_interval));
        }
    }

    /// Create [FundingTxInfo] for the "next_maker". Next maker is the last stored [NextPeerInfo] in the swp state.
    /// All other data from the swap state's last entries are collected and a [FundingTxInfo] protocol message data is generated.
    fn funding_info_for_next_maker(&self) -> Vec<FundingTxInfo> {
        // Get the reedemscripts.
        let (this_maker_multisig_redeemscripts, this_maker_contract_redeemscripts) =
            if self.ongoing_swap_state.taker_position == TakerPosition::FirstPeer {
                (
                    self.ongoing_swap_state
                        .outgoing_swapcoins
                        .iter()
                        .map(|s| s.get_multisig_redeemscript())
                        .collect::<Vec<_>>(),
                    self.ongoing_swap_state
                        .outgoing_swapcoins
                        .iter()
                        .map(|s| s.get_contract_redeemscript())
                        .collect::<Vec<_>>(),
                )
            } else {
                (
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .last()
                        .expect("swapcoin expected")
                        .iter()
                        .map(|s| s.get_multisig_redeemscript())
                        .collect::<Vec<_>>(),
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .last()
                        .expect("swapcoin expected")
                        .iter()
                        .map(|s| s.get_contract_redeemscript())
                        .collect::<Vec<_>>(),
                )
            };

        // Get the nonces.
        let maker_multisig_nonces = self
            .ongoing_swap_state
            .peer_infos
            .last()
            .expect("maker should exist")
            .multisig_nonces
            .iter();
        let maker_hashlock_nonces = self
            .ongoing_swap_state
            .peer_infos
            .last()
            .expect("maker should exist")
            .hashlock_nonces
            .iter();

        // Get the funding txs and merkle proofs.
        let (funding_txs, funding_txs_merkleproof) = self
            .ongoing_swap_state
            .funding_txs
            .last()
            .expect("funding txs should be known");

        let funding_tx_infos = funding_txs
            .iter()
            .zip(funding_txs_merkleproof.iter())
            .zip(this_maker_multisig_redeemscripts.iter())
            .zip(maker_multisig_nonces)
            .zip(this_maker_contract_redeemscripts.iter())
            .zip(maker_hashlock_nonces)
            .map(
                |(
                    (
                        (
                            (
                                (funding_tx, funding_tx_merkle_proof),
                                this_maker_multisig_reedeemscript,
                            ),
                            maker_multisig_nonce,
                        ),
                        this_maker_contract_reedemscript,
                    ),
                    maker_hashlock_nonce,
                )| {
                    FundingTxInfo {
                        funding_tx: funding_tx.clone(),
                        funding_tx_merkleproof: funding_tx_merkle_proof.clone(),
                        multisig_redeemscript: this_maker_multisig_reedeemscript.clone(),
                        multisig_nonce: *maker_multisig_nonce,
                        contract_redeemscript: this_maker_contract_reedemscript.clone(),
                        hashlock_nonce: *maker_hashlock_nonce,
                    }
                },
            )
            .collect::<Vec<_>>();

        funding_tx_infos
    }

    /// Send signatures to a maker, and initiate the next hop of the swap by finding a new maker.
    /// If no suitable makers are found in [OfferBook], next swap will not initiate and the swap round will fail.
    fn send_sigs_init_next_hop(
        &mut self,
        maker_refund_locktime: u16,
        funding_tx_infos: &[FundingTxInfo],
    ) -> Result<(NextPeerInfo, ContractSigsAsRecvrAndSender), TakerError> {
        // Configurable reconnection attempts for testing
        let reconnect_attempts = if cfg!(feature = "integration-test") {
            10
        } else {
            RECONNECT_ATTEMPTS
        };

        // Custom sleep delay for testing.
        let sleep_delay = if cfg!(feature = "integration-test") {
            1
        } else {
            RECONNECT_SHORT_SLEEP_DELAY
        };

        let mut ii = 0;

        let maker_oa = self
            .ongoing_swap_state
            .peer_infos
            .last()
            .expect("at least one active maker expected")
            .peer
            .clone();

        loop {
            ii += 1;
            match self.send_sigs_init_next_hop_once(maker_refund_locktime, funding_tx_infos) {
                Ok(ret) => return Ok(ret),
                Err(e) => {
                    log::warn!(
                        "Failed to connect to maker {} to send signatures and init next hop, \
                            reattempting {} of {} | error={:?}",
                        &maker_oa.address,
                        ii,
                        reconnect_attempts,
                        e
                    );
                    if ii <= reconnect_attempts {
                        sleep(Duration::from_secs(
                            if ii <= SHORT_LONG_SLEEP_DELAY_TRANSITION {
                                sleep_delay
                            } else {
                                RECONNECT_LONG_SLEEP_DELAY
                            },
                        ));
                        continue;
                    } else {
                        self.offerbook.add_bad_maker(&maker_oa);
                        return Err(e);
                    }
                }
            }
        }
    }

    /// [Internal] Single attempt to send signatures and initiate next hop.
    fn send_sigs_init_next_hop_once(
        &mut self,
        maker_refund_locktime: u16,
        funding_tx_infos: &[FundingTxInfo],
    ) -> Result<(NextPeerInfo, ContractSigsAsRecvrAndSender), TakerError> {
        let this_maker = &self
            .ongoing_swap_state
            .peer_infos
            .last()
            .expect("at least one active maker expected")
            .peer;

        let previous_maker = self.ongoing_swap_state.peer_infos.iter().rev().nth(1);

        log::info!(
            "Connecting to {} | Send Sigs Init Next Hop",
            this_maker.address
        );
        let address = this_maker.address.to_string();
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

        handshake_maker(&mut socket)?;
        let mut next_maker = this_maker.clone();
        let (
            next_peer_multisig_pubkeys,
            next_peer_multisig_keys_or_nonces,
            next_peer_hashlock_keys_or_nonces,
            contract_sigs_as_recvr_sender,
            next_swap_contract_redeemscripts,
            senders_sigs,
        ) = loop {
            //loop to help error handling, allowing us to keep trying new makers until
            //we find one for which our request is successful, or until we run out of makers
            let (
                next_peer_multisig_pubkeys,
                next_peer_multisig_keys_or_nonces,
                next_peer_hashlock_pubkeys,
                next_peer_hashlock_keys_or_nonces,
            ) = if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                let (my_recv_ms_pubkeys, my_recv_ms_nonce): (Vec<_>, Vec<_>) =
                    (0..self.ongoing_swap_state.swap_params.tx_count)
                        .map(|_| generate_keypair())
                        .unzip();
                let (my_recv_hashlock_pubkeys, my_recv_hashlock_nonce): (Vec<_>, Vec<_>) = (0
                    ..self.ongoing_swap_state.swap_params.tx_count)
                    .map(|_| generate_keypair())
                    .unzip();
                (
                    my_recv_ms_pubkeys,
                    my_recv_ms_nonce,
                    my_recv_hashlock_pubkeys,
                    my_recv_hashlock_nonce,
                )
            } else {
                next_maker = self.choose_next_maker()?.clone();
                //next_maker is only ever accessed when the next peer is a maker, not a taker
                //i.e. if its ever used when is_taker_next_peer == true, then thats a bug
                generate_maker_keys(
                    &next_maker.offer.tweakable_point,
                    self.ongoing_swap_state.swap_params.tx_count,
                )?
            };

            let this_maker_contract_txs =
                if self.ongoing_swap_state.taker_position == TakerPosition::FirstPeer {
                    self.ongoing_swap_state
                        .outgoing_swapcoins
                        .iter()
                        .map(|os| os.get_contract_tx())
                        .collect::<Vec<_>>()
                } else {
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .last()
                        .expect("at least one outgoing swpcoin expected")
                        .iter()
                        .map(|wos| wos.get_contract_tx())
                        .collect()
                };

            log::info!("===> ProofOfFunding | {}", this_maker.address);

            let funding_txids = funding_tx_infos
                .iter()
                .map(|fi| fi.funding_tx.compute_txid())
                .collect::<Vec<_>>();

            log::info!("Fundix Txids: {:?}", funding_txids);

            // Struct for information related to the next peer
            let next_maker_info = NextMakerInfo {
                next_peer_multisig_pubkeys: next_peer_multisig_pubkeys.clone(),
                next_peer_hashlock_pubkeys: next_peer_hashlock_pubkeys.clone(),
            };

            let this_maker_info = ThisMakerInfo {
                this_maker: this_maker.clone(),
                funding_tx_infos: funding_tx_infos.to_vec(),
                this_maker_contract_txs,
                this_maker_refund_locktime: maker_refund_locktime,
            };

            let (contract_sigs_as_recvr_sender, next_swap_contract_redeemscripts) =
                send_proof_of_funding_and_init_next_hop(
                    &mut socket,
                    this_maker_info,
                    next_maker_info,
                    self.get_preimage_hash(),
                    self.ongoing_swap_state.id.clone(),
                )?;
            log::info!(
                "<=== ReqContractSigsAsRecvrAndSender | {}",
                this_maker.address
            );

            // If This Maker is the Sender, and we (the Taker) are the Receiver (Last Hop). We provide the Sender's Contact Tx Sigs.
            let senders_sigs = if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer
            {
                log::info!("Taker is next peer. Signing Sender's Contract Txs");
                // Sign the seder's contract transactions with our multisig privkey.
                next_peer_multisig_keys_or_nonces
                    .iter()
                    .zip(
                        contract_sigs_as_recvr_sender
                            .senders_contract_txs_info
                            .iter(),
                    )
                    .map(
                        |(my_receiving_multisig_privkey, senders_contract_tx_info)| {
                            crate::protocol::contract::sign_contract_tx(
                                &senders_contract_tx_info.contract_tx,
                                &senders_contract_tx_info.multisig_redeemscript,
                                senders_contract_tx_info.funding_amount,
                                my_receiving_multisig_privkey,
                            )
                        },
                    )
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                // If Next Maker is the Receiver, and This Maker is The Sender, Request Sender's Contract Tx Sig to Next Maker.
                let watchonly_swapcoins = self.create_watch_only_swapcoins(
                    &contract_sigs_as_recvr_sender,
                    &next_peer_multisig_pubkeys,
                    &next_swap_contract_redeemscripts,
                )?;
                let sigs = match self.req_sigs_for_sender(
                    &next_maker.address,
                    &watchonly_swapcoins,
                    &next_peer_multisig_keys_or_nonces,
                    &next_peer_hashlock_keys_or_nonces,
                    maker_refund_locktime,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        self.offerbook.add_bad_maker(&next_maker);
                        log::info!(
                            "Failed to obtain sender's contract tx signature from next_maker {}, Banning Maker: {:?}",
                            next_maker.address,
                            e
                        );
                        continue; //go back to the start of the loop and try another maker
                    }
                };
                self.ongoing_swap_state
                    .watchonly_swapcoins
                    .push(watchonly_swapcoins);
                sigs.sigs
            };
            break (
                next_peer_multisig_pubkeys,
                next_peer_multisig_keys_or_nonces,
                next_peer_hashlock_keys_or_nonces,
                contract_sigs_as_recvr_sender,
                next_swap_contract_redeemscripts,
                senders_sigs,
            );
        };

        // If This Maker is the Reciver, and We (The Taker) are the Sender (First Hop), Sign the Contract Tx.
        let receivers_sigs = if self.ongoing_swap_state.taker_position == TakerPosition::FirstPeer {
            log::info!("Taker is previous peer. Signing Receivers Contract Txs");
            // Sign the receiver's contract using our [OutgoingSwapCoin].
            contract_sigs_as_recvr_sender
                .receivers_contract_txs
                .iter()
                .zip(self.ongoing_swap_state.outgoing_swapcoins.iter())
                .map(|(receivers_contract_tx, outgoing_swapcoin)| {
                    outgoing_swapcoin.sign_contract_tx_with_my_privkey(receivers_contract_tx)
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            // If Next Maker is the Receiver, and Previous Maker is the Sender, request Previous Maker to sign the Reciever's Contract Tx.
            let previous_maker = previous_maker.expect("Previous Maker should always exists");
            let previous_maker_addr = &previous_maker.peer.address;
            let previous_maker_watchonly_swapcoins =
                if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .last()
                        .expect("swapcoin expected")
                } else {
                    //if the next peer is a maker not a taker, then that maker's swapcoins are last
                    &self.ongoing_swap_state.watchonly_swapcoins
                        [self.ongoing_swap_state.watchonly_swapcoins.len() - 2]
                };

            match self.req_sigs_for_recvr(
                previous_maker_addr,
                previous_maker_watchonly_swapcoins,
                &contract_sigs_as_recvr_sender.receivers_contract_txs,
            ) {
                Ok(s) => s.sigs,
                Err(e) => {
                    log::error!("Could not get Receiver's signatures : {:?}", e);
                    log::warn!("Banning Maker : {}", previous_maker.peer.address);
                    self.offerbook.add_bad_maker(&previous_maker.peer);
                    return Err(e);
                }
            }
        };
        log::info!(
            "===> RespContractSigsForRecvrAndSender | {}",
            this_maker.address
        );
        let id = self.ongoing_swap_state.id.clone();
        send_message(
            &mut socket,
            &TakerToMakerMessage::RespContractSigsForRecvrAndSender(
                ContractSigsForRecvrAndSender {
                    receivers_sigs,
                    senders_sigs,
                    id,
                },
            ),
        )?;

        let next_swap_info = NextPeerInfo {
            peer: next_maker.clone(),
            multisig_pubkeys: next_peer_multisig_pubkeys,
            multisig_nonces: next_peer_multisig_keys_or_nonces,
            hashlock_nonces: next_peer_hashlock_keys_or_nonces,
            contract_reedemscripts: next_swap_contract_redeemscripts,
        };
        Ok((next_swap_info, contract_sigs_as_recvr_sender))
    }

    /// Create [WatchOnlySwapCoin] for the current Maker.
    pub(crate) fn create_watch_only_swapcoins(
        &self,
        contract_sigs_as_recvr_and_sender: &ContractSigsAsRecvrAndSender,
        next_peer_multisig_pubkeys: &[PublicKey],
        next_swap_contract_redeemscripts: &[ScriptBuf],
    ) -> Result<Vec<WatchOnlySwapCoin>, TakerError> {
        let next_swapcoins = contract_sigs_as_recvr_and_sender
            .senders_contract_txs_info
            .iter()
            .zip(next_peer_multisig_pubkeys.iter())
            .zip(next_swap_contract_redeemscripts.iter())
            .map(
                |((senders_contract_tx_info, &maker_multisig_pubkey), contract_redeemscript)| {
                    WatchOnlySwapCoin::new(
                        &senders_contract_tx_info.multisig_redeemscript,
                        maker_multisig_pubkey,
                        senders_contract_tx_info.contract_tx.clone(),
                        contract_redeemscript.clone(),
                        senders_contract_tx_info.funding_amount,
                    )
                },
            )
            .collect::<Result<Vec<WatchOnlySwapCoin>, _>>()?;
        //TODO error handle here the case where next_swapcoin.contract_tx script pubkey
        // is not equal to p2wsh(next_swap_contract_redeemscripts)
        for swapcoin in &next_swapcoins {
            self.wallet
                .import_watchonly_redeemscript(&swapcoin.get_multisig_redeemscript())?;
        }
        Ok(next_swapcoins)
    }

    /// Create the [IncomingSwapCoin] for this round. The Taker is always the "next_peer" here
    /// and the sender side is the laste Maker in the route.
    fn create_incoming_swapcoins(
        &mut self,
        multisig_redeemscripts: Vec<ScriptBuf>,
        funding_outpoints: Vec<OutPoint>,
    ) -> Result<Vec<IncomingSwapCoin>, TakerError> {
        let (funding_txs, funding_txs_merkleproofs) = self
            .ongoing_swap_state
            .funding_txs
            .last()
            .expect("funding transactions expected");

        let last_makers_funding_tx_values = funding_txs
            .iter()
            .zip(multisig_redeemscripts.iter())
            .map(|(makers_funding_tx, multisig_redeemscript)| {
                let multisig_spk = redeemscript_to_scriptpubkey(multisig_redeemscript)?;
                let index = makers_funding_tx
                    .output
                    .iter()
                    .enumerate()
                    .find(|(_i, o)| o.script_pubkey == multisig_spk)
                    .map(|(index, _)| index)
                    .expect("funding txout output doesn't match with mutlsig scriptpubkey");
                Ok(makers_funding_tx
                    .output
                    .get(index)
                    .expect("output expected at that index")
                    .value)
            })
            .collect::<Result<Vec<_>, TakerError>>()?;

        let my_receivers_contract_txes = funding_outpoints
            .iter()
            .zip(last_makers_funding_tx_values.iter())
            .zip(
                self.ongoing_swap_state
                    .peer_infos
                    .last()
                    .expect("expected")
                    .contract_reedemscripts
                    .iter(),
            )
            .map(
                |(
                    (&previous_funding_output, &maker_funding_tx_value),
                    next_contract_redeemscript,
                )| {
                    crate::protocol::contract::create_receivers_contract_tx(
                        previous_funding_output,
                        maker_funding_tx_value,
                        next_contract_redeemscript,
                        Amount::from_sat(MINER_FEE),
                    )
                },
            )
            .collect::<Result<Vec<Transaction>, _>>()?;

        let mut incoming_swapcoins = Vec::<IncomingSwapCoin>::new();
        let next_swap_info = self
            .ongoing_swap_state
            .peer_infos
            .last()
            .expect("next swap info expected");
        for (
            (
                (
                    (
                        (
                            (
                                (
                                    (multisig_redeemscript, &maker_funded_multisig_pubkey),
                                    &maker_funded_multisig_privkey,
                                ),
                                my_receivers_contract_tx,
                            ),
                            next_contract_redeemscript,
                        ),
                        &hashlock_privkey,
                    ),
                    &maker_funding_tx_value,
                ),
                _,
            ),
            _,
        ) in multisig_redeemscripts
            .iter()
            .zip(next_swap_info.multisig_pubkeys.iter())
            .zip(next_swap_info.multisig_nonces.iter())
            .zip(my_receivers_contract_txes.iter())
            .zip(next_swap_info.contract_reedemscripts.iter())
            .zip(next_swap_info.hashlock_nonces.iter())
            .zip(last_makers_funding_tx_values.iter())
            .zip(funding_txs.iter())
            .zip(funding_txs_merkleproofs.iter())
        {
            let (o_ms_pubkey1, o_ms_pubkey2) =
                crate::protocol::contract::read_pubkeys_from_multisig_redeemscript(
                    multisig_redeemscript,
                )?;
            let maker_funded_other_multisig_pubkey = if o_ms_pubkey1 == maker_funded_multisig_pubkey
            {
                o_ms_pubkey2
            } else {
                if o_ms_pubkey2 != maker_funded_multisig_pubkey {
                    return Err(ProtocolError::General("maker-funded multisig doesnt match").into());
                }
                o_ms_pubkey1
            };

            self.wallet.sync()?;

            let mut incoming_swapcoin = IncomingSwapCoin::new(
                maker_funded_multisig_privkey,
                maker_funded_other_multisig_pubkey,
                my_receivers_contract_tx.clone(),
                next_contract_redeemscript.clone(),
                hashlock_privkey,
                maker_funding_tx_value,
            )?;
            incoming_swapcoin.hash_preimage = Some(self.ongoing_swap_state.active_preimage);
            incoming_swapcoins.push(incoming_swapcoin);
        }

        Ok(incoming_swapcoins)
    }

    /// Request signatures for the [IncomingSwapCoin] from the last maker of the swap round.
    fn request_sigs_for_incoming_swap(&mut self) -> Result<(), TakerError> {
        // Intermediate hops completed. Perform the last receiving hop.
        let last_maker = self
            .ongoing_swap_state
            .peer_infos
            .iter()
            .rev()
            .nth(1)
            .expect("previous maker expected")
            .peer
            .clone();
        let receiver_contract_sig = match self.req_sigs_for_recvr(
            &last_maker.address,
            &self.ongoing_swap_state.incoming_swapcoins,
            &self
                .ongoing_swap_state
                .incoming_swapcoins
                .iter()
                .map(|swapcoin| swapcoin.contract_tx.clone())
                .collect::<Vec<Transaction>>(),
        ) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Banning Maker : {}", last_maker.address);
                self.offerbook.add_bad_maker(&last_maker);
                return Err(e);
            }
        };
        for (incoming_swapcoin, &receiver_contract_sig) in self
            .ongoing_swap_state
            .incoming_swapcoins
            .iter_mut()
            .zip(receiver_contract_sig.sigs.iter())
        {
            incoming_swapcoin.others_contract_sig = Some(receiver_contract_sig);
        }
        for incoming_swapcoin in &self.ongoing_swap_state.incoming_swapcoins {
            self.wallet.add_incoming_swapcoin(incoming_swapcoin);
        }

        self.wallet.save_to_disk()?;

        Ok(())
    }

    /// Request signatures for sender side of the swap.
    /// Keep trying until `first_connect_attempts` limit, with time delay of `first_connect_sleep_delay_sec`.
    fn req_sigs_for_sender<S: SwapCoin>(
        &self,
        maker_address: &MakerAddress,
        outgoing_swapcoins: &[S],
        maker_multisig_nonces: &[SecretKey],
        maker_hashlock_nonces: &[SecretKey],
        locktime: u16,
    ) -> Result<ContractSigsForSender, TakerError> {
        let reconnect_time_out = Duration::from_secs(FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC);
        // Configurable reconnection attempts for testing
        let first_connect_attempts = if cfg!(feature = "integration-test") {
            10
        } else {
            FIRST_CONNECT_ATTEMPTS
        };

        // Custom sleep delay for testing.
        let sleep_delay = if cfg!(feature = "integration-test") {
            1
        } else {
            FIRST_CONNECT_SLEEP_DELAY_SEC
        };

        let mut ii = 0;

        let maker_addr_str = maker_address.to_string();

        let mut socket = match self.config.connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(maker_addr_str.clone())?,
            ConnectionType::TOR => Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                &*maker_addr_str,
            )?
            .into_inner(),
        };

        socket.set_read_timeout(Some(reconnect_time_out))?;
        socket.set_write_timeout(Some(reconnect_time_out))?;

        loop {
            ii += 1;
            log::info!("===> ReqContractSigsForSender | {}", maker_addr_str);
            match req_sigs_for_sender_once(
                &mut socket,
                outgoing_swapcoins,
                maker_multisig_nonces,
                maker_hashlock_nonces,
                locktime,
            ) {
                Ok(ret) => {
                    return {
                        log::info!("<=== RespContractSigsForSender | {}", maker_addr_str);
                        Ok(ret)
                    }
                }
                Err(e) => {
                    log::warn!(
                        "Failed to connect to maker {} to request signatures for receiver, \
                                reattempting {} of {} | error={:?}",
                        &maker_addr_str,
                        ii,
                        first_connect_attempts,
                        e
                    );
                    if ii <= first_connect_attempts {
                        sleep(Duration::from_secs(
                            if ii <= SHORT_LONG_SLEEP_DELAY_TRANSITION {
                                sleep_delay
                            } else {
                                RECONNECT_LONG_SLEEP_DELAY
                            },
                        ));
                        continue;
                    } else {
                        log::warn!(
                            "Failed to connect to maker {} to request signatures for receiver, \
                                    reattempt limit exceeded",
                            &maker_addr_str,
                        );
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Request signatures for receiver side of the swap.
    /// Keep trying until `reconnect_attempts` limit, with a time delay.
    /// The time delay transitions from `reconnect_short_slepp_delay` to `reconnect_locg_sleep_delay`,
    /// after `short_long_sleep_delay_transition` time.
    fn req_sigs_for_recvr<S: SwapCoin>(
        &self,
        maker_address: &MakerAddress,
        incoming_swapcoins: &[S],
        receivers_contract_txes: &[Transaction],
    ) -> Result<ContractSigsForRecvr, TakerError> {
        let reconnect_time_out = Duration::from_secs(TCP_TIMEOUT_SECONDS);

        // Configurable reconnection attempts for testing
        let reconnect_attempts = if cfg!(feature = "integration-test") {
            10
        } else {
            RECONNECT_ATTEMPTS
        };

        // Custom sleep delay for testing.
        let sleep_delay = if cfg!(feature = "integration-test") {
            1
        } else {
            RECONNECT_SHORT_SLEEP_DELAY
        };

        let mut ii = 0;

        let maker_addr_str = maker_address.to_string();
        let mut socket = match self.config.connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(maker_addr_str.clone())?,
            ConnectionType::TOR => Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                &*maker_addr_str,
            )?
            .into_inner(),
        };

        socket.set_read_timeout(Some(reconnect_time_out))?;
        socket.set_write_timeout(Some(reconnect_time_out))?;

        loop {
            ii += 1;
            log::info!("===> ReqContractSigsForRecvr | {}", maker_addr_str);
            match req_sigs_for_recvr_once(&mut socket, incoming_swapcoins, receivers_contract_txes)
            {
                Ok(ret) => {
                    log::info!("<=== RespContractSigsForRecvr | {}", maker_addr_str);
                    return Ok(ret);
                }
                Err(e) => {
                    log::warn!(
                        "Failed to connect to maker {} to request signatures for receiver, \
                                reattempting, {} of {} |  error={:?}",
                        &maker_addr_str,
                        ii,
                        reconnect_attempts,
                        e
                    );
                    if ii <= reconnect_attempts {
                        sleep(Duration::from_secs(
                            if ii <= SHORT_LONG_SLEEP_DELAY_TRANSITION {
                                sleep_delay
                            } else {
                                RECONNECT_LONG_SLEEP_DELAY
                            },
                        ));
                        continue;
                    } else {
                        log::warn!(
                            "Failed to connect to maker {} to request signatures for receiver, \
                                    reattempt limit exceeded",
                            &maker_addr_str,
                        );
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Settle all the ongoing swaps. This routine sends the hash preimage to all the makers.
    /// Pass around the Maker's multisig privatekeys. Saves all the data in wallet file. This marks
    /// the ends of swap round.
    fn settle_all_swaps(&mut self) -> Result<(), TakerError> {
        let mut outgoing_privkeys: Option<Vec<MultisigPrivkey>> = None;

        // Because the last peer info is the Taker, we take upto (0..n-1), where n = peer_info.len()
        let maker_addresses = self.ongoing_swap_state.peer_infos
            [0..self.ongoing_swap_state.peer_infos.len() - 1]
            .iter()
            .map(|si| si.peer.clone())
            .collect::<Vec<_>>();

        for (index, maker_address) in maker_addresses.iter().enumerate() {
            if index == 0 {
                self.ongoing_swap_state.taker_position = TakerPosition::FirstPeer;
            } else if index == (self.ongoing_swap_state.swap_params.maker_count - 1) {
                self.ongoing_swap_state.taker_position = TakerPosition::LastPeer;
            } else {
                self.ongoing_swap_state.taker_position = TakerPosition::WatchOnly;
            }

            let senders_multisig_redeemscripts =
                if self.ongoing_swap_state.taker_position == TakerPosition::FirstPeer {
                    self.ongoing_swap_state
                        .outgoing_swapcoins
                        .iter()
                        .map(|sc| sc.get_multisig_redeemscript())
                        .collect::<Vec<_>>()
                } else {
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .get(index - 1)
                        .expect("Watchonly coins expected")
                        .iter()
                        .map(|sc| sc.get_multisig_redeemscript())
                        .collect::<Vec<_>>()
                };
            let receivers_multisig_redeemscripts =
                if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                    self.ongoing_swap_state
                        .incoming_swapcoins
                        .iter()
                        .map(|sc| sc.get_multisig_redeemscript())
                        .collect::<Vec<_>>()
                } else {
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .get(index)
                        .expect("watchonly coins expected")
                        .iter()
                        .map(|sc| sc.get_multisig_redeemscript())
                        .collect::<Vec<_>>()
                };

            let mut ii = 0;

            // Configurable reconnection attempts for testing
            let reconnect_attempts = if cfg!(feature = "integration-test") {
                10
            } else {
                RECONNECT_ATTEMPTS
            };

            // Custom sleep delay for testing.
            let sleep_delay = if cfg!(feature = "integration-test") {
                1
            } else {
                RECONNECT_SHORT_SLEEP_DELAY
            };

            loop {
                ii += 1;
                match self.settle_one_coinswap(
                    &maker_address.address,
                    index,
                    &mut outgoing_privkeys,
                    &senders_multisig_redeemscripts,
                    &receivers_multisig_redeemscripts,
                ) {
                    Ok(()) => break,
                    Err(e) => {
                        log::warn!(
                            "Failed to connect to maker {} to settle coinswap, \
                                    reattempting {} of {} error={:?}",
                            &maker_address.address,
                            ii,
                            reconnect_attempts,
                            e
                        );
                        if ii <= reconnect_attempts {
                            sleep(Duration::from_secs(
                                if ii <= SHORT_LONG_SLEEP_DELAY_TRANSITION {
                                    sleep_delay
                                } else {
                                    RECONNECT_LONG_SLEEP_DELAY
                                },
                            ));
                            continue;
                        } else {
                            log::warn!(
                                "Failed to connect to maker {} to settle coinswap, \
                                        reattempt limit exceeded",
                                &maker_address.address,
                            );
                            self.offerbook.add_bad_maker(maker_address);
                            return Err(e);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// [Internal] Setlle one swap. This is recursively called for all the makers.
    fn settle_one_coinswap(
        &mut self,
        maker_address: &MakerAddress,
        index: usize,
        outgoing_privkeys: &mut Option<Vec<MultisigPrivkey>>, // TODO: Instead of Option, just take a vector, where empty vector denotes the `None` equivalent.
        senders_multisig_redeemscripts: &[ScriptBuf],
        receivers_multisig_redeemscripts: &[ScriptBuf],
    ) -> Result<(), TakerError> {
        let maker_addr_str = maker_address.to_string();
        let mut socket = match self.config.connection_type {
            ConnectionType::CLEARNET => TcpStream::connect(maker_addr_str.clone())?,
            ConnectionType::TOR => Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                &*maker_addr_str,
            )?
            .into_inner(),
        };

        socket.set_read_timeout(Some(Duration::from_secs(TCP_TIMEOUT_SECONDS)))?;
        socket.set_write_timeout(Some(Duration::from_secs(TCP_TIMEOUT_SECONDS)))?;
        handshake_maker(&mut socket)?;

        log::info!("===> HashPreimage | {}", maker_address);
        let maker_private_key_handover = send_hash_preimage_and_get_private_keys(
            &mut socket,
            senders_multisig_redeemscripts,
            receivers_multisig_redeemscripts,
            &self.ongoing_swap_state.active_preimage,
        )?;
        log::info!("<=== PrivateKeyHandover | {}", maker_address);

        let privkeys_reply = if self.ongoing_swap_state.taker_position == TakerPosition::FirstPeer {
            self.ongoing_swap_state
                .outgoing_swapcoins
                .iter()
                .map(|outgoing_swapcoin| MultisigPrivkey {
                    multisig_redeemscript: outgoing_swapcoin.get_multisig_redeemscript(),
                    key: outgoing_swapcoin.my_privkey,
                })
                .collect::<Vec<MultisigPrivkey>>()
        } else {
            assert!(outgoing_privkeys.is_some());
            let reply = outgoing_privkeys
                .as_ref()
                .expect("outgoing privkey expected")
                .to_vec();
            *outgoing_privkeys = None;
            reply
        };
        (if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
            check_and_apply_maker_private_keys(
                &mut self.ongoing_swap_state.incoming_swapcoins,
                &maker_private_key_handover.multisig_privkeys,
            )
        } else {
            let ret = check_and_apply_maker_private_keys(
                self.ongoing_swap_state
                    .watchonly_swapcoins
                    .get_mut(index)
                    .expect("watchonly coins expected"),
                &maker_private_key_handover.multisig_privkeys,
            );
            *outgoing_privkeys = Some(maker_private_key_handover.multisig_privkeys);
            ret
        })?;
        log::info!("===> PrivateKeyHandover | {}", maker_address);
        send_message(
            &mut socket,
            &TakerToMakerMessage::RespPrivKeyHandover(PrivKeyHandover {
                multisig_privkeys: privkeys_reply,
            }),
        )?;
        Ok(())
    }

    // ######## UTILITY AND HELPERS ############

    /// Choose a suitable **untried** maker address from the offerbook that fits the swap params.
    fn choose_next_maker(&self) -> Result<&OfferAndAddress, TakerError> {
        let send_amount = self.ongoing_swap_state.swap_params.send_amount;
        if send_amount == Amount::ZERO {
            return Err(TakerError::SendAmountNotSet);
        }

        // Ensure that we don't select a maker we are already swaping with.
        Ok(self
            .offerbook
            .all_good_makers()
            .iter()
            .find(|oa| {
                send_amount >= Amount::from_sat(oa.offer.min_size)
                    && send_amount <= Amount::from_sat(oa.offer.max_size)
                    && !self
                        .ongoing_swap_state
                        .peer_infos
                        .iter()
                        .map(|pi| &pi.peer)
                        .any(|noa| noa == **oa)
            })
            .ok_or(TakerError::NotEnoughMakersInOfferBook)?)
    }

    /// Get the [Preimage] of the ongoing swap. If no swap is in progress will return a `[0u8; 32]`.
    fn get_preimage(&self) -> &Preimage {
        &self.ongoing_swap_state.active_preimage
    }

    /// Get the [Preimage] hash for the ongoing swap. If no swap is in progress will return `hash160([0u8; 32])`.
    fn get_preimage_hash(&self) -> Hash160 {
        Hash160::hash(self.get_preimage())
    }

    /// Clear the [OngoingSwapState].
    fn clear_ongoing_swaps(&mut self) {
        self.ongoing_swap_state = OngoingSwapState::default();
    }

    /// Get all the bad makers
    pub fn get_bad_makers(&self) -> Vec<&OfferAndAddress> {
        self.offerbook.get_bad_makers()
    }

    /// Save all the finalized swap data and reset the [OngoingSwapState].
    fn save_and_reset_swap_round(&mut self) -> Result<(), TakerError> {
        // Mark incoiming swapcoins as done
        for incoming_swapcoin in &self.ongoing_swap_state.incoming_swapcoins {
            self.wallet
                .find_incoming_swapcoin_mut(&incoming_swapcoin.get_multisig_redeemscript())
                .expect("Incoming swapcoin expeted")
                .other_privkey = incoming_swapcoin.other_privkey;
        }

        // Mark outgoing swapcoins as done.
        for outgoing_swapcoins in &self.ongoing_swap_state.outgoing_swapcoins {
            self.wallet
                .find_outgoing_swapcoin_mut(&outgoing_swapcoins.get_multisig_redeemscript())
                .expect("Outgoing swapcoin expected")
                .hash_preimage = Some(self.ongoing_swap_state.active_preimage);
        }

        self.wallet.sync_no_fail();

        self.wallet.save_to_disk()?;

        self.clear_ongoing_swaps();

        Ok(())
    }

    /// Checks if any contreact transactions have been broadcasted.
    /// Returns the txid list of all the broadcasted contract transaction.
    /// Empty vector if nothing is nothing is broadcasted. (usual case).
    pub(crate) fn check_for_broadcasted_contract_txes(&self) -> Vec<Txid> {
        let contract_txids = self
            .ongoing_swap_state
            .incoming_swapcoins
            .iter()
            .map(|sc| sc.contract_tx.compute_txid())
            .chain(
                self.ongoing_swap_state
                    .outgoing_swapcoins
                    .iter()
                    .map(|sc| sc.contract_tx.compute_txid()),
            )
            .chain(
                self.ongoing_swap_state
                    .watchonly_swapcoins
                    .iter()
                    .flatten()
                    .map(|sc| sc.contract_tx.compute_txid()),
            )
            .collect::<Vec<_>>();

        // TODO: Find out which txid was boradcasted first
        // This requires -txindex to be enabled in the node.
        let seen_txids = contract_txids
            .iter()
            .filter(|txid| self.wallet.rpc.get_raw_transaction_info(txid, None).is_ok())
            .cloned()
            .collect::<Vec<Txid>>();

        seen_txids
    }

    /// Recover from a bad swap
    pub fn recover_from_swap(&mut self) -> Result<(), TakerError> {
        let (incomings, outgoings) = self.wallet.find_unfinished_swapcoins();

        let incoming_contracts = incomings
            .iter()
            .map(|incoming| {
                Ok((
                    incoming.get_fully_signed_contract_tx()?,
                    incoming.get_multisig_redeemscript(),
                ))
            })
            .collect::<Result<Vec<_>, TakerError>>()?;

        // Broadcasted incoming contracts and remove them from the wallet.
        for (contract_tx, redeemscript) in &incoming_contracts {
            if self
                .wallet
                .rpc
                .get_raw_transaction_info(&contract_tx.compute_txid(), None)
                .is_ok()
            {
                log::info!(
                    "Incoming Contract already broadacsted. Txid : {}",
                    contract_tx.compute_txid()
                );
            } else {
                self.wallet.send_tx(contract_tx)?;
                log::info!(
                    "Broadcasting Incoming Contract. Removing from wallet. Txid : {}",
                    contract_tx.compute_txid()
                );
            }
            log::info!(
                "Incoming Swapcoin removed from wallet, Txid: {}",
                contract_tx.compute_txid()
            );
            self.wallet.remove_incoming_swapcoin(redeemscript)?;
        }

        let mut outgoing_infos = Vec::new();

        // Broadcast the Outgoing Contracts
        self.get_wallet_mut().sync()?;

        for outgoing in outgoings {
            let contract_tx = outgoing.get_fully_signed_contract_tx()?;
            if self
                .wallet
                .rpc
                .get_raw_transaction_info(&contract_tx.compute_txid(), None)
                .is_ok()
            {
                log::info!(
                    "Outgoing Contract already broadcasted | Txid: {}",
                    contract_tx.compute_txid()
                );
            } else {
                self.wallet.send_tx(&contract_tx)?;
                log::info!(
                    "Broadcasted Outgoing Contract | txid : {}",
                    contract_tx.compute_txid()
                );
            }
            let reedemscript = outgoing.get_multisig_redeemscript();
            let timelock = outgoing.get_timelock()?;
            let next_internal = &self.wallet.get_next_internal_addresses(1)?[0];

            self.get_wallet_mut().sync()?;

            let timelock_spend =
                self.wallet
                    .create_timelock_spend(&outgoing, next_internal, DEFAULT_TX_FEE_RATE)?;
            outgoing_infos.push(((reedemscript, contract_tx), (timelock, timelock_spend)));
        }

        // Check for contract confirmations and broadcast timelocked transaction
        let mut timelock_boardcasted = Vec::new();

        // Save the wallet file here before going into the expensive loop.
        self.wallet.sync()?;
        self.wallet.save_to_disk()?;
        log::info!("Wallet file synced and saved.");

        // Start the loop to keep checking for timelock maturity, and spend from the contract asap.
        loop {
            // Break early if nothing to broadcast.
            // This happens only when init_first_hop() fails at `NotEnoughMakersInOfferBook`
            if outgoing_infos.is_empty() {
                break;
            }
            for ((reedemscript, contract), (timelock, timelocked_tx)) in outgoing_infos.iter() {
                // We have already broadcasted this tx, so skip
                if timelock_boardcasted.contains(&timelocked_tx) {
                    continue;
                }
                // Check if the contract tx has reached required maturity
                // Failure here means the transaction hasn't been broadcasted yet. So do nothing and try again.
                if let Ok(result) = self
                    .wallet
                    .rpc
                    .get_raw_transaction_info(&contract.compute_txid(), None)
                {
                    log::info!(
                        "Contract Tx : {}, reached confirmation : {:?}, required : {}",
                        contract.compute_txid(),
                        result.confirmations,
                        timelock
                    );
                    if let Some(confirmation) = result.confirmations {
                        // Now the transaction is confirmed in a block, check for required maturity
                        if confirmation > (*timelock as u32) {
                            log::info!(
                                "Timelock maturity of {} blocks for Contract Tx is reached : {}",
                                timelock,
                                contract.compute_txid()
                            );
                            log::info!(
                                "Broadcasting timelocked tx: {}",
                                timelocked_tx.compute_txid()
                            );
                            self.wallet.send_tx(timelocked_tx)?;
                            timelock_boardcasted.push(timelocked_tx);

                            let outgoing_removed = self
                                .wallet
                                .remove_outgoing_swapcoin(reedemscript)?
                                .expect("outgoing swapcoin expected");
                            log::info!(
                                "Removed Outgoing Swapcoin from Wallet, Contract Txid: {}",
                                outgoing_removed.contract_tx.compute_txid()
                            );
                            log::info!("Initializing Wallet sync and save");
                            self.wallet.sync()?;
                            self.wallet.save_to_disk()?;
                            log::info!("Completed wallet sync and save");
                        }
                    }
                }
            }

            // Everything is broadcasted. Clear the connectionstate and break the loop
            log::info!(
                "{} outgoing contracts detected | {} timelock txs broadcasted.",
                outgoing_infos.len(),
                timelock_boardcasted.len()
            );
            if timelock_boardcasted.len() == outgoing_infos.len() {
                log::info!("All outgoing contracts reedemed. Cleared ongoing swap state");
                // TODO: Reevaluate this.
                self.clear_ongoing_swaps(); // This could be a bug if Taker is in middle of multiple swaps. For now we assume Taker will only do one swap at a time.
                break;
            }

            // Block wait time is varied between prod. and test builds.
            let block_wait_time = if cfg!(feature = "integration-test") {
                Duration::from_secs(10)
            } else {
                Duration::from_secs(10 * 60)
            };
            std::thread::sleep(block_wait_time);
        }
        log::info!("Recovery completed.");

        Ok(())
    }

    /// Synchronizes the offer book with addresses obtained from directory servers and local configurations.
    pub fn sync_offerbook(&mut self) -> Result<(), TakerError> {
        let dns_addr = match self.config.connection_type {
            ConnectionType::CLEARNET => {
                if cfg!(feature = "integration-test") {
                    format!("127.0.0.1:{}", 8080)
                } else {
                    self.config.directory_server_address.clone()
                }
            }
            ConnectionType::TOR => self.config.directory_server_address.clone(),
        };

        #[cfg(not(feature = "integration-test"))]
        let socks_port = Some(self.config.socks_port);

        #[cfg(feature = "integration-test")]
        let socks_port = None;

        log::info!("Fetching addresses from DNS: {}", dns_addr);

        let addresses_from_dns =
            match fetch_addresses_from_dns(socks_port, dns_addr, self.config.connection_type) {
                Ok(dns_addrs) => dns_addrs,
                Err(e) => {
                    log::error!("Could not connect to DNS Server: {:?}", e);
                    return Err(e);
                }
            };

        // For now, ask offers from everyone,
        // Because we don not have any smart update mechanism, not asking again could cause problem.
        // if a maker changes their offer without changing tor address, the taker will not ask them again for updated offer.
        // TODO: Add smarter update mechanism, where DNS would keep a flag for every update of maker offers and taker
        // will selectively redownload the offer from those makers only.
        // Further TODO: The Offer book needs to be restructured to store a unqiue value per fidelity bond. Similar to DNS.
        let offers = fetch_offer_from_makers(addresses_from_dns, &self.config)?;

        // TODO: Use better logic to update offerbook than to just rewrite everything.
        self.offerbook = OfferBook::default();

        for offer in offers {
            log::info!(
                "Verifying Fidelity Bond | Maker: {} | Outpoint: {}",
                offer.address,
                offer.offer.fidelity.bond.outpoint
            );

            if let Err(e) = self
                .wallet
                .verify_fidelity_proof(&offer.offer.fidelity, &offer.address.to_string())
            {
                log::warn!(
                    "Fidelity Proof Verification failed with error: {:?}. Adding this to bad maker list : {}",
                    e,
                    offer.address
                );
                self.offerbook.add_bad_maker(&offer);
            } else {
                log::info!("Fideity Bond verified. Adding offer from {}", offer.address);
                self.offerbook.add_new_offer(&offer);
            }
        }
        Ok(())
    }

    /// fetches only the offer data from DNS and returns the updated Offerbook.
    /// Used for taker cli app, in `fetch-offers` command.
    pub fn fetch_offers(&mut self) -> Result<&OfferBook, TakerError> {
        self.sync_offerbook()?;
        Ok(&self.offerbook)
    }

    /// Send any message to a maker
    fn send_to_maker(
        &self,
        maker_addr: &MakerAddress,
        msg: TakerToMakerMessage,
    ) -> Result<(), TakerError> {
        // Notify the maker that we are waiting for funding confirmation
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

        socket.set_write_timeout(Some(reconnect_timeout))?;

        send_message(&mut socket, &msg)?;
        log::info!("===> {} | {}", msg, maker_addr);

        Ok(())
    }
    /// Displays offer
    pub fn display_offer(&self, offer_and_address: &OfferAndAddress) -> Result<String, TakerError> {
        #[derive(Serialize, Deserialize, Debug)]
        struct Offer {
            base_fee: u64,
            amount_relative_fee_pct: f64,
            time_relative_fee_pct: f64,
            required_confirms: u32,
            minimum_locktime: u16,
            max_size: u64,
            min_size: u64,
            bond_outpoint: OutPoint,
            bond_value: Amount,
            bond_expiry: LockTime,
            tor_address: String,
        }

        let bond = offer_and_address.offer.fidelity.bond.clone();
        let bond_value = self.get_wallet().calculate_bond_value(&bond).unwrap();

        let offer = Offer {
            base_fee: offer_and_address.offer.base_fee,
            amount_relative_fee_pct: offer_and_address.offer.amount_relative_fee_pct,
            time_relative_fee_pct: offer_and_address.offer.time_relative_fee_pct,
            required_confirms: offer_and_address.offer.required_confirms,
            minimum_locktime: offer_and_address.offer.minimum_locktime,
            max_size: offer_and_address.offer.max_size,
            min_size: offer_and_address.offer.min_size,
            bond_outpoint: bond.outpoint,
            bond_value,
            bond_expiry: bond.lock_time,
            tor_address: offer_and_address.address.to_string(),
        };

        Ok(serde_json::to_string_pretty(&offer)?)
    }
}
