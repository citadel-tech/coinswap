//! All Maker related errors.

use std::sync::{MutexGuard, PoisonError, RwLockReadGuard, RwLockWriteGuard};

use bitcoin::secp256k1;

use crate::{
    error::{NetError, ProtocolError},
    protocol::error::ContractError,
    wallet::WalletError,
};

use super::MakerBehavior;

/// Enum to handle Maker related errors.
#[derive(Debug)]
pub enum MakerError {
    IO(std::io::Error),
    UnexpectedMessage { expected: String, got: String },
    General(&'static str),
    MutexPossion,
    Secp(secp256k1::Error),
    Wallet(WalletError),
    Net(NetError),
    SpecialBehaviour(MakerBehavior),
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
