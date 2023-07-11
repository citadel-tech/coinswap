use std::{
    collections::{BTreeSet, HashMap, HashSet},
    iter::once,
    time::Duration,
};

use tokio::{
    io::BufReader,
    net::{
        tcp::{ReadHalf, WriteHalf},
        TcpStream,
    },
    select,
    time::sleep,
};

use tokio_socks::tcp::Socks5Stream;

use bitcoin::{
    consensus::encode::deserialize,
    hashes::{hash160::Hash as Hash160, hex::ToHex, Hash},
    secp256k1::{
        rand::{rngs::OsRng, RngCore},
        SecretKey,
    },
    util::ecdsa::PublicKey,
    BlockHash, OutPoint, Script, Transaction, Txid,
};
use bitcoincore_rpc::{Client, RpcApi};

use itertools::izip;

use crate::{
    contracts::{
        calculate_coinswap_fee, create_contract_redeemscript, find_funding_output,
        validate_contract_tx, SwapCoin, WatchOnlySwapCoin, MAKER_FUNDING_TX_VBYTE_SIZE,
    },
    error::TeleportError,
    messages::{
        ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForRecvrAndSender,
        ContractSigsForSender, ContractTxInfoForRecvr, ContractTxInfoForSender, FundingTxInfo,
        HashPreimage, MakerToTakerMessage, MultisigPrivkey, NextHopInfo, Offer, Preimage,
        PrivKeyHandover, ProofOfFunding, ReqContractSigsForRecvr, ReqContractSigsForSender,
        TakerHello, TakerToMakerMessage,
    },
};

use crate::{
    offerbook_sync::{sync_offerbook, MakerAddress, OfferAndAddress},
    wallet_sync::{generate_keypair, IncomingSwapCoin, OutgoingSwapCoin, Wallet},
};

use crate::watchtower_protocol::{
    check_for_broadcasted_contract_txes, ContractTransaction, ContractsInfo,
};

use crate::util::*;

//relatively low value for now so that its easier to test without having to wait too much
//right now only the very brave will try coinswap out on mainnet with non-trivial amounts
pub const REFUND_LOCKTIME: u16 = 48; //in blocks
pub const REFUND_LOCKTIME_STEP: u16 = 48; //in blocks

//first connect means the first time you're ever connecting, without having gotten any txes
// confirmed yet, so the taker will not be very persistent since there should be plenty of other
// makers out there
//but also it should allow for flaky connections, otherwise you exclude raspberry pi nodes running
// in people's closets, which are very important for decentralization
pub const FIRST_CONNECT_ATTEMPTS: u32 = 5;
pub const FIRST_CONNECT_SLEEP_DELAY_SEC: u64 = 1;
pub const FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC: u64 = 20;

//reconnect means when connecting to a maker again after having already gotten txes confirmed
// as it would be a waste of miner fees to give up, the taker is coded to be very persistent
//taker will first attempt to connect with a short delay between attempts
// after that will attempt to connect with a longer delay between attempts
//these figures imply that taker will attempt to connect for just over 48 hours
// of course the user can ctrl+c before then if they give up themselves
const RECONNECT_ATTEMPTS: u32 = 3200;
const RECONNECT_SHORT_SLEEP_DELAY_SEC: u64 = 10;
const RECONNECT_LONG_SLEEP_DELAY_SEC: u64 = 60;
const SHORT_LONG_SLEEP_DELAY_TRANSITION: u32 = 60; //after this many attempts, switch to sleeping longer
const RECONNECT_ATTEMPT_TIMEOUT_SEC: u64 = 60 * 5;

/// Various global configurations defining the Taker behavior.
/// TODO: Optionally read this from a config file.
struct TakerConfig {
    refund_locktime: u16,
    refund_locktime_step: u16,

    first_connect_attempts: u32,
    first_connect_sleep_delay_sec: u64,
    first_connect_attempt_timeout_sec: u64,

    reconnect_attempts: u32,
    reconnect_short_slepp_delay: u64,
    reconnect_locg_sleep_delay: u64,
    short_long_sleep_delay_transition: u32,
    reconnect_attempt_timeout_sec: u64,
}

impl Default for TakerConfig {
    fn default() -> Self {
        Self {
            refund_locktime: REFUND_LOCKTIME,
            refund_locktime_step: REFUND_LOCKTIME_STEP,
            first_connect_attempts: FIRST_CONNECT_ATTEMPTS,
            first_connect_sleep_delay_sec: FIRST_CONNECT_SLEEP_DELAY_SEC,
            first_connect_attempt_timeout_sec: FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC,
            reconnect_attempts: RECONNECT_ATTEMPTS,
            reconnect_short_slepp_delay: RECONNECT_SHORT_SLEEP_DELAY_SEC,
            reconnect_locg_sleep_delay: RECONNECT_LONG_SLEEP_DELAY_SEC,
            short_long_sleep_delay_transition: SHORT_LONG_SLEEP_DELAY_TRANSITION,
            reconnect_attempt_timeout_sec: RECONNECT_ATTEMPT_TIMEOUT_SEC,
        }
    }
}

/// Swap specific parameters. These are user's policy and can differ among swaps.
/// SwapParams govern the criteria to find suitable set of makers from the offerbook.
/// If no maker matches with a given SwapParam, that coinswap round will fail.
#[derive(Debug, Clone, Copy)]
pub struct SwapParams {
    /// Total Amount to Swap.
    pub send_amount: u64,
    /// How many hops.
    pub maker_count: u16,
    /// How many splits
    pub tx_count: u32,
    // TODO: Following two should be moved to TakerConfig as global configuration.
    /// Confirmation count required for funding txs.
    pub required_confirms: i32,
    /// Fee rate for funding txs.
    pub fee_rate: u64,
}

// Default implies an "unset" SwapParams. Using this would fail the round as send_amount is 0.
impl Default for SwapParams {
    fn default() -> Self {
        Self {
            send_amount: 0,
            maker_count: 0,
            tx_count: 0,
            required_confirms: 0,
            fee_rate: 0,
        }
    }
}

/// An ephemeral Offerbook tracking good and bad makers. Currently, Offerbook is initiated
/// at start of every swap. So good and bad maker list will ot be persisted.
// TODO: Persist the offerbook in disk.
#[derive(Debug, Default)]
struct OfferBook {
    all_makers: BTreeSet<OfferAndAddress>,
    good_makers: BTreeSet<OfferAndAddress>,
    bad_makers: BTreeSet<OfferAndAddress>,
}

impl OfferBook {
    fn get_all_untried(&self) -> BTreeSet<OfferAndAddress> {
        // TODO: Remove the clones and return BTreeSet<&OfferAndAddress>
        self.all_makers
            .difference(&self.bad_makers.union(&self.good_makers).cloned().collect())
            .cloned()
            .collect()
    }

