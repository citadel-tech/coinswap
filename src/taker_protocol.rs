use std::{
    collections::{HashMap, HashSet},
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
    BlockHash, Script, Transaction, Txid,
};
use bitcoincore_rpc::{Client, RpcApi};

use itertools::izip;

use crate::{
    contracts::{
        calculate_coinswap_fee, create_contract_redeemscript, find_funding_output,
        validate_contract_tx, SwapCoin, WatchOnlySwapCoin, MAKER_FUNDING_TX_VBYTE_SIZE,
    },
    error::Error,
    messages::{
        ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForRecvrAndSender,
        ContractSigsForSender, ContractTxInfoForRecvr, ContractTxInfoForSender, FundingTxInfo,
        HashPreimage, MakerToTakerMessage, MultisigPrivkey, NextHopInfo, Preimage, PrivKeyHandover,
        ProofOfFunding, ReqContractSigsForRecvr, ReqContractSigsForSender, TakerHello,
        TakerToMakerMessage,
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

// TODO: Put them into a config file.
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

/// Parameters to control the Swap.
/// Satisfying a coinswap depends heavily upon choosing the correct SwapParams.
/// If SwapParams are unstisfiable, the coinswap round will fail.
#[derive(Debug, Clone, Copy)]
pub struct SwapParams {
    // Total Amount to Swap.
    pub send_amount: u64,
    // How many hops.
    pub maker_count: u16,
    // How many splits
    pub tx_count: u32,
    // Confirmation count required for funding txs. TODO: This should be in TakerConfig
    pub required_confirms: i32,
    // Fee rate for funding txs. TODO: This should be in TakerConfig.
    pub fee_rate: u64,
}

#[tokio::main]
pub async fn start_taker(rpc: &Client, wallet: &mut Wallet, config: SwapParams) {
    match run(rpc, wallet, config).await {
        Ok(_o) => (),
        Err(e) => log::error!("err {:?}", e),
    };
}

async fn run(rpc: &Client, wallet: &mut Wallet, config: SwapParams) -> Result<(), Error> {
    let offers_addresses = sync_offerbook(wallet.network)
        .await
        .expect("unable to sync maker addresses from directory servers");
    log::info!("<=== Got Offers ({} offers)", offers_addresses.len());
    log::debug!("Offers : {:#?}", offers_addresses);
    send_coinswap(rpc, wallet, config, &offers_addresses).await?;
    Ok(())
}

async fn send_coinswap(
    rpc: &Client,
    wallet: &mut Wallet,
    config: SwapParams,
    all_maker_offers_addresses: &Vec<OfferAndAddress>,
) -> Result<(), Error> {
    let mut preimage = [0u8; 32];
    let mut rng = OsRng::new().unwrap();
    rng.fill_bytes(&mut preimage);
    let hashvalue = Hash160::hash(&preimage);

    // TODO: REFUND_LOCKTIME_STEP should be a Maker's policy, to defend against possible DOS by very high LockTime value
    // by malicious Takers. Different Makers could have different LockTime requirements. Taker needs to know this value at the OfferBook layer.
    // This will require a moderate redesign of the current protocol to implement. Maker's need to be predetermined before starting the
    // swap, for this to work. Currently they are found on the fly during the swapping process.
    let first_swap_locktime = REFUND_LOCKTIME + REFUND_LOCKTIME_STEP * config.maker_count;

    let mut maker_offers_addresses = all_maker_offers_addresses
        .iter()
        .collect::<Vec<&OfferAndAddress>>();

    let (
        first_maker,
        mut maker_multisig_nonce,
        mut maker_hashlock_nonce,
        my_funding_txes,
        mut outgoing_swapcoins,
        contract_sigs_for_sender,
    ) = loop {
        // This loop attempts to create the first coinswap between a taker and a maker.
        // The loop iterates over all the makers, and returns an [`OutgoingSwapCoins`] for the first suitable maker.
        // Return Error if no suitable makers are found.
        let first_maker = choose_next_maker(&mut maker_offers_addresses, config.send_amount)
            .expect("not enough offers");
        let (
            maker_multisig_pubkeys,
            maker_multisig_nonce,
            maker_hashlock_pubkeys,
            maker_hashlock_nonce,
        ) = generate_maker_keys(&first_maker.offer.tweakable_point, config.tx_count);
        let (my_funding_txes, outgoing_swapcoins, _my_total_miner_fee) = wallet
            .initalize_coinswap(
                rpc,
                config.send_amount,
                &maker_multisig_pubkeys,
                &maker_hashlock_pubkeys,
                hashvalue,
                first_swap_locktime,
                config.fee_rate,
            )
            .unwrap();
        let contract_sigs_for_sender = match req_contract_sigs_for_sender(
            &first_maker.address,
            &outgoing_swapcoins,
            &maker_multisig_nonce,
            &maker_hashlock_nonce,
            first_swap_locktime,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                log::debug!(
                    "Failed to obtain senders contract tx signature from first_maker {}: {:?}",
                    first_maker.address,
                    e
                );
                continue; //go back to the start and try another maker
            }
        };
        break (
            first_maker,
            maker_multisig_nonce,
            maker_hashlock_nonce,
            my_funding_txes,
            outgoing_swapcoins,
            contract_sigs_for_sender,
        );
    };

    // TODO: Contract sigs can be inserted into OutGoingSwapcoins and update the wallet, in the loop above.
    contract_sigs_for_sender
        .sigs
        .iter()
        .zip(outgoing_swapcoins.iter_mut())
        .for_each(|(sig, outgoing_swapcoin)| outgoing_swapcoin.others_contract_sig = Some(*sig));
    for outgoing_swapcoin in &outgoing_swapcoins {
        wallet.add_outgoing_swapcoin(outgoing_swapcoin.clone());
    }
    wallet.update_swapcoins_list().unwrap();

    log::debug!("My Funding Tx:  {:#?}", my_funding_txes);
    log::debug!("Outgoing SwapCoins: {:#?}", outgoing_swapcoins);
    for my_funding_tx in my_funding_txes.iter() {
        let txid = rpc.send_raw_transaction(my_funding_tx)?;
        log::info!("Broadcasting My Funding Tx: {}", txid);
        assert_eq!(txid, my_funding_tx.txid());
    }
    let (mut funding_txes, mut funding_tx_merkleproofs) = wait_for_funding_tx_confirmation(
        rpc,
        &my_funding_txes
            .iter()
            .map(|tx| tx.txid())
            .collect::<Vec<Txid>>(),
        first_maker.offer.required_confirms,
        &[],
        &mut None,
    )
    .await?
    .unwrap();
    //unwrap the option without checking for Option::None because we passed no contract txes
    //to watch and therefore they cant be broadcast

    let mut active_maker_addresses = Vec::<&MakerAddress>::new();
    let mut next_maker = first_maker;
    let mut previous_maker: Option<&OfferAndAddress> = None;

    let mut watchonly_swapcoins = Vec::<Vec<WatchOnlySwapCoin>>::new();
    let mut incoming_swapcoins = Vec::<IncomingSwapCoin>::new();

    let mut last_checked_block_height: Option<u64> = None;

    // This loop is performed for all the makers to make intermediate coinswap hops.
    for maker_index in 0..config.maker_count {
        let is_taker_next_peer = maker_index == config.maker_count - 1;
        let is_taker_previous_peer = maker_index == 0;

        let maker_refund_locktime =
            REFUND_LOCKTIME + REFUND_LOCKTIME_STEP * (config.maker_count - maker_index - 1);
        let (
            this_maker_multisig_redeemscripts,
            this_maker_contract_redeemscripts,
            this_maker_contract_txes,
        ) = if is_taker_previous_peer {
            (
                outgoing_swapcoins
                    .iter()
                    .map(|s| s.get_multisig_redeemscript())
                    .collect::<Vec<Script>>(),
                outgoing_swapcoins
                    .iter()
                    .map(|s| s.get_contract_redeemscript())
                    .collect::<Vec<Script>>(),
                outgoing_swapcoins
                    .iter()
                    .map(|s| s.get_contract_tx())
                    .collect::<Vec<Transaction>>(),
            )
        } else {
            (
                watchonly_swapcoins
                    .last()
                    .unwrap()
                    .iter()
                    .map(|s| s.get_multisig_redeemscript())
                    .collect::<Vec<Script>>(),
                watchonly_swapcoins.last().unwrap()
                    .iter()
                    .map(|s| s.get_contract_redeemscript())
                    .collect::<Vec<Script>>(),
                watchonly_swapcoins.last().unwrap()
                    .iter()
                    .map(|s| s.get_contract_tx())
                    .collect::<Vec<Transaction>>(),
            )
        };

        let this_maker = next_maker;
        //TODO: Create dedicated struct for `next_peer_info`.
        let (
            next_peer_multisig_pubkeys,
            next_peer_multisig_nonces,
            next_peer_hashlock_nonces,
            req_contract_sigs_as_sender_and_recvr,
            next_swap_contract_redeemscripts,
            found_next_maker,
        ) = exchange_signatures_and_find_next_maker(
            rpc,
            &config,
            &mut maker_offers_addresses,
            &this_maker,
            previous_maker,
            is_taker_previous_peer,
            is_taker_next_peer,
            &funding_txes,
            &funding_tx_merkleproofs,
            &this_maker_multisig_redeemscripts,
            &maker_multisig_nonce,
            &this_maker_contract_redeemscripts,
            &maker_hashlock_nonce,
            &this_maker_contract_txes,
            maker_refund_locktime,
            hashvalue,
            &outgoing_swapcoins,
            &mut watchonly_swapcoins,
        )
        .await?;
        next_maker = found_next_maker;
        active_maker_addresses.push(&this_maker.address);

        // TODO: Simplify this function call.
        let wait_for_confirm_result = wait_for_funding_tx_confirmation(
            rpc,
            &req_contract_sigs_as_sender_and_recvr
                .senders_contract_txs_info
                .iter()
                .map(|senders_contract_tx_info| {
                    senders_contract_tx_info.contract_tx.input[0]
                        .previous_output
                        .txid
                })
                .collect::<Vec<Txid>>(),
            if is_taker_next_peer {
                config.required_confirms
            } else {
                next_maker.offer.required_confirms
            },
            &watchonly_swapcoins
                .iter()
                .map(|watchonly_swapcoin_list| {
                    watchonly_swapcoin_list
                        .iter()
                        .map(|watchonly_swapcoin| watchonly_swapcoin.contract_tx.clone())
                        .collect::<Vec<Transaction>>()
                })
                .chain(once(
                    outgoing_swapcoins
                        .iter()
                        .map(|osc| osc.contract_tx.clone())
                        .collect::<Vec<Transaction>>(),
                ))
                .collect::<Vec<Vec<Transaction>>>(),
            &mut last_checked_block_height,
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
        let (next_funding_txes, next_funding_tx_merkleproofs) = wait_for_confirm_result.unwrap();
        funding_txes = next_funding_txes;
        funding_tx_merkleproofs = next_funding_tx_merkleproofs;

        //TODO: Because this will only run once for the last hop, take this out of the loop and
        // put it in the next section. This will be more intuitive to read and reduce size of the
        // the intermediate hop code.
        if is_taker_next_peer {
            incoming_swapcoins = create_incoming_swapcoins(
                rpc,
                &wallet,
                &req_contract_sigs_as_sender_and_recvr,
                &funding_txes,
                &funding_tx_merkleproofs,
                &next_swap_contract_redeemscripts,
                &next_peer_hashlock_nonces,
                &next_peer_multisig_pubkeys,
                &next_peer_multisig_nonces,
                &preimage,
            )
            .unwrap();
            //TODO reason about why this unwrap is here without any error handling
            //do we expect this to never error? are the conditions checked earlier?
        }
        maker_multisig_nonce = next_peer_multisig_nonces;
        maker_hashlock_nonce = next_peer_hashlock_nonces;
        previous_maker = Some(this_maker);
    }

    // Intermediate hops completed. Perform the last receiving hop.
    let last_maker = previous_maker.unwrap();
    log::info!(
        "===> Sending ReqContractSigsForRecvr to {}",
        last_maker.address
    );
    let receiver_contract_sig = req_contract_sigs_for_recvr(
        &last_maker.address,
        &incoming_swapcoins,
        &incoming_swapcoins
            .iter()
            .map(|swapcoin| swapcoin.contract_tx.clone())
            .collect::<Vec<Transaction>>(),
    )
    .await?;
    for (incoming_swapcoin, &receiver_contract_sig) in incoming_swapcoins
        .iter_mut()
        .zip(receiver_contract_sig.sigs.iter())
    {
        incoming_swapcoin.others_contract_sig = Some(receiver_contract_sig);
    }
    for incoming_swapcoin in &incoming_swapcoins {
        wallet.add_incoming_swapcoin(incoming_swapcoin.clone());
    }
    wallet.update_swapcoins_list().unwrap();

    settle_all_coinswaps(
        &config,
        &preimage,
        &active_maker_addresses,
        &outgoing_swapcoins,
        &mut watchonly_swapcoins,
        &mut incoming_swapcoins,
    )
    .await?;

    for (index, watchonly_swapcoin) in watchonly_swapcoins.iter().enumerate() {
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
        incoming_swapcoins
            .iter()
            .map(|w| w.contract_tx.input[0].previous_output.txid)
            .collect::<Vec<_>>()
    );

    //TODO: update incoming_swapcoins with privkey on disk here
    for incoming_swapcoin in &incoming_swapcoins {
        wallet
            .find_incoming_swapcoin_mut(&incoming_swapcoin.get_multisig_redeemscript())
            .unwrap()
            .other_privkey = incoming_swapcoin.other_privkey;
    }
    wallet.update_swapcoins_list().unwrap();

    log::info!("Successfully Completed Coinswap");
    Ok(())
}

/// Performs a handshake with a Maker and returns and Reader and Writer halves.
pub async fn handshake_maker<'a>(
    socket: &'a mut TcpStream,
    maker_address: &MakerAddress,
) -> Result<(BufReader<ReadHalf<'a>>, WriteHalf<'a>), Error> {
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
            return Err(Error::Protocol("expected method makerhello"));
        };
    log::debug!("{:#?}", makerhello);
    Ok((socket_reader, socket_writer))
}

