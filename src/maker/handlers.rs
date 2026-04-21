//! Message handlers for the Maker.

use std::{sync::Arc, time::Instant};

use bitcoin::{Amount, PublicKey, Transaction};

use super::error::MakerError;
use crate::{
    protocol::{
        common_messages::{
            AckSwapDetails, FidelityProof, GetOffer, MakerHello, MakerToTakerMessage, Offer,
            ProtocolVersion, SwapDetails, TakerHello, TakerToMakerMessage,
        },
        legacy_messages::LegacyTakerMessage,
        taproot_messages::TaprootTakerMessage,
    },
    wallet::swapcoin::{IncomingSwapCoin, OutgoingSwapCoin},
};

/// Test-only behavior overrides for the maker.
#[cfg(feature = "integration-test")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MakerBehavior {
    /// Normal operation (no test override).
    #[default]
    Normal,
    /// Receive contract sigs and save swapcoins, but skip funding broadcast
    /// and close the connection. Simulates last-maker misbehavior.
    SkipFundingBroadcast,
    /// Close connection when receiving ReqContractSigsForSender (abort2 scenarios).
    CloseAtReqContractSigsForSender,
    /// Close connection when receiving ProofOfFunding (abort2 scenario).
    CloseAtProofOfFunding,
    /// Close connection when receiving RespContractSigsForRecvrAndSender (abort3 scenario).
    CloseAtContractSigsForRecvrAndSender,
    /// Close connection when receiving ReqContractSigsForRecvr (abort3 scenario).
    CloseAtContractSigsForRecvr,
    /// Close connection when receiving PrivateKeyHandover / hash preimage (abort3 scenario).
    CloseAtHashPreimage,
    /// Broadcast contract transactions after setup, then close (malice scenario).
    BroadcastContractAfterSetup,
    /// Close connection after sending AckSwapDetails (taproot maker abort).
    CloseAfterAckResponse,
    /// Close connection at private key handover phase (taproot maker abort).
    CloseAtPrivateKeyHandover,
    /// Close connection at contract sigs exchange (taproot recovery test).
    CloseAtContractSigsExchange,
    /// Close connection after sweep (taproot hashlock recovery test).
    CloseAfterSweep,
    /// Use an invalid fidelity bond timelock (fidelity timelock violation test).
    InvalidFidelityTimelock,
}

/// Minimum time required to react to contract broadcasts (in blocks).
pub const MIN_CONTRACT_REACTION_TIME: u16 = 10;

/// Connection state for protocol handling.
#[derive(Debug, Clone)]
pub struct ConnectionState {
    /// Protocol version being used for this connection.
    pub protocol: ProtocolVersion,
    /// Current phase of the swap.
    pub phase: SwapPhase,
    /// Unique swap identifier.
    pub swap_id: Option<String>,
    /// Swap amount.
    pub swap_amount: Amount,
    /// Timelock value (Legacy: relative CSV, Taproot: absolute CLTV height).
    pub timelock: u32,
    /// Relative locktime offset for deterministic fee calculation.
    pub refund_locktime_offset: u16,
    /// Incoming swap coins (we receive).
    pub incoming_swapcoins: Vec<IncomingSwapCoin>,
    /// Outgoing swap coins (we send).
    pub outgoing_swapcoins: Vec<OutgoingSwapCoin>,
    /// Pending funding transactions (not yet broadcast).
    pub pending_funding_txes: Vec<Transaction>,
    /// Contract fee rate for multi-hop swap creation.
    pub contract_feerate: f64,
    /// Whether the funding transaction was actually broadcast to the network.
    pub funding_broadcast: bool,
    /// Reserved UTXOs for this swap (prevents concurrent double-spending).
    pub reserve_utxo: Vec<bitcoin::OutPoint>,
    /// Last activity timestamp.
    pub last_activity: Instant,
    /// Swap start time for duration tracking in reports.
    pub swap_start_time: Instant,
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

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState {
            protocol: ProtocolVersion::Legacy,
            phase: SwapPhase::AwaitingHello,
            swap_id: None,
            swap_amount: Amount::ZERO,
            timelock: 0,
            refund_locktime_offset: 0,
            incoming_swapcoins: Vec::new(),
            outgoing_swapcoins: Vec::new(),
            pending_funding_txes: Vec::new(),
            contract_feerate: 0.0,
            funding_broadcast: false,
            reserve_utxo: Vec::new(),
            last_activity: Instant::now(),
            swap_start_time: Instant::now(),
        }
    }
}

