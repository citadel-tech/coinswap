//! All Wallet-related errors.

use crate::protocol::error::ProtocolError;

use super::fidelity::FidelityError;

/// Enum for handling wallet-related errors.
#[derive(Debug)]
pub enum WalletError {
    IO(std::io::Error),
    Cbor(serde_cbor::Error),
    Rpc(bitcoind::bitcoincore_rpc::Error),
    BIP32(bitcoin::bip32::Error),
    BIP39(bip39::Error),
    General(String),
    Protocol(ProtocolError),
    Fidelity(FidelityError),
    Locktime(bitcoin::blockdata::locktime::absolute::ConversionError),
    Secp(bitcoin::secp256k1::Error),
    Consensus(String),
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

impl From<ProtocolError> for WalletError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
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

impl From<bitcoin::consensus::encode::Error> for WalletError {
    fn from(value: bitcoin::consensus::encode::Error) -> Self {
        Self::Consensus(value.to_string())
    }
}
