//! Various operational routines/functions.
//!
//! It includes functions for handshaking, requesting contract signatures, sending proofs of funding, and downloading maker offers.
//! It also defines structs for contract transactions and contract information.
//! Notable types include [ThisMakerInfo], and [NextMakerInfo].
//! It also handles downloading maker offers with retry mechanisms and implements the necessary message structures
//! for communication between taker and maker.

use chrono::Utc;
use socks::Socks5Stream;
use std::{net::TcpStream, thread::sleep, time::Duration};

use crate::{
    protocol::{
        contract::{
            calculate_coinswap_fee, create_contract_redeemscript, find_funding_output_index,
            validate_contract_tx,
        },
        error::ProtocolError,
        messages::{
            ContractSigsAsRecvrAndSender, ContractSigsForRecvr, ContractSigsForSender,
            ContractTxInfoForRecvr, ContractTxInfoForSender, FundingTxInfo, GiveOffer,
            HashPreimage, MakerToTakerMessage, NextHopInfo, Offer, Preimage, PrivKeyHandover,
            ProofOfFunding, ReqContractSigsForRecvr, ReqContractSigsForSender, TakerHello,
            TakerToMakerMessage,
        },
        Hash160,
    },
    utill::{calculate_fee_sats, read_message, ConnectionType, MIN_FEE_RATE},
    wallet::WalletError,
};
use bitcoin::{secp256k1::SecretKey, Amount, PublicKey, ScriptBuf, Transaction};

use super::{
    config::TakerConfig,
    error::TakerError,
    offers::{MakerAddress, OfferAndAddress},
    send_message_with_prefix,
};

use crate::taker::api::{
    FIRST_CONNECT_ATTEMPTS, FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC, FIRST_CONNECT_SLEEP_DELAY_SEC,
};

use crate::wallet::SwapCoin;

/// Make a handshake with a maker.
/// Ensures that the Maker is alive and responding.
///
// In the future, handshake can be used to find protocol compatibility across multiple versions.
pub(crate) fn handshake_maker(socket: &mut TcpStream) -> Result<(), TakerError> {
    send_message_with_prefix(
        socket,
        &TakerToMakerMessage::TakerHello(TakerHello {
            protocol_version_min: 1,
            protocol_version_max: 1,
        }),
    )?;
    let msg_bytes = read_message(socket)?;
    let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;

    // Check that protocol version is always 1.
    match msg {
        MakerToTakerMessage::MakerHello(m) => {
            if m.protocol_version_max == 1 && m.protocol_version_min == 1 {
                Ok(())
            } else {
                Err(ProtocolError::WrongMessage {
                    expected: "Only protocol version 1 is allowed".to_string(),
                    received: format!(
                        "min/max version  = {}/{}",
                        m.protocol_version_min, m.protocol_version_max
                    ),
                }
                .into())
            }
        }
        any => Err((ProtocolError::WrongMessage {
            expected: "MakerHello".to_string(),
            received: format!("{any}"),
        })
        .into()),
    }
}

