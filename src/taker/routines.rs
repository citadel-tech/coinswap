use serde::{Deserialize, Serialize};
use std::time::Duration;

use bitcoin::{secp256k1::SecretKey, PublicKey, ScriptBuf, Transaction};
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
    error::ProtocolError,
    protocol::{
        contract::{
            calculate_coinswap_fee, create_contract_redeemscript, find_funding_output_index,
            validate_contract_tx, FUNDING_TX_VBYTE_SIZE,
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
    error::TakerError,
    offers::{MakerAddress, OfferAndAddress},
};

use crate::wallet::SwapCoin;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ContractTransaction {
    pub tx: Transaction,
    pub redeemscript: ScriptBuf,
    pub hashlock_spend_without_preimage: Option<Transaction>,
    pub timelock_spend: Option<Transaction>,
    pub timelock_spend_broadcasted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ContractsInfo {
    pub contract_txes: Vec<ContractTransaction>,
    pub wallet_label: String,
}

/// Performs a handshake with a Maker and returns and Reader and Writer halves.
pub async fn handshake_maker<'a>(
    socket: &'a mut TcpStream,
    maker_address: &MakerAddress,
) -> Result<(BufReader<ReadHalf<'a>>, WriteHalf<'a>), TakerError> {
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
        &TakerToMakerMessage::TakerHello(TakerHello {
            protocol_version_min: 0,
            protocol_version_max: 0,
        }),
    )
    .await?;
    let makerhello = match read_message(&mut socket_reader).await {
        Ok(MakerToTakerMessage::MakerHello(m)) => m,
        Ok(any) => {
            return Err(ProtocolError::WrongMessage {
                expected: "MakerHello".to_string(),
                received: format!("{}", any),
            }
            .into());
        }
        Err(e) => return Err(e.into()),
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
) -> Result<ContractSigsForSender, TakerError> {
    log::info!("Connecting to {}", maker_address);
    let mut socket = TcpStream::connect(maker_address.get_tcpstream_address()).await?;
    let (mut socket_reader, mut socket_writer) =
        handshake_maker(&mut socket, maker_address).await?;
    log::info!("===> Sending ReqContractSigsForSender to {}", maker_address);

    // TODO: Take this construction out of function body.
    let txs_info = maker_multisig_nonces
        .iter()
        .zip(maker_hashlock_nonces.iter())
        .zip(outgoing_swapcoins.iter())
        .map(
            |((&multisig_key_nonce, &hashlock_key_nonce), outgoing_swapcoin)| {
                ContractTxInfoForSender {
                    multisig_nonce: multisig_key_nonce,
                    hashlock_nonce: hashlock_key_nonce,
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
        &TakerToMakerMessage::ReqContractSigsForSender(ReqContractSigsForSender {
            txs_info,
            hashvalue: outgoing_swapcoins[0].get_hashvalue(),
            locktime,
        }),
    )
    .await?;
    let contract_sigs_for_sender = match read_message(&mut socket_reader).await {
        Ok(MakerToTakerMessage::RespContractSigsForSender(m)) => {
            if m.sigs.len() != outgoing_swapcoins.len() {
                return Err(ProtocolError::WrongNumOfSigs {
                    expected: outgoing_swapcoins.len(),
                    received: m.sigs.len(),
                }
                .into());
            } else {
                m
            }
        }
        Ok(any) => {
            return Err(ProtocolError::WrongMessage {
                expected: "RespContractSigsForSender".to_string(),
                received: format!("{}", any),
            }
            .into());
        }
        Err(e) => return Err(e.into()),
    };

    for (sig, outgoing_swapcoin) in contract_sigs_for_sender
        .sigs
        .iter()
        .zip(outgoing_swapcoins.iter())
    {
        outgoing_swapcoin.verify_contract_tx_sender_sig(sig)?;
    }
    log::info!("<=== Received ContractSigsForSender from {}", maker_address);
    Ok(contract_sigs_for_sender)
}

/// Request signatures for receiver side of the hop. Attempt once.
pub(crate) async fn req_sigs_for_recvr_once<S: SwapCoin>(
    maker_address: &MakerAddress,
    incoming_swapcoins: &[S],
    receivers_contract_txes: &[Transaction],
) -> Result<ContractSigsForRecvr, TakerError> {
    log::info!("Connecting to {}", maker_address);
    let mut socket = TcpStream::connect(maker_address.get_tcpstream_address()).await?;
    let (mut socket_reader, mut socket_writer) =
        handshake_maker(&mut socket, maker_address).await?;

    // TODO: Take the message construction out of function body.
    send_message(
        &mut socket_writer,
        &TakerToMakerMessage::ReqContractSigsForRecvr(ReqContractSigsForRecvr {
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
    let contract_sigs_for_recvr = match read_message(&mut socket_reader).await {
        Ok(MakerToTakerMessage::RespContractSigsForRecvr(m)) => {
            if m.sigs.len() != incoming_swapcoins.len() {
                return Err(ProtocolError::WrongNumOfSigs {
                    expected: incoming_swapcoins.len(),
                    received: m.sigs.len(),
                }
                .into());
            } else {
                m
            }
        }
        Ok(any) => {
            return Err(ProtocolError::WrongMessage {
                expected: "ContractSigsForRecvr".to_string(),
                received: format!("{}", any),
            }
            .into());
        }
        Err(e) => return Err(e.into()),
    };

    for (sig, swapcoin) in contract_sigs_for_recvr
        .sigs
        .iter()
        .zip(incoming_swapcoins.iter())
    {
        swapcoin.verify_contract_tx_receiver_sig(sig)?;
    }

    log::info!("<=== Received ContractSigsForRecvr from {}", maker_address);
    Ok(contract_sigs_for_recvr)
}

// Type for information related to `this maker` consisting of:
// `this_maker`, `funding_txs_infos`, `this_maker_contract_txs`
#[derive(Clone)]
pub struct ThisMakerInfo {
    pub this_maker: OfferAndAddress,
    pub funding_tx_infos: Vec<FundingTxInfo>,
    pub this_maker_contract_txs: Vec<Transaction>,
}

// Type for information related to the next peer
#[derive(Clone)]
pub struct NextPeerInfoArgs {
    pub next_peer_multisig_pubkeys: Vec<PublicKey>,
    pub next_peer_hashlock_pubkeys: Vec<PublicKey>,
    pub next_maker_refund_locktime: u16,
    pub next_maker_fee_rate: u64,
}

/// [Internal] Send a Proof funding to the maker and init next hop.
pub(crate) async fn send_proof_of_funding_and_init_next_hop(
    socket_reader: &mut BufReader<ReadHalf<'_>>,
    socket_writer: &mut WriteHalf<'_>,
    tmi: ThisMakerInfo,
    npi: NextPeerInfoArgs,
    hashvalue: Hash160,
) -> Result<(ContractSigsAsRecvrAndSender, Vec<ScriptBuf>), TakerError> {
    send_message(
        socket_writer,
        &TakerToMakerMessage::RespProofOfFunding(ProofOfFunding {
            confirmed_funding_txes: tmi.funding_tx_infos.clone(),
            next_coinswap_info: npi
                .next_peer_multisig_pubkeys
                .iter()
                .zip(npi.next_peer_hashlock_pubkeys.iter())
                .map(
                    |(&next_coinswap_multisig_pubkey, &next_hashlock_pubkey)| NextHopInfo {
                        next_multisig_pubkey: next_coinswap_multisig_pubkey,
                        next_hashlock_pubkey,
                    },
                )
                .collect::<Vec<NextHopInfo>>(),
            next_locktime: npi.next_maker_refund_locktime,
            next_fee_rate: npi.next_maker_fee_rate,
        }),
    )
    .await?;
    let contract_sigs_as_recvr_and_sender = match read_message(socket_reader).await {
        Ok(MakerToTakerMessage::ReqContractSigsAsRecvrAndSender(m)) => {
            if m.receivers_contract_txs.len() != tmi.funding_tx_infos.len() {
                return Err(ProtocolError::WrongNumOfContractTxs {
                    expected: tmi.funding_tx_infos.len(),
                    received: m.receivers_contract_txs.len(),
                }
                .into());
            } else if m.senders_contract_txs_info.len() != npi.next_peer_multisig_pubkeys.len() {
                return Err(ProtocolError::WrongNumOfContractTxs {
                    expected: m.senders_contract_txs_info.len(),
                    received: npi.next_peer_multisig_pubkeys.len(),
                }
                .into());
            } else {
                m
            }
        }
        Ok(any) => {
            return Err(ProtocolError::WrongMessage {
                expected: "ContractSigsAsRecvrAndSender".to_string(),
                received: format!("{}", any),
            }
            .into());
        }
        Err(e) => return Err(e.into()),
    };

    let funding_tx_values = tmi
        .funding_tx_infos
        .iter()
        .map(|funding_info| {
            let funding_output_index =
                find_funding_output_index(funding_info).map_err(ProtocolError::Contract)?;
            Ok(funding_info
                .funding_tx
                .output
                .get(funding_output_index as usize)
                .expect("funding output expected")
                .value)
        })
        .collect::<Result<Vec<u64>, TakerError>>()?;

    let this_amount = funding_tx_values.iter().sum::<u64>();

    let next_amount = contract_sigs_as_recvr_and_sender
        .senders_contract_txs_info
        .iter()
        .map(|i| i.funding_amount)
        .sum::<u64>();
    let coinswap_fees = calculate_coinswap_fee(
        tmi.this_maker.offer.absolute_fee_sat,
        tmi.this_maker.offer.amount_relative_fee_ppb,
        tmi.this_maker.offer.time_relative_fee_ppb,
        this_amount,
        1, //time_in_blocks just 1 for now
    );
    let miner_fees_paid_by_taker = FUNDING_TX_VBYTE_SIZE
        * npi.next_maker_fee_rate
        * (npi.next_peer_multisig_pubkeys.len() as u64)
        / 1000;
    let calculated_next_amount = this_amount - coinswap_fees - miner_fees_paid_by_taker;
    if calculated_next_amount != next_amount {
        return Err(ProtocolError::IncorrectFundingAmount {
            expected: calculated_next_amount,
            found: next_amount,
        }
        .into());
    }
    log::info!(
        "this_amount={} coinswap_fees={} miner_fees_paid_by_taker={} next_amount={}",
        this_amount,
        coinswap_fees,
        miner_fees_paid_by_taker,
        next_amount
    );

    for ((receivers_contract_tx, contract_tx), contract_redeemscript) in
        contract_sigs_as_recvr_and_sender
            .receivers_contract_txs
            .iter()
            .zip(tmi.this_maker_contract_txs.iter())
            .zip(
                tmi.funding_tx_infos
                    .iter()
                    .map(|fi| &fi.contract_redeemscript),
            )
    {
        validate_contract_tx(
            receivers_contract_tx,
            Some(&contract_tx.input[0].previous_output),
            contract_redeemscript,
        )
        .map_err(ProtocolError::Contract)?;
    }
    let next_swap_contract_redeemscripts = npi
        .next_peer_hashlock_pubkeys
        .iter()
        .zip(
            contract_sigs_as_recvr_and_sender
                .senders_contract_txs_info
                .iter(),
        )
        .map(|(hashlock_pubkey, senders_contract_tx_info)| {
            create_contract_redeemscript(
                hashlock_pubkey,
                &senders_contract_tx_info.timelock_pubkey,
                &hashvalue,
                &npi.next_maker_refund_locktime,
            )
        })
        .collect::<Vec<_>>();
    Ok((
        contract_sigs_as_recvr_and_sender,
        next_swap_contract_redeemscripts,
    ))
}

/// Send hash preimage via the writer and read the response.
pub(crate) async fn send_hash_preimage_and_get_private_keys(
    socket_reader: &mut BufReader<ReadHalf<'_>>,
    socket_writer: &mut WriteHalf<'_>,
    senders_multisig_redeemscripts: &[ScriptBuf],
    receivers_multisig_redeemscripts: &[ScriptBuf],
    preimage: &Preimage,
) -> Result<PrivKeyHandover, TakerError> {
    send_message(
        socket_writer,
        &TakerToMakerMessage::RespHashPreimage(HashPreimage {
            senders_multisig_redeemscripts: senders_multisig_redeemscripts.to_vec(),
            receivers_multisig_redeemscripts: receivers_multisig_redeemscripts.to_vec(),
            preimage: *preimage,
        }),
    )
    .await?;
    let privkey_handover = match read_message(socket_reader).await {
        Ok(MakerToTakerMessage::RespPrivKeyHandover(m)) => {
            if m.multisig_privkeys.len() != receivers_multisig_redeemscripts.len() {
                return Err(ProtocolError::WrongNumOfPrivkeys {
                    expected: receivers_multisig_redeemscripts.len(),
                    received: m.multisig_privkeys.len(),
                }
                .into());
            } else {
                m
            }
        }
        Ok(any) => {
            return Err(ProtocolError::WrongMessage {
                expected: "PrivkeyHandover".to_string(),
                received: format!("{}", any),
            }
            .into());
        }
        Err(e) => return Err(e.into()),
    };

    Ok(privkey_handover)
}

async fn download_maker_offer_attempt_once(addr: &MakerAddress) -> Result<Offer, TakerError> {
    log::debug!(target: "offerbook", "Connecting to {}", addr);
    let mut socket = TcpStream::connect(addr.get_tcpstream_address()).await?;
    let (mut socket_reader, mut socket_writer) = handshake_maker(&mut socket, addr).await?;

    send_message(
        &mut socket_writer,
        &TakerToMakerMessage::ReqGiveOffer(GiveOffer),
    )
    .await?;

    let offer = match read_message(&mut socket_reader).await {
        Ok(MakerToTakerMessage::RespOffer(m)) => m,
        Ok(any) => {
            return Err(ProtocolError::WrongMessage {
                expected: "RespOffer".to_string(),
                received: format!("{}", any),
            }
            .into());
        }
        Err(e) => return Err(e.into()),
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
