//! Collection of all message handlers for a Maker using Taproot protocol.
//!
//! Implements the logic for message handling based on the new taproot message protocol.
//! This module handles the new message flow: GetOffer -> Offer -> SwapDetails -> AckResponse ->
//! SendersContract -> ReceiverToSenderContract -> PartialSignaturesAndNonces.
//! Manages the taproot-based swap protocol with MuSig2 signatures.

use std::sync::Arc;

use super::{
    api2::{ConnectionState, Maker},
    error::MakerError,
};

use crate::protocol::messages2::{
    AckResponse, GetOffer, MakerToTakerMessage, PrivateKeyHandover, SendersContract, SwapDetails,
    TakerToMakerMessage,
};

/// The Global Handle Message function for taproot protocol. Takes in a [`Arc<Maker>`] and handles
/// messages according to the new taproot message flow without requiring state expectations.
pub(crate) fn handle_message_taproot(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    message: TakerToMakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::debug!(
        "[{}] Handling message: {:?}",
        maker.config.network_port,
        message
    );

    // Handle messages based on their type, not on expected state
    match message {
        TakerToMakerMessage::GetOffer(get_offer_msg) => {
            handle_get_offer(maker, connection_state, get_offer_msg)
        }
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

/// Handles GetOffer message and returns an Offer with fidelity proof
fn handle_get_offer(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    _get_offer: GetOffer,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!("[{}] Handling GetOffer request", maker.config.network_port);

    // Create offer using the new api2 implementation
    let offer = maker.create_offer(connection_state)?;

    log::info!(
        "[{}] Sending offer: min_size={}, max_size={}",
        maker.config.network_port,
        offer.min_size,
        offer.max_size
    );

    Ok(Some(MakerToTakerMessage::RespOffer(Box::new(offer))))
}

/// Handles SwapDetails message and validates the swap parameters
fn handle_swap_details(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    swap_details: SwapDetails,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Handling SwapDetails: amount={}, timelock={}, tx_count={}",
        maker.config.network_port,
        swap_details.amount,
        swap_details.timelock,
        swap_details.no_of_tx
    );

    // Validate swap parameters using api2
    maker.validate_swap_parameters(&swap_details)?;

    // Store swap details in connection state
    connection_state.swap_amount = swap_details.amount;
    connection_state.timelock = swap_details.timelock;

    // Calculate our fee for this swap
    let our_fee = maker.calculate_swap_fee(swap_details.amount, swap_details.timelock);
    log::info!(
        "[{}] Calculated fee: {}",
        maker.config.network_port,
        our_fee
    );

    // Send acknowledgment
    Ok(Some(MakerToTakerMessage::AckResponse(AckResponse::Ack)))
}

/// Handles SendersContract message and creates our receiver contract
fn handle_senders_contract(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    senders_contract: SendersContract,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Handling SendersContract with {} contracts",
        maker.config.network_port,
        senders_contract.contract_txs.len()
    );

    // Process the sender's contract and create our response
    let receiver_contract =
        maker.verify_and_process_senders_contract(&senders_contract, connection_state)?;

    log::info!(
        "[{}] Sending SenderContractFromMaker with {} contracts",
        maker.config.network_port,
        receiver_contract.contract_txs.len()
    );

    Ok(Some(MakerToTakerMessage::SenderContractFromMaker(
        receiver_contract,
    )))
}

fn handle_privkey_handover(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    privkey_handover: PrivateKeyHandover,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    let response = maker.process_private_key_handover(&privkey_handover, connection_state)?;
    Ok(Some(MakerToTakerMessage::PrivateKeyHandover(response)))
}