/// Request signatures for sender side of the hop. Attempt once.
pub(crate) fn req_sigs_for_sender_once<S: SwapCoin>(
    socket: &mut TcpStream,
    outgoing_swapcoins: &[S],
    maker_multisig_nonces: &[SecretKey],
    maker_hashlock_nonces: &[SecretKey],
    locktime: u16,
) -> Result<ContractSigsForSender, TakerError> {
    handshake_maker(socket)?;
    let txs_info = maker_multisig_nonces
        .iter()
        .zip(maker_hashlock_nonces.iter())
        .zip(outgoing_swapcoins.iter())
        .map(
            |((&multisig_key_nonce, &hashlock_key_nonce), outgoing_swapcoin)| {
                Ok(ContractTxInfoForSender {
                    multisig_nonce: multisig_key_nonce,
                    hashlock_nonce: hashlock_key_nonce,
                    timelock_pubkey: outgoing_swapcoin.get_timelock_pubkey()?,
                    senders_contract_tx: outgoing_swapcoin.get_contract_tx(),
                    multisig_redeemscript: outgoing_swapcoin.get_multisig_redeemscript(),
                    funding_input_value: outgoing_swapcoin.get_funding_amount(),
                })
            },
        )
        .collect::<Result<Vec<ContractTxInfoForSender>, WalletError>>()?;

    send_message_with_prefix(
        socket,
        &TakerToMakerMessage::ReqContractSigsForSender(ReqContractSigsForSender {
            txs_info,
            hashvalue: outgoing_swapcoins[0].get_hashvalue()?,
            locktime,
        }),
    )?;

    let msg_bytes = read_message(socket)?;
    let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;
    let contract_sigs_for_sender = match msg {
        MakerToTakerMessage::RespContractSigsForSender(m) => {
            if m.sigs.len() != outgoing_swapcoins.len() {
                return Err((ProtocolError::WrongNumOfSigs {
                    expected: outgoing_swapcoins.len(),
                    received: m.sigs.len(),
                })
                .into());
            } else {
                m
            }
        }
        any => {
            return Err((ProtocolError::WrongMessage {
                expected: "RespContractSigsForSender".to_string(),
                received: format!("{any}"),
            })
            .into());
        }
    };

    for (sig, outgoing_swapcoin) in contract_sigs_for_sender
        .sigs
        .iter()
        .zip(outgoing_swapcoins.iter())
    {
        outgoing_swapcoin.verify_contract_tx_sender_sig(sig)?;
    }
    Ok(contract_sigs_for_sender)
}

/// Request signatures for receiver side of the hop. Attempt once.
pub(crate) fn req_sigs_for_recvr_once<S: SwapCoin>(
    socket: &mut TcpStream,
    incoming_swapcoins: &[S],
    receivers_contract_txes: &[Transaction],
) -> Result<ContractSigsForRecvr, TakerError> {
    handshake_maker(socket)?;

    let txs_info = incoming_swapcoins
        .iter()
        .zip(receivers_contract_txes.iter())
        .map(|(swapcoin, receivers_contract_tx)| ContractTxInfoForRecvr {
            multisig_redeemscript: swapcoin.get_multisig_redeemscript(),
            contract_tx: receivers_contract_tx.clone(),
        })
        .collect::<Vec<ContractTxInfoForRecvr>>();

    send_message_with_prefix(
        socket,
        &TakerToMakerMessage::ReqContractSigsForRecvr(ReqContractSigsForRecvr { txs: txs_info }),
    )?;

    let msg_bytes = read_message(socket)?;
    let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;
    let contract_sigs_for_recvr = match msg {
        MakerToTakerMessage::RespContractSigsForRecvr(m) => {
            if m.sigs.len() != incoming_swapcoins.len() {
                return Err((ProtocolError::WrongNumOfSigs {
                    expected: incoming_swapcoins.len(),
                    received: m.sigs.len(),
                })
                .into());
            } else {
                m
            }
        }
        any => {
            return Err((ProtocolError::WrongMessage {
                expected: "ContractSigsForRecvr".to_string(),
                received: format!("{any}"),
            })
            .into());
        }
    };

    for (sig, swapcoin) in contract_sigs_for_recvr
        .sigs
        .iter()
        .zip(incoming_swapcoins.iter())
    {
        swapcoin.verify_contract_tx_receiver_sig(sig)?;
    }
    Ok(contract_sigs_for_recvr)
}

// Type for information related to `this maker` consisting of:
// `this_maker`, `funding_txs_infos`, `this_maker_contract_txs`
#[derive(Clone)]
pub(crate) struct ThisMakerInfo {
    pub(crate) this_maker: OfferAndAddress,
    pub(crate) funding_tx_infos: Vec<FundingTxInfo>,
    pub(crate) this_maker_contract_txs: Vec<Transaction>,
    pub this_maker_refund_locktime: u16,
}

// Type for information related to the next peer // why not next Maker?
#[derive(Clone)]
pub struct NextMakerInfo {
    pub next_peer_multisig_pubkeys: Vec<PublicKey>,
    pub next_peer_hashlock_pubkeys: Vec<PublicKey>,
}

