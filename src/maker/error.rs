//! All Maker related errors.

use std::sync::{MutexGuard, PoisonError, RwLockReadGuard, RwLockWriteGuard};

use bitcoin::secp256k1;

use crate::{
    error::{NetError, ProtocolError},
    protocol::error::ContractError,
    wallet::WalletError,
};

use super::MakerBehavior;

/// Represents errors that can occur during maker operations.
///
/// Encapsulates errors from:
/// - IO operations
/// - Protocol message handling
/// - Wallet operations
/// - Network operations
/// - Threading and synchronization
#[derive(Debug)]
pub enum MakerError {
    /// Standard IO errors during file operations.
    IO(std::io::Error),
    /// Protocol message mismatch between expected and received.
    UnexpectedMessage { expected: String, got: String },
    /// Static string describing a general error condition.
    General(&'static str),
    /// Threading error when a mutex is poisoned due to a thread panic.
    MutexPossion,
    /// Cryptographic operation errors from secp256k1.
    Secp(secp256k1::Error),
    /// Errors from wallet operations like signing or broadcasting.
    Wallet(WalletError),
    /// Network-related errors during connections or data transfer.
    Net(NetError),
    /// Testing-only errors triggered by special behavior modes.
    SpecialBehaviour(MakerBehavior),
    /// Errors related to swap protocol violations or failures.
    Protocol(ProtocolError),
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

impl From<ContractError> for MakerError {
    fn from(value: ContractError) -> Self {
        Self::Protocol(ProtocolError::from(value))
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

impl From<ProtocolError> for MakerError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}
