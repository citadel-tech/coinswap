//! Collection of all message handlers for a Maker using Taproot protocol.
//!
//! Implements the logic for message handling based on the new taproot message protocol.
//! This module handles the new message flow: GetOffer -> Offer -> SwapDetails -> AckResponse ->
//! SendersContract -> ReceiverToSenderContract -> PartialSignaturesAndNonces.
//! Manages the taproot-based swap protocol with MuSig2 signatures.

use bitcoin::OutPoint;

use super::{
    api2::{ConnectionState, Maker},
    error::MakerError,
};
use std::{sync::Arc, time::Instant};

use crate::{
    protocol::{
        self,
        messages2::{
            GetOffer, MakerToTakerMessage, PrivateKeyHandover, SendersContract, SwapDetails,
            TakerToMakerMessage,
        },
    },
    utill::MIN_FEE_RATE,
};

/// The Global Handle Message function for taproot protocol. Takes in a [`Arc<Maker>`] and handles
/// messages according to the new taproot message flow without requiring state expectations.
pub(crate) fn handle_message_taproot(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    message: TakerToMakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::debug!("Handling message: {:?}", message);

    match message {
        TakerToMakerMessage::GetOffer(get_offer_msg) => handle_get_offer(maker, get_offer_msg),
        TakerToMakerMessage::SwapDetails(swap_details) => {
            handle_swap_details(maker, connection_state, swap_details)
        }
        TakerToMakerMessage::SendersContract(senders_contract) => {
            handle_senders_contract(maker, connection_state, senders_contract)
        }
        TakerToMakerMessage::PrivateKeyHandover(privkey_handover_message) => {
            handle_privkey_handover(maker, connection_state, privkey_handover_message)
        }
    }
}

fn handle_get_offer(
    maker: &Arc<Maker>,
    _get_offer: GetOffer,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!("Handling GetOffer request");
    let offer = maker.create_offer()?;
    log::info!(
        "Sending offer: min_size={}, max_size={}",
        offer.min_size,
        offer.max_size
    );
    Ok(Some(MakerToTakerMessage::RespOffer(Box::new(offer))))
}

fn handle_privkey_handover(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    privkey_handover: PrivateKeyHandover,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    let response = maker.process_private_key_handover(&privkey_handover, connection_state)?;
    Ok(Some(MakerToTakerMessage::PrivateKeyHandover(response)))
}

fn handle_swap_details(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    swap_details: SwapDetails,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[Swap: {}] Handling SwapDetails: amount={}, timelock={}, tx_count={}",
        swap_details.id,
        swap_details.amount,
        swap_details.timelock,
        swap_details.no_of_tx
    );

    let (privkey, pubkey) = maker.wallet.read()?.get_tweakable_keypair()?;
    connection_state.incoming_contract.my_privkey = Some(privkey);
    connection_state.incoming_contract.my_pubkey = Some(pubkey);

    maker.validate_swap_parameters(&swap_details)?;

    connection_state.swap_amount = swap_details.amount;
    connection_state.timelock = swap_details.timelock;

    {
        let mut wallet_write = maker.wallet.write()?;
        wallet_write.sync_and_save()?;
        log::info!("Sync at:----handle_swap_details----");
    }

    {
        let required_amount = swap_details.amount;
        let mut ongoing_state_lock = maker.ongoing_swap_state.lock()?;

        let excluded_utxos = ongoing_state_lock
            .iter()
            .filter(|(id, _)| *id != &swap_details.id)
            .flat_map(|(_, (state, _))| state.reserve_utxo.clone())
            .collect();

        let wallet = maker.wallet.read()?;
        let selected_utxos =
            wallet.coin_select(required_amount, MIN_FEE_RATE, None, Some(excluded_utxos))?;

        connection_state.reserve_utxo = selected_utxos
            .iter()
            .map(|(utxo, _)| OutPoint::new(utxo.txid, utxo.vout))
            .collect();

        log::debug!(
            "[Swap: {}] Reserved {} UTXOs for swap",
            swap_details.id,
            connection_state.reserve_utxo.len(),
        );

        ongoing_state_lock.insert(
            swap_details.id.clone(),
            (connection_state.clone(), Instant::now()),
        );
    }

    log::info!("Inserted state for swap id: {}", swap_details.id);

    connection_state.swap_start_time = Some(std::time::Instant::now());

    let our_fee = maker.calculate_swap_fee(swap_details.amount, swap_details.timelock);
    log::info!("[Swap: {}] Calculated fee: {}", swap_details.id, our_fee);

    #[cfg(feature = "integration-test")]
    if maker.behavior == super::api2::MakerBehavior::CloseAfterAckResponse {
        log::warn!(
            "[Swap: {}] Maker behavior: CloseAfterAckResponse - Closing connection",
            swap_details.id
        );
        return Err(MakerError::General(
            "Maker closing connection after sending AckResponse to taker (test behavior)",
        ));
    }

    Ok(Some(MakerToTakerMessage::AckResponse(
        protocol::messages2::AckResponse {
            tweakable_point: Some(pubkey),
        },
    )))
}

fn handle_senders_contract(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    senders_contract: SendersContract,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[Swap: {}] Handling SendersContract with {} contracts",
        senders_contract.id,
        senders_contract.contract_txs.len()
    );

    #[cfg(feature = "integration-test")]
    if maker.behavior == super::api2::MakerBehavior::CloseAtContractSigsExchange {
        log::warn!(
            "[Swap: {}] Maker behavior: CloseAtContractSigsExchange - Closing connection",
            senders_contract.id
        );
        return Err(MakerError::General(
            "Maker closing connection at contract exchange (test behavior)",
        ));
    }

    let receiver_contract =
        maker.verify_and_process_senders_contract(&senders_contract, connection_state)?;

    let swap_id = format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        connection_state
            .incoming_contract
            .contract_tx
            .compute_txid()
    );
    connection_state.incoming_contract.swap_id = Some(swap_id.clone());
    connection_state.outgoing_contract.swap_id = Some(swap_id.clone());

    {
        let mut wallet = maker.wallet().write()?;
        wallet.add_incoming_swapcoin_v2(&connection_state.incoming_contract);
        wallet.add_outgoing_swapcoin_v2(&connection_state.outgoing_contract);
        wallet.save_to_disk()?;
        log::info!(
            "Persisted incoming and outgoing swapcoins with swap_id={} to wallet",
            swap_id
        );
    }

    log::info!(
        "[Swap: {}] Sending SenderContractFromMaker with {} contracts",
        senders_contract.id,
        receiver_contract.contract_txs.len()
    );

    Ok(Some(MakerToTakerMessage::SenderContractFromMaker(
        receiver_contract,
    )))
}
