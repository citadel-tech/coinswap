//! Taproot (MuSig2) Protocol Messages (Phase 3-4).

use super::common_messages::PrivateKeyHandover;
use bitcoin::{
    secp256k1::{Scalar, SecretKey, XOnlyPublicKey},
    Amount, PublicKey, ScriptBuf, Transaction,
};
use serde::{Deserialize, Serialize};
use std::convert::TryInto;

/// Serializable wrapper for secp256k1 Scalar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerializableScalar(pub Vec<u8>);

impl SerializableScalar {
    /// Create from raw bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        SerializableScalar(bytes)
    }
}

impl From<Scalar> for SerializableScalar {
    fn from(scalar: Scalar) -> Self {
        SerializableScalar(scalar.to_be_bytes().to_vec())
    }
}

impl From<SerializableScalar> for Scalar {
    fn from(scalar: SerializableScalar) -> Self {
        let bytes: [u8; 32] = scalar.0.try_into().expect("invalid scalar length");
        Scalar::from_be_bytes(bytes).expect("invalid scalar value")
    }
}

/// Taproot contract data with MuSig2-specific fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaprootContractData {
    /// Unique swap ID.
    pub id: String,
    /// Public keys used in the contracts.
    pub pubkeys: Vec<PublicKey>,
    /// Tweakable point for the next hop.
    pub next_hop_point: PublicKey,
    /// The aggregated internal key for the Taproot output.
    pub internal_key: XOnlyPublicKey,
    /// The tap tweak applied to the internal key.
    pub tap_tweak: SerializableScalar,
    /// Hashlock script for preimage spending (script-path fallback).
    pub hashlock_script: ScriptBuf,
    /// Timelock script for timeout spending (script-path fallback).
    pub timelock_script: ScriptBuf,
    /// Contract transactions (in Taproot, the funding tx IS the contract).
    pub contract_txs: Vec<Transaction>,
    /// Contract amounts.
    pub amounts: Vec<Amount>,
    /// Nonce for the receiver to reconstruct hashlock_privkey.
    /// hashlock_privkey = tweakable_privkey + hashlock_nonce.
    /// None in maker→taker responses (taker already knows).
    #[serde(default)]
    pub hashlock_nonce: Option<SecretKey>,
    /// Nonce for the NEXT hop's hashlock script.
    /// Maker uses this: next_hashlock_pubkey = next_hop_point + next_hashlock_nonce * G.
    /// None for the last maker (next hop is taker, who uses its own key).
    #[serde(default)]
    pub next_hashlock_nonce: Option<SecretKey>,
}

impl TaprootContractData {
    /// Create a new TaprootContractData message.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        pubkeys: Vec<PublicKey>,
        next_hop_point: PublicKey,
        internal_key: XOnlyPublicKey,
        tap_tweak: SerializableScalar,
        hashlock_script: ScriptBuf,
        timelock_script: ScriptBuf,
        contract_txs: Vec<Transaction>,
        amounts: Vec<Amount>,
        hashlock_nonce: Option<SecretKey>,
        next_hashlock_nonce: Option<SecretKey>,
    ) -> Self {
        Self {
            id,
            pubkeys,
            next_hop_point,
            internal_key,
            tap_tweak,
            hashlock_script,
            timelock_script,
            contract_txs,
            amounts,
            hashlock_nonce,
            next_hashlock_nonce,
        }
    }

    /// Convenience method to get the tap tweak as a Scalar.
    pub fn tap_tweak_scalar(&self) -> bitcoin::secp256k1::Scalar {
        self.tap_tweak.clone().into()
    }
}

/// All Taproot-specific messages sent from Taker to Maker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaprootTakerMessage {
    /// Contract data exchange.
    ContractData(Box<TaprootContractData>),
    /// Private key handover.
    PrivateKeyHandover(PrivateKeyHandover),
}

impl TaprootTakerMessage {
    /// Returns the swap ID.
    pub fn swap_id(&self) -> &str {
        match self {
            Self::ContractData(data) => &data.id,
            Self::PrivateKeyHandover(handover) => &handover.id,
        }
    }
}
