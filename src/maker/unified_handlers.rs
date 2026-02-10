//! Unified message handlers for the Maker.

use std::{sync::Arc, time::Instant};

use bitcoin::{Amount, PublicKey, Transaction};

use super::error::MakerError;
use crate::{
    protocol::{
        common_messages::{
            AckSwapDetails, FidelityProof, GetOffer, MakerHello, Offer, ProtocolVersion,
            SwapDetails, TakerHello,
        },
        legacy_messages::LegacyTakerMessage,
        router::{MakerToTakerMessage, TakerToMakerMessage},
        taproot_messages::TaprootTakerMessage,
    },
    wallet::unified_swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
};

/// Minimum time required to react to contract broadcasts (in blocks).
pub const MIN_CONTRACT_REACTION_TIME: u16 = 10;

/// Connection state for unified protocol handling.
#[derive(Debug, Clone)]
pub struct UnifiedConnectionState {
    /// Protocol version being used for this connection.
    pub protocol: ProtocolVersion,
    /// Current phase of the swap.
    pub phase: SwapPhase,
    /// Unique swap identifier.
    pub swap_id: Option<String>,
    /// Swap amount.
    pub swap_amount: Amount,
    /// Timelock value.
    pub timelock: u16,
    /// Incoming swap coins (we receive).
    pub incoming_swapcoins: Vec<IncomingSwapCoin>,
    /// Outgoing swap coins (we send).
    pub outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    /// Pending funding transactions (not yet broadcast).
    pub pending_funding_txes: Vec<Transaction>,
    /// Contract fee rate for multi-hop swap creation.
    pub contract_feerate: f64,
    /// Last activity timestamp.
    pub last_activity: Instant,
}

/// Phases of a swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapPhase {
    /// Initial state, awaiting hello.
    AwaitingHello,
    /// Hello received, awaiting offer request.
    AwaitingOfferRequest,
    /// Offer sent, awaiting swap details.
    AwaitingSwapDetails,
    /// Swap details received, awaiting contract data.
    AwaitingContractData,
    /// Contract data received (ECDSA: awaiting signatures, Taproot: awaiting preimage).
    AwaitingSignaturesOrPreimage,
    /// Signatures/preimage received, awaiting private key handover.
    AwaitingPrivateKeyHandover,
    /// Swap completed.
    Completed,
}

impl Default for UnifiedConnectionState {
    fn default() -> Self {
        UnifiedConnectionState {
            protocol: ProtocolVersion::Legacy,
            phase: SwapPhase::AwaitingHello,
            swap_id: None,
            swap_amount: Amount::ZERO,
            timelock: 0,
            incoming_swapcoins: Vec::new(),
            outgoing_swapcoins: Vec::new(),
            pending_funding_txes: Vec::new(),
            contract_feerate: 0.0,
            last_activity: Instant::now(),
        }
    }
}

impl UnifiedConnectionState {
    /// Create a new connection state for a specific protocol version.
    pub fn new(protocol: ProtocolVersion) -> Self {
        UnifiedConnectionState {
            protocol,
            ..Default::default()
        }
    }

    /// Update the last activity timestamp.
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Check if the swap has timed out.
    pub fn is_timed_out(&self, timeout_secs: u64) -> bool {
        self.last_activity.elapsed().as_secs() > timeout_secs
    }
}

/// Trait for unified maker operations.
pub trait UnifiedMaker: Send + Sync {
    /// Get the network port for logging.
    fn network_port(&self) -> u16;

    /// Get the Bitcoin network.
    fn network(&self) -> bitcoin::Network;

    /// Get the tweakable keypair for swap address derivation.
    fn get_tweakable_keypair(
        &self,
    ) -> Result<(bitcoin::secp256k1::SecretKey, PublicKey), MakerError>;

    /// Get the highest fidelity proof.
    fn get_fidelity_proof(&self) -> Result<FidelityProof, MakerError>;

    /// Get maker configuration values.
    fn get_config(&self) -> MakerConfig;

    /// Validate swap parameters.
    fn validate_swap_parameters(&self, details: &SwapDetails) -> Result<(), MakerError>;

    /// Calculate the swap fee.
    fn calculate_swap_fee(&self, amount: Amount, timelock: u16) -> Amount;

    /// Create a funding transaction for Legacy (P2WSH) address.
    fn create_funding_transaction(
        &self,
        amount: Amount,
        address: bitcoin::Address,
    ) -> Result<(Transaction, u32), MakerError>;

    /// Broadcast a transaction.
    fn broadcast_transaction(&self, tx: &Transaction) -> Result<bitcoin::Txid, MakerError>;

    /// Save incoming swapcoin to wallet.
    fn save_incoming_swapcoin(&self, swapcoin: &IncomingSwapCoin) -> Result<(), MakerError>;

