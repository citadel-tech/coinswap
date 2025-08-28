//! This module defines the messages communicated between the parties(Taker, Maker and DNS)
use crate::wallet::FidelityBond;
use bitcoin::hashes::sha256::Hash;
use bitcoin::{Amount, PublicKey, ScriptBuf, Transaction, Txid};
use secp256k1::musig::{PartialSignature, PublicNonce};
use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use std::fmt::Display;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
/// Serializable wrapper for secp256k1 PublicNonce
pub struct SerializablePublicNonce(#[serde(with = "serde_bytes")] pub Vec<u8>);

/// Defines the length of the Preimage.
pub(crate) const PREIMAGE_LEN: usize = 32;

/// Type for Preimage.
pub(crate) type Preimage = [u8; PREIMAGE_LEN];

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
/// Serializable wrapper for secp256k1 Scalar
pub struct SerializableScalar(#[serde(with = "serde_bytes")] pub Vec<u8>);

impl From<PublicNonce> for SerializablePublicNonce {
    fn from(nonce: PublicNonce) -> Self {
        SerializablePublicNonce(nonce.serialize().to_vec())
    }
}

impl From<SerializablePublicNonce> for PublicNonce {
    fn from(nonce: SerializablePublicNonce) -> Self {
        if nonce.0.len() != 66 {
            panic!(
                "Invalid nonce byte length: expected 66, got {}",
                nonce.0.len()
            );
        }
        let bytes: [u8; 66] = nonce.0.try_into().unwrap();
        PublicNonce::from_byte_array(&bytes).expect("valid nonce bytes")
    }
}

impl From<bitcoin::secp256k1::Scalar> for SerializableScalar {
    fn from(scalar: bitcoin::secp256k1::Scalar) -> Self {
        SerializableScalar(scalar.to_be_bytes().to_vec())
    }
}

impl From<SerializableScalar> for bitcoin::secp256k1::Scalar {
    fn from(scalar: SerializableScalar) -> Self {
        if scalar.0.len() != 32 {
            panic!(
                "Invalid scalar byte length: expected 32, got {}",
                scalar.0.len()
            );
        }
        let bytes: [u8; 32] = scalar.0.try_into().unwrap();
        bitcoin::secp256k1::Scalar::from_be_bytes(bytes).expect("valid scalar bytes")
    }
}

// Note: Nonces should be generated using proper MuSig2 procedures with transaction context,
// not converted from arbitrary secret keys

impl Display for TakerToMakerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GetOffer(_) => write!(f, "GetOffer"),
            Self::SwapDetails(_) => write!(f, "SwapDetails"),
            Self::SendersContract(_) => write!(f, "SendersContract"),
            Self::SpendingTxAndReceiverNonce(_) => write!(f, "SpendingTxAndReceiverNonce"),
            Self::PartialSigAndSendersNonce(_) => write!(f, "PartialSigAndSendersNonce"),
        }
    }
}

