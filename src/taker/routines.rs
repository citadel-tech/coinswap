use std::time::Duration;

use bitcoin::{secp256k1::SecretKey, PublicKey, Script, Transaction};
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

use crate::{
    error::TeleportError,
    protocol::{
        contract::{
            calculate_coinswap_fee, create_contract_redeemscript, find_funding_output,
            validate_contract_tx, MAKER_FUNDING_TX_VBYTE_SIZE,
        },
        messages::{
            ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForSender,
            ContractTxInfoForRecvr, ContractTxInfoForSender, FundingTxInfo, GiveOffer,
            HashPreimage, MakerToTakerMessage, NextHopInfo, Offer, Preimage, PrivKeyHandover,
            ProofOfFunding, ReqContractSigsForRecvr, ReqContractSigsForSender, TakerHello,
            TakerToMakerMessage,
        },
        Hash160,
    },
    utill::{read_message, send_message},
};

use super::{
    config::TakerConfig,
    offers::{MakerAddress, OfferAndAddress},
};

use crate::wallet::SwapCoin;

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

/// Request signatures for sender side of the hop. Attempt once.
pub(crate) async fn req_sigs_for_sender_once<S: SwapCoin>(
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
    let txs_info = maker_multisig_nonces
        .iter()
        .zip(maker_hashlock_nonces.iter())
        .zip(outgoing_swapcoins.iter())
        .map(
            |((&multisig_key_nonce, &hashlock_key_nonce), outgoing_swapcoin)| {
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
        .collect::<Vec<ContractTxInfoForSender>>();
    send_message(
        &mut socket_writer,
        TakerToMakerMessage::ReqContractSigsForSender(ReqContractSigsForSender {
            txs_info,
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

/// Request signatures for receiver side of the hop. Attempt once.
pub(crate) async fn req_sigs_for_recvr_once<S: SwapCoin>(
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

/// [Internal] Send a Proof funding to the maker and init next hop.
pub(crate) async fn send_proof_of_funding_and_init_next_hop(
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

/// Send hash preimage via the writer and read the response.
pub(crate) async fn send_hash_preimage_and_get_private_keys(
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

async fn download_maker_offer_attempt_once(addr: &MakerAddress) -> Result<Offer, TeleportError> {
    log::debug!(target: "offerbook", "Connecting to {}", addr);
    let mut socket = TcpStream::connect(addr.get_tcpstream_address()).await?;
    let (mut socket_reader, mut socket_writer) = handshake_maker(&mut socket, addr).await?;

    send_message(
        &mut socket_writer,
        TakerToMakerMessage::ReqGiveOffer(GiveOffer),
    )
    .await?;

    let offer = if let MakerToTakerMessage::RespOffer(o) = read_message(&mut socket_reader).await? {
        o
    } else {
        return Err(TeleportError::Protocol("expected method offer"));
    };

    log::debug!(target: "offerbook", "Obtained offer from {}", addr);
    Ok(offer)
}

pub async fn download_maker_offer(
    address: MakerAddress,
    config: TakerConfig,
) -> Option<OfferAndAddress> {
    let mut ii = 0;
    loop {
        ii += 1;
        select! {
            ret = download_maker_offer_attempt_once(&address) => {
                match ret {
                    Ok(offer) => return Some(OfferAndAddress { offer, address }),
                    Err(e) => {
                        log::debug!(target: "offerbook",
                            "Failed to request offer from maker {}, \
                            reattempting... error={:?}",
                            address,
                            e
                        );
                        if ii <= config.first_connect_attempts {
                            sleep(Duration::from_secs(config.first_connect_sleep_delay_sec)).await;
                            continue;
                        } else {
                            return None;
                        }
                    }
                }
            },
            _ = sleep(Duration::from_secs(config.first_connect_attempt_timeout_sec)) => {
                log::debug!(target: "offerbook",
                    "Timeout for request offer from maker {}, reattempting...",
                    address
                );
                if ii <= config.first_connect_attempts {
                    continue;
                } else {
                    return None;
                }
            },
        }
    }
}
