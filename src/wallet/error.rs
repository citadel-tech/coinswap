//! All Wallet-related errors.

use super::fidelity::FidelityError;
use crate::protocol::error::ContractError;

/// Errors that can occur during wallet operations.
///
/// Encapsulates errors from:
/// - IO operations
/// - Serialization
/// - Bitcoin Core RPC
/// - Key derivation
/// - Contract handling
#[derive(Debug)]
pub enum WalletError {
    /// Standard IO errors.
    IO(std::io::Error),
    /// CBOR serialization errors.
    Cbor(serde_cbor::Error),
    /// Bitcoin Core RPC errors.
    Rpc(bitcoind::bitcoincore_rpc::Error),
    /// Protocol violation errors with description.
    Protocol(String),
    /// BIP32 hierarchical key derivation errors.
    BIP32(bitcoin::bip32::Error),
    /// BIP39 mnemonic handling errors.
    BIP39(bip39::Error),
    /// Contract creation or validation errors.
    Contract(ContractError),
    /// Fidelity bond operation errors.
    Fidelity(FidelityError),
    /// Timelock conversion errors.
    Locktime(bitcoin::blockdata::locktime::absolute::ConversionError),
    /// Secp256k1 cryptographic errors.
    Secp(bitcoin::secp256k1::Error),
    /// Consensus rule violation errors.
    Consensus(String),
    /// Insufficient funds for operation.
    InsufficientFund { available: f64, required: f64 },
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

impl From<ContractError> for WalletError {
    fn from(value: ContractError) -> Self {
        Self::Contract(value)
    }
}

impl From<serde_cbor::Error> for WalletError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Cbor(value)
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