impl Display for MakerToTakerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RespOffer(_) => write!(f, "RespOffer"),
            Self::AckResponse(_) => write!(f, "AckResponse"),
            Self::SenderContractFromMaker(_) => write!(f, "SenderContractFromMaker"),
            Self::NoncesPartialSigsAndSpendingTx(_) => write!(f, "NoncesPartialSigsAndSpendingTx"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
/// Serializable wrapper for secp256k1 PartialSignature
pub struct SerializablePartialSignature(#[serde(with = "serde_bytes")] pub Vec<u8>);

impl From<PartialSignature> for SerializablePartialSignature {
    fn from(sig: PartialSignature) -> Self {
        SerializablePartialSignature(sig.serialize().to_vec())
    }
}

impl From<SerializablePartialSignature> for PartialSignature {
    fn from(sig: SerializablePartialSignature) -> Self {
        let bytes: [u8; 32] = sig.0.try_into().expect("valid signature bytes");
        PartialSignature::from_byte_array(&bytes).expect("valid signature bytes")
    }
}

/// Fidelity proof for the maker
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FidelityProof {
    pub(crate) bond: FidelityBond,
    pub(crate) cert_hash: Hash,
    pub(crate) cert_sig: bitcoin::secp256k1::ecdsa::Signature,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum TakerToMakerMessage {
    GetOffer(GetOffer),
    SwapDetails(SwapDetails),
    SendersContract(SendersContract),
    SpendingTxAndReceiverNonce(SpendingTxAndReceiverNonce),
    PartialSigAndSendersNonce(PartialSigAndSendersNonce),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum MakerToTakerMessage {
    RespOffer(Box<Offer>),
    AckResponse(AckResponse),
    SenderContractFromMaker(SenderContractFromMaker),
    NoncesPartialSigsAndSpendingTx(NoncesPartialSigsAndSpendingTx),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GetOffer {
    pub(crate) protocol_version_min: u32,
    pub(crate) protocol_version_max: u32,
    pub(crate) number_of_transactions: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ContractInfo {
    pub(crate) contract_tx: Transaction,
    pub(crate) hashlock_script: ScriptBuf,
    pub(crate) timelock_script: ScriptBuf,
    pub(crate) sender_nonce: Option<SerializablePublicNonce>,
    pub(crate) receiver_nonce: Option<SerializablePublicNonce>,
}

/// An offer from a maker to participate in a coinswap
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Offer {
    /// The tweakable public key for the maker
    pub tweakable_point: PublicKey,
    /// Base fee charged by the maker (in satoshis)
    pub base_fee: u64,
    /// Fee as a percentage relative to the swap amount
    pub amount_relative_fee: f64,
    /// Fee as a percentage relative to the time lock duration
    pub time_relative_fee: f64,
    /// Minimum time lock duration required by the maker
    pub minimum_locktime: u16,
    /// Fidelity proof demonstrating the maker's commitment
    pub fidelity: FidelityProof,
    /// Minimum swap amount the maker will accept (in satoshis)
    pub min_size: u64,
    /// Maximum swap amount the maker can handle (in satoshis)
    pub max_size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SwapDetails {
    pub(crate) amount: Amount,
    pub(crate) no_of_tx: u8,
    pub(crate) timelock: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum AckResponse {
    Ack,
    Nack,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct SendersContract {
    pub(crate) contract_txs: Vec<Txid>,
    // Below data is used to verify transaction
    pub(crate) pubkeys_a: Vec<PublicKey>,
    pub(crate) hashlock_scripts: Vec<ScriptBuf>,
    pub(crate) timelock_scripts: Vec<ScriptBuf>,
    // Tweakable point for allowing maker to create next contract
    pub(crate) next_party_tweakable_point: bitcoin::PublicKey,
    // MuSig2 data for cooperative spending
    pub(crate) internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>,
    pub(crate) tap_tweak: Option<SerializableScalar>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SenderContractFromMaker {
    pub(crate) contract_txs: Vec<Txid>,
    // Below data is used to verify transaction
    pub(crate) pubkeys_a: Vec<PublicKey>,
    pub(crate) hashlock_scripts: Vec<ScriptBuf>,
    pub(crate) timelock_scripts: Vec<ScriptBuf>,
    // MuSig2 data for cooperative spending
    pub(crate) internal_key: Option<bitcoin::secp256k1::XOnlyPublicKey>,
    pub(crate) tap_tweak: Option<SerializableScalar>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct PartialSignaturesAndNonces {
    pub(crate) partial_signatures: Vec<SerializablePartialSignature>,
    pub(crate) sender_nonces: Vec<SerializablePublicNonce>,
    pub(crate) receiver_nonces: Vec<SerializablePublicNonce>,
    // Transaction structure for cooperative spending - the sweeping party constructs this
    pub(crate) spending_transaction: Option<Transaction>,
}

#[derive(Debug, Serialize, Deserialize)]
/// Response from DNS server for registration requests
pub enum DnsResponse {
    /// Acknowledgment that the registration was successful
    Ack,
    /// Negative acknowledgment with a reason for rejection
    Nack(String),
}

impl std::fmt::Display for DnsResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsResponse::Ack => write!(f, "Ack"),
            DnsResponse::Nack(reason) => write!(f, "Nack: {}", reason),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
/// Metadata for the maker
pub struct DnsMetadata {
    pub(crate) url: String,
    pub(crate) proof: FidelityProof,
}

#[derive(Debug, Serialize, Deserialize)]
/// Request to DNS server for maker registration
pub enum DnsRequest {
    /// Post request to register maker metadata
    Post {
        /// Metadata containing maker information and fidelity proof
        metadata: DnsMetadata,
    },
    /// Get request to retrieve maker list
    Get,
}

// New backwards sweeping protocol message types

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SpendingTxAndReceiverNonce {
    pub(crate) spending_transaction: Transaction,
    pub(crate) receiver_nonce: SerializablePublicNonce,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NoncesPartialSigsAndSpendingTx {
    pub(crate) sender_nonce: SerializablePublicNonce,
    pub(crate) receiver_nonce: SerializablePublicNonce,
    pub(crate) partial_signatures: Vec<SerializablePartialSignature>,
    pub(crate) spending_transaction: Transaction,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PartialSigAndSendersNonce {
    pub(crate) partial_signatures: Vec<SerializablePartialSignature>,
    pub(crate) sender_nonce: SerializablePublicNonce,
}
