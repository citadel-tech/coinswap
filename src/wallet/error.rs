//! All Wallet-related errors.

use crate::protocol::error::ProtocolError;

use super::fidelity::FidelityError;

/// Represents various errors that can occur within a wallet.
///
/// This enum consolidates errors from multiple sources such as I/O, CBOR parsing,
/// and custom application logic.
#[derive(Debug)]
pub enum WalletError {
    /// Represents a standard I/O error.
    ///
    /// Typically occurs during file or network operations.
    IO(std::io::Error),

    /// Represents an error during CBOR (Concise Binary Object Representation) serialization or deserialization.
    ///
    /// This is used for encoding/decoding data structures.
    Cbor(crate::utill::cbor::Error),

    /// Represents an error during JSON serialization or deserialization.
    ///
    /// This is used for encoding/decoding the wallet backup.
    Json(serde_json::Error),

    /// Represents an error returned by the Bitcoin Core RPC client.
    ///
    /// Typically occurs during communication with a Bitcoin node.
    Rpc(bitcoind::bitcoincore_rpc::Error),

    /// Represents an error related to BIP32 (Hierarchical Deterministic Wallets).
    ///
    /// This may occur during key derivation or wallet operations involving BIP32 paths.
    BIP32(bitcoin::bip32::Error),

    /// Represents an error related to BIP39 (Mnemonic Codes for Generating Deterministic Keys).
    ///
    /// Typically occurs when handling mnemonic phrases for seed generation.
    BIP39(bip39::Error),

    /// Represents a general error with a descriptive message.
    ///
    /// Use this variant for errors that do not fall under any specific category.
    General(String),

    /// Represents an error related to protocol violations or unexpected protocol behavior.
    Protocol(ProtocolError),

    /// Represents an error related to fidelity operations.
    ///
    /// Typically specific to application-defined fidelity processes.
    Fidelity(FidelityError),

    /// Represents an error with Bitcoin's locktime conversion.
    ///
    /// This occurs when converting between absolute and relative locktime representations.
    Locktime(bitcoin::blockdata::locktime::absolute::ConversionError),

    /// Represents an error from the Secp256k1 cryptographic library.
    ///
    /// Typically occurs during signature generation or verification.
    Secp(bitcoin::secp256k1::Error),

    /// Represents an error related to Bitcoin consensus rules.
    ///
    /// Use this variant to indicate issues related to transaction or block validation.
    Consensus(String),

    /// Represents an error when the wallet has insufficient funds to complete an operation.
    ///
    /// - `available`: The amount of funds available in the wallet.
    /// - `required`: The amount of funds needed to complete the operation.
    InsufficientFund {
        /// The amount of funds available in the wallet.
        available: u64,
        /// The amount of funds needed to complete the operation.
        required: u64,
    },

    /// Represents an error from the rust-coinselect library.
    ///
    /// Typically occurs during fee calculation or coin selection operations.
    Selection(rust_coinselect::types::SelectionError),
}

impl From<std::io::Error> for WalletError {
    fn from(e: std::io::Error) -> Self {
        Self::IO(e)
    }
}

impl From<bitcoind::bitcoincore_rpc::Error> for WalletError {
    fn from(value: bitcoind::bitcoincore_rpc::Error) -> Self {
        Self::Rpc(value)
    }
}

impl From<bitcoin::bip32::Error> for WalletError {
    fn from(value: bitcoin::bip32::Error) -> Self {
        Self::BIP32(value)
    }
}

impl From<bip39::Error> for WalletError {
    fn from(value: bip39::Error) -> Self {
        Self::BIP39(value)
    }
}

impl From<ProtocolError> for WalletError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}
impl From<crate::utill::cbor::Error> for WalletError {
    fn from(value: crate::utill::cbor::Error) -> Self {
        Self::Cbor(value)
    }
}

impl From<serde_json::Error> for WalletError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<FidelityError> for WalletError {
    fn from(value: FidelityError) -> Self {
        Self::Fidelity(value)
    }
}

impl From<bitcoin::blockdata::locktime::absolute::ConversionError> for WalletError {
    fn from(value: bitcoin::blockdata::locktime::absolute::ConversionError) -> Self {
        Self::Locktime(value)
    }
}

impl From<bitcoin::secp256k1::Error> for WalletError {
    fn from(value: bitcoin::secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<bitcoin::sighash::P2wpkhError> for WalletError {
    fn from(value: bitcoin::sighash::P2wpkhError) -> Self {
        Self::Consensus(value.to_string())
    }
}

impl From<bitcoin::sighash::TaprootError> for WalletError {
    fn from(value: bitcoin::sighash::TaprootError) -> Self {
        Self::Consensus(value.to_string())
    }
}

impl From<bitcoin::key::UncompressedPublicKeyError> for WalletError {
    fn from(value: bitcoin::key::UncompressedPublicKeyError) -> Self {
        Self::Consensus(value.to_string())
    }
}

impl From<bitcoin::transaction::InputsIndexError> for WalletError {
    fn from(value: bitcoin::transaction::InputsIndexError) -> Self {
        Self::Consensus(value.to_string())
    }
}

impl From<bitcoin::consensus::encode::Error> for WalletError {
    fn from(value: bitcoin::consensus::encode::Error) -> Self {
        Self::Consensus(value.to_string())
    }
}

impl From<rust_coinselect::types::SelectionError> for WalletError {
    fn from(value: rust_coinselect::types::SelectionError) -> Self {
        Self::Selection(value)
    }
}

impl std::fmt::Display for WalletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalletError::IO(e) => write!(f, "I/O error: {}", e),
            WalletError::Cbor(e) => write!(f, "CBOR error: {}", e),
            WalletError::Json(e) => write!(f, "JSON error: {}", e),
            WalletError::Rpc(e) => write!(f, "Bitcoin RPC error: {}", e),
            WalletError::BIP32(e) => write!(f, "BIP32 error: {}", e),
            WalletError::BIP39(e) => write!(f, "BIP39 error: {}", e),
            WalletError::General(msg) => write!(f, "{}", msg),
            WalletError::Protocol(e) => write!(f, "Protocol error: {}", e),
            WalletError::Fidelity(e) => write!(f, "Fidelity error: {}", e),
            WalletError::Locktime(e) => write!(f, "Locktime conversion error: {}", e),
            WalletError::Secp(e) => write!(f, "Secp256k1 error: {}", e),
            WalletError::Consensus(msg) => write!(f, "Consensus error: {}", msg),
            WalletError::InsufficientFund {
                available,
                required,
            } => {
                write!(
                    f,
                    "Insufficient funds: available {} sats, required {} sats",
                    available, required
                )
            }
            WalletError::Selection(e) => write!(f, "Coin selection error: {:?}", e),
        }
    }
}

impl std::error::Error for WalletError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WalletError::IO(e) => Some(e),
            WalletError::Cbor(e) => Some(e),
            WalletError::Json(e) => Some(e),
            WalletError::Rpc(e) => Some(e),
            WalletError::BIP32(e) => Some(e),
            WalletError::BIP39(e) => Some(e),
            WalletError::Locktime(e) => Some(e),
            WalletError::Secp(e) => Some(e),
            _ => None,
        }
    }
}
