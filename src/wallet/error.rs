//! All Wallet-related errors.

use super::fidelity::FidelityError;
use crate::protocol::error::ContractError;

/// Enum for handling wallet-related errors.
#[derive(Debug)]
pub enum WalletError {
    File(std::io::Error),
    Cbor(serde_cbor::Error),
    Rpc(bitcoind::bitcoincore_rpc::Error),
    Protocol(String),
    BIP32(bitcoin::bip32::Error),
    BIP39(bip39::Error),
    Contract(ContractError),
    Fidelity(FidelityError),
    Locktime(bitcoin::blockdata::locktime::absolute::ConversionError),
    Secp(bitcoin::secp256k1::Error),
}

impl From<std::io::Error> for WalletError {
    fn from(e: std::io::Error) -> Self {
        Self::File(e)
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
