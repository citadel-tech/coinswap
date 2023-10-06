//! CoinSwap Taker Protocol
//!
//! Implementation of coinswap Taker protocol described in https://github.com/utxo-teleport/teleport-transactions#protocol-between-takers-and-makers
//! The Taker handles all the necessary communications between one or many makers to route the swap across various makers.
//!
//! This module describes the main [Taker] structure and all other associated data sets related to a coinswap round.
//!
//! [TakerConfig]: Set of configuration parameters defining [Taker]'s behavior.
//! [SwapParams]: Set of parameters defining a specific Swap round.
//! [OngoingSwapState]: Represents the State of an ongoing swap round. All swap related data are stored in this state.
//!
//! [Taker::send_coinswap]: The routine running all other protocol subroutines.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

use bip39::Mnemonic;
use bitcoind::bitcoincore_rpc::RpcApi;
use tokio::{net::TcpStream, select, time::sleep};

use bitcoin::{
    consensus::encode::deserialize,
    hashes::{hash160::Hash as Hash160, Hash},
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        SecretKey,
    },
    BlockHash, OutPoint, PublicKey, ScriptBuf, Transaction, Txid,
};

use crate::{
    error::{NetError, ProtocolError},
    protocol::{
        error::ContractError,
        messages::{
            ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForRecvrAndSender,
            ContractSigsForSender, FundingTxInfo, MultisigPrivkey, Preimage, PrivKeyHandover,
            TakerToMakerMessage,
        },
    },
    taker::{config::TakerConfig, offers::OfferBook},
    wallet::{
        IncomingSwapCoin, OutgoingSwapCoin, RPCConfig, SwapCoin, Wallet, WalletSwapCoin,
        WatchOnlySwapCoin,
    },
};

use super::{
    error::TakerError,
    offers::{MakerAddress, OfferAndAddress},
    routines::*,
};
use crate::utill::*;

/// Swap specific parameters. These are user's policy and can differ among swaps.
/// SwapParams govern the criteria to find suitable set of makers from the offerbook.
/// If no maker matches with a given SwapParam, that coinswap round will fail.
#[derive(Debug, Default, Clone, Copy)]
pub struct SwapParams {
    /// Total Amount to Swap.
    pub send_amount: u64,
    /// How many hops.
    pub maker_count: u16,
    /// How many splits
    pub tx_count: u32,
    // TODO: Following two should be moved to TakerConfig as global configuration.
    /// Confirmation count required for funding txs.
    pub required_confirms: u64,
    /// Fee rate for funding txs.
    pub fee_rate: u64,
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
    pub swap_params: SwapParams,
    /// SwapCoins going out from the Taker.
    pub outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    /// SwapCoins between Makers.
    pub watchonly_swapcoins: Vec<Vec<WatchOnlySwapCoin>>,
    /// SwapCoins received by the Taker.
    pub incoming_swapcoins: Vec<IncomingSwapCoin>,
    /// Information regarding all the swap participants (Makers).
    /// The last entry at the end of the swap round will be the Taker, as it's the last peer.
    pub peer_infos: Vec<NextPeerInfo>,
    /// List of funding transactions with optional merkleproofs.
    pub funding_txs: Vec<(Vec<Transaction>, Vec<String>)>,
    /// The preimage being used for this coinswap round.
    pub active_preimage: Preimage,
    /// Enum defining the position of the Taker at each steps of a multihop swap.
    pub taker_position: TakerPosition,
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

#[derive(Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum TakerBehavior {
    // No special behavior
    #[default]
    Normal,
    // This depicts the behavior when the taker drops connections after the full coinswap setup
    DropConnectionAfterFullSetup,
}

/// The Taker structure that performs bulk of the coinswap protocol. Taker connects
/// to multiple Makers and send protocol messages sequentially to them. The communication
/// sequence and corresponding SwapCoin infos are stored in `ongoing_swap_state`.
pub struct Taker {
    wallet: Wallet,
    config: TakerConfig,
    offerbook: OfferBook,
    ongoing_swap_state: OngoingSwapState,
    behavior: TakerBehavior,
}

impl Taker {
    // ######## MAIN PUBLIC INTERFACE ############

    /// Initialize a Taker with a wallet, optional RPC and Wallet Mode.
    /// If RPC config isn't provide, default RPC configuration will be used.
    /// WalletMode is used to control wallet operation mode, Normal or Testing, Defaults to Normal.
    /// Errors if RPC connection fails.
    pub fn init(
        wallet_file: &PathBuf,
        rpc_config: Option<RPCConfig>,
        behavior: TakerBehavior,
    ) -> Result<Taker, TakerError> {
        let mut wallet = if wallet_file.exists() {
            Wallet::load(&rpc_config.unwrap_or_default(), wallet_file)?
        } else {
            let mnemonic = Mnemonic::generate(12).unwrap();
            let seedphrase = mnemonic.to_string();
            Wallet::init(
                wallet_file,
                &rpc_config.unwrap_or_default(),
                seedphrase,
                "".to_string(),
            )?
        };

        wallet.sync()?;

        Ok(Self {
            wallet,
            config: TakerConfig::default(),
            offerbook: OfferBook::default(),
            ongoing_swap_state: OngoingSwapState::default(),
            behavior,
        })
    }