    /// Save outgoing swapcoin to wallet.
    fn save_outgoing_swapcoin(&self, swapcoin: &OutgoingSwapCoin) -> Result<(), MakerError>;

    /// Register outpoint for watching.
    fn register_watch_outpoint(&self, outpoint: bitcoin::OutPoint);

    /// Sweep incoming swapcoins after successful swap.
    fn sweep_incoming_swapcoins(&self) -> Result<(), MakerError>;

    /// Store connection state for persistence across connections.
    fn store_connection_state(&self, swap_id: &str, state: &UnifiedConnectionState);

    /// Retrieve stored connection state.
    fn get_connection_state(&self, swap_id: &str) -> Option<UnifiedConnectionState>;

    /// Verify and sign sender's contract transactions.
    fn verify_and_sign_sender_contract_txs(
        &self,
        txs_info: &[crate::protocol::legacy_messages::ContractTxInfoForSender],
        hashvalue: &crate::protocol::Hash160,
        locktime: u16,
    ) -> Result<Vec<bitcoin::ecdsa::Signature>, MakerError>;

    /// Verify proof of funding and return the hashvalue.
    fn verify_proof_of_funding(
        &self,
        message: &crate::protocol::legacy_messages::ProofOfFunding,
    ) -> Result<crate::protocol::Hash160, MakerError>;

    /// Initialize outgoing coinswap.
    fn initialize_coinswap(
        &self,
        send_amount: Amount,
        next_multisig_pubkeys: &[PublicKey],
        next_hashlock_pubkeys: &[PublicKey],
        hashvalue: crate::protocol::Hash160,
        locktime: u16,
        contract_feerate: f64,
    ) -> Result<(Vec<Transaction>, Vec<OutgoingSwapCoin>, Amount), MakerError>;

    /// Find outgoing swapcoin by its multisig redeemscript.
    fn find_outgoing_swapcoin(
        &self,
        multisig_redeemscript: &bitcoin::ScriptBuf,
    ) -> Option<OutgoingSwapCoin>;
}

/// Maker configuration values.
#[derive(Debug, Clone)]
pub struct MakerConfig {
    /// Base fee in satoshis.
    pub base_fee: u64,
    /// Amount-relative fee percentage.
    pub amount_relative_fee_pct: f64,
    /// Time-relative fee percentage.
    pub time_relative_fee_pct: f64,
    /// Minimum swap amount.
    pub min_swap_amount: u64,
    /// Maximum swap amount.
    pub max_swap_amount: u64,
    /// Required confirmations.
    pub required_confirms: u32,
    /// Supported protocol versions.
    pub supported_protocols: Vec<ProtocolVersion>,
}

/// Unified message handler
pub fn handle_message<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    message: TakerToMakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.touch();

    log::debug!(
        "[{}] Handling message: {:?} (phase: {:?}, protocol: {:?})",
        UnifiedMaker::network_port(maker.as_ref()),
        message,
        state.phase,
        state.protocol
    );

    match message {
        TakerToMakerMessage::TakerHello(hello) => handle_taker_hello(maker, state, hello),
        TakerToMakerMessage::GetOffer(get_offer) => handle_get_offer(maker, state, get_offer),
        TakerToMakerMessage::SwapDetails(details) => handle_swap_details(maker, state, details),

        TakerToMakerMessage::ReqContractSigsForSender(req) => handle_legacy_dispatch(
            maker,
            state,
            LegacyTakerMessage::ReqContractSigsForSender(req),
        ),
        TakerToMakerMessage::ProofOfFunding(pof) => {
            handle_legacy_dispatch(maker, state, LegacyTakerMessage::ProofOfFunding(pof))
        }
        TakerToMakerMessage::RespContractSigsForRecvrAndSender(resp) => handle_legacy_dispatch(
            maker,
            state,
            LegacyTakerMessage::RespContractSigsForRecvrAndSender(resp),
        ),
        TakerToMakerMessage::ReqContractSigsForRecvr(req) => handle_legacy_dispatch(
            maker,
            state,
            LegacyTakerMessage::ReqContractSigsForRecvr(req),
        ),
        TakerToMakerMessage::LegacyHashPreimage(preimage) => {
            handle_legacy_dispatch(maker, state, LegacyTakerMessage::HashPreimage(preimage))
        }
        TakerToMakerMessage::LegacyPrivateKeyHandover(handover) => handle_legacy_dispatch(
            maker,
            state,
            LegacyTakerMessage::PrivateKeyHandover(handover),
        ),
        TakerToMakerMessage::TaprootContractData(data) => {
            handle_taproot_dispatch(maker, state, TaprootTakerMessage::ContractData(data))
        }
        TakerToMakerMessage::TaprootHashPreimage(preimage) => {
            handle_taproot_dispatch(maker, state, TaprootTakerMessage::HashPreimage(preimage))
        }
        TakerToMakerMessage::TaprootPrivateKeyHandover(handover) => handle_taproot_dispatch(
            maker,
            state,
            TaprootTakerMessage::PrivateKeyHandover(handover),
        ),
    }
}