/// Request Contract transaction signatures *required by* a "Sender".
/// "Sender" = The party funding into the coinswap multisig.
/// Could be both Taker or Maker, depending on the position in the hop.
///
/// For Ex: If the Sender is a Taker, and Receiver is a Maker, this function will return
/// the Maker's Signatures.
async fn req_contract_sigs_for_sender<S: SwapCoin>(
    maker_address: &MakerAddress,
    outgoing_swapcoins: &[S],
    maker_multisig_nonces: &[SecretKey],
    maker_hashlock_nonces: &[SecretKey],
    locktime: u16,
) -> Result<ContractSigsForSender, Error> {
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
                        if ii <= FIRST_CONNECT_ATTEMPTS {
                            sleep(Duration::from_secs(FIRST_CONNECT_SLEEP_DELAY_SEC)).await;
                            continue;
                        } else {
                            return Err(e);
                        }
                    }
                }
            },
            _ = sleep(Duration::from_secs(FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC)) => {
                log::warn!(
                    "Timeout for request senders contract tx sig from maker {}, reattempting...",
                    maker_address
                );
                if ii <= FIRST_CONNECT_ATTEMPTS {
                    continue;
                } else {
                    return Err(Error::Protocol(
                        "Timed out of request_senders_contract_tx_signatures attempt"));
                }
            },
        }
    }
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
) -> Result<ContractSigsForSender, Error> {
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
        return Err(Error::Protocol("expected method senderscontractsig"));
    };
    if maker_senders_contract_sig.sigs.len() != outgoing_swapcoins.len() {
        return Err(Error::Protocol("wrong number of signatures from maker"));
    }
    if maker_senders_contract_sig
        .sigs
        .iter()
        .zip(outgoing_swapcoins.iter())
        .any(|(sig, outgoing_swapcoin)| !outgoing_swapcoin.verify_contract_tx_sender_sig(&sig))
    {
        return Err(Error::Protocol("invalid signature from maker"));
    }
    log::info!("<=== Received SendersContractSig from {}", maker_address);
    Ok(maker_senders_contract_sig)
}