    pub fn get_wallet(&self) -> &Wallet {
        &self.wallet
    }

    pub fn get_wallet_mut(&mut self) -> &mut Wallet {
        &mut self.wallet
    }

    /// Perform a coinswap round with given [SwapParams]. The Taker will try to perform swap with makers
    /// in it's [OfferBook] sequentially as per the maker_count given in swap params.
    /// If [SwapParams] doesn't fit suitably with any available offers, or not enough makers
    /// respond back, the swap round will fail.
    #[tokio::main]
    pub async fn send_coinswap(&mut self, swap_params: SwapParams) -> Result<(), TakerError> {
        let got_offers = self
            .offerbook
            .sync_offerbook(self.wallet.store.network, &self.config)
            .await?;

        log::info!("<=== Got Offers ({} offers)", got_offers.len());
        log::debug!("Offers : {:#?}", got_offers);

        // Generate new random preimage and initiate the first hop.
        let mut preimage = [0u8; 32];
        OsRng.fill_bytes(&mut preimage);

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.swap_params = swap_params;

        // Try first hop. Abort if error happens.
        if let Err(e) = self.init_first_hop().await {
            log::error!("Could not initiate first hop: {:?}", e);
            self.recover_from_swap()?;
            return Err(e);
        }

        // Iterate until `maker_count` numbers of Makers are found and initiate swap between them sequentially.
        for maker_index in 0..self.ongoing_swap_state.swap_params.maker_count {
            if maker_index == 0 {
                self.ongoing_swap_state.taker_position = TakerPosition::FirstPeer
            } else if maker_index == self.ongoing_swap_state.swap_params.maker_count - 1 {
                self.ongoing_swap_state.taker_position = TakerPosition::LastPeer
            } else {
                self.ongoing_swap_state.taker_position = TakerPosition::WatchOnly
            }

            // Refund lock time decreases by `refund_locktime_step` for each hop.
            let maker_refund_locktime = self.config.refund_locktime
                + self.config.refund_locktime_step
                    * (self.ongoing_swap_state.swap_params.maker_count - maker_index - 1);

            let funding_tx_infos = self.funding_info_for_next_maker();

            // Attempt to initiate the next hop of the swap. If anything goes wrong, abort immediately.
            // If succeeded, collect the funding_outpoints and multisig_reedemscripts of the next hop.
            // If error then aborts from current swap. Ban the Peer.
            let (funding_outpoints, multisig_reedemscripts) = match self
                .send_sigs_init_next_hop(maker_refund_locktime, &funding_tx_infos)
                .await
            {
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
            match self.watch_for_txs(&txids_to_watch).await {
                Ok(r) => self.ongoing_swap_state.funding_txs.push(r),
                Err(e) => {
                    log::error!("Error: {:?}", e);
                    log::warn!("Starting recovery from existing swap");
                    if let TakerError::FundingTxWaitTimeOut = e {
                        let bad_maker =
                            &self.ongoing_swap_state.peer_infos[maker_index as usize].peer;
                        self.offerbook.add_bad_maker(&bad_maker);
                    }
                    self.recover_from_swap()?;
                    return Ok(());
                }
            }

            // For the last hop, initiate the incoming swapcoins, and request the sigs for it.
            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                let incoming_swapcoins =
                    self.create_incoming_swapcoins(multisig_reedemscripts, funding_outpoints)?;
                self.ongoing_swap_state.incoming_swapcoins = incoming_swapcoins;
                match self.request_sigs_for_incoming_swap().await {
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

        match self.settle_all_swaps().await {
            Ok(_) => (),
            Err(e) => {
                log::error!("Swap Settlement Failed : {:?}", e);
                log::warn!("Starting recovery from existing swap");
                self.recover_from_swap()?;
                return Ok(());
            }
        }
        self.save_and_reset_swap_round()?;
        log::info!("Successfully Completed Coinswap");
        Ok(())
    }

    // ######## PROTOCOL SUBROUTINES ############

    /// Initiate the first coinswap hop. Makers are selected from the [OfferBook], and round will
    /// fail if no suitable makers are found.
    /// Creates and stores the [OutgoingSwapCoin] into [OngoingSwapState], and also saves it into the [Wallet] file.
    async fn init_first_hop(&mut self) -> Result<(), TakerError> {
        // Set the Taker Position state
        self.ongoing_swap_state.taker_position = TakerPosition::FirstPeer;

        // Locktime to be used for this swap.
        let swap_locktime = self.config.refund_locktime
            + self.config.refund_locktime_step * self.ongoing_swap_state.swap_params.maker_count;

        // Loop until we find a live maker who responded to our signature request.
        let (maker, funding_txs) = loop {
            // Fail early if not enough good makers in the list to satisfy swap requirements.
            let untried_maker_count = self.offerbook.get_all_untried().len();
            if untried_maker_count < self.ongoing_swap_state.swap_params.maker_count as usize {
                return Err(TakerError::NotEnoughMakersInOfferBook);
            }
            let maker = self.choose_next_maker()?.clone();
            let (multisig_pubkeys, multisig_nonces, hashlock_pubkeys, hashlock_nonces) =
                generate_maker_keys(
                    &maker.offer.tweakable_point,
                    self.ongoing_swap_state.swap_params.tx_count,
                );

            //TODO: Figure out where to use the fee.
            let (funding_txs, mut outgoing_swapcoins, _fee) = self.wallet.initalize_coinswap(
                self.ongoing_swap_state.swap_params.send_amount,
                &multisig_pubkeys,
                &hashlock_pubkeys,
                self.get_preimage_hash(),
                swap_locktime,
                self.ongoing_swap_state.swap_params.fee_rate,
            )?;

            let contract_reedemscripts = outgoing_swapcoins
                .iter()
                .map(|swapcoin| swapcoin.contract_redeemscript.clone())
                .collect();

            // Request for Sender's Signatures
            let contract_sigs = match self
                .req_sigs_for_sender(
                    &maker.address,
                    &outgoing_swapcoins,
                    &multisig_nonces,
                    &hashlock_nonces,
                    swap_locktime,
                )
                .await
            {
                Ok(contract_sigs) => contract_sigs,
                Err(e) => {
                    // Bad maker, mark it, and try next one.
                    self.offerbook.add_bad_maker(&maker);
                    log::debug!(
                        "Failed to obtain senders contract tx signature from first_maker {}: {:?}",
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
                    outgoing_swapcoin.others_contract_sig = Some(*sig)
                });

            for outgoing_swapcoin in &outgoing_swapcoins {
                self.wallet.add_outgoing_swapcoin(outgoing_swapcoin);
            }
            self.wallet.save_to_disk()?;

            self.ongoing_swap_state.outgoing_swapcoins = outgoing_swapcoins;

            break (maker, funding_txs);
        };

        // Boradcast amd wait for funding txs to confirm
        log::debug!("My Funding Txids:  {:#?}", funding_txs);
        log::debug!(
            "Outgoing SwapCoins: {:#?}",
            self.ongoing_swap_state.outgoing_swapcoins
        );

        let funding_txids = funding_txs
            .iter()
            .map(|tx| {
                let txid = self.wallet.rpc.send_raw_transaction(tx)?;
                log::info!("Broadcasting My Funding Tx: {}", txid);
                assert_eq!(txid, tx.txid());
                Ok(txid)
            })
            .collect::<Result<_, TakerError>>()?;

        // Watch for the funding transactions to be confirmed.
        // This errors in two cases.
        // TakerError::ContractsBroadcasted and TakerError::FundingTxWaitTimeOut.
        // For all cases, abort from swap immediately.
        // For the timeout case also ban the Peer.
        match self.watch_for_txs(&funding_txids).await {
            Ok(stuffs) => {
                self.ongoing_swap_state.funding_txs.push(stuffs);
                self.offerbook.add_good_maker(&maker);
            }
            Err(e) => {
                log::error!("Error: {:?}", e);
                if let TakerError::FundingTxWaitTimeOut = e {
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
    async fn watch_for_txs(
        &self,
        funding_txids: &Vec<Txid>,
    ) -> Result<(Vec<Transaction>, Vec<String>), TakerError> {
        let mut txid_tx_map = HashMap::<Txid, Transaction>::new();
        let mut txid_blockhash_map = HashMap::<Txid, BlockHash>::new();

        // Required confirmation target for the funding txs.
        let required_confirmations =
            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                self.ongoing_swap_state.swap_params.required_confirms
            } else {
                self.ongoing_swap_state
                    .peer_infos
                    .last()
                    .expect("Maker information expected in swap state")
                    .peer
                    .offer
                    .required_confirms
            };
        log::info!(
            "Waiting for funding transaction confirmations ({} conf required)",
            required_confirmations
        );
        let mut txids_seen_once = HashSet::<Txid>::new();

        // Wait for this much time for txs to appear in mempool.
        let wait_time = if cfg!(feature = "integration-test") {
            10u64 // 10 secs for the tests
        } else {
            60 * 5 // 5mins for production
        };

        let start_time = Instant::now();

        loop {
            // Abort if any of the contract transaction is broadcasted
            // TODO: Find the culprit Maker, and ban it's fidelity bond.
            let contracts_broadcasted = self.check_for_broadcasted_contract_txes();
            if !contracts_broadcasted.is_empty() {
                log::info!(
                    "Contract transactions were broadcasted : {:?}",
                    contracts_broadcasted
                );
                return Err(TakerError::ContractsBroadcasted(contracts_broadcasted));
            }

            // Check for funding transactions
            for txid in funding_txids {
                if txid_tx_map.contains_key(txid) {
                    continue;
                }
                let gettx = match self.wallet.rpc.get_raw_transaction_info(txid, None) {
                    Ok(r) => r,
                    // Transaction haven't arrived in our mempool, keep looping.
                    Err(_e) => {
                        let elapsed = start_time.elapsed().as_secs();
                        log::info!("Waiting for mempool transaction for {}secs", elapsed);
                        if elapsed > wait_time {
                            return Err(TakerError::FundingTxWaitTimeOut);
                        }
                        continue;
                    }
                };
                if !txids_seen_once.contains(txid) {
                    txids_seen_once.insert(*txid);
                    if gettx.confirmations == None {
                        let mempool_tx = match self.wallet.rpc.get_mempool_entry(txid) {
                            Ok(m) => m,
                            Err(_e) => continue,
                        };
                        log::info!(
                            "Seen in mempool: {} [{:.1} sat/vbyte]",
                            txid,
                            mempool_tx.fees.base.to_sat() as f32 / mempool_tx.vsize as f32
                        );
                    }
                }
                //TODO handle confirm<0
                if gettx.confirmations >= Some(required_confirmations as u32) {
                    txid_tx_map.insert(*txid, deserialize::<Transaction>(&gettx.hex).unwrap());
                    txid_blockhash_map.insert(*txid, gettx.blockhash.unwrap());
                    log::debug!(
                        "funding tx {} reached {} confirmation(s)",
                        txid,
                        required_confirmations
                    );
                }
            }
            if txid_tx_map.len() == funding_txids.len() {
                log::info!("Funding Transactions confirmed");
                let txes = funding_txids
                    .iter()
                    .map(|txid| txid_tx_map.get(txid).unwrap().clone())
                    .collect::<Vec<Transaction>>();
                let merkleproofs = funding_txids
                    .iter()
                    .map(|&txid| {
                        self.wallet
                            .rpc
                            .get_tx_out_proof(&[txid], Some(txid_blockhash_map.get(&txid).unwrap()))
                            .map(|gettxoutproof_result| to_hex(&gettxoutproof_result))
                    })
                    .collect::<Result<Vec<String>, _>>()?;
                return Ok((txes, merkleproofs));
            }
            sleep(Duration::from_millis(1000)).await;
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
                        .unwrap()
                        .iter()
                        .map(|s| s.get_multisig_redeemscript())
                        .collect::<Vec<_>>(),
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .last()
                        .unwrap()
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
    async fn send_sigs_init_next_hop(
        &mut self,
        maker_refund_locktime: u16,
        funding_tx_infos: &Vec<FundingTxInfo>,
    ) -> Result<(NextPeerInfo, ContractSigsAsRecvrAndSender), TakerError> {
        let reconnect_timeout_sec = self.config.reconnect_attempt_timeout_sec;
        // Configurable reconnection attempts for testing
        let reconnect_attempts = if cfg!(feature = "integration-test") {
            10
        } else {
            self.config.reconnect_attempts
        };

        // Custom sleep delay for testing.
        let sleep_delay = if cfg!(feature = "integration-test") {
            1
        } else {
            self.config.reconnect_short_sleep_delay
        };

        let mut ii = 0;
        loop {
            ii += 1;
            select! {
                ret = self.send_sigs_init_next_hop_once(
                    maker_refund_locktime,
                    funding_tx_infos
                ) => {
                    match ret {
                        Ok(return_value) => return Ok(return_value),
                        Err(e) => {
                            let maker = &self.ongoing_swap_state.peer_infos.last().expect("at least one active maker expected").peer;
                            log::error!(
                                "Failed to exchange signatures with maker {}, \
                                reattempting... error={:?}",
                                &maker.address,
                                e
                            );
                            // If its a protocol error and not just connection error, scream hard.
                            if let TakerError::Protocol(msg) = e {
                                return Err(TakerError::Protocol(msg))
                            }
                            if ii <= reconnect_attempts {
                                sleep(Duration::from_secs(
                                    if ii <= self.config.short_long_sleep_delay_transition {
                                        sleep_delay
                                    } else {
                                        self.config.reconnect_long_sleep_delay
                                    },
                                ))
                                .await;
                                continue;
                            } else {
                                // Attempt count exceeded. Ban this maker.
                                log::warn!("Connection attempt count exceeded with Maker:{}, Banning Maker.", maker.address);
                                self.offerbook.add_bad_maker(&maker);
                                return Err(e);
                            }
                        }
                    }
                },
                _ = sleep(Duration::from_secs(reconnect_timeout_sec)) => {
                    log::warn!(
                        "Timeout for exchange signatures with maker {}, reattempting...",
                        &self.ongoing_swap_state.peer_infos.last().expect("at least one active maker expected").peer.address
                    );
                    if ii <= reconnect_attempts {
                        continue;
                    } else {
                        // Timeout exceeded. Ban this maker.
                        let maker = &self.ongoing_swap_state.peer_infos.last().expect("atleast one maker expected at this stage").peer;
                        log::warn!("Connection timeout exceeded with Maker:{}, Banning Maker.", maker.address);
                        self.offerbook.add_bad_maker(maker);
                        return Err(NetError::ConnectionTimedOut.into());
                    }
                },
            }
        }
    }

    /// [Internal] Single attempt to send signatures and initiate next hop.
    async fn send_sigs_init_next_hop_once(
        &mut self,
        maker_refund_locktime: u16,
        funding_tx_infos: &Vec<FundingTxInfo>,
    ) -> Result<(NextPeerInfo, ContractSigsAsRecvrAndSender), TakerError> {
        let this_maker = &self
            .ongoing_swap_state
            .peer_infos
            .last()
            .expect("at least one active maker expected")
            .peer;

        let previous_maker = self.ongoing_swap_state.peer_infos.iter().rev().nth(1);

        log::info!("Connecting to {}", this_maker.address);
        let mut socket = TcpStream::connect(this_maker.address.get_tcpstream_address()).await?;
        let (mut socket_reader, mut socket_writer) =
            handshake_maker(&mut socket, &this_maker.address).await?;
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
                )
            };

            let this_maker_contract_txs =
                if self.ongoing_swap_state.taker_position == TakerPosition::FirstPeer {
                    self.ongoing_swap_state
                        .outgoing_swapcoins
                        .iter()
                        .map(|os| os.get_contract_tx())
                        .collect()
                } else {
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .last()
                        .expect("at least one outgoing swpcoin expected")
                        .iter()
                        .map(|wos| wos.get_contract_tx())
                        .collect()
                };

            log::info!("===> Sending ProofOfFunding to {}", this_maker.address);

            let funding_txids = funding_tx_infos
                .iter()
                .map(|fi| fi.funding_tx.txid())
                .collect::<Vec<_>>();

            log::info!("Fundix Txids: {:?}", funding_txids);

            let (contract_sigs_as_recvr_sender, next_swap_contract_redeemscripts) =
                send_proof_of_funding_and_init_next_hop(
                    &mut socket_reader,
                    &mut socket_writer,
                    this_maker,
                    funding_tx_infos,
                    &next_peer_multisig_pubkeys,
                    &next_peer_hashlock_pubkeys,
                    maker_refund_locktime,
                    self.ongoing_swap_state.swap_params.fee_rate,
                    &this_maker_contract_txs,
                    self.get_preimage_hash(),
                )
                .await?;
            log::info!(
                "<=== Recieved ContractSigsAsRecvrAndSender from {}",
                this_maker.address
            );

            // If This Maker is the Sender, and we (the Taker) are the Receiver (Last Hop). We provide the Sender's Contact Tx Sigs.
            let senders_sigs = if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer
            {
                log::info!("Taker is next peer. Signing Sender's Contract Txs",);
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
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(ProtocolError::Contract)?
            } else {
                // If Next Maker is the Receiver, and This Maker is The Sender, Request Sender's Contract Tx Sig to Next Maker.
                let watchonly_swapcoins = self.create_watch_only_swapcoins(
                    &contract_sigs_as_recvr_sender,
                    &next_peer_multisig_pubkeys,
                    &next_swap_contract_redeemscripts,
                )?;
                let sigs = match self
                    .req_sigs_for_sender(
                        &next_maker.address,
                        &watchonly_swapcoins,
                        &next_peer_multisig_keys_or_nonces,
                        &next_peer_hashlock_keys_or_nonces,
                        maker_refund_locktime,
                    )
                    .await
                {
                    Ok(r) => {
                        self.offerbook.add_good_maker(&next_maker);
                        r
                    }
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
            log::info!("Taker is previous peer. Signing Receivers Contract Txs",);
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
            log::info!(
                "===> Sending SignReceiversContractTx, previous maker is {}",
                previous_maker_addr,
            );
            let previous_maker_watchonly_swapcoins =
                if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                    self.ongoing_swap_state.watchonly_swapcoins.last().unwrap()
                } else {
                    //if the next peer is a maker not a taker, then that maker's swapcoins are last
                    &self.ongoing_swap_state.watchonly_swapcoins
                        [self.ongoing_swap_state.watchonly_swapcoins.len() - 2]
                };
            let sigs = match self
                .req_sigs_for_recvr(
                    previous_maker_addr,
                    previous_maker_watchonly_swapcoins,
                    &contract_sigs_as_recvr_sender.receivers_contract_txs,
                )
                .await
            {
                Ok(s) => s.sigs,
                Err(e) => {
                    log::error!("Could not get Receiver's signatures : {:?}", e);
                    log::warn!("Banning Maker : {}", previous_maker.peer.address);
                    self.offerbook.add_bad_maker(&previous_maker.peer);
                    return Err(e);
                }
            };
            sigs
        };
        log::info!(
            "===> Sending ContractSigsAsReceiverAndSender to {}",
            this_maker.address
        );
        send_message(
            &mut socket_writer,
            &TakerToMakerMessage::RespContractSigsForRecvrAndSender(
                ContractSigsForRecvrAndSender {
                    receivers_sigs,
                    senders_sigs,
                },
            ),
        )
        .await?;
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
    pub fn create_watch_only_swapcoins(
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
                .import_watchonly_redeemscript(&swapcoin.get_multisig_redeemscript())?
        }
        Ok(next_swapcoins)
    }

    /// Create the [IncomingSwapCoin] for this round. The Taker is always the "next_peer" here
    /// and the sender side is the laste Maker in the route.
    fn create_incoming_swapcoins(
        &self,
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
                let multisig_spk = redeemscript_to_scriptpubkey(&multisig_redeemscript);
                let index = makers_funding_tx
                    .output
                    .iter()
                    .enumerate()
                    .find(|(_i, o)| o.script_pubkey == multisig_spk)
                    .map(|(index, _)| index)
                    .expect("funding txout output doesn't match with mutlsig scriptpubkey");
                makers_funding_tx
                    .output
                    .iter()
                    .nth(index)
                    .expect("output expected at that index")
                    .value
            })
            .collect::<Vec<_>>();

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
            .collect::<Vec<Transaction>>();

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
                funding_tx,
            ),
            funding_tx_merkleproof,
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
                )
                .map_err(ProtocolError::Contract)?;
            let maker_funded_other_multisig_pubkey = if o_ms_pubkey1 == maker_funded_multisig_pubkey
            {
                o_ms_pubkey2
            } else {
                if o_ms_pubkey2 != maker_funded_multisig_pubkey {
                    return Err(ProtocolError::Contract(ContractError::Protocol(
                        "maker-funded multisig doesnt match",
                    ))
                    .into());
                }
                o_ms_pubkey1
            };

            self.wallet
                .import_wallet_multisig_redeemscript(&o_ms_pubkey1, &o_ms_pubkey2)?;
            self.wallet
                .import_tx_with_merkleproof(funding_tx, funding_tx_merkleproof)?;
            self.wallet
                .import_wallet_contract_redeemscript(next_contract_redeemscript)?;

            let mut incoming_swapcoin = IncomingSwapCoin::new(
                maker_funded_multisig_privkey,
                maker_funded_other_multisig_pubkey,
                my_receivers_contract_tx.clone(),
                next_contract_redeemscript.clone(),
                hashlock_privkey,
                maker_funding_tx_value,
            );
            incoming_swapcoin.hash_preimage = Some(self.ongoing_swap_state.active_preimage);
            incoming_swapcoins.push(incoming_swapcoin);
        }

        Ok(incoming_swapcoins)
    }

    /// Request signatures for the [IncomingSwapCoin] from the last maker of the swap round.
    async fn request_sigs_for_incoming_swap(&mut self) -> Result<(), TakerError> {
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
        log::info!(
            "===> Sending ReqContractSigsForRecvr to {}",
            last_maker.address
        );
        let receiver_contract_sig = match self
            .req_sigs_for_recvr(
                &last_maker.address,
                &self.ongoing_swap_state.incoming_swapcoins,
                &self
                    .ongoing_swap_state
                    .incoming_swapcoins
                    .iter()
                    .map(|swapcoin| swapcoin.contract_tx.clone())
                    .collect::<Vec<Transaction>>(),
            )
            .await
        {
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
    async fn req_sigs_for_sender<S: SwapCoin>(
        &self,
        maker_address: &MakerAddress,
        outgoing_swapcoins: &[S],
        maker_multisig_nonces: &[SecretKey],
        maker_hashlock_nonces: &[SecretKey],
        locktime: u16,
    ) -> Result<ContractSigsForSender, TakerError> {
        // Configurable reconnection attempts for testing
        let first_connect_attempts = if cfg!(feature = "integration-test") {
            10
        } else {
            self.config.first_connect_attempts
        };

        // Custom sleep delay for testing.
        let sleep_delay = if cfg!(feature = "integration-test") {
            1
        } else {
            self.config.first_connect_sleep_delay_sec
        };

        let mut ii = 0;
        loop {
            ii += 1;
            select! {
                ret = req_sigs_for_sender_once(
                    maker_address,
                    outgoing_swapcoins,
                    maker_multisig_nonces,
                    maker_hashlock_nonces,
                    locktime,
                ) => {
                    match ret {
                        Ok(sigs) => return Ok(sigs),
                        Err(e) => {
                            log::warn!(
                                "Failed to request senders contract tx sigs from maker {}, \
                                reattempting... error={:?}",
                                maker_address,
                                e
                            );
                            if ii <= first_connect_attempts {
                                sleep(Duration::from_secs(sleep_delay)).await;
                                continue;
                            } else {
                                return Err(e);
                            }
                        }
                    }
                },
                _ = sleep(Duration::from_secs(self.config.first_connect_attempt_timeout_sec)) => {
                    log::warn!(
                        "Timeout for request senders contract tx sig from maker {}, reattempting...",
                        maker_address
                    );
                    if ii <= self.config.first_connect_attempts {
                        continue;
                    } else {
                        return Err(NetError::ConnectionTimedOut.into());
                    }
                },
            }
        }
    }

    /// Request signatures for receiver side of the swap.
    /// Keep trying until `reconnect_attempts` limit, with a time delay.
    /// The time delay transitions from `reconnect_short_slepp_delay` to `reconnect_locg_sleep_delay`,
    /// after `short_long_sleep_delay_transition` time.
    async fn req_sigs_for_recvr<S: SwapCoin>(
        &self,
        maker_address: &MakerAddress,
        incoming_swapcoins: &[S],
        receivers_contract_txes: &[Transaction],
    ) -> Result<ContractSigsForRecvr, TakerError> {
        // Configurable reconnection attempts for testing
        let reconnect_attempts = if cfg!(feature = "integration-test") {
            10
        } else {
            self.config.reconnect_attempts
        };

        // Custom sleep delay for testing.
        let sleep_delay = if cfg!(feature = "integration-test") {
            1
        } else {
            self.config.reconnect_short_sleep_delay
        };

        let mut ii = 0;
        loop {
            ii += 1;
            select! {
                ret = req_sigs_for_recvr_once(
                    maker_address,
                    incoming_swapcoins,
                    receivers_contract_txes,
                ) => {
                    match ret {
                        Ok(sigs) => return Ok(sigs),
                        Err(e) => {
                            log::warn!(
                                "Failed to request receivers contract tx sigs from maker {}, \
                                reattempting... error={:?}",
                                maker_address,
                                e
                            );
                            if ii <= reconnect_attempts {
                                sleep(Duration::from_secs(
                                    if ii <= self.config.short_long_sleep_delay_transition {
                                        sleep_delay
                                    } else {
                                        self.config.reconnect_long_sleep_delay
                                    },
                                ))
                                .await;
                                continue;
                            } else {
                                return Err(e);
                            }
                        }
                    }
                },
                _ = sleep(Duration::from_secs(self.config.reconnect_attempt_timeout_sec)) => {
                    log::warn!(
                        "Timeout for request receivers contract tx sig from maker {}, reattempting...",
                        maker_address
                    );
                    if ii <= self.config.reconnect_attempts {
                        continue;
                    } else {
                        return Err(NetError::ConnectionTimedOut.into());
                    }
                },
            }
        }
    }

    /// Settle all the ongoing swaps. This routine sends the hash preimage to all the makers.
    /// Pass around the Maker's multisig privatekeys. Saves all the data in wallet file. This marks
    /// the ends of swap round.
    async fn settle_all_swaps(&mut self) -> Result<(), TakerError> {
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
            } else if index == (self.ongoing_swap_state.swap_params.maker_count - 1) as usize {
                self.ongoing_swap_state.taker_position = TakerPosition::LastPeer
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

            let reconnect_time_out = self.config.reconnect_attempt_timeout_sec;

            let mut ii = 0;
            loop {
                // Configurable reconnection attempts for testing
                let reconnect_attempts = if cfg!(feature = "integration-test") {
                    10
                } else {
                    self.config.reconnect_attempts
                };

                // Custom sleep delay for testing.
                let sleep_delay = if cfg!(feature = "integration-test") {
                    1
                } else {
                    self.config.reconnect_short_sleep_delay
                };

                ii += 1;
                select! {
                    ret = self.settle_one_coinswap(
                        &maker_address.address,
                        index,
                        &mut outgoing_privkeys,
                        &senders_multisig_redeemscripts,
                        &receivers_multisig_redeemscripts,
                    ) => {
                        if let Err(e) = ret {
                            log::warn!(
                                "Failed to connect to maker {} to settle coinswap, \
                                reattempting... error={:?}",
                                &maker_address.address,
                                e
                            );
                            if ii <= reconnect_attempts {
                                sleep(Duration::from_secs(
                                    if ii <= self.config.short_long_sleep_delay_transition {
                                        sleep_delay
                                    } else {
                                        self.config.reconnect_long_sleep_delay
                                    },
                                ))
                                .await;
                                continue;
                            } else {
                                self.offerbook.add_bad_maker(&maker_address);
                                return Err(e);
                            }
                        }
                        break;
                    },
                    _ = sleep(Duration::from_secs(reconnect_time_out)) => {
                        log::warn!(
                            "Timeout for settling coinswap with maker {}, reattempting...",
                            maker_address.address
                        );
                        if ii <= self.config.reconnect_attempts {
                            continue;
                        } else {
                            self.offerbook.add_bad_maker(&maker_address);
                            return Err(NetError::ConnectionTimedOut.into());
                        }
                    },
                }
            }
        }
        Ok(())
    }

    /// [Internal] Setlle one swap. This is recursively called for all the makers.
    async fn settle_one_coinswap<'a>(
        &mut self,
        maker_address: &MakerAddress,
        index: usize,
        outgoing_privkeys: &mut Option<Vec<MultisigPrivkey>>,
        senders_multisig_redeemscripts: &Vec<ScriptBuf>,
        receivers_multisig_redeemscripts: &Vec<ScriptBuf>,
    ) -> Result<(), TakerError> {
        log::info!("Connecting to {}", maker_address);
        let mut socket = TcpStream::connect(maker_address.get_tcpstream_address()).await?;
        let (mut socket_reader, mut socket_writer) =
            handshake_maker(&mut socket, maker_address).await?;

        log::info!("===> Sending HashPreimage to {}", maker_address);
        let maker_private_key_handover = send_hash_preimage_and_get_private_keys(
            &mut socket_reader,
            &mut socket_writer,
            senders_multisig_redeemscripts,
            receivers_multisig_redeemscripts,
            &self.ongoing_swap_state.active_preimage,
        )
        .await?;
        log::info!("<=== Received PrivateKeyHandover from {}", maker_address);

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
            let reply = outgoing_privkeys.as_ref().unwrap().to_vec();
            *outgoing_privkeys = None;
            reply
        };
        if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
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
        }?;
        log::info!("===> Sending PrivateKeyHandover to {}", maker_address);
        send_message(
            &mut socket_writer,
            &TakerToMakerMessage::RespPrivKeyHandover(PrivKeyHandover {
                multisig_privkeys: privkeys_reply,
            }),
        )
        .await?;
        Ok(())
    }