/// Handle TakerHello message.
fn handle_taker_hello<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    _hello: TakerHello,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Received TakerHello",
        UnifiedMaker::network_port(maker.as_ref()),
    );

    let config = maker.get_config();
    state.phase = SwapPhase::AwaitingOfferRequest;

    log::info!(
        "[{}] Supported protocols: {:?}",
        UnifiedMaker::network_port(maker.as_ref()),
        config.supported_protocols
    );

    Ok(Some(MakerToTakerMessage::MakerHello(MakerHello {
        supported_protocols: config.supported_protocols,
    })))
}

/// Handle GetOffer message.
fn handle_get_offer<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    _get_offer: GetOffer,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Received GetOffer request",
        UnifiedMaker::network_port(maker.as_ref())
    );

    let (_, tweakable_point) = maker.get_tweakable_keypair()?;
    let fidelity = maker.get_fidelity_proof()?;
    let config = maker.get_config();

    state.phase = SwapPhase::AwaitingSwapDetails;

    let offer = Offer {
        base_fee: config.base_fee,
        amount_relative_fee_pct: config.amount_relative_fee_pct,
        time_relative_fee_pct: config.time_relative_fee_pct,
        required_confirms: config.required_confirms,
        minimum_locktime: MIN_CONTRACT_REACTION_TIME,
        max_size: config.max_swap_amount,
        min_size: config.min_swap_amount,
        tweakable_point,
        fidelity,
    };

    log::info!(
        "[{}] Sending offer: min={}, max={}",
        UnifiedMaker::network_port(maker.as_ref()),
        offer.min_size,
        offer.max_size
    );

    Ok(Some(MakerToTakerMessage::Offer(Box::new(offer))))
}

/// Handle SwapDetails message.
fn handle_swap_details<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    details: SwapDetails,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    log::info!(
        "[{}] Received SwapDetails: amount={}, timelock={}, protocol={:?}",
        UnifiedMaker::network_port(maker.as_ref()),
        details.amount,
        details.timelock,
        details.protocol_version
    );

    maker.validate_swap_parameters(&details)?;

    state.swap_id = Some(details.id.clone());
    state.swap_amount = details.amount;
    state.timelock = details.timelock;
    state.protocol = details.protocol_version;
    state.phase = SwapPhase::AwaitingContractData;

    maker.store_connection_state(&details.id, state);

    let (_, tweakable_point) = maker.get_tweakable_keypair()?;

    log::info!(
        "[{}] Accepting swap (id: {})",
        UnifiedMaker::network_port(maker.as_ref()),
        details.id
    );

    Ok(Some(MakerToTakerMessage::AckSwapDetails(
        AckSwapDetails::accept(tweakable_point),
    )))
}

/// Restore connection state if this is a new/reconnected connection.
fn restore_state_if_needed<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    swap_id: &str,
) {
    if state.swap_amount == Amount::ZERO || state.outgoing_swapcoins.is_empty() {
        if let Some(stored) = maker.get_connection_state(swap_id) {
            log::info!(
                "[{}] Restored state for {}: amount={}, timelock={}, outgoing_count={}",
                maker.network_port(),
                swap_id,
                stored.swap_amount,
                stored.timelock,
                stored.outgoing_swapcoins.len()
            );
            state.swap_id = Some(swap_id.to_string());
            state.swap_amount = stored.swap_amount;
            state.timelock = stored.timelock;
            state.protocol = stored.protocol;
            state.incoming_swapcoins = stored.incoming_swapcoins;
            state.outgoing_swapcoins = stored.outgoing_swapcoins;
            state.pending_funding_txes = stored.pending_funding_txes;
        }
    }
}

/// Dispatch to Legacy handlers.
fn handle_legacy_dispatch<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    legacy_msg: LegacyTakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    let swap_id = legacy_msg.swap_id().to_string();

    log::info!(
        "[{}] Dispatching Legacy message: {} (swap_id: {})",
        maker.network_port(),
        legacy_msg,
        swap_id
    );

    restore_state_if_needed(maker, state, &swap_id);

    super::legacy_handlers::handle_legacy_message(maker, state, legacy_msg)
}

/// Dispatch to Taproot handlers.
fn handle_taproot_dispatch<M: UnifiedMaker>(
    maker: &Arc<M>,
    state: &mut UnifiedConnectionState,
    taproot_msg: TaprootTakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    let swap_id = taproot_msg.swap_id().to_string();

    log::info!(
        "[{}] Dispatching Taproot message: {:?} (swap_id: {})",
        maker.network_port(),
        taproot_msg,
        swap_id
    );

    restore_state_if_needed(maker, state, &swap_id);

    super::taproot_handlers::handle_taproot_message(maker, state, taproot_msg)
}