async fn req_contract_sigs_for_recvr<S: SwapCoin>(
    maker_address: &MakerAddress,
    incoming_swapcoins: &[S],
    receivers_contract_txes: &[Transaction],
) -> Result<ContractSigsForRecvr, Error> {
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
                        if ii <= RECONNECT_ATTEMPTS {
                            sleep(Duration::from_secs(
                                if ii <= SHORT_LONG_SLEEP_DELAY_TRANSITION {
                                    RECONNECT_SHORT_SLEEP_DELAY_SEC
                                } else {
                                    RECONNECT_LONG_SLEEP_DELAY_SEC
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
            _ = sleep(Duration::from_secs(RECONNECT_ATTEMPT_TIMEOUT_SEC)) => {
                log::warn!(
                    "Timeout for request receivers contract tx sig from maker {}, reattempting...",
                    maker_address
                );
                if ii <= RECONNECT_ATTEMPTS {
                    continue;
                } else {
                    return Err(Error::Protocol(
                        "Timed out of request_receivers_contract_tx_signatures attempt"));
                }
            },
        }
    }
}

async fn req_contract_sigs_for_recvr_once<S: SwapCoin>(
    maker_address: &MakerAddress,
    incoming_swapcoins: &[S],
    receivers_contract_txes: &[Transaction],
) -> Result<ContractSigsForRecvr, Error> {
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
        return Err(Error::Protocol("expected method receiverscontractsig"));
    };
    if maker_receiver_contract_sig.sigs.len() != incoming_swapcoins.len() {
        return Err(Error::Protocol("wrong number of signatures from maker"));
    }
    if maker_receiver_contract_sig
        .sigs
        .iter()
        .zip(incoming_swapcoins.iter())
        .any(|(sig, swapcoin)| !swapcoin.verify_contract_tx_receiver_sig(&sig))
    {
        return Err(Error::Protocol("invalid signature from maker"));
    }

    log::info!("<=== Received ReceiversContractSig from {}", maker_address);
    Ok(maker_receiver_contract_sig)
}

//return a list of the transactions and merkleproofs if the funding txes confirmed
//return None if any of the contract transactions were seen on the network
// if it turns out i want to return data in the contract tx broadcast case, then maybe use an enum
// TODO: This should be a wallet API.
async fn wait_for_funding_tx_confirmation(
    rpc: &Client,
    funding_txids: &[Txid],
    required_confirmations: i32,
    contract_to_watch: &[Vec<Transaction>],
    last_checked_block_height: &mut Option<u64>,
) -> Result<Option<(Vec<Transaction>, Vec<String>)>, Error> {
    let mut txid_tx_map = HashMap::<Txid, Transaction>::new();
    let mut txid_blockhash_map = HashMap::<Txid, BlockHash>::new();
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
            let gettx = match rpc.get_transaction(txid, Some(true)) {
                Ok(r) => r,
                //if we lose connection to the node, just try again, no point returning an error
                Err(_e) => continue,
            };
            if !txids_seen_once.contains(txid) {
                txids_seen_once.insert(*txid);
                if gettx.info.confirmations == 0 {
                    let mempool_tx = match rpc.get_mempool_entry(txid) {
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
                    rpc.get_tx_out_proof(&[txid], Some(&txid_blockhash_map.get(&txid).unwrap()))
                        .map(|gettxoutproof_result| gettxoutproof_result.to_hex())
                })
                .collect::<Result<Vec<String>, bitcoincore_rpc::Error>>()?;
            return Ok(Some((txes, merkleproofs)));
        }
        if !contract_to_watch.is_empty() {
            let contracts_broadcasted = check_for_broadcasted_contract_txes(
                rpc,
                &contract_to_watch
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
                        wallet_label: String::new(),
                    })
                    .collect::<Vec<ContractsInfo>>(),
                last_checked_block_height,
            )?;
            if !contracts_broadcasted.is_empty() {
                log::info!("Contract transactions were broadcasted! Aborting");
                return Ok(None);
            }
        }
        sleep(Duration::from_millis(1000)).await;
    }
}

