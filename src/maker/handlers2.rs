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
    AckResponse, GetOffer, MakerToTakerMessage, PartialSigAndSendersNonce, SendersContract,
    SpendingTxAndReceiverNonce, SwapDetails, TakerToMakerMessage,
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
        maker.config.network_port(),
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
        // New backwards sweeping protocol messages
        TakerToMakerMessage::SpendingTxAndReceiverNonce(spending_tx_msg) => {
            handle_spending_tx_and_receiver_nonce(maker, connection_state, spending_tx_msg)
        }
        TakerToMakerMessage::PartialSigAndSendersNonce(partial_sig_msg) => {
            handle_partial_sig_and_senders_nonce(maker, connection_state, partial_sig_msg)
        }
    }
}

/// Handles GetOffer message and returns an Offer with fidelity proof
fn handle_get_offer(
    maker: &Arc<Maker>,
    _connection_state: &mut ConnectionState,
    _get_offer: GetOffer,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Handling GetOffer request",
        maker.config.network_port()
    );

    // Create offer using the new api2 implementation
    let offer = maker.create_offer(_connection_state)?;

    log::info!(
        "[{}] Sending offer: min_size={}, max_size={}",
        maker.config.network_port(),
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
        maker.config.network_port(),
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
        maker.config.network_port(),
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
        maker.config.network_port(),
        senders_contract.contract_txs.len()
    );

    // Process the sender's contract and create our response
    let receiver_contract =
        maker.verify_and_process_senders_contract(&senders_contract, connection_state)?;

    log::info!(
        "[{}] Sending SenderContractFromMaker with {} contracts",
        maker.config.network_port(),
        receiver_contract.contract_txs.len()
    );

    Ok(Some(MakerToTakerMessage::SenderContractFromMaker(
        receiver_contract,
    )))
}

/// Handles SpendingTxAndReceiverNonce message in the new backwards sweeping protocol
/// This is step 13 or 16 in the protocol where taker sends spending transaction and receiver nonce
fn handle_spending_tx_and_receiver_nonce(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    spending_tx_msg: SpendingTxAndReceiverNonce,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Handling SpendingTxAndReceiverNonce",
        maker.config.network_port()
    );

    // Generate nonces and partial signature for this sweep
    let response =
        maker.process_spending_tx_and_receiver_nonce(&spending_tx_msg, connection_state)?;

    Ok(Some(MakerToTakerMessage::NoncesPartialSigsAndSpendingTx(
        response,
    )))
}

/// Handles PartialSigAndSendersNonce message in the new backwards sweeping protocol
/// This is step 18 or 20 in the protocol where taker relays partial signature to complete maker's sweep
fn handle_partial_sig_and_senders_nonce(
    maker: &Arc<Maker>,
    connection_state: &mut ConnectionState,
    partial_sig_and_senders_nonce: PartialSigAndSendersNonce,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Handling PartialSigAndSendersNonce",
        maker.config.network_port()
    );

    // Complete the maker's sweep transaction with the received partial signature
    maker
        .complete_sweep_with_partial_signature(&partial_sig_and_senders_nonce, connection_state)?;

    // No response needed for this message type
    Ok(None)
}