/// Send a Proof funding to the maker and init next hop.
pub(crate) fn send_proof_of_funding_and_init_next_hop(
    socket: &mut TcpStream,
    tmi: ThisMakerInfo,
    npi: NextMakerInfo,
    hashvalue: Hash160,
    id: String,
) -> Result<(ContractSigsAsRecvrAndSender, Vec<ScriptBuf>), TakerError> {
    // Send POF
    let next_coinswap_info = npi
        .next_peer_multisig_pubkeys
        .iter()
        .zip(npi.next_peer_hashlock_pubkeys.iter())
        .map(
            |(&next_coinswap_multisig_pubkey, &next_hashlock_pubkey)| NextHopInfo {
                next_multisig_pubkey: next_coinswap_multisig_pubkey,
                next_hashlock_pubkey,
            },
        )
        .collect::<Vec<NextHopInfo>>();

    let pof_msg = TakerToMakerMessage::RespProofOfFunding(ProofOfFunding {
        confirmed_funding_txes: tmi.funding_tx_infos.clone(),
        next_coinswap_info,
        refund_locktime: tmi.this_maker_refund_locktime,
        contract_feerate: MIN_FEE_RATE,
        id,
    });

    send_message_with_prefix(socket, &pof_msg)?;

    // Recv ContractSigsAsRecvrAndSender.
    let msg_bytes = read_message(socket)?;
    let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;
    let contract_sigs_as_recvr_and_sender = match msg {
        MakerToTakerMessage::ReqContractSigsAsRecvrAndSender(m) => {
            if m.receivers_contract_txs.len() != tmi.funding_tx_infos.len() {
                return Err((ProtocolError::WrongNumOfContractTxs {
                    expected: tmi.funding_tx_infos.len(),
                    received: m.receivers_contract_txs.len(),
                })
                .into());
            } else if m.senders_contract_txs_info.len() != npi.next_peer_multisig_pubkeys.len() {
                return Err((ProtocolError::WrongNumOfContractTxs {
                    expected: m.senders_contract_txs_info.len(),
                    received: npi.next_peer_multisig_pubkeys.len(),
                })
                .into());
            } else {
                m
            }
        }
        any => {
            return Err((ProtocolError::WrongMessage {
                expected: "ContractSigsAsRecvrAndSender".to_string(),
                received: format!("{any}"),
            })
            .into());
        }
    };

    let funding_tx_values = tmi
        .funding_tx_infos
        .iter()
        .map(|funding_info| {
            let funding_output_index = find_funding_output_index(funding_info)?;
            Ok(funding_info
                .funding_tx
                .output
                .get(funding_output_index as usize)
                .expect("funding output expected")
                .value
                .to_sat())
        })
        .collect::<Result<Vec<u64>, TakerError>>()?;

    let this_amount = funding_tx_values.iter().sum::<u64>();

    let next_amount = contract_sigs_as_recvr_and_sender
        .senders_contract_txs_info
        .iter()
        .map(|i| i.funding_amount)
        .sum::<Amount>();

    let coinswap_fees = calculate_coinswap_fee(
        this_amount,
        tmi.this_maker_refund_locktime,
        tmi.this_maker.offer.base_fee,
        tmi.this_maker.offer.amount_relative_fee_pct,
        tmi.this_maker.offer.time_relative_fee_pct,
    );

    let tx_size = tmi.funding_tx_infos.iter().fold(0u64, |acc, info| {
        acc + info.funding_tx.weight().to_vbytes_ceil()
    });

    let miner_fees_paid_by_taker = calculate_fee_sats(tx_size);

    let calculated_next_amount = this_amount - coinswap_fees - miner_fees_paid_by_taker;

    if Amount::from_sat(calculated_next_amount) != next_amount {
        return Err((ProtocolError::IncorrectFundingAmount {
            expected: Amount::from_sat(calculated_next_amount),
            found: next_amount,
        })
        .into());
    }

    log::info!(
        "Maker Received = {} | Maker is Forwarding = {} |  Coinswap Fees = {}",
        Amount::from_sat(this_amount),
        next_amount,
        Amount::from_sat(coinswap_fees),
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
        )?;
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
                &tmi.this_maker_refund_locktime,
            )
        })
        .collect::<Vec<_>>();
    Ok((
        contract_sigs_as_recvr_and_sender,
        next_swap_contract_redeemscripts,
    ))
}