    // ######## UTILITY AND HELPERS ############

    /// Choose a suitable **untried** maker address from the offerbook that fits the swap params.
    fn choose_next_maker(&self) -> Result<&OfferAndAddress, TakerError> {
        let send_amount = self.ongoing_swap_state.swap_params.send_amount;
        if send_amount == 0 {
            return Err(TakerError::SendAmountNotSet);
        }

        // Ensure that we don't select a maker we are already swaping with.
        Ok(self
            .offerbook
            .get_all_untried()
            .iter()
            .find(|oa| {
                send_amount > oa.offer.min_size
                    && send_amount < oa.offer.max_size
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

    pub fn get_bad_makers(&self) -> Vec<&OfferAndAddress> {
        self.offerbook.get_bad_makers()
    }

    /// Save all the finalized swap data and reset the [OngoingSwapState].
    fn save_and_reset_swap_round(&mut self) -> Result<(), TakerError> {
        for (index, watchonly_swapcoin) in self
            .ongoing_swap_state
            .watchonly_swapcoins
            .iter()
            .enumerate()
        {
            log::debug!(
                "maker[{}] funding txes = {:#?}",
                index,
                watchonly_swapcoin
                    .iter()
                    .map(|w| w.contract_tx.input[0].previous_output.txid)
                    .collect::<Vec<_>>()
            );
        }
        log::debug!(
            "my incoming txes = {:#?}",
            self.ongoing_swap_state
                .incoming_swapcoins
                .iter()
                .map(|w| w.contract_tx.input[0].previous_output.txid)
                .collect::<Vec<_>>()
        );

        for incoming_swapcoin in &self.ongoing_swap_state.incoming_swapcoins {
            self.wallet
                .find_incoming_swapcoin_mut(&incoming_swapcoin.get_multisig_redeemscript())
                .unwrap()
                .other_privkey = incoming_swapcoin.other_privkey;
        }
        self.wallet.save_to_disk()?;

        self.clear_ongoing_swaps();

        Ok(())
    }

    /// Checks if any contreact transactions have been broadcasted.
    /// Returns the txid list of all the broadcasted contract transaction.
    /// Empty vector if nothing is nothing is broadcasted. (usual case).
    pub fn check_for_broadcasted_contract_txes(&self) -> Vec<Txid> {
        let contract_txids = self
            .ongoing_swap_state
            .incoming_swapcoins
            .iter()
            .map(|sc| sc.contract_tx.txid())
            .chain(
                self.ongoing_swap_state
                    .outgoing_swapcoins
                    .iter()
                    .map(|sc| sc.contract_tx.txid()),
            )
            .chain(
                self.ongoing_swap_state
                    .watchonly_swapcoins
                    .iter()
                    .flatten()
                    .map(|sc| sc.contract_tx.txid()),
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
            if let Ok(_) = self
                .wallet
                .rpc
                .get_raw_transaction_info(&contract_tx.txid(), None)
            {
                log::info!("Incoming Contract already broadacsted");
            } else {
                self.wallet.rpc.send_raw_transaction(contract_tx)?;
                log::info!(
                    "Broadcasted Incoming Contract. Removing from wallet. Contract Txid {}",
                    contract_tx.txid()
                );
            }
            log::info!(
                "Incoming Swapcoin removed from wallet, Contact Txid: {}",
                contract_tx.txid()
            );
            self.wallet.remove_incoming_swapcoin(&redeemscript)?;
        }

        let mut outgoing_infos = Vec::new();

        // Broadcast the Outgoing Contracts
        for outgoing in outgoings {
            let contract_tx = outgoing.get_fully_signed_contract_tx()?;
            if let Ok(_) = self
                .wallet
                .rpc
                .get_raw_transaction_info(&contract_tx.txid(), None)
            {
                log::info!("Outgoing Contract already broadcasted");
            } else {
                self.wallet.rpc.send_raw_transaction(&contract_tx)?;
                log::info!(
                    "Broadcasted Outgoing Contract, Contract txid : {}",
                    contract_tx.txid()
                );
            }
            let reedemscript = outgoing.get_multisig_redeemscript();
            let timelock = outgoing.get_timelock();
            let next_internal = &self.wallet.get_next_internal_addresses(1)?[0];
            let timelock_spend = outgoing.create_timelock_spend(next_internal);
            outgoing_infos.push(((reedemscript, contract_tx), (timelock, timelock_spend)));
        }

        // Check for contract confirmations and broadcast timelocked transaction
        let mut timelock_boardcasted = Vec::new();

        // Start the loop to keep checking for timelock maturity, and spend from the contract asap.
        loop {
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
                    .get_raw_transaction_info(&contract.txid(), None)
                {
                    log::info!(
                        "Contract Tx : {}, reached confirmation : {:?}, required : {}",
                        contract.txid(),
                        result.confirmations,
                        timelock
                    );
                    if let Some(confirmation) = result.confirmations {
                        // Now the transaction is confirmed in a block, check for required maturity
                        if confirmation > *timelock as u32 {
                            log::info!(
                                "Timelock maturity of {} blocks for Contract Tx is reached : {}",
                                timelock,
                                contract.txid()
                            );
                            log::info!("Broadcasting timelocked tx: {}", timelocked_tx.txid());
                            self.wallet.rpc.send_raw_transaction(timelocked_tx).unwrap();
                            timelock_boardcasted.push(timelocked_tx);

                            let outgoing_removed = self
                                .wallet
                                .remove_outgoing_swapcoin(&reedemscript)
                                .unwrap()
                                .expect("outgoing swapcoin expected");
                            log::info!(
                                "Removed Outgoing Swapcoin from Wallet, Contract Txid: {}",
                                outgoing_removed.contract_tx.txid()
                            );
                        }
                    }
                }
                // Everything is broadcasted. Clear the connectionstate and break the loop
                if timelock_boardcasted.len() == outgoing_infos.len() {
                    self.clear_ongoing_swaps(); // This could be a bug if Taker is in middle of multiple swaps. For now we assume Taker will only do one swap at a time.
                    log::info!("All outgoing contracts reedemed. Cleared ongoing swap state",);
                    self.wallet.sync()?;
                    return Ok(());
                }
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
}