// TODO: Simplify this function signature. Group related items into dedictaed structs.
// TODO: Add function doc.
/// Exchange all the required signatures with a Maker. Find the next Maker in the hop.
/// Initiate Coinswap between this Maker and the next Maker. This call will keep persisting
/// connection with a Maker if it is unresponsive, until a timeout.
async fn exchange_signatures_and_find_next_maker<'a>(
    rpc: &Client,
    config: &SwapParams,
    maker_offers_addresses: &mut Vec<&'a OfferAndAddress>,
    this_maker: &'a OfferAndAddress,
    previous_maker: Option<&'a OfferAndAddress>,
    is_taker_previous_peer: bool,
    is_taker_next_peer: bool,
    funding_txes: &[Transaction],
    funding_tx_merkleproofs: &[String],
    this_maker_multisig_redeemscripts: &[Script],
    this_maker_multisig_nonce: &[SecretKey],
    this_maker_contract_redeemscripts: &[Script],
    this_maker_hashlock_nonce: &[SecretKey],
    this_maker_contract_txes: &[Transaction],
    maker_refund_locktime: u16,
    hashvalue: Hash160,
    outgoing_swapcoins: &Vec<OutgoingSwapCoin>,
    watchonly_swapcoins: &mut Vec<Vec<WatchOnlySwapCoin>>,
) -> Result<
    (
        Vec<PublicKey>,
        Vec<SecretKey>,
        Vec<SecretKey>,
        ContractSigsAsRecvrAndSender,
        Vec<Script>,
        &'a OfferAndAddress,
    ),
    Error,