/// Send hash preimage via the writer and read the response.
pub(crate) fn send_hash_preimage_and_get_private_keys(
    socket: &mut TcpStream,
    senders_multisig_redeemscripts: &[ScriptBuf],
    receivers_multisig_redeemscripts: &[ScriptBuf],
    preimage: &Preimage,
) -> Result<PrivKeyHandover, TakerError> {
    let hash_preimage_msg = TakerToMakerMessage::RespHashPreimage(HashPreimage {
        senders_multisig_redeemscripts: senders_multisig_redeemscripts.to_vec(),
        receivers_multisig_redeemscripts: receivers_multisig_redeemscripts.to_vec(),
        preimage: *preimage,
    });

    send_message_with_prefix(socket, &hash_preimage_msg)?;

    let msg_bytes = read_message(socket)?;
    let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;
    let privkey_handover = match msg {
        MakerToTakerMessage::RespPrivKeyHandover(m) => {
            if m.multisig_privkeys.len() != receivers_multisig_redeemscripts.len() {
                return Err((ProtocolError::WrongNumOfPrivkeys {
                    expected: receivers_multisig_redeemscripts.len(),
                    received: m.multisig_privkeys.len(),
                })
                .into());
            } else {
                m
            }
        }
        any => {
            return Err((ProtocolError::WrongMessage {
                expected: "PrivkeyHandover".to_string(),
                received: format!("{any}"),
            })
            .into());
        }
    };

    Ok(privkey_handover)
}

fn download_maker_offer_attempt_once(
    addr: &MakerAddress,
    config: &TakerConfig,
) -> Result<Offer, TakerError> {
    let maker_addr = addr.to_string();
    log::info!("Downloading offer from {maker_addr}");
    let mut socket = match config.connection_type {
        ConnectionType::CLEARNET => TcpStream::connect(&maker_addr)?,
        ConnectionType::TOR => Socks5Stream::connect(
            format!("127.0.0.1:{}", config.socks_port).as_str(),
            maker_addr.as_ref(),
        )?
        .into_inner(),
    };

    socket.set_read_timeout(Some(Duration::from_secs(FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC)))?;
    socket.set_write_timeout(Some(Duration::from_secs(FIRST_CONNECT_ATTEMPT_TIMEOUT_SEC)))?;

    handshake_maker(&mut socket)?;

    send_message_with_prefix(&mut socket, &TakerToMakerMessage::ReqGiveOffer(GiveOffer))?;

    let msg_bytes = read_message(&mut socket)?;
    let msg: MakerToTakerMessage = serde_cbor::from_slice(&msg_bytes)?;
    let offer = match msg {
        MakerToTakerMessage::RespOffer(offer) => offer,
        msg => {
            return Err(ProtocolError::WrongMessage {
                expected: "RespOffer".to_string(),
                received: format!("{msg}"),
            }
            .into());
        }
    };

    log::info!("Downloaded offer from : {maker_addr} ");

    Ok(*offer)
}

pub(crate) fn download_maker_offer(
    address: MakerAddress,
    config: TakerConfig,
) -> Option<OfferAndAddress> {
    let mut ii = 0;

    loop {
        ii += 1;
        match download_maker_offer_attempt_once(&address, &config) {
            Ok(offer) => {
                return Some(OfferAndAddress {
                    offer,
                    address,
                    timestamp: Utc::now(),
                })
            }
            Err(e) => {
                if ii <= FIRST_CONNECT_ATTEMPTS {
                    log::warn!(
                        "Failed to request offer from maker {address}, with error: {e:?} reattempting {ii} of {FIRST_CONNECT_ATTEMPTS}"
                    );
                    sleep(Duration::from_secs(FIRST_CONNECT_SLEEP_DELAY_SEC));
                    continue;
                } else {
                    log::error!(
                        "Connection attempt exceeded for request offer from maker {address}"
                    );
                    return None;
                }
            }
        }
    }
}
