//! High-level network and protocol errors.

use bitcoin::Amount;

use crate::maker::MakerError;
use crate::market::directory::DirectoryServerError;
use crate::protocol::error::ContractError;
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
/// Enum to handle applicaiton related errors at the binary level
#[derive(Debug)]
pub enum AppError {
    Maker(MakerError),
    Taker(TakerError),
    DNS(DirectoryServerError),
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

impl From<MakerError> for AppError {
    fn from(value: MakerError) -> Self {
        AppError::Maker(value)
    }
}

impl From<TakerError> for AppError {
    fn from(value: TakerError) -> Self {
        AppError::Taker(value)
    }
}

impl From<DirectoryServerError> for AppError {
    fn from(value: DirectoryServerError) -> Self {
        AppError::DNS(value)
    }
}

impl From<std::io::Error> for AppError{
    fn from(value: std::io::Error) -> Self {
        AppError::Maker(MakerError::IO(value))
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
