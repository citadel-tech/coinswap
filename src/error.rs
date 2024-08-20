//! High-level network and protocol errors.

use bitcoin::Amount;

use crate::protocol::error::ContractError;
use crate::maker::error::MakerError;
use crate::taker::error::TakerError;

/// Includes all network-related errors.
#[derive(Debug)]
pub enum NetError {
    IO(std::io::Error),
    ReachedEOF,
    ConnectionTimedOut,
    InvalidNetworkAddress,
    Cbor(serde_cbor::Error),
}

pub enum AppError {
    MakerError(MakerError), 
    TakerError(TakerError),
    ThreadPanic,
}

impl From<std::io::Error> for NetError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(value)
    }
}

impl From<serde_cbor::Error> for NetError {
    fn from(value: serde_cbor::Error) -> Self {
        Self::Cbor(value)
    }
}

impl From<MakerError> for AppError { // Do we need AppError to directly handle MutexPoisson, walleterror etc? or is this sufficient?
    fn from(error: MakerError) -> Self {
        AppError::MakerError(error)
    }
}

impl From<TakerError> for AppError {
    fn from(error: TakerError) -> Self {
        AppError::TakerError(error)
    }
}
/// Includes all Protocol-level errors.
#[derive(Debug)]
pub enum ProtocolError {
    WrongMessage { expected: String, received: String },
    WrongNumOfSigs { expected: usize, received: usize },
    WrongNumOfContractTxs { expected: usize, received: usize },
    WrongNumOfPrivkeys { expected: usize, received: usize },
    IncorrectFundingAmount { expected: Amount, found: Amount },
    Contract(ContractError),
}

impl From<ContractError> for ProtocolError {
    fn from(value: ContractError) -> Self {
        Self::Contract(value)
    }
}