    fn add_new_offer(&mut self, offer: &OfferAndAddress) -> bool {
        self.all_makers.insert(offer.clone())
    }

    fn add_good_maker(&mut self, good_maker: &OfferAndAddress) -> bool {
        self.good_makers.insert(good_maker.clone())
    }

    fn add_bad_maker(&mut self, bad_maker: &OfferAndAddress) -> bool {
        self.bad_makers.insert(bad_maker.clone())
    }
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
/// This states can be used to recover from a failed swap round. Looking at the State at the time of failure
/// will give us all the information regarding the failure and recovery.
#[derive(Debug, Default)]
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
    /// The last entry at the end of the swap round from this Taker as it's the last peer.
    pub peer_infos: Vec<NextPeerInfo>,
    /// List of funding transactions with optional merkleproofs.
    /// TODO: Drop Option. Make merkleproofs "required".
    pub funding_txs: Vec<(Vec<Transaction>, Vec<String>)>,
    /// The preimage being used for this coinswap round.
    pub active_preimage: Preimage,
    /// Enum defining the position of the Taker at each steps of a multihop swap.
    pub taker_position: TakerPosition,
    /// Height that the wallet last checked for relevant transactions of this swap.
    pub last_synced_height: Option<u64>,
}

/// Information for the next peer in the hop.
#[derive(Debug, Clone)]
struct NextPeerInfo {
    peer: OfferAndAddress,
    multisig_pubkeys: Vec<PublicKey>,
    multisig_nonces: Vec<SecretKey>,
    hashlock_nonces: Vec<SecretKey>,
    // TODO: Remove. This information is already available in swapcoins.
    contract_reedemscripts: Vec<Script>,
}

/// The Taker structure that performs bulk of the coinswap protocol. Taker connects
/// to multiple Makers and send protocol messages sequentially to them. The communication
/// sequence and corresponding SwapCoin infos are stored in `ongoing_swap_state`.
struct Taker<'taker> {
    /// Wllate managed by the Taker.
    // TODO: Take ownership instead of reference.
    wallet: &'taker mut Wallet,
    /// RPC client used for wallet operations.
    // TODO: This should be owned by the wallet.
    rpc: &'taker Client,
    config: TakerConfig,
    offerbook: OfferBook,
    ongoing_swap_state: OngoingSwapState,
}

impl<'taker> Taker<'taker> {
    fn init(wallet: &'taker mut Wallet, rpc: &'taker Client, offers: Vec<OfferAndAddress>) -> Self {
        let mut offerbook = OfferBook::default();
        offers.iter().for_each(|offer| {
            offerbook.add_new_offer(offer);
        });
        Self {
            wallet,
            rpc,
            config: TakerConfig::default(),
            offerbook,
            ongoing_swap_state: OngoingSwapState::default(),
        }
    }

    async fn send_coinswap(&mut self, swap_params: SwapParams) -> Result<(), TeleportError> {
        let mut preimage = [0u8; 32];
        let mut rng = OsRng::new().unwrap();
        rng.fill_bytes(&mut preimage);

        self.ongoing_swap_state.active_preimage = preimage;
        self.ongoing_swap_state.swap_params = swap_params;

        self.initiate_coinswap().await?;

        // This loop is performed for all the makers to make intermediate coinswap hops.
        for maker_index in 0..self.ongoing_swap_state.swap_params.maker_count {
            if maker_index == 0 {
                self.ongoing_swap_state.taker_position = TakerPosition::FirstPeer
            } else if maker_index == self.ongoing_swap_state.swap_params.maker_count - 1 {
                self.ongoing_swap_state.taker_position = TakerPosition::LastPeer
            } else {
                self.ongoing_swap_state.taker_position = TakerPosition::WatchOnly
            }

            let maker_refund_locktime = self.config.refund_locktime
                + self.config.refund_locktime_step
                    * (self.ongoing_swap_state.swap_params.maker_count - maker_index - 1);

            let funding_tx_infos = self.create_fundingtxs_info_for_next_maker();

            let (next_swap_info, contract_sigs_as_recvr_and_sender) = self
                .exchange_signatures_and_find_next_maker(maker_refund_locktime, &funding_tx_infos)
                .await?;

            self.ongoing_swap_state
                .peer_infos
                .push(next_swap_info.clone());

            let wait_for_confirm_result = self
                .wait_for_funding_tx_confirmation(
                    &contract_sigs_as_recvr_and_sender
                        .senders_contract_txs_info
                        .iter()
                        .map(|senders_contract_tx_info| {
                            senders_contract_tx_info.contract_tx.input[0]
                                .previous_output
                                .txid
                        })
                        .collect::<Vec<Txid>>(),
                )
                .await?;

            // TODO: Recovery script should be run automatically when this happens.
            // With more logging information of which maker deviated, and banning their fidelity bond.
            if wait_for_confirm_result.is_none() {
                log::info!(concat!(
                    "Somebody deviated from the protocol by broadcasting one or more contract",
                    " transactions! Use main method `recover-from-incomplete-coinswap` to recover",
                    " coins"
                ));
                panic!("ending");
            }
            let (next_funding_txes, next_funding_tx_merkleproofs) =
                wait_for_confirm_result.unwrap();

            self.ongoing_swap_state
                .funding_txs
                .push((next_funding_txes, next_funding_tx_merkleproofs));

            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                let incoming_swapcoins =
                    self.create_incoming_swapcoins(&contract_sigs_as_recvr_and_sender)?;
                self.ongoing_swap_state.incoming_swapcoins = incoming_swapcoins;
            }
        }

        self.request_signature_for_last_hop().await?;

        self.settle_all_coinswaps().await?;

        self.finish_and_save_swap_round();

