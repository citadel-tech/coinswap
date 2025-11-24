//! The Taker API.
//!
//! This module describes the main [`Taker`] structure and all other associated data sets related to a coinswap round.
//! This includes the main protocol workflow and all the subroutines that are called sequentially.
//!
//! [Taker::do_coinswap] is the main routine running all other subroutines. Follow the white rabbit from here.
//!
use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom,
    io::BufWriter,
    net::TcpStream,
    path::PathBuf,
    sync::mpsc,
    thread::{self, sleep},
    time::{Duration, Instant},
};

use bitcoind::bitcoincore_rpc::{json::ListUnspentResultEntry, RpcApi};

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
    offers::{fetch_offer_from_makers, MakerAddress, OfferAndAddress},
    routines::*,
};
use crate::{
    protocol::{
        contract::calculate_coinswap_fee,
        error::ProtocolError,
        messages::{
            ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForRecvrAndSender,
            ContractSigsForSender, FundingTxInfo, MultisigPrivkey, Preimage, PrivKeyHandover,
            TakerToMakerMessage,
        },
    },
    security::{load_sensitive_struct_from_value, SerdeJson},
    taker::{
        config::TakerConfig,
        ffi::{MakerFeeInfo, SwapReport},
        offers::OfferBook,
    },
    utill::*,
    wallet::{
        IncomingSwapCoin, OutgoingSwapCoin, RPCConfig, SwapCoin, Wallet, WalletBackup, WalletError,
        WatchOnlySwapCoin,
    },
    watch_tower::{
        registry_storage::FileRegistry,
        rpc_backend::BitcoinRpc,
        service::WatchService,
        watcher::{Watcher, WatcherEvent},
        zmq_backend::ZmqBackend,
    },
};

// Default values for Taker configurations
pub(crate) const REFUND_LOCKTIME: u16 = 20;
pub(crate) const REFUND_LOCKTIME_STEP: u16 = 20;
pub(crate) const FIRST_CONNECT_ATTEMPTS: u32 = 2;
pub(crate) const FIRST_CONNECT_SLEEP_DELAY_SEC: u64 = 500;
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

/// Swap specific parameters. These are user's policy and can differ among swaps.
/// SwapParams govern the criteria to find suitable set of makers from the offerbook.
///
/// If no maker matches with a given SwapParam, that coinswap round will fail.
#[derive(Debug, Default, Clone)]
pub struct SwapParams {
    /// Total Amount to Swap.
    pub send_amount: Amount,
    /// How many hops.
    pub maker_count: usize,
    /// User selected UTXOs
    pub manually_selected_outpoints: Option<Vec<OutPoint>>,
}

// Defines the Taker's position in the current ongoing swap.
#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord, Clone)]
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
/// performing a swap. Various data are appended into the lists and are only read from the last entry as the
/// swap progresses. This ensures the swap state is always consistent.
///
/// These states can be used to recover from a failed swap round.
#[derive(Default, Clone)]
struct OngoingSwapState {
    /// SwapParams used in current swap round.
    pub(crate) swap_params: SwapParams,
    /// List of all the suitable makers for the current swap round.
    pub(crate) suitable_makers: Vec<OfferAndAddress>,
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
    #[cfg(feature = "integration-test")]
    behavior: TakerBehavior,
    data_dir: PathBuf,
    watch_service: WatchService,
}

impl Drop for Taker {
    fn drop(&mut self) {
        log::info!("Shutting down taker.");
        self.offerbook
            .write_to_disk(&self.data_dir.join("offerbook.json"))
            .unwrap();
        self.watch_service.shutdown();
        log::info!("offerbook data saved to disk.");
        self.wallet.save_to_disk().unwrap();
        log::info!("Wallet data saved to disk.");
    }
}

impl Taker {
    // ######## MAIN PUBLIC INTERFACE ############

