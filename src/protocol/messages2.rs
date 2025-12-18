//! This module defines the messages communicated between the parties(Taker, Maker)
use crate::wallet::FidelityBond;
use bitcoin::{hashes::sha256::Hash, Amount, PublicKey, ScriptBuf, Txid};
use secp256k1::musig::{PartialSignature, PublicNonce};
use serde::{Deserialize, Serialize};
use std::{convert::TryInto, fmt::Display};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
/// Serializable wrapper for secp256k1 PublicNonce
pub struct SerializablePublicNonce(pub Vec<u8>);

/// Defines the length of the Preimage.
pub(crate) const PREIMAGE_LEN: usize = 32;

/// Type for Preimage.
pub(crate) type Preimage = [u8; PREIMAGE_LEN];

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
/// Serializable wrapper for secp256k1 Scalar
pub struct SerializableScalar(pub Vec<u8>);

impl From<PublicNonce> for SerializablePublicNonce {
    fn from(nonce: PublicNonce) -> Self {
        SerializablePublicNonce(nonce.serialize().to_vec())
    }
}

impl From<SerializablePublicNonce> for PublicNonce {
    fn from(nonce: SerializablePublicNonce) -> Self {
        let bytes: [u8; 66] = nonce.0.try_into().expect("invalid nonce value");
        PublicNonce::from_byte_array(&bytes).expect("invalid nonce value")
    }
}

impl From<bitcoin::secp256k1::Scalar> for SerializableScalar {
    fn from(scalar: bitcoin::secp256k1::Scalar) -> Self {
        SerializableScalar(scalar.to_be_bytes().to_vec())
    }
}

impl From<SerializableScalar> for bitcoin::secp256k1::Scalar {
    fn from(scalar: SerializableScalar) -> Self {
        let bytes: [u8; 32] = scalar.0.try_into().expect("invalid scalar length");
        bitcoin::secp256k1::Scalar::from_be_bytes(bytes).expect("invalid scalar value")
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
            Self::PrivateKeyHandover(_) => write!(f, "PrivateKeyHandover"),
        }
    }
}

impl Display for MakerToTakerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RespOffer(_) => write!(f, "RespOffer"),
            Self::AckResponse(_) => write!(f, "AckResponse"),
            Self::SenderContractFromMaker(_) => write!(f, "SenderContractFromMaker"),
            Self::PrivateKeyHandover(_) => write!(f, "PrivateKeyHandover"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
/// Serializable wrapper for secp256k1 PartialSignature
pub struct SerializablePartialSignature(pub Vec<u8>);

impl From<PartialSignature> for SerializablePartialSignature {
    fn from(sig: PartialSignature) -> Self {
        SerializablePartialSignature(sig.serialize().to_vec())
    }
}

impl From<SerializablePartialSignature> for PartialSignature {
    fn from(sig: SerializablePartialSignature) -> Self {
        let bytes: [u8; 32] = sig.0.try_into().expect("invalid signature value");
        PartialSignature::from_byte_array(&bytes).expect("invalid signature value")
    }
}

/// Represents a fidelity proof in the Coinswap protocol
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FidelityProof {
    pub(crate) bond: FidelityBond,
    pub(crate) cert_hash: Hash,
    pub(crate) cert_sig: bitcoin::secp256k1::ecdsa::Signature,
}

/// Private key handover message exchanged during taproot coinswap sweeps
///
/// After contract transactions are established and broadcasted, parties exchange
/// their outgoing contract private keys to enable independent sweeping without
/// requiring MuSig2 coordination.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PrivateKeyHandover {
    /// Swap ID being used as an identifier for the swap
    pub(crate) id: String,
    /// The outgoing contract private key
    pub(crate) secret_key: bitcoin::secp256k1::SecretKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum TakerToMakerMessage {
    GetOffer(GetOffer),
    SwapDetails(SwapDetails),
    SendersContract(SendersContract),
    PrivateKeyHandover(PrivateKeyHandover),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum MakerToTakerMessage {
    RespOffer(Box<Offer>),
    AckResponse(AckResponse),
    SenderContractFromMaker(SenderContractFromMaker),
    PrivateKeyHandover(PrivateKeyHandover),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GetOffer {
    pub(crate) id: String,
    pub(crate) protocol_version_min: u32,
    pub(crate) protocol_version_max: u32,
    pub(crate) number_of_transactions: u32,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SwapDetails {
    pub(crate) id: String,
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
    pub(crate) id: String,
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

/// Mempool transaction
#[derive(Serialize, Deserialize, Debug)]
pub struct MempoolTx {
    /// Txid of the transaction spending the utxo
    pub txid: String,
    /// Hex encoded raw transaction
    pub rawtx: String,
}