> {
    let mut ii = 0;
    loop {
        ii += 1;
        select! {
            ret = exchange_signatures_and_find_next_maker_attempt_once(
                rpc,
                config,
                maker_offers_addresses,
                this_maker,
                previous_maker,
                is_taker_previous_peer,
                is_taker_next_peer,
                funding_txes,
                funding_tx_merkleproofs,
                this_maker_multisig_redeemscripts,
                this_maker_multisig_nonce,
                this_maker_contract_redeemscripts,
                this_maker_hashlock_nonce,
                this_maker_contract_txes,
                maker_refund_locktime,
                hashvalue,
                outgoing_swapcoins,
                watchonly_swapcoins,
            ) => {
                match ret {
                    Ok(return_value) => return Ok(return_value),
                    Err(e) => {
                        log::warn!(
                            "Failed to exchange signatures with maker {}, \
                            reattempting... error={:?}",
                            this_maker.address,
                            e
                        );
                        if ii <= RECONNECT_ATTEMPTS {
                            sleep(Duration::from_secs(
                                if ii <= SHORT_LONG_SLEEP_DELAY_TRANSITION {
                                    RECONNECT_SHORT_SLEEP_DELAY_SEC
                                } else {
                                    RECONNECT_LONG_SLEEP_DELAY_SEC
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
            _ = sleep(Duration::from_secs(RECONNECT_ATTEMPT_TIMEOUT_SEC)) => {
                log::warn!(
                    "Timeout for exchange signatures with maker {}, reattempting...",
                    this_maker.address
                );
                if ii <= RECONNECT_ATTEMPTS {
                    continue;
                } else {
                    return Err(Error::Protocol(
                        "Timed out of exchange_signatures_and_find_next_maker attempt"));
                }
            },
        }
    }
}

async fn exchange_signatures_and_find_next_maker_attempt_once<'a>(
    rpc: &Client,
    config: &SwapParams,
    maker_offers_addresses: &mut Vec<&'a OfferAndAddress>,
    this_maker: &'a OfferAndAddress,
    previous_maker: Option<&'a OfferAndAddress>,
    is_taker_previous_peer: bool,
    is_taker_next_peer: bool,
    funding_txes: &[Transaction],
    funding_tx_merkleproofs: &[String],
    this_maker_multisig_redeemscripts: &[Script],
    this_maker_multisig_nonce: &[SecretKey],
    this_maker_contract_redeemscripts: &[Script],
    this_maker_hashlock_nonce: &[SecretKey],
    this_maker_contract_txes: &[Transaction],
    maker_refund_locktime: u16,
    hashvalue: Hash160,
    outgoing_swapcoins: &Vec<OutgoingSwapCoin>,
    watchonly_swapcoins: &mut Vec<Vec<WatchOnlySwapCoin>>,
) -> Result<
    (
        Vec<PublicKey>,
        Vec<SecretKey>,
        Vec<SecretKey>,
        ContractSigsAsRecvrAndSender,
        Vec<Script>,
        &'a OfferAndAddress,
    ),
    Error,
> {
    //return next_peer_multisig_pubkeys, next_peer_multisig_keys_or_nonces,
    //    next_peer_hashlock_keys_or_nonces, (), next_swap_contract_redeemscripts, found_next_maker

    log::info!("Connecting to {}", this_maker.address);
    let mut socket = TcpStream::connect(this_maker.address.get_tcpstream_address()).await?;
    let (mut socket_reader, mut socket_writer) =
        handshake_maker(&mut socket, &this_maker.address).await?;
    let mut next_maker = this_maker;
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
        ) = if is_taker_next_peer {
            let (my_recv_ms_pubkeys, my_recv_ms_nonce): (Vec<_>, Vec<_>) =
                (0..config.tx_count).map(|_| generate_keypair()).unzip();
            let (my_recv_hashlock_pubkeys, my_recv_hashlock_nonce): (Vec<_>, Vec<_>) =
                (0..config.tx_count).map(|_| generate_keypair()).unzip();
            (
                my_recv_ms_pubkeys,
                my_recv_ms_nonce,
                my_recv_hashlock_pubkeys,
                my_recv_hashlock_nonce,
            )
        } else {
            next_maker = choose_next_maker(maker_offers_addresses, config.send_amount)
                .expect("not enough offers");
            //next_maker is only ever accessed when the next peer is a maker, not a taker
            //i.e. if its ever used when is_taker_next_peer == true, then thats a bug
            generate_maker_keys(&next_maker.offer.tweakable_point, config.tx_count)
        };
        log::info!("===> Sending ProofOfFunding to {}", this_maker.address);
        let (sign_contract_txs_for_maker_as_receiver_and_sender, next_swap_contract_redeemscripts) =
            send_proof_of_funding_and_init_next_hop(
                &mut socket_reader,
                &mut socket_writer,
                this_maker,
                funding_txes,
                funding_tx_merkleproofs,
                this_maker_multisig_redeemscripts,
                this_maker_multisig_nonce,
                this_maker_contract_redeemscripts,
                this_maker_hashlock_nonce,
                &next_peer_multisig_pubkeys,
                &next_peer_hashlock_pubkeys,
                maker_refund_locktime,
                config.fee_rate,
                this_maker_contract_txes,
                hashvalue,
            )
            .await?;
        log::info!(
            "<=== Recieved SignSendersAndReceiversContractTxes from {}",
            this_maker.address
        );

        // If This Maker is the Sender, and we (the Taker) are the Receiver (Last Hop). We provide the Sender's Contact Tx Sigs.
        let senders_sigs = if is_taker_next_peer {
            log::info!("Taker is next peer. Signing Sender's Contract Txs",);
            sign_senders_contract_txs(
                &next_peer_multisig_keys_or_nonces,
                &sign_contract_txs_for_maker_as_receiver_and_sender,
            )?
        } else {
            // If Next Maker is the Receiver, and This Maker is The Sender, Request Sender's Contract Tx Sig to Next Maker.
            let next_swapcoins = create_watch_only_swapcoins(
                rpc,
                &sign_contract_txs_for_maker_as_receiver_and_sender,
                &next_peer_multisig_pubkeys,
                &next_swap_contract_redeemscripts,
            )?;
            let sigs = match req_contract_sigs_for_sender(
                &next_maker.address,
                &next_swapcoins,
                &next_peer_multisig_keys_or_nonces,
                &next_peer_hashlock_keys_or_nonces,
                maker_refund_locktime,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    log::debug!(
                        "Fail to obtain sender's contract tx signature from next_maker {}: {:?}",
                        next_maker.address,
                        e
                    );
                    continue; //go back to the start of the loop and try another maker
                }
            };
            watchonly_swapcoins.push(next_swapcoins);
            sigs.sigs
        };
        break (
            next_peer_multisig_pubkeys,
            next_peer_multisig_keys_or_nonces,
            next_peer_hashlock_keys_or_nonces,
            sign_contract_txs_for_maker_as_receiver_and_sender,
            next_swap_contract_redeemscripts,
            senders_sigs,
        );
    };

    // If This Maker is the Reciver, and We (The Taker) are the Sender (First Hop), Sign the Contract Tx.
    let receivers_sigs = if is_taker_previous_peer {
        log::info!("Taker is previous peer. Signing Receivers Contract Txs",);
        sign_receivers_contract_txs(
            &maker_sign_sender_and_receiver_contracts.receivers_contract_txs,
            outgoing_swapcoins,
        )?
    } else {
        // If Next Maker is the Receiver, and Previous Maker is the Sender, request Previous Maker to sign the Reciever's Contract Tx.
        assert!(previous_maker.is_some());
        let previous_maker_addr = &previous_maker.unwrap().address;
        log::info!(
            "===> Sending SignReceiversContractTx, previous maker is {}",
            previous_maker_addr,
        );
        let previous_maker_watchonly_swapcoins = if is_taker_next_peer {
            watchonly_swapcoins.last().unwrap()
        } else {
            //if the next peer is a maker not a taker, then that maker's swapcoins are last
            &watchonly_swapcoins[watchonly_swapcoins.len() - 2]
        };
        req_contract_sigs_for_recvr(
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
    Ok((
        next_peer_multisig_pubkeys,
        next_peer_multisig_keys_or_nonces,
        next_peer_hashlock_keys_or_nonces,
        maker_sign_sender_and_receiver_contracts,
        next_swap_contract_redeemscripts,
        next_maker,
    ))
}

// TODO: Simplify this function. Use dedicated structs for related items.
/// Send proof of funding to a Maker and initiate next Coinswap hop with this Maker and the Next Maker.
async fn send_proof_of_funding_and_init_next_hop(
    socket_reader: &mut BufReader<ReadHalf<'_>>,
    socket_writer: &mut WriteHalf<'_>,
    this_maker: &OfferAndAddress,
    funding_txes: &[Transaction],
    funding_tx_merkleproofs: &[String],
    this_maker_multisig_redeemscripts: &[Script],
    this_maker_multisig_nonces: &[SecretKey],
    this_maker_contract_redeemscripts: &[Script],
    this_maker_hashlock_nonces: &[SecretKey],
    next_peer_multisig_pubkeys: &[PublicKey],
    next_peer_hashlock_pubkeys: &[PublicKey],
    next_maker_refund_locktime: u16,
    next_maker_fee_rate: u64,
    this_maker_contract_txes: &[Transaction],
    hashvalue: Hash160,
) -> Result<(ContractSigsAsRecvrAndSender, Vec<Script>), Error> {
    send_message(
        socket_writer,
        TakerToMakerMessage::RespProofOfFunding(ProofOfFunding {
            confirmed_funding_txes: izip!(
                funding_txes.iter(),
                funding_tx_merkleproofs.iter(),
                this_maker_multisig_redeemscripts.iter(),
                this_maker_multisig_nonces.iter(),
                this_maker_contract_redeemscripts.iter(),
                this_maker_hashlock_nonces.iter()
            )
            .map(
                |(
                    funding_tx,
                    funding_tx_merkleproof,
                    multisig_redeemscript,
                    &multisig_key_nonce,
                    contract_redeemscript,
                    &hashlock_key_nonce,
                )| FundingTxInfo {
                    funding_tx: funding_tx.clone(),
                    funding_tx_merkleproof: funding_tx_merkleproof.clone(),
                    multisig_redeemscript: multisig_redeemscript.clone(),
                    multisig_nonce: multisig_key_nonce,
                    contract_redeemscript: contract_redeemscript.clone(),
                    hashlock_nonce: hashlock_key_nonce,
                },
            )
            .collect::<Vec<FundingTxInfo>>(),
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
            return Err(Error::Protocol(
                "expected method signsendersandreceiverscontracttxes",
            ));
        };
    if maker_sign_sender_and_receiver_contracts
        .receivers_contract_txs
        .len()
        != this_maker_multisig_redeemscripts.len()
    {
        return Err(Error::Protocol(
            "wrong number of receivers contracts tx from maker",
        ));
    }
    if maker_sign_sender_and_receiver_contracts
        .senders_contract_txs_info
        .len()
        != next_peer_multisig_pubkeys.len()
    {
        return Err(Error::Protocol(
            "wrong number of senders contract txes from maker",
        ));
    }

    let funding_tx_values = funding_txes
        .iter()
        .zip(this_maker_multisig_redeemscripts.iter())
        .map(|(makers_funding_tx, multisig_redeemscript)| {
            find_funding_output(&makers_funding_tx, &multisig_redeemscript)
                .ok_or(Error::Protocol(
                    "multisig redeemscript not found in funding tx",
                ))
                .map(|txout| txout.1.value)
        })
        .collect::<Result<Vec<u64>, Error>>()?;
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
        return Err(Error::Protocol("next_amount incorrect"));
    }
    log::info!(
        "this_amount={} coinswap_fees={} miner_fees_paid_by_taker={} next_amount={}",
        this_amount,
        coinswap_fees,
        miner_fees_paid_by_taker,
        next_amount
    );

    for (receivers_contract_tx, contract_tx, contract_redeemscript) in izip!(
        maker_sign_sender_and_receiver_contracts
            .receivers_contract_txs
            .iter(),
        this_maker_contract_txes.iter(),
        this_maker_contract_redeemscripts.iter()
    ) {
        validate_contract_tx(
            &receivers_contract_tx,
            Some(&contract_tx.input[0].previous_output),
            &contract_redeemscript,
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

/// Settle all coinswaps by sending hash preimages and privkeys.
async fn settle_all_coinswaps(
    config: &SwapParams,
    preimage: &Preimage,
    active_maker_addresses: &Vec<&MakerAddress>,
    outgoing_swapcoins: &Vec<OutgoingSwapCoin>,
    watchonly_swapcoins: &mut Vec<Vec<WatchOnlySwapCoin>>,
    incoming_swapcoins: &mut Vec<IncomingSwapCoin>,
) -> Result<(), Error> {
    let mut outgoing_privkeys: Option<Vec<MultisigPrivkey>> = None;
    for (index, maker_address) in active_maker_addresses.iter().enumerate() {
        let is_taker_previous_peer = index == 0;
        let is_taker_next_peer = (index as u16) == config.maker_count - 1;

        let senders_multisig_redeemscripts = if is_taker_previous_peer {
            outgoing_swapcoins
                .iter()
                .map(|sc| sc.get_multisig_redeemscript())
                .collect::<Vec<_>>()
        } else {
            watchonly_swapcoins[index - 1]
                .iter()
                .map(|sc| sc.get_multisig_redeemscript())
                .collect::<Vec<_>>()
        };
        let receivers_multisig_redeemscripts = if is_taker_next_peer {
            incoming_swapcoins
                .iter()
                .map(|sc| sc.get_multisig_redeemscript())
                .collect::<Vec<_>>()
        } else {
            watchonly_swapcoins[index]
                .iter()
                .map(|sc| sc.get_multisig_redeemscript())
                .collect::<Vec<_>>()
        };

        let mut ii = 0;
        loop {
            ii += 1;
            select! {
                ret = settle_one_coinswap(
                    maker_address,
                    index,
                    is_taker_previous_peer,
                    is_taker_next_peer,
                    &mut outgoing_privkeys,
                    outgoing_swapcoins,
                    watchonly_swapcoins,
                    incoming_swapcoins,
                    &senders_multisig_redeemscripts,
                    &receivers_multisig_redeemscripts,
                    preimage,
                ) => {
                    if let Err(e) = ret {
                        log::warn!(
                            "Failed to connect to maker {} to settle coinswap, \
                            reattempting... error={:?}",
                            maker_address,
                            e
                        );
                        if ii <= RECONNECT_ATTEMPTS {
                            sleep(Duration::from_secs(
                                if ii <= SHORT_LONG_SLEEP_DELAY_TRANSITION {
                                    RECONNECT_SHORT_SLEEP_DELAY_SEC
                                } else {
                                    RECONNECT_LONG_SLEEP_DELAY_SEC
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
                _ = sleep(Duration::from_secs(RECONNECT_ATTEMPT_TIMEOUT_SEC)) => {
                    log::warn!(
                        "Timeout for settling coinswap with maker {}, reattempting...",
                        maker_address
                    );
                    if ii <= RECONNECT_ATTEMPTS {
                        continue;
                    } else {
                        return Err(Error::Protocol(
                            "Timed out of settle_one_coinswap attempt"));
                    }
                },
            }
        }
    }
    Ok(())
}

async fn settle_one_coinswap(
    maker_address: &MakerAddress,
    index: usize,
    is_taker_previous_peer: bool,
    is_taker_next_peer: bool,
    outgoing_privkeys: &mut Option<Vec<MultisigPrivkey>>,
    outgoing_swapcoins: &Vec<OutgoingSwapCoin>,
    watchonly_swapcoins: &mut Vec<Vec<WatchOnlySwapCoin>>,
    incoming_swapcoins: &mut Vec<IncomingSwapCoin>,
    senders_multisig_redeemscripts: &Vec<Script>,
    receivers_multisig_redeemscripts: &Vec<Script>,
    preimage: &Preimage,
) -> Result<(), Error> {
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
        preimage,
    )
    .await?;
    log::info!("<=== Received PrivateKeyHandover from {}", maker_address);

    let privkeys_reply = if is_taker_previous_peer {
        outgoing_swapcoins
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
            incoming_swapcoins,
            &maker_private_key_handover.multisig_privkeys,
        )
    } else {
        let ret = check_and_apply_maker_private_keys(
            &mut watchonly_swapcoins[index],
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

/// The final step of Coinswap. When all the signatures are passed around, perform the private key handover.
async fn send_hash_preimage_and_get_private_keys(
    socket_reader: &mut BufReader<ReadHalf<'_>>,
    socket_writer: &mut WriteHalf<'_>,
    senders_multisig_redeemscripts: &Vec<Script>,
    receivers_multisig_redeemscripts: &Vec<Script>,
    preimage: &Preimage,
) -> Result<PrivKeyHandover, Error> {
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
            return Err(Error::Protocol("expected method privatekeyhandover"));
        };
    if maker_private_key_handover.multisig_privkeys.len() != receivers_multisig_redeemscripts_len {
        return Err(Error::Protocol("wrong number of private keys from maker"));
    }
    Ok(maker_private_key_handover)
}
