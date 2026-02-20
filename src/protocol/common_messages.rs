//! Common Coinswap Protocol Messages (Shared Phase 1-2).

use bitcoin::{Amount, PublicKey};
use serde::{Deserialize, Serialize};

// Re-export FidelityProof from messages.rs (single canonical definition).
pub use crate::protocol::messages::FidelityProof;

/// Protocol version identifier for coinswap operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ProtocolVersion {
    /// Legacy ECDSA-based protocol with script-evaluated HTLCs.
    #[default]
    Legacy,
    /// Taproot MuSig2-based protocol with scriptless contracts.
    Taproot,
}

/// Initial handshake from Taker to Maker.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TakerHello;

/// Handshake response from Maker to Taker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MakerHello {
    /// Protocol versions this maker supports.
    pub supported_protocols: Vec<ProtocolVersion>,
}

/// Request for offer from Taker to Maker.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GetOffer;

/// Maker's offer advertisement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Offer {
    /// Base fee charged per swap in satoshis (fixed cost component).
    pub base_fee: u64,
    /// Percentage fee relative to swap amount.
    pub amount_relative_fee_pct: f64,
    /// Percentage fee for time-locked funds.
    pub time_relative_fee_pct: f64,
    /// Minimum confirmations required before proceeding with swap.
    pub required_confirms: u32,
    /// Minimum timelock duration in blocks for contract transactions.
    pub minimum_locktime: u16,
    /// Maximum swap amount accepted in sats.
    pub max_size: u64,
    /// Minimum swap amount accepted in sats.
    pub min_size: u64,
    /// Tweakable public key for receiving swaps.
    /// Actual swap addresses are derived using unique nonces per swap.
    pub tweakable_point: PublicKey,
    /// Cryptographic proof of fidelity bond for Sybil resistance.
    pub fidelity: FidelityProof,
}

/// Swap details from Taker to Maker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwapDetails {
    /// Unique 8-byte ID to identify this swap.
    pub id: String,
    /// Protocol version to use for this swap.
    pub protocol_version: ProtocolVersion,
    /// Amount to swap in satoshis.
    pub amount: Amount,
    /// Number of contract transactions.
    pub tx_count: u32,
    /// Timelock value.
    /// - Legacy: relative block count (CSV).
    /// - Taproot: absolute block height (CLTV).
    pub timelock: u32,
}

/// Acknowledgment of swap details from Maker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AckSwapDetails {
    /// Whether the swap is accepted.
    /// If Some, contains the tweakable point for this swap.
    /// If None, swap is rejected.
    pub tweakable_point: Option<PublicKey>,
}

impl AckSwapDetails {
    /// Create an acceptance response.
    pub fn accept(tweakable_point: PublicKey) -> Self {
        AckSwapDetails {
            tweakable_point: Some(tweakable_point),
        }
    }
}

/// A private key exchanged during swap completion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwapPrivkey {
    /// The redeemscript (ECDSA) or script identifier (Taproot) this key belongs to.
    pub identifier: bitcoin::ScriptBuf,
    /// The private key.
    pub key: bitcoin::secp256k1::SecretKey,
}

/// Private key handover for swap completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivateKeyHandover {
    /// Unique swap ID.
    pub id: String,
    /// Private keys for cooperative spending.
    pub privkeys: Vec<SwapPrivkey>,
}
