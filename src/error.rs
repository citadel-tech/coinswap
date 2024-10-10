//! High-level network and protocol errors.

use std::error::Error;

use bitcoin::Amount;

use crate::protocol::error::ContractError;

/// Includes all network-related errors.
#[derive(Debug)]
pub enum NetError {
    IO(std::io::Error),
    ReachedEOF,
    ConnectionTimedOut,
    InvalidNetworkAddress,
    Cbor(serde_cbor::Error),
    InvalidAppNetwork,
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for NetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
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