        log::info!("Successfully Completed Coinswap");
        Ok(())
    }

    /// Choose a suitable untried maker address from the offerbook that fits the current swap params.
    fn choose_next_maker(&self) -> Result<OfferAndAddress, TeleportError> {
        let send_amount = self.ongoing_swap_state.swap_params.send_amount;
        if send_amount == 0 {
            return Err(TeleportError::Protocol("Coinswap send amount not set!!"));
        }

        Ok(self
            .offerbook
            .get_all_untried()
            .iter()
            .find(|oa| send_amount > oa.offer.min_size && send_amount < oa.offer.max_size)
            .ok_or(TeleportError::Protocol(
                "Could not find suitable maker matching requirements of swap parameters",
            ))?
            .clone())
    }

    /// Request Contract transaction signatures *required by* a "Sender".
    /// "Sender" = The party funding into the coinswap multisig.
    /// Could be both Taker or Maker, depending on the position in the hop.
    ///
    /// For Ex: If the Sender is a Taker, and Receiver is a Maker, this function will return
    /// the Maker's Signatures.
    async fn req_contract_sigs_for_sender<S: SwapCoin>(
        &self,
        maker_address: &MakerAddress,
        outgoing_swapcoins: &[S],
        maker_multisig_nonces: &[SecretKey],
        maker_hashlock_nonces: &[SecretKey],
        locktime: u16,
    ) -> Result<ContractSigsForSender, TeleportError> {
        let mut ii = 0;
        loop {
            ii += 1;
            select! {
                ret = req_contract_sigs_for_sender_once(
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
                            if ii <= self.config.first_connect_attempts {
                                sleep(Duration::from_secs(self.config.first_connect_sleep_delay_sec)).await;
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
                        return Err(TeleportError::Protocol(
                            "Timed out of request_senders_contract_tx_signatures attempt"));
                    }
                },
            }
        }
    }

    async fn req_contract_sigs_for_recvr<S: SwapCoin>(
        &self,
        maker_address: &MakerAddress,
        incoming_swapcoins: &[S],
        receivers_contract_txes: &[Transaction],
    ) -> Result<ContractSigsForRecvr, TeleportError> {
        let mut ii = 0;
        loop {
            ii += 1;
            select! {
                ret = req_contract_sigs_for_recvr_once(
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
                            if ii <= self.config.reconnect_attempts {
                                sleep(Duration::from_secs(
                                    if ii <= self.config.short_long_sleep_delay_transition {
                                        self.config.reconnect_short_slepp_delay
                                    } else {
                                        self.config.reconnect_locg_sleep_delay
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
                        return Err(TeleportError::Protocol(
                            "Timed out of request_receivers_contract_tx_signatures attempt"));
                    }
                },
            }
        }
    }

    /// Return a list of confirmed funding txs with their corresponding merkel proofs.
    /// Returns None, if any of the watching contract transactions has been broadcast, which indicates violation
    /// of the protocol by one of the makers.
    async fn wait_for_funding_tx_confirmation(
        &mut self,
        funding_txids: &Vec<Txid>,
    ) -> Result<Option<(Vec<Transaction>, Vec<String>)>, TeleportError> {
        let mut txid_tx_map = HashMap::<Txid, Transaction>::new();
        let mut txid_blockhash_map = HashMap::<Txid, BlockHash>::new();

        let contracts_to_watch = self
            .ongoing_swap_state
            .watchonly_swapcoins
            .iter()
            .map(|watchonly_swapcoin_list| {
                watchonly_swapcoin_list
                    .iter()
                    .map(|watchonly_swapcoin| watchonly_swapcoin.contract_tx.clone())
                    .collect::<Vec<Transaction>>()
            })
            .chain(once(
                self.ongoing_swap_state
                    .outgoing_swapcoins
                    .iter()
                    .map(|osc| osc.contract_tx.clone())
                    .collect::<Vec<Transaction>>(),
            ))
            .collect::<Vec<Vec<Transaction>>>();

        // Required confirmation target for the funding txs.
        let required_confirmations =
            if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                self.ongoing_swap_state.swap_params.required_confirms
            } else {
                self.ongoing_swap_state
                    .peer_infos
                    .last()
                    .expect("Maker information excpected in swap state")
                    .peer
                    .offer
                    .required_confirms
            };
        log::info!(
            "Waiting for funding transaction confirmations ({} conf required)",
            required_confirmations
        );
        let mut txids_seen_once = HashSet::<Txid>::new();
        loop {
            for txid in funding_txids {
                if txid_tx_map.contains_key(txid) {
                    continue;
                }
                let gettx = match self.rpc.get_transaction(txid, Some(true)) {
                    Ok(r) => r,
                    //if we lose connection to the node, just try again, no point returning an error
                    Err(_e) => continue,
                };
                if !txids_seen_once.contains(txid) {
                    txids_seen_once.insert(*txid);
                    if gettx.info.confirmations == 0 {
                        let mempool_tx = match self.rpc.get_mempool_entry(txid) {
                            Ok(m) => m,
                            Err(_e) => continue,
                        };
                        log::info!(
                            "Seen in mempool: {} [{:.1} sat/vbyte]",
                            txid,
                            mempool_tx.fees.base.as_sat() as f32 / mempool_tx.vsize as f32
                        );
                    }
                }
                //TODO handle confirm<0
                if gettx.info.confirmations >= required_confirmations {
                    txid_tx_map.insert(*txid, deserialize::<Transaction>(&gettx.hex).unwrap());
                    txid_blockhash_map.insert(*txid, gettx.info.blockhash.unwrap());
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
                        self.rpc
                            .get_tx_out_proof(
                                &[txid],
                                Some(&txid_blockhash_map.get(&txid).unwrap()),
                            )
                            .map(|gettxoutproof_result| gettxoutproof_result.to_hex())
                    })
                    .collect::<Result<Vec<String>, bitcoincore_rpc::Error>>()?;
                return Ok(Some((txes, merkleproofs)));
            }
            if !contracts_to_watch.is_empty() {
                let contracts_broadcasted = check_for_broadcasted_contract_txes(
                    self.rpc,
                    &contracts_to_watch
                        .iter()
                        .map(|txes| ContractsInfo {
                            contract_txes: txes
                                .iter()
                                .map(|tx| ContractTransaction {
                                    tx: tx.clone(),
                                    redeemscript: Script::new(),
                                    hashlock_spend_without_preimage: None,
                                    timelock_spend: None,
                                    timelock_spend_broadcasted: false,
                                })
                                .collect::<Vec<ContractTransaction>>(),
                            wallet_label: String::new(), // TODO: Set appropriate wallet label
                        })
                        .collect::<Vec<ContractsInfo>>(),
                    &mut self.ongoing_swap_state.last_synced_height,
                )?;
                if !contracts_broadcasted.is_empty() {
                    log::info!("Contract transactions were broadcasted! Aborting");
                    return Ok(None);
                }
            }
            sleep(Duration::from_millis(1000)).await;
        }
    }

    /// Initiate the first coinswap hop. Stores all the relevant data into OngoingSwapState.
    async fn initiate_coinswap(&mut self) -> Result<(), TeleportError> {
        // Set the Taker Position state
        self.ongoing_swap_state.taker_position = TakerPosition::FirstPeer;

        // Locktime to be used for this swap.
        let swap_locktime = self.config.refund_locktime
            + self.config.refund_locktime_step * self.ongoing_swap_state.swap_params.maker_count;

        // Loop until we find a live maker who responded to our signature request.
        let funding_txs = loop {
            let maker = self.choose_next_maker()?.clone();
            let (multisig_pubkeys, multisig_nonces, hashlock_pubkeys, hashlock_nonces) =
                generate_maker_keys(
                    &maker.offer.tweakable_point,
                    self.ongoing_swap_state.swap_params.tx_count,
                );

            //TODO: Figure out where to use the fee.
            let (funding_txs, mut outgoing_swapcoins, _fee) = self.wallet.initalize_coinswap(
                self.rpc,
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
                .req_contract_sigs_for_sender(
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

            // Maker has returned a valid signature, save all the data in memory,
            // and persist in disk.
            self.offerbook.add_good_maker(&maker);
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
                self.wallet.add_outgoing_swapcoin(outgoing_swapcoin.clone());
            }
            self.wallet.save_to_disk().unwrap();

            self.ongoing_swap_state.outgoing_swapcoins = outgoing_swapcoins;

            break funding_txs;
        };

        // Wait for funding txs to confirm
        log::debug!("My Funding Txids:  {:#?}", funding_txs);
        log::debug!(
            "Outgoing SwapCoins: {:#?}",
            self.ongoing_swap_state.outgoing_swapcoins
        );

        let funding_txids = funding_txs
            .iter()
            .map(|tx| {
                let txid = self.rpc.send_raw_transaction(tx)?;
                log::info!("Broadcasting My Funding Tx: {}", txid);
                assert_eq!(txid, tx.txid());
                Ok(txid)
            })
            .collect::<Result<_, TeleportError>>()?;

        //unwrap the option without checking for Option::None because we passed no contract txes
        //to watch and therefore they cant be broadcast
        let (funding_txs, funding_tx_merkleproofs) = self
            .wait_for_funding_tx_confirmation(&funding_txids)
            .await?
            .unwrap();

        self.ongoing_swap_state
            .funding_txs
            .push((funding_txs, funding_tx_merkleproofs));

        Ok(())
    }

    fn get_preimage(&self) -> &Preimage {
        &self.ongoing_swap_state.active_preimage
    }

    fn get_preimage_hash(&self) -> Hash160 {
        Hash160::hash(self.get_preimage())
    }

    fn clear_ongoing_swaps(&mut self) {
        self.ongoing_swap_state = OngoingSwapState::default();
    }

    // I always know who's the next maker from currentswapcoin
    fn create_fundingtxs_info_for_next_maker(&self) -> Vec<FundingTxInfo> {
        let (this_maker_multisig_redeemscripts, this_maker_contract_redeemscripts) =
            if self.ongoing_swap_state.taker_position == TakerPosition::FirstPeer {
                (
                    self.ongoing_swap_state
                        .outgoing_swapcoins
                        .iter()
                        .map(|s| s.get_multisig_redeemscript())
                        .collect::<Vec<Script>>(),
                    self.ongoing_swap_state
                        .outgoing_swapcoins
                        .iter()
                        .map(|s| s.get_contract_redeemscript())
                        .collect::<Vec<Script>>(),
                )
            } else {
                (
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .iter()
                        .rev()
                        .nth(0)
                        .unwrap()
                        .iter()
                        .map(|s| s.get_multisig_redeemscript())
                        .collect::<Vec<Script>>(),
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .iter()
                        .rev()
                        .nth(0)
                        .unwrap()
                        .iter()
                        .map(|s| s.get_contract_redeemscript())
                        .collect::<Vec<Script>>(),
                )
            };

        let maker_multisig_nonces = self
            .ongoing_swap_state
            .peer_infos
            .iter()
            .rev()
            .nth(0)
            .expect("maker should exist")
            .multisig_nonces
            .iter();
        let maker_hashlock_nonces = self
            .ongoing_swap_state
            .peer_infos
            .iter()
            .rev()
            .nth(0)
            .expect("maker should exist")
            .hashlock_nonces
            .iter();

        let (funding_txs, funding_txs_merkleproof) = self
            .ongoing_swap_state
            .funding_txs
            .iter()
            .rev()
            .nth(0)
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
                        multisig_nonce: maker_multisig_nonce.clone(),
                        contract_redeemscript: this_maker_contract_reedemscript.clone(),
                        hashlock_nonce: maker_hashlock_nonce.clone(),
                    }
                },
            )
            .collect::<Vec<_>>();

        funding_tx_infos
    }

    fn create_incoming_swapcoins(
        &self,
        maker_sign_sender_and_receiver_contracts: &ContractSigsAsRecvrAndSender,
    ) -> Result<Vec<IncomingSwapCoin>, TeleportError> {
        let next_swap_multisig_redeemscripts = maker_sign_sender_and_receiver_contracts
            .senders_contract_txs_info
            .iter()
            .map(|senders_contract_tx_info| senders_contract_tx_info.multisig_redeemscript.clone())
            .collect::<Vec<Script>>();
        let next_swap_funding_outpoints = maker_sign_sender_and_receiver_contracts
            .senders_contract_txs_info
            .iter()
            .map(|senders_contract_tx_info| {
                senders_contract_tx_info.contract_tx.input[0].previous_output
            })
            .collect::<Vec<OutPoint>>();

        let (funding_txs, funding_txs_merkleproofs) = self
            .ongoing_swap_state
            .funding_txs
            .iter()
            .rev()
            .nth(0)
            .expect("funding transactions expected");

        let last_makers_funding_tx_values = funding_txs
            .iter()
            .zip(next_swap_multisig_redeemscripts.iter())
            .map(|(makers_funding_tx, multisig_redeemscript)| {
                find_funding_output(&makers_funding_tx, &multisig_redeemscript)
                    .ok_or(TeleportError::Protocol(
                        "multisig redeemscript not found in funding tx",
                    ))
                    .map(|txout| txout.1.value)
            })
            .collect::<Result<Vec<u64>, TeleportError>>()?;
        let my_receivers_contract_txes = izip!(
            next_swap_funding_outpoints.iter(),
            last_makers_funding_tx_values.iter(),
            self.ongoing_swap_state
                .peer_infos
                .iter()
                .rev()
                .nth(0)
                .expect("expected")
                .contract_reedemscripts
                .iter()
        )
        .map(
            |(&previous_funding_output, &maker_funding_tx_value, next_contract_redeemscript)| {
                crate::contracts::create_receivers_contract_tx(
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
            .iter()
            .rev()
            .nth(0)
            .expect("next swap info expected");
        for (
            multisig_redeemscript,
            &maker_funded_multisig_pubkey,
            &maker_funded_multisig_privkey,
            my_receivers_contract_tx,
            next_contract_redeemscript,
            &hashlock_privkey,
            &maker_funding_tx_value,
            funding_tx,
            funding_tx_merkleproof,
        ) in izip!(
            next_swap_multisig_redeemscripts.iter(),
            next_swap_info.multisig_pubkeys.iter(),
            next_swap_info.multisig_nonces.iter(),
            my_receivers_contract_txes.iter(),
            next_swap_info.contract_reedemscripts.iter(),
            next_swap_info.hashlock_nonces.iter(),
            last_makers_funding_tx_values.iter(),
            funding_txs.iter(),
            funding_txs_merkleproofs.iter(),
        ) {
            let (o_ms_pubkey1, o_ms_pubkey2) =
                crate::contracts::read_pubkeys_from_multisig_redeemscript(multisig_redeemscript)
                    .ok_or(TeleportError::Protocol(
                        "invalid pubkeys in multisig redeemscript",
                    ))?;
            let maker_funded_other_multisig_pubkey = if o_ms_pubkey1 == maker_funded_multisig_pubkey
            {
                o_ms_pubkey2
            } else {
                if o_ms_pubkey2 != maker_funded_multisig_pubkey {
                    return Err(TeleportError::Protocol(
                        "maker-funded multisig doesnt match",
                    ));
                }
                o_ms_pubkey1
            };

            self.wallet.import_wallet_multisig_redeemscript(
                &self.rpc,
                &o_ms_pubkey1,
                &o_ms_pubkey2,
            )?;
            self.wallet.import_tx_with_merkleproof(
                &self.rpc,
                funding_tx,
                funding_tx_merkleproof.clone(),
            )?;
            self.wallet
                .import_wallet_contract_redeemscript(self.rpc, &next_contract_redeemscript)?;

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

    /// Settle all coinswaps by sending hash preimages and privkeys.
    async fn settle_all_coinswaps(&mut self) -> Result<(), TeleportError> {
        let mut outgoing_privkeys: Option<Vec<MultisigPrivkey>> = None;
        let maker_addresses = self.ongoing_swap_state.peer_infos
            [0..self.ongoing_swap_state.peer_infos.len() - 1]
            .iter()
            .map(|si| si.peer.address.clone())
            .collect::<Vec<_>>();

        for (index, maker_address) in maker_addresses.iter().enumerate() {
            let is_taker_previous_peer = index == 0;
            let is_taker_next_peer =
                (index as u16) == self.ongoing_swap_state.swap_params.maker_count - 1;

            let senders_multisig_redeemscripts = if is_taker_previous_peer {
                self.ongoing_swap_state
                    .outgoing_swapcoins
                    .iter()
                    .map(|sc| sc.get_multisig_redeemscript())
                    .collect::<Vec<_>>()
            } else {
                self.ongoing_swap_state
                    .watchonly_swapcoins
                    .iter()
                    .nth(index - 1)
                    .expect("Watchonly coins expected")
                    .iter()
                    .map(|sc| sc.get_multisig_redeemscript())
                    .collect::<Vec<_>>()
            };
            let receivers_multisig_redeemscripts = if is_taker_next_peer {
                self.ongoing_swap_state
                    .incoming_swapcoins
                    .iter()
                    .map(|sc| sc.get_multisig_redeemscript())
                    .collect::<Vec<_>>()
            } else {
                self.ongoing_swap_state
                    .watchonly_swapcoins
                    .iter()
                    .nth(index)
                    .expect("watchonly coins expected")
                    .iter()
                    .map(|sc| sc.get_multisig_redeemscript())
                    .collect::<Vec<_>>()
            };

            let reconnect_time_out = self.config.reconnect_attempt_timeout_sec;

            let mut ii = 0;
            loop {
                ii += 1;
                select! {
                    ret = self.settle_one_coinswap(
                        &maker_address,
                        index,
                        is_taker_previous_peer,
                        is_taker_next_peer,
                        &mut outgoing_privkeys,
                        // &taker.current_swap_info.outgoing_swapcoins,
                        // &mut taker.current_swap_info.watchonly_swapcoins,
                        // &mut taker.current_swap_info.incoming_swapcoins,
                        //taker,
                        &senders_multisig_redeemscripts,
                        &receivers_multisig_redeemscripts,
                        //&taker.current_swap_info.active_preimage,
                    ) => {
                        if let Err(e) = ret {
                            log::warn!(
                                "Failed to connect to maker {} to settle coinswap, \
                                reattempting... error={:?}",
                                maker_address,
                                e
                            );
                            if ii <= self.config.reconnect_attempts {
                                sleep(Duration::from_secs(
                                    if ii <= self.config.short_long_sleep_delay_transition {
                                        self.config.reconnect_locg_sleep_delay
                                    } else {
                                        self.config.reconnect_locg_sleep_delay
                                    },
                                ))
                                .await;
                                continue;
                            } else {
                                return Err(e);
                            }
                        }
                        break;
                    },
                    _ = sleep(Duration::from_secs(reconnect_time_out)) => {
                        log::warn!(
                            "Timeout for settling coinswap with maker {}, reattempting...",
                            maker_address
                        );
                        if ii <= self.config.reconnect_attempts {
                            continue;
                        } else {
                            return Err(TeleportError::Protocol(
                                "Timed out of settle_one_coinswap attempt"));
                        }
                    },
                }
            }
        }
        Ok(())
    }

    // Use active coinswap info.
    async fn settle_one_coinswap<'a>(
        &mut self,
        maker_address: &MakerAddress,
        index: usize,
        is_taker_previous_peer: bool,
        is_taker_next_peer: bool,
        outgoing_privkeys: &mut Option<Vec<MultisigPrivkey>>,
        senders_multisig_redeemscripts: &Vec<Script>,
        receivers_multisig_redeemscripts: &Vec<Script>,
    ) -> Result<(), TeleportError> {
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

        let privkeys_reply = if is_taker_previous_peer {
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
        if is_taker_next_peer {
            check_and_apply_maker_private_keys(
                &mut self.ongoing_swap_state.incoming_swapcoins,
                &maker_private_key_handover.multisig_privkeys,
            )
        } else {
            let ret = check_and_apply_maker_private_keys(
                &mut self
                    .ongoing_swap_state
                    .watchonly_swapcoins
                    .iter_mut()
                    .nth(index)
                    .expect("watchonly coins expected"),
                &maker_private_key_handover.multisig_privkeys,
            );
            *outgoing_privkeys = Some(maker_private_key_handover.multisig_privkeys);
            ret
        }?;
        log::info!("===> Sending PrivateKeyHandover to {}", maker_address);
        send_message(
            &mut socket_writer,
            TakerToMakerMessage::RespPrivKeyHandover(PrivKeyHandover {
                multisig_privkeys: privkeys_reply,
            }),
        )
        .await?;
        Ok(())
    }

    async fn exchange_signatures_and_find_next_maker(
        &mut self,
        maker_refund_locktime: u16,
        funding_tx_infos: &Vec<FundingTxInfo>,
    ) -> Result<(NextPeerInfo, ContractSigsAsRecvrAndSender), TeleportError> {
        let reconnect_timeout_sec = self.config.reconnect_attempt_timeout_sec;
        let mut ii = 0;
        loop {
            ii += 1;
            select! {
                ret = self.exchange_signatures_and_find_next_maker_attempt_once(
                    maker_refund_locktime,
                    funding_tx_infos
                ) => {
                    match ret {
                        Ok(return_value) => return Ok(return_value),
                        Err(e) => {
                            log::warn!(
                                "Failed to exchange signatures with maker {}, \
                                reattempting... error={:?}",
                                &self.ongoing_swap_state.peer_infos.iter().rev().nth(0).expect("at least one active maker expected").peer.address,
                                e
                            );
                            if ii <= self.config.reconnect_attempts {
                                sleep(Duration::from_secs(
                                    if ii <= self.config.short_long_sleep_delay_transition {
                                        self.config.reconnect_short_slepp_delay
                                    } else {
                                        self.config.reconnect_locg_sleep_delay
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
                _ = sleep(Duration::from_secs(reconnect_timeout_sec)) => {
                    log::warn!(
                        "Timeout for exchange signatures with maker {}, reattempting...",
                        &self.ongoing_swap_state.peer_infos.iter().rev().nth(0).expect("at least one active maker expected").peer.address
                    );
                    if ii <= RECONNECT_ATTEMPTS {
                        continue;
                    } else {
                        return Err(TeleportError::Protocol(
                            "Timed out of exchange_signatures_and_find_next_maker attempt"));
                    }
                },
            }
        }
    }

    async fn exchange_signatures_and_find_next_maker_attempt_once(
        &mut self,
        maker_refund_locktime: u16,
        funding_tx_infos: &Vec<FundingTxInfo>,
    ) -> Result<(NextPeerInfo, ContractSigsAsRecvrAndSender), TeleportError> {
        let this_maker = &self
            .ongoing_swap_state
            .peer_infos
            .iter()
            .rev()
            .nth(0)
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
            maker_sign_sender_and_receiver_contracts,
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
                        .iter()
                        .rev()
                        .nth(0)
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
                    &this_maker,
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
                "<=== Recieved SignSendersAndReceiversContractTxes from {}",
                this_maker.address
            );

            // If This Maker is the Sender, and we (the Taker) are the Receiver (Last Hop). We provide the Sender's Contact Tx Sigs.
            let senders_sigs = if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer
            {
                log::info!("Taker is next peer. Signing Sender's Contract Txs",);
                sign_senders_contract_txs(
                    &next_peer_multisig_keys_or_nonces,
                    &contract_sigs_as_recvr_sender,
                )?
            } else {
                // If Next Maker is the Receiver, and This Maker is The Sender, Request Sender's Contract Tx Sig to Next Maker.
                let next_swapcoins = create_watch_only_swapcoins(
                    self.rpc,
                    &contract_sigs_as_recvr_sender,
                    &next_peer_multisig_pubkeys,
                    &next_swap_contract_redeemscripts,
                )?;
                let sigs = match self
                    .req_contract_sigs_for_sender(
                        &next_maker.address,
                        &next_swapcoins,
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
                        log::debug!(
                            "Fail to obtain sender's contract tx signature from next_maker {}: {:?}",
                            next_maker.address,
                            e
                        );
                        continue; //go back to the start of the loop and try another maker
                    }
                };
                self.ongoing_swap_state
                    .watchonly_swapcoins
                    .push(next_swapcoins);
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
            sign_receivers_contract_txs(
                &maker_sign_sender_and_receiver_contracts.receivers_contract_txs,
                &self.ongoing_swap_state.outgoing_swapcoins,
            )?
        } else {
            // If Next Maker is the Receiver, and Previous Maker is the Sender, request Previous Maker to sign the Reciever's Contract Tx.
            assert!(previous_maker.is_some());
            let previous_maker_addr = &previous_maker.unwrap().peer.address;
            log::info!(
                "===> Sending SignReceiversContractTx, previous maker is {}",
                previous_maker_addr,
            );
            let previous_maker_watchonly_swapcoins =
                if self.ongoing_swap_state.taker_position == TakerPosition::LastPeer {
                    self.ongoing_swap_state
                        .watchonly_swapcoins
                        .iter()
                        .rev()
                        .nth(0)
                        .unwrap()
                } else {
                    //if the next peer is a maker not a taker, then that maker's swapcoins are last
                    &self.ongoing_swap_state.watchonly_swapcoins
                        [self.ongoing_swap_state.watchonly_swapcoins.len() - 2]
                };
            self.req_contract_sigs_for_recvr(
                &previous_maker_addr,
                previous_maker_watchonly_swapcoins,
                &maker_sign_sender_and_receiver_contracts.receivers_contract_txs,
            )
            .await?
            .sigs
        };
        log::info!(
            "===> Sending ContractSigsAsReceiverAndSender to {}",
            this_maker.address
        );
        send_message(
            &mut socket_writer,
            TakerToMakerMessage::RespContractSigsForRecvrAndSender(ContractSigsForRecvrAndSender {
                receivers_sigs,
                senders_sigs,
            }),
        )
        .await?;
        let next_swap_info = NextPeerInfo {
            peer: next_maker.clone(),
            multisig_pubkeys: next_peer_multisig_pubkeys,
            multisig_nonces: next_peer_multisig_keys_or_nonces,
            hashlock_nonces: next_peer_hashlock_keys_or_nonces,
            contract_reedemscripts: next_swap_contract_redeemscripts,
        };
        Ok((next_swap_info, maker_sign_sender_and_receiver_contracts))
    }

    async fn request_signature_for_last_hop(&mut self) -> Result<(), TeleportError> {
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
        let receiver_contract_sig = self
            .req_contract_sigs_for_recvr(
                &last_maker.address,
                &self.ongoing_swap_state.incoming_swapcoins,
                &self
                    .ongoing_swap_state
                    .incoming_swapcoins
                    .iter()
                    .map(|swapcoin| swapcoin.contract_tx.clone())
                    .collect::<Vec<Transaction>>(),
            )
            .await?;
        for (incoming_swapcoin, &receiver_contract_sig) in self
            .ongoing_swap_state
            .incoming_swapcoins
            .iter_mut()
            .zip(receiver_contract_sig.sigs.iter())
        {
            incoming_swapcoin.others_contract_sig = Some(receiver_contract_sig);
        }
        for incoming_swapcoin in &self.ongoing_swap_state.incoming_swapcoins {
            self.wallet.add_incoming_swapcoin(incoming_swapcoin.clone());
        }

        self.wallet.save_to_disk().unwrap();

        Ok(())
    }

    fn finish_and_save_swap_round(&mut self) {
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

        //TODO: update incoming_swapcoins with privkey on disk here
        for incoming_swapcoin in &self.ongoing_swap_state.incoming_swapcoins {
            self.wallet
                .find_incoming_swapcoin_mut(&incoming_swapcoin.get_multisig_redeemscript())
                .unwrap()
                .other_privkey = incoming_swapcoin.other_privkey;
        }
        self.wallet.save_to_disk().unwrap();

        self.clear_ongoing_swaps();
    }
}

#[tokio::main]
pub async fn start_taker(rpc: &Client, wallet: &mut Wallet, config: SwapParams) {
    match run(rpc, wallet, config).await {
        Ok(_o) => (),
        Err(e) => log::error!("err {:?}", e),
    };
}

async fn run(
    rpc: &Client,
    wallet: &mut Wallet,
    swap_params: SwapParams,
) -> Result<(), TeleportError> {
    let offers_addresses = sync_offerbook(wallet.network)
        .await
        .expect("unable to sync maker addresses from directory servers");
    log::info!("<=== Got Offers ({} offers)", offers_addresses.len());
    log::debug!("Offers : {:#?}", offers_addresses);
    let mut taker = Taker::init(wallet, rpc, offers_addresses);
    taker.send_coinswap(swap_params).await?;
    Ok(())
}

/// Performs a handshake with a Maker and returns and Reader and Writer halves.
pub async fn handshake_maker<'a>(
    socket: &'a mut TcpStream,
    maker_address: &MakerAddress,
) -> Result<(BufReader<ReadHalf<'a>>, WriteHalf<'a>), TeleportError> {
    let socket = match maker_address {
        MakerAddress::Clearnet { address: _ } => socket,
        MakerAddress::Tor { address } => Socks5Stream::connect_with_socket(socket, address.clone())
            .await?
            .into_inner(),
    };
    let (reader, mut socket_writer) = socket.split();
    let mut socket_reader = BufReader::new(reader);
    send_message(
        &mut socket_writer,
        TakerToMakerMessage::TakerHello(TakerHello {
            protocol_version_min: 0,
            protocol_version_max: 0,
        }),
    )
    .await?;
    let makerhello =
        if let MakerToTakerMessage::MakerHello(m) = read_message(&mut socket_reader).await? {
            m
        } else {
            return Err(TeleportError::Protocol("expected method makerhello"));
        };
    log::debug!("{:#?}", makerhello);
    Ok((socket_reader, socket_writer))
}

/// Request Contract transaction signatures *required by* a "Receiver".
/// "Receiver" = The party receiving the fund in coinswap multisig.
/// Could be both Taker or Maker, depending on the position in the hop.
///
/// For Ex: If the Sender is a Taker, and Receiver is a Maker, this function will return
/// the Taker's Signatures.
async fn req_contract_sigs_for_sender_once<S: SwapCoin>(
    maker_address: &MakerAddress,
    outgoing_swapcoins: &[S],
    maker_multisig_nonces: &[SecretKey],
    maker_hashlock_nonces: &[SecretKey],
    locktime: u16,
) -> Result<ContractSigsForSender, TeleportError> {
    log::info!("Connecting to {}", maker_address);
    let mut socket = TcpStream::connect(maker_address.get_tcpstream_address()).await?;
    let (mut socket_reader, mut socket_writer) =
        handshake_maker(&mut socket, maker_address).await?;
    log::info!("===> Sending SignSendersContractTx to {}", maker_address);
    send_message(
        &mut socket_writer,
        TakerToMakerMessage::ReqContractSigsForSender(ReqContractSigsForSender {
            txs_info: izip!(
                maker_multisig_nonces.iter(),
                maker_hashlock_nonces.iter(),
                outgoing_swapcoins.iter()
            )
            .map(
                |(&multisig_key_nonce, &hashlock_key_nonce, outgoing_swapcoin)| {
                    ContractTxInfoForSender {
                        multisig_key_nonce,
                        hashlock_key_nonce,
                        timelock_pubkey: outgoing_swapcoin.get_timelock_pubkey(),
                        senders_contract_tx: outgoing_swapcoin.get_contract_tx(),
                        multisig_redeemscript: outgoing_swapcoin.get_multisig_redeemscript(),
                        funding_input_value: outgoing_swapcoin.get_funding_amount(),
                    }
                },
            )
            .collect::<Vec<ContractTxInfoForSender>>(),
            hashvalue: outgoing_swapcoins[0].get_hashvalue(),
            locktime,
        }),
    )
    .await?;
    let maker_senders_contract_sig = if let MakerToTakerMessage::RespContractSigsForSender(m) =
        read_message(&mut socket_reader).await?
    {
        m
    } else {
        return Err(TeleportError::Protocol(
            "expected method senderscontractsig",
        ));
    };
    if maker_senders_contract_sig.sigs.len() != outgoing_swapcoins.len() {
        return Err(TeleportError::Protocol(
            "wrong number of signatures from maker",
        ));
    }
    if maker_senders_contract_sig
        .sigs
        .iter()
        .zip(outgoing_swapcoins.iter())
        .any(|(sig, outgoing_swapcoin)| !outgoing_swapcoin.verify_contract_tx_sender_sig(&sig))
    {
        return Err(TeleportError::Protocol("invalid signature from maker"));
    }
    log::info!("<=== Received SendersContractSig from {}", maker_address);
    Ok(maker_senders_contract_sig)
}

async fn req_contract_sigs_for_recvr_once<S: SwapCoin>(
    maker_address: &MakerAddress,
    incoming_swapcoins: &[S],
    receivers_contract_txes: &[Transaction],
) -> Result<ContractSigsForRecvr, TeleportError> {
    log::info!("Connecting to {}", maker_address);
    let mut socket = TcpStream::connect(maker_address.get_tcpstream_address()).await?;
    let (mut socket_reader, mut socket_writer) =
        handshake_maker(&mut socket, maker_address).await?;
    send_message(
        &mut socket_writer,
        TakerToMakerMessage::ReqContractSigsForRecvr(ReqContractSigsForRecvr {
            txs: incoming_swapcoins
                .iter()
                .zip(receivers_contract_txes.iter())
                .map(|(swapcoin, receivers_contract_tx)| ContractTxInfoForRecvr {
                    multisig_redeemscript: swapcoin.get_multisig_redeemscript(),
                    contract_tx: receivers_contract_tx.clone(),
                })
                .collect::<Vec<ContractTxInfoForRecvr>>(),
        }),
    )
    .await?;
    let maker_receiver_contract_sig = if let MakerToTakerMessage::RespContractSigsForRecvr(m) =
        read_message(&mut socket_reader).await?
    {
        m
    } else {
        return Err(TeleportError::Protocol(
            "expected method receiverscontractsig",
        ));
    };
    if maker_receiver_contract_sig.sigs.len() != incoming_swapcoins.len() {
        return Err(TeleportError::Protocol(
            "wrong number of signatures from maker",
        ));
    }
    if maker_receiver_contract_sig
        .sigs
        .iter()
        .zip(incoming_swapcoins.iter())
        .any(|(sig, swapcoin)| !swapcoin.verify_contract_tx_receiver_sig(&sig))
    {
        return Err(TeleportError::Protocol("invalid signature from maker"));
    }

    log::info!("<=== Received ReceiversContractSig from {}", maker_address);
    Ok(maker_receiver_contract_sig)
}

// TODO: Simplify this function. Use dedicated structs for related items.
/// Send proof of funding to a Maker and initiate next Coinswap hop with this Maker and the Next Maker.
async fn send_proof_of_funding_and_init_next_hop(
    socket_reader: &mut BufReader<ReadHalf<'_>>,
    socket_writer: &mut WriteHalf<'_>,
    this_maker: &OfferAndAddress,
    funding_tx_infos: &Vec<FundingTxInfo>,
    next_peer_multisig_pubkeys: &Vec<PublicKey>,
    next_peer_hashlock_pubkeys: &Vec<PublicKey>,
    next_maker_refund_locktime: u16,
    next_maker_fee_rate: u64,
    this_maker_contract_txes: &Vec<Transaction>,
    hashvalue: Hash160,
) -> Result<(ContractSigsAsRecvrAndSender, Vec<Script>), TeleportError> {
    send_message(
        socket_writer,
        TakerToMakerMessage::RespProofOfFunding(ProofOfFunding {
            confirmed_funding_txes: funding_tx_infos.clone(),
            next_coinswap_info: next_peer_multisig_pubkeys
                .iter()
                .zip(next_peer_hashlock_pubkeys.iter())
                .map(
                    |(&next_coinswap_multisig_pubkey, &next_hashlock_pubkey)| NextHopInfo {
                        next_multisig_pubkey: next_coinswap_multisig_pubkey,
                        next_hashlock_pubkey,
                    },
                )
                .collect::<Vec<NextHopInfo>>(),
            next_locktime: next_maker_refund_locktime,
            next_fee_rate: next_maker_fee_rate,
        }),
    )
    .await?;
    let maker_sign_sender_and_receiver_contracts =
        if let MakerToTakerMessage::ReqContractSigsAsRecvrAndSender(m) =
            read_message(socket_reader).await?
        {
            m
        } else {
            return Err(TeleportError::Protocol(
                "expected method signsendersandreceiverscontracttxes",
            ));
        };
    if maker_sign_sender_and_receiver_contracts
        .receivers_contract_txs
        .len()
        != funding_tx_infos.len()
    {
        return Err(TeleportError::Protocol(
            "wrong number of receivers contracts tx from maker",
        ));
    }
    if maker_sign_sender_and_receiver_contracts
        .senders_contract_txs_info
        .len()
        != next_peer_multisig_pubkeys.len()
    {
        return Err(TeleportError::Protocol(
            "wrong number of senders contract txes from maker",
        ));
    }

    let funding_tx_values = funding_tx_infos
        .iter()
        .map(|funding_info| {
            find_funding_output(
                &funding_info.funding_tx,
                &funding_info.multisig_redeemscript,
            )
            .ok_or(TeleportError::Protocol(
                "multisig redeemscript not found in funding tx",
            ))
            .map(|txout| txout.1.value)
        })
        .collect::<Result<Vec<u64>, TeleportError>>()?;

    let this_amount = funding_tx_values.iter().sum::<u64>();

    let next_amount = maker_sign_sender_and_receiver_contracts
        .senders_contract_txs_info
        .iter()
        .map(|i| i.funding_amount)
        .sum::<u64>();
    let coinswap_fees = calculate_coinswap_fee(
        this_maker.offer.absolute_fee_sat,
        this_maker.offer.amount_relative_fee_ppb,
        this_maker.offer.time_relative_fee_ppb,
        this_amount,
        1, //time_in_blocks just 1 for now
    );
    let miner_fees_paid_by_taker = MAKER_FUNDING_TX_VBYTE_SIZE
        * next_maker_fee_rate
        * (next_peer_multisig_pubkeys.len() as u64)
        / 1000;
    let calculated_next_amount = this_amount - coinswap_fees - miner_fees_paid_by_taker;
    if calculated_next_amount != next_amount {
        return Err(TeleportError::Protocol("next_amount incorrect"));
    }
    log::info!(
        "this_amount={} coinswap_fees={} miner_fees_paid_by_taker={} next_amount={}",
        this_amount,
        coinswap_fees,
        miner_fees_paid_by_taker,
        next_amount
    );

    for ((receivers_contract_tx, contract_tx), contract_redeemscript) in
        maker_sign_sender_and_receiver_contracts
            .receivers_contract_txs
            .iter()
            .zip(this_maker_contract_txes.iter())
            .zip(funding_tx_infos.iter().map(|fi| &fi.contract_redeemscript))
    {
        validate_contract_tx(
            &receivers_contract_tx,
            Some(&contract_tx.input[0].previous_output),
            contract_redeemscript,
        )?;
    }
    let next_swap_contract_redeemscripts = next_peer_hashlock_pubkeys
        .iter()
        .zip(
            maker_sign_sender_and_receiver_contracts
                .senders_contract_txs_info
                .iter(),
        )
        .map(|(hashlock_pubkey, senders_contract_tx_info)| {
            create_contract_redeemscript(
                hashlock_pubkey,
                &senders_contract_tx_info.timelock_pubkey,
                hashvalue,
                next_maker_refund_locktime,
            )
        })
        .collect::<Vec<Script>>();
    Ok((
        maker_sign_sender_and_receiver_contracts,
        next_swap_contract_redeemscripts,
    ))
}

/// The final step of Coinswap. When all the signatures are passed around, perform the private key handover.
async fn send_hash_preimage_and_get_private_keys(
    socket_reader: &mut BufReader<ReadHalf<'_>>,
    socket_writer: &mut WriteHalf<'_>,
    senders_multisig_redeemscripts: &Vec<Script>,
    receivers_multisig_redeemscripts: &Vec<Script>,
    preimage: &Preimage,
) -> Result<PrivKeyHandover, TeleportError> {
    let receivers_multisig_redeemscripts_len = receivers_multisig_redeemscripts.len();
    send_message(
        socket_writer,
        TakerToMakerMessage::RespHashPreimage(HashPreimage {
            senders_multisig_redeemscripts: senders_multisig_redeemscripts.to_vec(),
            receivers_multisig_redeemscripts: receivers_multisig_redeemscripts.to_vec(),
            preimage: *preimage,
        }),
    )
    .await?;
    let maker_private_key_handover =
        if let MakerToTakerMessage::RespPrivKeyHandover(m) = read_message(socket_reader).await? {
            m
        } else {
            return Err(TeleportError::Protocol(
                "expected method privatekeyhandover",
            ));
        };
    if maker_private_key_handover.multisig_privkeys.len() != receivers_multisig_redeemscripts_len {
        return Err(TeleportError::Protocol(
            "wrong number of private keys from maker",
        ));
    }
    Ok(maker_private_key_handover)
}