impl ConnectionState {
    /// Create a new connection state for a specific protocol version.
    pub fn new(protocol: ProtocolVersion) -> Self {
        ConnectionState {
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

    /// Enforce that the current phase matches one of the expected phases.
    pub fn expect_phase(&self, expected: &[SwapPhase]) -> Result<(), MakerError> {
        if expected.contains(&self.phase) {
            Ok(())
        } else {
            Err(MakerError::General(
                format!(
                    "Unexpected message in phase {:?} (expected one of {:?})",
                    self.phase, expected
                )
                .leak(),
            ))
        }
    }

    /// Verify that the incoming message's swap_id matches the state's swap_id.
    /// If the state has no swap_id yet (initial setup), this is a no-op.
    pub fn check_swap_id(&self, msg_swap_id: &str) -> Result<(), MakerError> {
        if let Some(ref expected) = self.swap_id {
            if expected != msg_swap_id {
                return Err(MakerError::General(
                    format!(
                        "Swap ID mismatch: state has '{}' but message has '{}'",
                        expected, msg_swap_id
                    )
                    .leak(),
                ));
            }
        }
        Ok(())
    }
}

/// Trait for maker operations.
pub trait Maker: Send + Sync {
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
    fn calculate_swap_fee(&self, amount: Amount, timelock: u32) -> Amount;

    /// Create a funding transaction for Legacy (P2WSH) address.
    fn create_funding_transaction(
        &self,
        amount: Amount,
        address: bitcoin::Address,
        excluded_outpoints: Option<Vec<bitcoin::OutPoint>>,
    ) -> Result<(Transaction, u32), MakerError>;

    /// Broadcast a transaction.
    fn broadcast_transaction(&self, tx: &Transaction) -> Result<bitcoin::Txid, MakerError>;

    /// Save incoming swapcoin to wallet.
    fn save_incoming_swapcoin(&self, swapcoin: &IncomingSwapCoin) -> Result<(), MakerError>;

    /// Save outgoing swapcoin to wallet.
    fn save_outgoing_swapcoin(&self, swapcoin: &OutgoingSwapCoin) -> Result<(), MakerError>;

    /// Register outpoint for watching.
    fn register_watch_outpoint(&self, outpoint: bitcoin::OutPoint);

    /// Unregister outpoint from watching (after swap completion).
    fn unwatch_outpoint(&self, outpoint: bitcoin::OutPoint);

    /// Sync wallet with Bitcoin Core and save state to disk.
    fn sync_and_save_wallet(&self) -> Result<(), MakerError>;

    /// Sweep incoming swapcoins after successful swap.
    fn sweep_incoming_swapcoins(&self) -> Result<(), MakerError>;

    /// Store connection state for persistence across connections.
    fn store_connection_state(&self, swap_id: &str, state: &ConnectionState);

    /// Retrieve stored connection state.
    fn get_connection_state(&self, swap_id: &str) -> Option<ConnectionState>;

    /// Remove connection state for a completed swap.
    fn remove_connection_state(&self, swap_id: &str);

    /// Get the data directory path for saving reports.
    fn data_dir(&self) -> &std::path::Path;

    /// Collect reserved UTXOs from all other active swaps (for concurrent double-spend prevention).
    fn collect_excluded_utxos(&self, current_swap_id: &str) -> Vec<bitcoin::OutPoint>;

    /// Get the current block height from the Bitcoin node.
    fn get_current_height(&self) -> Result<u32, MakerError>;

    /// Verify that a contract transaction is on-chain or in the mempool.
    fn verify_contract_tx_on_chain(&self, txid: &bitcoin::Txid) -> Result<(), MakerError>;

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
    #[allow(clippy::too_many_arguments)]
    fn initialize_coinswap(
        &self,
        send_amount: Amount,
        next_multisig_pubkeys: &[PublicKey],
        next_hashlock_pubkeys: &[PublicKey],
        hashvalue: crate::protocol::Hash160,
        locktime: u16,
        contract_feerate: f64,
        excluded_outpoints: Option<Vec<bitcoin::OutPoint>>,
    ) -> Result<(Vec<Transaction>, Vec<OutgoingSwapCoin>, Amount), MakerError>;

    /// Find outgoing swapcoin by its multisig redeemscript.
    fn find_outgoing_swapcoin(
        &self,
        multisig_redeemscript: &bitcoin::ScriptBuf,
    ) -> Option<OutgoingSwapCoin>;

    /// Get the test behavior override.
    #[cfg(feature = "integration-test")]
    fn behavior(&self) -> MakerBehavior;
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

/// Message handler
#[hotpath::measure]
pub fn handle_message<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
    message: TakerToMakerMessage,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.touch();

    log::debug!(
        "[{}] Handling message: {:?} (phase: {:?}, protocol: {:?})",
        Maker::network_port(maker.as_ref()),
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
        TakerToMakerMessage::LegacyPrivateKeyHandover(handover) => handle_legacy_dispatch(
            maker,
            state,
            LegacyTakerMessage::PrivateKeyHandover(handover),
        ),
        TakerToMakerMessage::TaprootContractData(data) => {
            handle_taproot_dispatch(maker, state, TaprootTakerMessage::ContractData(data))
        }
        TakerToMakerMessage::TaprootPrivateKeyHandover(handover) => handle_taproot_dispatch(
            maker,
            state,
            TaprootTakerMessage::PrivateKeyHandover(handover),
        ),
        TakerToMakerMessage::WaitingFundingConfirmation(ref id) => {
            log::info!(
                "[{}] Taker is waiting for funding confirmation (swap {}). Resetting timer.",
                maker.network_port(),
                id
            );
            state.touch();
            Ok(None)
        }
    }
}

/// Handle TakerHello message.
#[hotpath::measure]
fn handle_taker_hello<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
    _hello: TakerHello,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.expect_phase(&[SwapPhase::AwaitingHello])?;

    log::info!(
        "[{}] Received TakerHello",
        Maker::network_port(maker.as_ref()),
    );

    let config = maker.get_config();
    state.phase = SwapPhase::AwaitingOfferRequest;

    log::info!(
        "[{}] Supported protocols: {:?}",
        Maker::network_port(maker.as_ref()),
        config.supported_protocols
    );

    Ok(Some(MakerToTakerMessage::MakerHello(MakerHello {
        supported_protocols: config.supported_protocols,
    })))
}

/// Handle GetOffer message.
#[hotpath::measure]
fn handle_get_offer<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
    _get_offer: GetOffer,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.expect_phase(&[SwapPhase::AwaitingOfferRequest])?;