    ///  Initializes a Taker structure.
    ///
    /// This function sets up a Taker instance with configurable parameters.
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
        #[cfg(feature = "integration-test")] behavior: TakerBehavior,
        control_port: Option<u16>,
        tor_auth_password: Option<String>,
        zmq_addr: String,
    ) -> Result<Taker, TakerError> {
        // Get provided data directory or the default data directory.
        let data_dir = data_dir.unwrap_or(get_taker_dir());
        let wallets_dir = data_dir.join("wallets");

        // Use the provided name or default to `taker-wallet` if not specified.
        let wallet_file_name = wallet_file_name.unwrap_or_else(|| "taker-wallet".to_string());
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

        let mut watcher = Watcher::new(backend, rpc_backend, registry, rx_requests, tx_events);
        _ = thread::Builder::new()
            .name("Watcher thread".to_string())
            .spawn(move || watcher.run());

        let watch_service = WatchService::new(tx_requests, rx_responses);

        let mut wallet = Wallet::load_or_init_wallet(&wallet_path, &rpc_config)?;

        // If config file doesn't exist, default config will be loaded.
        let mut config = TakerConfig::new(Some(&data_dir.join("config.toml")))?;

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

        // Load offerbook. If it doesn't exist, creates fresh file.
        let offerbook_path = data_dir.join("offerbook.json");
        let offerbook = if offerbook_path.exists() {
            // If read fails, recreate a fresh offerbook.
            match OfferBook::read_from_disk(&offerbook_path) {
                Ok(offerbook) => {
                    log::info!("Succesfully loaded offerbook at : {offerbook_path:?}");
                    offerbook
                }
                Err(e) => {
                    log::error!("Offerbook data corrupted. Recreating. {e:?}");
                    let empty_book = OfferBook::default();
                    empty_book.write_to_disk(&offerbook_path)?;
                    empty_book
                }
            }
        } else {
            // Create a new offer book
            let empty_book = OfferBook::default();
            let file = std::fs::File::create(&offerbook_path)?;
            let writer = BufWriter::new(file);
            serde_json::to_writer_pretty(writer, &empty_book)?;
            empty_book
        };

        wallet.sync_and_save()?;

        Ok(Self {
            wallet,
            config,
            offerbook,
            ongoing_swap_state: OngoingSwapState::default(),
            #[cfg(feature = "integration-test")]
            behavior,
            data_dir,
            watch_service,
        })
    }

    /// Restores a wallet from a backup file using the given configuration.
    ///
    /// This is a convenience wrapper around [`Wallet::restore_interactive`] that constructs the
    /// wallet path from optional inputs.
    ///
    /// # Behavior
    ///
    /// Constructs the full wallet path as:
    /// `{data_dir_or_default}/wallets/{wallet_file_name_or_default}`
    ///
    /// Then calls [`Wallet::restore_interactive`] with the resolved backup file path, RPC config,
    /// and destination wallet path.
    pub fn restore_wallet(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: Option<RPCConfig>,
        backup_file: &String,
    ) {
        let backup_file_path = PathBuf::from(backup_file);
        let restored_wallet_filename = wallet_file_name.unwrap_or("".to_string()); //Empty string to use wallet_file_name inside the backup

        let restored_wallet_path = data_dir
            .clone()
            .unwrap_or(get_taker_dir())
            .join("wallets")
            .join(restored_wallet_filename);

        Wallet::restore_interactive(
            &backup_file_path,
            &rpc_config.unwrap_or_default(),
            &restored_wallet_path,
        );
    }

    /// Restores a wallet from an encrypted or unencrypted JSON backup string for GUI/FFI applications.
    ///
    /// This is a non-interactive restore method designed for programmatic use via FFI bindings.
    /// Unlike `restore_wallet`, this function accepts a JSON string directly and handles both
    /// encrypted and unencrypted backups using [`load_sensitive_struct_from_value`].
    ///
    /// # Behavior
    ///
    /// 1. Parses the JSON backup string into a [`WalletBackup`] structure
    /// 2. If encrypted, decrypts using the provided password and preserves encryption material
    /// 3. Constructs the wallet path: `{data_dir_or_default}/wallets/{wallet_file_name_or_default}`
    /// 4. Calls [`Wallet::restore`] to reconstruct the wallet with all UTXOs and metadata
    ///
    /// # Parameters
    ///
    /// - `data_dir`: Target directory, defaults to `~/.coinswap/taker`
    /// - `wallet_file_name`: Restored wallet filename, defaults to name from backup if empty
    /// - `backup_file`: JSON string containing the wallet backup (encrypted or plain)
    /// - `password`: Required if backup is encrypted, ignored otherwise
    pub fn restore_wallet_gui_app(
        data_dir: Option<PathBuf>,
        wallet_file_name: Option<String>,
        rpc_config: RPCConfig,
        backup_file: String,
        password: Option<String>,
    ) {
        let value = serde_json::from_str(&backup_file).unwrap();
        let (backup, encryption_material) =
            load_sensitive_struct_from_value::<WalletBackup, SerdeJson>(&value, password.unwrap());
        let restored_wallet_filename = wallet_file_name.unwrap_or("".to_string());

        let restored_wallet_path = data_dir
            .clone()
            .unwrap_or(get_taker_dir())
            .join("wallets")
            .join(restored_wallet_filename);

        if let Err(e) = Wallet::restore(
            &backup,
            &restored_wallet_path,
            &rpc_config,
            encryption_material,
        ) {
            log::error!("Wallet restore failed: {e:?}");
        } else {
            println!("Wallet restore succeeded!");
        }
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
    pub fn do_coinswap(
        &mut self,
        swap_params: SwapParams,
    ) -> Result<Option<SwapReport>, TakerError> {
        self.send_coinswap(swap_params)
    }

    /// Perform a coinswap round with given [`SwapParams`]. The Taker will try to perform swap with makers
    /// in it's [`OfferBook`] sequentially as per the maker_count given in swap params.
    /// If [`SwapParams`] doesn't fit suitably with any available offers, or not enough makers
    /// respond back, the swap round will fail.
    ///
    /// Depending upon the failure situation, Taker will automatically try to recover from failed swaps
    /// by executing the contract txs. If that fails too for any reason, user should manually call the [Taker::recover_from_swap].
    ///
    /// If that fails too. Open an issue at [our github](https://github.com/citadel-tech/coinswap/issues)
    pub(crate) fn send_coinswap(
        &mut self,
        swap_params: SwapParams,
    ) -> Result<Option<SwapReport>, TakerError> {
        let swap_start_time = std::time::Instant::now();
        let initial_utxoset = self.wallet.list_all_utxo();
        self.ongoing_swap_state.swap_params = swap_params.clone();

        // Check if we have enough balance - try regular first, then swap
        let balances = self.wallet.get_balances()?;
        let estimated_fee = Amount::from_sat(calculate_fee_sats(200));
        let required = swap_params.send_amount + estimated_fee;

        // Try regular balance first
        if balances.regular >= required {
            log::info!("Using regular UTXOs for coinswap");
        }
        // If regular insufficient, try swap balance
        else if balances.swap >= required {
            log::info!("Using swap UTXOs for coinswap");
        }
        // If both insufficient, error with regular balance (consistent with wallet)
        else {
            let err = WalletError::InsufficientFund {
                available: balances.regular.to_sat(),
                required: required.to_sat(),
            };
            log::error!("Not enough balance to do swap : {err:?}");
            return Err(err.into());
        }
        log::info!("Syncing Offerbook");
        self.sync_offerbook()?;

        // Error early if there aren't enough suitable makers for the given amount
        let suitable_makers = self.find_suitable_makers();
        if swap_params.maker_count > suitable_makers.len() {
            log::error!(
                "Not enough suitable makers for the requested amount. Required {}, available {}",
                swap_params.maker_count,
                suitable_makers.len()
            );
            return Err(TakerError::NotEnoughMakersInOfferBook);
        }

        log::info!(
            "Found {} suitable makers for this swap round",
            suitable_makers.len()
        );
        self.ongoing_swap_state.suitable_makers = suitable_makers;

        // Error early if less than 2 makers.
        if swap_params.maker_count < 2 {
            log::error!("Cannot swap with less than 2 makers");
            return Err(ProtocolError::General("Swap maker count < 2").into());
        }

        // Generate new random preimage and initiate the first hop.
        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        let unique_id = preimage[0..8].to_hex_string(Case::Lower);

        log::info!("Initiating coinswap with id : {unique_id}");

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.id = unique_id;

        // Try first hop. Abort if error happens.
        if let Err(e) = self.init_first_hop() {
            log::error!("Could not initiate first hop: {e:?}");
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
                        log::error!("Could not initiate next hop. Error : {e:?}");
                        log::warn!("Starting recovery from existing swap");
                        self.recover_from_swap()?;
                        return Ok(None);
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
                    log::error!("Error: {e:?}");
                    log::warn!("Starting recovery from existing swap");
                    if let TakerError::FundingTxWaitTimeOut = e {
                        let bad_maker = &self.ongoing_swap_state.peer_infos[maker_index].peer;
                        self.offerbook.add_bad_maker(bad_maker);
                    }
                    self.recover_from_swap()?;
                    return Ok(None);
                }
            }

            // For the last hop, initiate the incoming swapcoins, and request the sigs for it.
            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                let incoming_swapcoins =
                    self.create_incoming_swapcoins(multisig_reedemscripts, funding_outpoints)?;
                log::debug!("Incoming Swapcoins: {incoming_swapcoins:?}");
                self.ongoing_swap_state.incoming_swapcoins = incoming_swapcoins;
                match self.request_sigs_for_incoming_swap() {
                    Ok(_) => (),
                    Err(e) => {
                        log::error!("Incoming SwapCoin Generation failed : {e:?}");
                        log::warn!("Starting recovery from existing swap");
                        self.recover_from_swap()?;
                        return Ok(None);
                    }
                }
            }
        } // Contract establishment completed.

        #[cfg(feature = "integration-test")]
        {
            if self.behavior == TakerBehavior::DropConnectionAfterFullSetup {
                log::error!("Dropping Swap Process after full setup");
                return Ok(None);
            }

            if self.behavior == TakerBehavior::BroadcastContractAfterFullSetup {
                log::error!("Special Behavior BroadcastContractAfterFullSetup");
                self.recover_from_swap()?;
                return Ok(None);
            }
        }

        match self.settle_all_swaps() {
            Ok(_) => {
                log::info!(
                    "Swaps settled successfully. Sweeping the coins and reseting everything."
                );
                let prereset_swapstate = &self.ongoing_swap_state.clone();
                self.save_and_reset_swap_round()?;
                // Sweep incoming swapcoins after successful swap completion
                log::info!("Sweeping completed incoming swap coins...");
                let swept_txids = self.wallet.sweep_incoming_swapcoins(MIN_FEE_RATE)?;
                if !swept_txids.is_empty() {
                    log::info!(
                        "Successfully swept {} incoming swap coins: {:?}",
                        swept_txids.len(),
                        swept_txids
                    );
                }

                // Generate post-swap report
                self.wallet.sync_and_save()?;
                let swap_report = Some(self.generate_swap_report(
                    prereset_swapstate,
                    swap_start_time,
                    initial_utxoset,
                )?);
                log::info!("Successfully Completed Coinswap.");
                Ok(swap_report)
            }
            Err(e) => {
                log::error!("Swap Settlement Failed : {e:?}");
                log::warn!("Starting recovery from existing swap");
                self.recover_from_swap()?;
                Ok(None)
            }
        }
    }

    fn generate_swap_report(
        &self,
        prereset_swapstate: &OngoingSwapState,
        start_time: std::time::Instant,
        initial_utxos: Vec<ListUnspentResultEntry>,
    ) -> Result<SwapReport, TakerError> {
        let swap_state = &prereset_swapstate;

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

        // Present in initial set but not in current set (destroyed)
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

        // Present in current set but not in initial regular set (created)
        let output_change_amounts = output_regular_utxos
            .iter()
            .map(|utxo| utxo.amount.to_sat())
            .collect::<Vec<u64>>();

        let network = self.wallet.store.network;

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
        println!("                            🪙 COINSWAP REPORT 🪙");
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
        println!("\x1b[1;37mTarget Amount     :\x1b[0m {target_amount} Sats",);
        println!("\x1b[1;37mTotal Input       :\x1b[0m {total_input_amount} Sats",);
        println!("\x1b[1;37mTotal Output      :\x1b[0m {total_output_amount} Sats",);
        println!(
            "\x1b[1;37mMakers Involved   :\x1b[0m {}",
            swap_state.swap_params.maker_count
        );

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                                Makers Used");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");
        for (index, peer_info) in swap_state.peer_infos.iter().enumerate() {
            if index < swap_state.swap_params.maker_count {
                println!(
                    "  \x1b[1;33m{}.\x1b[0m {}",
                    index + 1,
                    peer_info.peer.address
                );
            }
        }

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                            Transaction Details");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");
        let total_funding_txs: usize = swap_state
            .funding_txs
            .iter()
            .map(|(txs, _)| txs.len())
            .sum();
        println!("\x1b[1;37mTotal Funding Txs :\x1b[0m {total_funding_txs}");

        println!("\x1b[1;37mFunding Transaction IDs:\x1b[0m");
        for (hop_index, (txs, _)) in swap_state.funding_txs.iter().enumerate() {
            println!("  \x1b[1;35mHop {}:\x1b[0m", hop_index + 1);
            for (tx_index, tx) in txs.iter().enumerate() {
                println!(
                    "    Tx {} → \x1b[2m{}\x1b[0m",
                    tx_index + 1,
                    tx.compute_txid()
                );
            }
        }

        println!("\n\x1b[1;36m────────────────────────────────────────────────────────────────────────────────");
        println!("                              Fee Information");
        println!("────────────────────────────────────────────────────────────────────────────────\x1b[0m");

        let total_fee = total_input_amount - total_output_amount;
        println!("\x1b[1;37mTotal Fees        :\x1b[0m \x1b[1;31m{total_fee} sats\x1b[0m",);

        // Collect maker fee information
        let mut maker_fee_info = Vec::new();
        let mut temp_target_amount = swap_state.swap_params.send_amount.to_sat();

        let total_maker_fees = (0..swap_state.swap_params.maker_count)
            .map(|maker_index| {
                let maker_refund_locktime = REFUND_LOCKTIME
                    + REFUND_LOCKTIME_STEP
                        * (swap_state.swap_params.maker_count - maker_index - 1) as u16;

                let base_fee = swap_state.peer_infos[maker_index].peer.offer.base_fee as f64;
                let amount_rel_fee = (swap_state.peer_infos[maker_index]
                    .peer
                    .offer
                    .amount_relative_fee_pct
                    * temp_target_amount as f64)
                    / 1_00.00;
                let time_rel_fee = (swap_state.peer_infos[maker_index]
                    .peer
                    .offer
                    .time_relative_fee_pct
                    * maker_refund_locktime as f64
                    * temp_target_amount as f64)
                    / 1_00.00;

                println!("\n\x1b[1;33mMaker {}:\x1b[0m", maker_index + 1);
                println!("    Base Fee             : {base_fee}");
                println!("    Amount Relative Fee  : {amount_rel_fee:.2}");
                println!("    Time Relative Fee    : {time_rel_fee:.2}");

                let total_maker_fee = base_fee + amount_rel_fee + time_rel_fee;

                // Store maker fee info
                maker_fee_info.push(MakerFeeInfo {
                    maker_index,
                    maker_address: swap_state.peer_infos[maker_index].peer.address.to_string(),
                    base_fee,
                    amount_relative_fee: amount_rel_fee,
                    time_relative_fee: time_rel_fee,
                    total_fee: total_maker_fee,
                });

                temp_target_amount -= total_maker_fee as u64;
                total_maker_fee
            })
            .sum::<f64>() as u64;

        let mining_fee = total_fee - total_maker_fees;
        println!("\n\x1b[1;37mMining Fees       :\x1b[0m \x1b[36m{mining_fee} sats\x1b[0m",);
        let fee_percentage =
            (total_fee as f64 / swap_state.swap_params.send_amount.to_sat() as f64) * 100.0;
        println!("\x1b[1;37mTotal Fee Rate    :\x1b[0m \x1b[1;31m{fee_percentage:.2} %\x1b[0m",);

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
            .peer_infos
            .iter()
            .take(swap_state.swap_params.maker_count)
            .map(|peer_info| peer_info.peer.address.to_string())
            .collect::<Vec<_>>();

        // Collect funding txids by hop
        let funding_txids_by_hop = swap_state
            .funding_txs
            .iter()
            .map(|(txs, _)| {
                txs.iter()
                    .map(|tx| tx.compute_txid().to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let report = SwapReport {
            swap_id: swap_state.id.clone(),
            swap_duration_seconds: swap_duration.as_secs_f64(),
            target_amount: swap_state.swap_params.send_amount.to_sat(),
            total_input_amount,
            total_output_amount,
            makers_count: swap_state.swap_params.maker_count,
            maker_addresses,
            total_funding_txs,
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

    // ######## PROTOCOL SUBROUTINES ############

    /// Initiate the first coinswap hop. Makers are selected from the [`OfferBook`], and round will
    /// fail if no suitable makers are found.
    /// Creates and stores the [`OutgoingSwapCoin`] into [`OngoingSwapState`], and also saves it into the [`Wallet`] file.
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
                generate_maker_keys(&maker.offer.tweakable_point, 1)?;
            let (funding_txs, mut outgoing_swapcoins, _) = self.wallet.initalize_coinswap(
                &self.ongoing_swap_state.swap_params,
                &multisig_pubkeys,
                &hashlock_pubkeys,
                self.get_preimage_hash(),
                swap_locktime,
                MIN_FEE_RATE,
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
                let txid = self.wallet.send_tx(tx)?;
                log::info!("Broadcasted Funding tx. txid: {txid}");
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
    /// The error contanis the list of broadcasted contract [`Txid`]s.
    fn watch_for_txs(
        &self,
        funding_txids: &Vec<Txid>,
    ) -> Result<(Vec<Transaction>, Vec<String>), TakerError> {
        let mut txid_tx_map = HashMap::<Txid, Transaction>::new();
        let mut txid_blockhash_map = HashMap::<Txid, BlockHash>::new();

        // Find next maker's details
        let required_confirmations =
            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                REQUIRED_CONFIRMS
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

        log::info!("Waiting for funding transaction confirmation. Txids : {funding_txids:?}");

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
            let contracts_broadcasted = self.check_for_broadcasted_contract_txes();
            if !contracts_broadcasted.is_empty() {
                log::error!(
                    "Fatal! Contract txs broadcasted by makers. Txids : {contracts_broadcasted:?}"
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
                        log::info!("Waiting for funding tx to appear in mempool | {elapsed} secs");
                        if elapsed > mempool_wait_timeout {
                            log::error!("Timed out waiting for funding tx to appear in mempool. | No tx seen in {elapsed} secs");
                            return Err(TakerError::FundingTxWaitTimeOut);
                        }
                        continue;
                    }
                };

                // log that its waiting for confirmation.
                if gettx.confirmations.is_none() {
                    let elapsed = start_time.elapsed().as_secs();
                    log::info!(
                        "Funding tx Seen in Mempool. Waiting for confirmation for {elapsed} secs",
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
                            log::error!("error sending wait-notif to maker {addr} | {e:?}");
                        }
                    }
                }

                // handle confirmations
                if gettx.confirmations >= Some(required_confirmations) {
                    txid_tx_map.insert(
                        *txid,
                        deserialize::<Transaction>(&gettx.hex).map_err(WalletError::from)?,
                    );
                    txid_blockhash_map.insert(*txid, gettx.blockhash.expect("Blockhash expected"));
                    log::info!("Tx {txid} | Confirmed at {required_confirmations}");
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

    /// Create [`FundingTxInfo`] for the "next_maker". Next maker is the last stored [`NextPeerInfo`] in the swp state.
    /// All other data from the swap state's last entries are collected and a [`FundingTxInfo`] protocol message data is generated.
    fn funding_info_for_next_maker(&self) -> Vec<FundingTxInfo> {
        // Get the redeemscripts.
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
    /// If no suitable makers are found in [`OfferBook`], next swap will not initiate and the swap round will fail.
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

    /// Single attempt to send signatures and initiate next hop.
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
        let mut socket = if cfg!(feature = "integration-test") {
            TcpStream::connect(address)?
        } else {
            Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                address.as_str(),
            )?
            .into_inner()
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
                let (my_recv_ms_pubkey, my_recv_ms_nonce) = generate_keypair();
                let (my_recv_hashlock_pubkey, my_recv_hashlock_nonce) = generate_keypair();
                (
                    vec![my_recv_ms_pubkey],
                    vec![my_recv_ms_nonce],
                    vec![my_recv_hashlock_pubkey],
                    vec![my_recv_hashlock_nonce],
                )
            } else {
                next_maker = self.choose_next_maker()?.clone();
                //next_maker is only ever accessed when the next peer is a maker, not a taker
                //i.e. if its ever used when is_taker_next_peer == true, then thats a bug
                generate_maker_keys(&next_maker.offer.tweakable_point, 1)?
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

            log::info!("Fundix Txids: {funding_txids:?}");

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
            // Sign the receiver's contract using our `OutgoingSwapCoin`.
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
                    log::error!("Could not get Receiver's signatures : {e:?}");
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

    /// Create [`WatchOnlySwapCoin`] for the current Maker.
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
        for swapcoin in &next_swapcoins {
            self.wallet
                .import_watchonly_redeemscript(&swapcoin.get_multisig_redeemscript())?;
        }
        Ok(next_swapcoins)
    }

    /// Create the [`IncomingSwapCoin`] for this round. The Taker is always the "next_peer" here
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

            self.wallet.sync_and_save()?;

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

    /// Request signatures for the [`IncomingSwapCoin`] from the last maker of the swap round.
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

        let mut socket = if cfg!(feature = "integration-test") {
            TcpStream::connect(maker_addr_str.clone())?
        } else {
            Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                &*maker_addr_str,
            )?
            .into_inner()
        };

        socket.set_read_timeout(Some(reconnect_time_out))?;
        socket.set_write_timeout(Some(reconnect_time_out))?;

        loop {
            ii += 1;
            log::info!("===> ReqContractSigsForSender | {maker_addr_str}");
            match req_sigs_for_sender_once(
                &mut socket,
                outgoing_swapcoins,
                maker_multisig_nonces,
                maker_hashlock_nonces,
                locktime,
            ) {
                Ok(ret) => {
                    return {
                        log::info!("<=== RespContractSigsForSender | {maker_addr_str}");
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
        let mut socket = if cfg!(feature = "integration-test") {
            TcpStream::connect(maker_addr_str.clone())?
        } else {
            Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                &*maker_addr_str,
            )?
            .into_inner()
        };

        socket.set_read_timeout(Some(reconnect_time_out))?;
        socket.set_write_timeout(Some(reconnect_time_out))?;

        loop {
            ii += 1;
            log::info!("===> ReqContractSigsForRecvr | {maker_addr_str}");
            match req_sigs_for_recvr_once(&mut socket, incoming_swapcoins, receivers_contract_txes)
            {
                Ok(ret) => {
                    log::info!("<=== RespContractSigsForRecvr | {maker_addr_str}");
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
        let mut outgoing_privkeys: Vec<MultisigPrivkey> = Vec::new();

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

    /// Setlle one swap. This is recursively called for all the makers.
    fn settle_one_coinswap(
        &mut self,
        maker_address: &MakerAddress,
        index: usize,
        outgoing_privkeys: &mut Vec<MultisigPrivkey>,
        senders_multisig_redeemscripts: &[ScriptBuf],
        receivers_multisig_redeemscripts: &[ScriptBuf],
    ) -> Result<(), TakerError> {
        let maker_addr_str = maker_address.to_string();
        let mut socket = if cfg!(feature = "integration-test") {
            TcpStream::connect(maker_addr_str.clone())?
        } else {
            Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                &*maker_addr_str,
            )?
            .into_inner()
        };

        socket.set_read_timeout(Some(Duration::from_secs(TCP_TIMEOUT_SECONDS)))?;
        socket.set_write_timeout(Some(Duration::from_secs(TCP_TIMEOUT_SECONDS)))?;
        handshake_maker(&mut socket)?;

        log::info!("===> HashPreimage | {maker_address}");
        let maker_private_key_handover = send_hash_preimage_and_get_private_keys(
            &mut socket,
            senders_multisig_redeemscripts,
            receivers_multisig_redeemscripts,
            &self.ongoing_swap_state.active_preimage,
        )?;
        log::info!("<=== PrivateKeyHandover | {maker_address}");

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
            assert!(!outgoing_privkeys.is_empty());
            let reply = outgoing_privkeys.clone();
            *outgoing_privkeys = Vec::new();
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
            *outgoing_privkeys = maker_private_key_handover.multisig_privkeys;
            ret
        })?;
        log::info!("===> PrivateKeyHandover | {maker_address}");
        send_message(
            &mut socket,
            &TakerToMakerMessage::RespPrivKeyHandover(PrivKeyHandover {
                multisig_privkeys: privkeys_reply,
                id: self.ongoing_swap_state.id.clone(),
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
        self.ongoing_swap_state
            .suitable_makers
            .iter()
            .find(|oa| {
                !self
                    .ongoing_swap_state
                    .peer_infos
                    .iter()
                    .map(|pi| &pi.peer)
                    .any(|noa| noa == *oa)
                    && !self.offerbook.bad_makers.contains(oa)
            })
            .ok_or(TakerError::NotEnoughMakersInOfferBook)
    }

    /// Get the [`Preimage`] of the ongoing swap. If no swap is in progress will return a `[0u8; 32]`.
    fn get_preimage(&self) -> &Preimage {
        &self.ongoing_swap_state.active_preimage
    }

    /// Get the [`Preimage`] hash for the ongoing swap. If no swap is in progress will return `hash160([0u8; 32])`.
    fn get_preimage_hash(&self) -> Hash160 {
        Hash160::hash(self.get_preimage())
    }

    /// Clear the [`OngoingSwapState`].
    fn clear_ongoing_swaps(&mut self) {
        self.ongoing_swap_state = OngoingSwapState::default();
    }

    /// Get all the bad makers
    pub fn get_bad_makers(&self) -> Vec<&OfferAndAddress> {
        self.offerbook.get_bad_makers()
    }

    /// Save all the finalized swap data and reset the [`OngoingSwapState`].
    fn save_and_reset_swap_round(&mut self) -> Result<(), TakerError> {
        // Mark incoiming swapcoins as done
        for incoming_swapcoin in &self.ongoing_swap_state.incoming_swapcoins {
            self.wallet
                .find_incoming_swapcoin_mut(&incoming_swapcoin.get_multisig_redeemscript())
                .expect("Incoming swapcoin expeted")
                .other_privkey = incoming_swapcoin.other_privkey;
            let contract_txid = incoming_swapcoin.contract_tx.compute_txid();
            for (vout, _) in incoming_swapcoin.contract_tx.output.iter().enumerate() {
                let outpoint = OutPoint {
                    txid: contract_txid,
                    vout: vout as u32,
                };
                self.watch_service.unwatch(outpoint);
            }
        }

        // Mark outgoing swapcoins as done.
        for outgoing_swapcoins in &self.ongoing_swap_state.outgoing_swapcoins {
            self.wallet
                .find_outgoing_swapcoin_mut(&outgoing_swapcoins.get_multisig_redeemscript())
                .expect("Outgoing swapcoin expected")
                .hash_preimage = Some(self.ongoing_swap_state.active_preimage);
            let contract_txid = outgoing_swapcoins.contract_tx.compute_txid();
            for (vout, _) in outgoing_swapcoins.contract_tx.output.iter().enumerate() {
                let outpoint = OutPoint {
                    txid: contract_txid,
                    vout: vout as u32,
                };
                self.watch_service.unwatch(outpoint);
            }
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

        // First try to spend the hashlock contracts.
        // If fails, then try to spend the timelock contracts.
        if let Ok(incoming_infos) = self
            .get_wallet_mut()
            .broadcast_incoming_contracts(incomings)
        {
            log::info!(
                "Incoming swapcoins are unspent. Trying to recover the incoming swapcoins first."
            );
            // Start the loop to keep checking for the hashlock contract to broadcast and spend from the contract asap.
            loop {
                //spend from the hashlock contract.
                let hashlock_broadcasted = self
                    .wallet
                    .spend_from_hashlock_contract(&incoming_infos, &self.watch_service)?;

                // If Everything is broadcasted i.e. [`spend_from_hashlock_contract`] works fine,then clear the connectionstate and break the loop
                log::info!(
                    "{} incoming contracts detected | {} hashlock txs broadcasted.",
                    incoming_infos.len(),
                    hashlock_broadcasted.len()
                );
                if hashlock_broadcasted.len() == incoming_infos.len() {
                    log::info!("All incoming contracts redeemed. Cleared ongoing swap state");
                    self.clear_ongoing_swaps();
                    break;
                }
                std::thread::sleep(BLOCK_DELAY);
            }
        } else {
            log::info!(
                "Incoming swapcoins are already spent or empty. Trying to recover the outgoing swapcoins."
            );
            let outgoing_infos = self
                .get_wallet_mut()
                .broadcast_outgoing_contracts(outgoings)?;

            // Start the loop to keep checking for timelock maturity, and spend from the contract asap.
            loop {
                // Break early if nothing to broadcast.
                // This happens only when init_first_hop() fails at `NotEnoughMakersInOfferBook`
                if outgoing_infos.is_empty() {
                    break;
                }
                let timelock_broadcasted = self
                    .wallet
                    .spend_from_timelock_contract(&outgoing_infos, &self.watch_service)?;

                // Everything is broadcasted. Clear the connectionstate and break the loop
                log::info!(
                    "{} outgoing contracts detected | {} timelock txs broadcasted.",
                    outgoing_infos.len(),
                    timelock_broadcasted.len()
                );
                if timelock_broadcasted.len() == outgoing_infos.len() {
                    log::info!("All outgoing contracts redeemed. Cleared ongoing swap state");
                    self.clear_ongoing_swaps();
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
        }
        log::info!("Recovery completed.");
        Ok(())
    }

    /// Synchronizes the offer book with addresses obtained from directory servers and local configurations.
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

        let new_offers = fetch_offer_from_makers(addrs_to_fetch, &self.config)?;

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
            .write_to_disk(&self.data_dir.join("offerbook.json"))?;

        Ok(())
    }

    /// fetches only the offer data from Tracker and returns the updated Offerbook.
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
        let mut socket = if cfg!(feature = "integration-test") {
            TcpStream::connect(address.clone())?
        } else {
            Socks5Stream::connect(
                format!("127.0.0.1:{}", self.config.socks_port).as_str(),
                address.as_str(),
            )?
            .into_inner()
        };

        let reconnect_timeout = Duration::from_secs(TCP_TIMEOUT_SECONDS);

        socket.set_write_timeout(Some(reconnect_timeout))?;

        send_message(&mut socket, &msg)?;
        log::info!("===> {msg} | {maker_addr}");

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

    /// Gets all good makers that can handle a specific amount.
    /// Filters makers based on their min_size and max_size limits.
    fn find_suitable_makers(&self) -> Vec<OfferAndAddress> {
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
}
