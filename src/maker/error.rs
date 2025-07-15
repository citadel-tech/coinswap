//! All Maker related errors.

use std::sync::{MutexGuard, PoisonError, RwLockReadGuard, RwLockWriteGuard};

use bitcoin::secp256k1;

use crate::{
    error::NetError, protocol::error::ProtocolError, utill::TorError, wallet::WalletError,
};

use super::MakerBehavior;

/// Enum to handle Maker-related errors.
///
/// This enum encapsulates different types of errors that can occur while interacting
/// with the maker. Each variant represents a specific category of error and provides
/// relevant details to help diagnose issues.
#[derive(Debug)]
pub enum MakerError {
    /// Represents a standard IO error.
    IO(std::io::Error),
    /// Represents an unexpected message received during communication.
    UnexpectedMessage {
        /// The expected message.
        expected: String,
        /// The received message.
        got: String,
    },
    /// Represents a general error with a static message.
    General(&'static str),
    /// Represents a mutex poisoning error.
    MutexPossion,
    /// Represents an error related to secp256k1 operations.
    Secp(secp256k1::Error),
    /// Represents an error related to wallet operations.
    Wallet(WalletError),
    /// Represents a network-related error.
    Net(NetError),
    /// Represents an error triggered by special maker behavior.
    SpecialBehaviour(MakerBehavior),
    /// Represents a protocol-related error.
    Protocol(ProtocolError),
    /// Tor Error.
    TorError(TorError),
}

impl From<TorError> for MakerError {
    fn from(value: TorError) -> Self {
        Self::TorError(value)
    }
}

impl From<std::io::Error> for MakerError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(value)
    }
}

impl From<serde_cbor::Error> for MakerError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Net(NetError::Cbor(value))
    }
}

impl<'a, T> From<PoisonError<RwLockReadGuard<'a, T>>> for MakerError {
    fn from(_: PoisonError<RwLockReadGuard<'a, T>>) -> Self {
        Self::MutexPossion
    }
}

impl<'a, T> From<PoisonError<RwLockWriteGuard<'a, T>>> for MakerError {
    fn from(_: PoisonError<RwLockWriteGuard<'a, T>>) -> Self {
        Self::MutexPossion
    }
}

impl<'a, T> From<PoisonError<MutexGuard<'a, T>>> for MakerError {
    fn from(_: PoisonError<MutexGuard<'a, T>>) -> Self {
        Self::MutexPossion
    }
}

impl From<secp256k1::Error> for MakerError {
    fn from(value: secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<ProtocolError> for MakerError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<WalletError> for MakerError {
    fn from(value: WalletError) -> Self {
        Self::Wallet(value)
    }
}

impl From<MakerBehavior> for MakerError {
    fn from(value: MakerBehavior) -> Self {
        Self::SpecialBehaviour(value)
    }
}

impl From<NetError> for MakerError {
    fn from(value: NetError) -> Self {
        Self::Net(value)
    }
}
