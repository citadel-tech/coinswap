//! Top-Level Protocol Messages.

use serde::{Deserialize, Serialize};

use super::{
    common_messages::{
        AckSwapDetails, GetOffer, MakerHello, Offer, PrivateKeyHandover, SwapDetails, TakerHello,
    },
    legacy_messages::{
        LegacyHashPreimage, ProofOfFunding, ReqContractSigsAsRecvrAndSender,
        ReqContractSigsForRecvr, ReqContractSigsForSender, RespContractSigsForRecvr,
        RespContractSigsForRecvrAndSender, RespContractSigsForSender,
    },
    taproot_messages::{TaprootContractData, TaprootHashPreimage},
};

/// All messages sent from Taker to Maker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TakerToMakerMessage {
    /// Initial handshake with version negotiation.
    TakerHello(TakerHello),
    /// Request maker's offer.
    GetOffer(GetOffer),
    /// Propose swap parameters (determines protocol path).
    SwapDetails(SwapDetails),
    /// Request signatures for sender's contract (initial hop setup).
    ReqContractSigsForSender(ReqContractSigsForSender),
    /// Proof that funding transaction is confirmed.
    ProofOfFunding(ProofOfFunding),
    /// Response with both receiver and sender signatures.
    RespContractSigsForRecvrAndSender(RespContractSigsForRecvrAndSender),
    /// Request signatures for receiver's contract.
    ReqContractSigsForRecvr(ReqContractSigsForRecvr),
    /// Legacy hash preimage revelation.
    LegacyHashPreimage(LegacyHashPreimage),
    /// Legacy private key handover.
    LegacyPrivateKeyHandover(PrivateKeyHandover),
    /// Taproot contract data exchange (MuSig2).
    TaprootContractData(Box<TaprootContractData>),
    /// Taproot hash preimage revelation.
    TaprootHashPreimage(TaprootHashPreimage),
    /// Taproot private key handover.
    TaprootPrivateKeyHandover(PrivateKeyHandover),
}

/// All messages sent from Maker to Taker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MakerToTakerMessage {
    /// Handshake response with version negotiation.
    MakerHello(MakerHello),
    /// Maker's offer (fees, limits, fidelity bond).
    Offer(Box<Offer>),
    /// Acknowledgment of swap parameters.
    AckSwapDetails(AckSwapDetails),
    /// Response with signatures for sender's contract.
    RespContractSigsForSender(RespContractSigsForSender),
    /// Request signatures for both receiver and sender contracts.
    ReqContractSigsAsRecvrAndSender(ReqContractSigsAsRecvrAndSender),
    /// Response with signatures for receiver's contract.
    RespContractSigsForRecvr(RespContractSigsForRecvr),
    /// Legacy private key handover.
    LegacyPrivateKeyHandover(PrivateKeyHandover),
    /// Taproot contract data exchange (MuSig2).
    TaprootContractData(Box<TaprootContractData>),
    /// Taproot private key handover.
    TaprootPrivateKeyHandover(PrivateKeyHandover),
}