    log::info!(
        "[{}] Received GetOffer request",
        Maker::network_port(maker.as_ref())
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
        Maker::network_port(maker.as_ref()),
        offer.min_size,
        offer.max_size
    );

    Ok(Some(MakerToTakerMessage::Offer(Box::new(offer))))
}

/// Handle SwapDetails message.
#[hotpath::measure]
fn handle_swap_details<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
    details: SwapDetails,
) -> Result<Option<MakerToTakerMessage>, MakerError> {
    state.expect_phase(&[SwapPhase::AwaitingSwapDetails])?;

    log::info!(
        "[{}] Received SwapDetails: amount={}, timelock={}, protocol={:?}",
        Maker::network_port(maker.as_ref()),
        details.amount,
        details.timelock,
        details.protocol_version
    );

    maker.validate_swap_parameters(&details)?;

    state.swap_id = Some(details.id.clone());
    state.swap_amount = details.amount;
    state.timelock = details.timelock;
    state.refund_locktime_offset = details.refund_locktime_offset;
    state.protocol = details.protocol_version;
    state.phase = SwapPhase::AwaitingContractData;

    maker.store_connection_state(&details.id, state);

    let (_, tweakable_point) = maker.get_tweakable_keypair()?;

    log::info!(
        "[{}] Accepting swap (id: {})",
        Maker::network_port(maker.as_ref()),
        details.id
    );

    #[cfg(feature = "integration-test")]
    if maker.behavior() == MakerBehavior::CloseAfterAckResponse {
        log::warn!(
            "[{}] Test behavior: closing after AckSwapDetails",
            maker.network_port()
        );
        return Err(MakerError::General("Test: closing after ack response"));
    }

    Ok(Some(MakerToTakerMessage::AckSwapDetails(
        AckSwapDetails::accept(tweakable_point),
    )))
}

/// Restore connection state if this is a new/reconnected connection.
#[hotpath::measure]
fn restore_state_if_needed<M: Maker>(maker: &Arc<M>, state: &mut ConnectionState, swap_id: &str) {
    if state.swap_amount == Amount::ZERO || state.outgoing_swapcoins.is_empty() {
        if let Some(stored) = maker.get_connection_state(swap_id) {
            log::info!(
                "[{}] Restored state for {}: amount={}, timelock={}, phase={:?}, outgoing_count={}",
                maker.network_port(),
                swap_id,
                stored.swap_amount,
                stored.timelock,
                stored.phase,
                stored.outgoing_swapcoins.len()
            );
            state.swap_id = Some(swap_id.to_string());
            state.swap_amount = stored.swap_amount;
            state.timelock = stored.timelock;
            state.protocol = stored.protocol;
            state.phase = stored.phase;
            state.incoming_swapcoins = stored.incoming_swapcoins;
            state.outgoing_swapcoins = stored.outgoing_swapcoins;
            state.pending_funding_txes = stored.pending_funding_txes;
            state.funding_broadcast = stored.funding_broadcast;
            state.contract_feerate = stored.contract_feerate;
        }
    }
}

/// Dispatch to Legacy handlers.
#[hotpath::measure]
fn handle_legacy_dispatch<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
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
#[hotpath::measure]
fn handle_taproot_dispatch<M: Maker>(
    maker: &Arc<M>,
    state: &mut ConnectionState,
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
